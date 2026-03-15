// RoomListView — the sidebar listing all joined rooms.
//
// Rooms are grouped into sections: DMs, each space, and ungrouped rooms.
// Section headers are inserted as pseudo-items in the ListStore with
// is_header=true.

mod imp {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use std::cell::RefCell;

    use crate::models::RoomObject;
    use crate::widgets::room_row::RoomRow;

    pub struct RoomListView {
        pub list_store: gio::ListStore,
        pub selection: gtk::SingleSelection,
        pub list_view: gtk::ListView,
        pub on_room_selected: RefCell<Option<Box<dyn Fn(String, String)>>>,
    }

    impl Default for RoomListView {
        fn default() -> Self {
            let list_store = gio::ListStore::new::<RoomObject>();

            let factory = gtk::SignalListItemFactory::new();

            factory.connect_setup(|_factory, list_item| {
                let list_item = list_item
                    .downcast_ref::<gtk::ListItem>()
                    .expect("ListItem expected");
                list_item.set_child(Some(&RoomRow::new()));
            });

            factory.connect_bind(|_factory, list_item| {
                let list_item = list_item
                    .downcast_ref::<gtk::ListItem>()
                    .expect("ListItem expected");
                let room_obj = list_item
                    .item()
                    .and_downcast::<RoomObject>()
                    .expect("RoomObject expected");
                let row = list_item
                    .child()
                    .and_downcast::<RoomRow>()
                    .expect("RoomRow expected");

                row.bind_room(&room_obj);

                // Headers shouldn't be selectable/activatable.
                list_item.set_selectable(!room_obj.is_header());
                list_item.set_activatable(!room_obj.is_header());
            });

            let selection = gtk::SingleSelection::new(Some(list_store.clone()));
            let list_view = gtk::ListView::builder()
                .model(&selection)
                .factory(&factory)
                .css_classes(["navigation-sidebar"])
                .build();

            Self {
                list_store,
                selection,
                list_view,
                on_room_selected: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RoomListView {
        const NAME: &'static str = "MxRoomListView";
        type Type = super::RoomListView;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for RoomListView {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);
            obj.set_vexpand(true);

            let scrolled = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&self.list_view)
                .build();

            obj.append(&scrolled);

            // When the user clicks a room, fire the callback.
            let view = obj.clone();
            self.selection.connect_selection_changed(move |selection, _, _| {
                let imp = view.imp();
                if let Some(item) = selection.selected_item() {
                    if let Some(room_obj) = item.downcast_ref::<crate::models::RoomObject>() {
                        // Ignore header clicks.
                        if !room_obj.is_header() {
                            if let Some(ref cb) = *imp.on_room_selected.borrow() {
                                cb(room_obj.room_id(), room_obj.name());
                            }
                        }
                    }
                }
            });
        }
    }

    impl WidgetImpl for RoomListView {}
    impl BoxImpl for RoomListView {}
}

use gtk::glib;
use gtk::subclass::prelude::*;
use std::collections::BTreeMap;

use crate::matrix::{RoomInfo, RoomKind};
use crate::models::RoomObject;

glib::wrapper! {
    pub struct RoomListView(ObjectSubclass<imp::RoomListView>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::Orientable;
}

impl RoomListView {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn connect_room_selected<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_room_selected.replace(Some(Box::new(f)));
    }

    pub fn update_rooms(&self, rooms: &[RoomInfo]) {
        let store = &self.imp().list_store;
        store.remove_all();

        let (dms, by_space, ungrouped, cleanup) = group_and_sort_rooms(rooms);

        if !dms.is_empty() {
            store.append(&RoomObject::new_header("Direct Messages"));
            for r in &dms {
                store.append(&Self::room_to_obj(r));
            }
        }

        for (space_name, space_rooms) in &by_space {
            store.append(&RoomObject::new_header(space_name));
            for r in space_rooms {
                store.append(&Self::room_to_obj(r));
            }
        }

        if !ungrouped.is_empty() {
            store.append(&RoomObject::new_header("Rooms"));
            for r in &ungrouped {
                store.append(&Self::room_to_obj(r));
            }
        }

        if !cleanup.is_empty() {
            store.append(&RoomObject::new_header("Suggested Cleanup"));
            for r in &cleanup {
                store.append(&Self::room_to_obj(r));
            }
        }
    }

    fn room_to_obj(r: &RoomInfo) -> RoomObject {
        let kind_str = match r.kind {
            RoomKind::DirectMessage => "dm",
            RoomKind::Room => "room",
            RoomKind::Space => "space",
        };
        RoomObject::new(
            &r.room_id,
            &r.name,
            kind_str,
            r.is_encrypted,
            r.parent_space.as_deref().unwrap_or(""),
            r.is_pinned,
        )
    }
}

/// Group rooms into sections and sort within each section.
/// Returns (dms, by_space, ungrouped, cleanup).
pub(crate) fn group_and_sort_rooms(
    rooms: &[RoomInfo],
) -> (
    Vec<&RoomInfo>,
    BTreeMap<String, Vec<&RoomInfo>>,
    Vec<&RoomInfo>,
    Vec<&RoomInfo>,
) {
    let mut dms = Vec::new();
    let mut by_space: BTreeMap<String, Vec<&RoomInfo>> = BTreeMap::new();
    let mut ungrouped = Vec::new();
    let mut cleanup = Vec::new();

    for r in rooms {
        if r.name.eq_ignore_ascii_case("empty room") {
            cleanup.push(r);
            continue;
        }
        match r.kind {
            RoomKind::DirectMessage => dms.push(r),
            RoomKind::Room | RoomKind::Space => {
                if let Some(ref space) = r.parent_space {
                    by_space.entry(space.clone()).or_default().push(r);
                } else {
                    ungrouped.push(r);
                }
            }
        }
    }

    let sort_by_activity = |a: &&RoomInfo, b: &&RoomInfo| {
        b.is_pinned
            .cmp(&a.is_pinned)
            .then(b.last_activity_ts.cmp(&a.last_activity_ts))
    };

    dms.sort_by(sort_by_activity);
    ungrouped.sort_by(sort_by_activity);
    for rooms in by_space.values_mut() {
        rooms.sort_by(sort_by_activity);
    }

    (dms, by_space, ungrouped, cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_room(
        name: &str,
        kind: RoomKind,
        parent_space: Option<&str>,
        is_pinned: bool,
        last_activity_ts: u64,
    ) -> RoomInfo {
        RoomInfo {
            room_id: format!("!{}:matrix.org", name.to_lowercase().replace(' ', "_")),
            name: name.to_string(),
            last_activity_ts,
            kind,
            is_encrypted: false,
            parent_space: parent_space.map(|s| s.to_string()),
            is_pinned,
        }
    }

    #[test]
    fn test_dm_grouping() {
        let rooms = vec![
            make_room("Alice", RoomKind::DirectMessage, None, false, 100),
            make_room("General", RoomKind::Room, None, false, 200),
        ];
        let (dms, _, ungrouped, _) = group_and_sort_rooms(&rooms);
        assert_eq!(dms.len(), 1);
        assert_eq!(dms[0].name, "Alice");
        assert_eq!(ungrouped.len(), 1);
        assert_eq!(ungrouped[0].name, "General");
    }

    #[test]
    fn test_space_grouping() {
        let rooms = vec![
            make_room("Dev Chat", RoomKind::Room, Some("Work"), false, 100),
            make_room("Random", RoomKind::Room, Some("Work"), false, 200),
            make_room("General", RoomKind::Room, None, false, 300),
        ];
        let (_, by_space, ungrouped, _) = group_and_sort_rooms(&rooms);
        assert_eq!(by_space.len(), 1);
        assert_eq!(by_space["Work"].len(), 2);
        assert_eq!(ungrouped.len(), 1);
    }

    #[test]
    fn test_empty_room_cleanup() {
        let rooms = vec![
            make_room("Empty Room", RoomKind::Room, None, false, 100),
            make_room("empty room", RoomKind::DirectMessage, None, false, 50),
            make_room("General", RoomKind::Room, None, false, 200),
        ];
        let (dms, _, ungrouped, cleanup) = group_and_sort_rooms(&rooms);
        assert_eq!(cleanup.len(), 2);
        assert!(dms.is_empty());
        assert_eq!(ungrouped.len(), 1);
    }

    #[test]
    fn test_pinned_sort_first() {
        let rooms = vec![
            make_room("Old Pinned", RoomKind::DirectMessage, None, true, 1),
            make_room("Recent", RoomKind::DirectMessage, None, false, 9999),
        ];
        let (dms, _, _, _) = group_and_sort_rooms(&rooms);
        assert_eq!(dms[0].name, "Old Pinned");
        assert_eq!(dms[1].name, "Recent");
    }

    #[test]
    fn test_activity_sort_descending() {
        let rooms = vec![
            make_room("Old", RoomKind::Room, None, false, 100),
            make_room("New", RoomKind::Room, None, false, 500),
            make_room("Middle", RoomKind::Room, None, false, 300),
        ];
        let (_, _, ungrouped, _) = group_and_sort_rooms(&rooms);
        assert_eq!(ungrouped[0].name, "New");
        assert_eq!(ungrouped[1].name, "Middle");
        assert_eq!(ungrouped[2].name, "Old");
    }

    #[test]
    fn test_pinned_before_activity() {
        let rooms = vec![
            make_room("Unpinned Active", RoomKind::DirectMessage, None, false, 9999),
            make_room("Pinned Stale", RoomKind::DirectMessage, None, true, 1),
            make_room("Pinned Active", RoomKind::DirectMessage, None, true, 500),
        ];
        let (dms, _, _, _) = group_and_sort_rooms(&rooms);
        // Pinned rooms first, sorted by activity among themselves.
        assert_eq!(dms[0].name, "Pinned Active");
        assert_eq!(dms[1].name, "Pinned Stale");
        assert_eq!(dms[2].name, "Unpinned Active");
    }

    #[test]
    fn test_multiple_spaces_sorted_alphabetically() {
        let rooms = vec![
            make_room("Zeta", RoomKind::Room, Some("Zebra"), false, 100),
            make_room("Alpha", RoomKind::Room, Some("Aardvark"), false, 200),
        ];
        let (_, by_space, _, _) = group_and_sort_rooms(&rooms);
        let keys: Vec<&String> = by_space.keys().collect();
        assert_eq!(keys, vec!["Aardvark", "Zebra"]);
    }

    #[test]
    fn test_empty_input() {
        let (dms, by_space, ungrouped, cleanup) = group_and_sort_rooms(&[]);
        assert!(dms.is_empty());
        assert!(by_space.is_empty());
        assert!(ungrouped.is_empty());
        assert!(cleanup.is_empty());
    }
}
