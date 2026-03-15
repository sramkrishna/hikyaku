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
        pub on_room_selected: RefCell<Option<Box<dyn Fn(String)>>>,
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
                                cb(room_obj.room_id());
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

    pub fn connect_room_selected<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_room_selected.replace(Some(Box::new(f)));
    }

    pub fn update_rooms(&self, rooms: &[RoomInfo]) {
        let store = &self.imp().list_store;
        store.remove_all();

        // Separate "Empty Room" entries for cleanup suggestions.
        let is_empty_room = |r: &&RoomInfo| {
            r.name.eq_ignore_ascii_case("empty room")
        };

        // Group rooms by section.
        let mut dms = Vec::new();
        let mut by_space: BTreeMap<String, Vec<&RoomInfo>> = BTreeMap::new();
        let mut ungrouped = Vec::new();
        let mut cleanup = Vec::new();

        for r in rooms {
            if is_empty_room(&&r) {
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

        // Sort helper: pinned first, then by last_activity_ts descending.
        let sort_by_activity = |a: &&RoomInfo, b: &&RoomInfo| {
            b.is_pinned.cmp(&a.is_pinned)
                .then(b.last_activity_ts.cmp(&a.last_activity_ts))
        };

        // Sort each group by activity.
        dms.sort_by(sort_by_activity);
        ungrouped.sort_by(sort_by_activity);
        for rooms in by_space.values_mut() {
            rooms.sort_by(sort_by_activity);
        }

        // DMs section.
        if !dms.is_empty() {
            store.append(&RoomObject::new_header("Direct Messages"));
            for r in &dms {
                store.append(&Self::room_to_obj(r));
            }
        }

        // Each space section.
        for (space_name, space_rooms) in &by_space {
            store.append(&RoomObject::new_header(space_name));
            for r in space_rooms {
                store.append(&Self::room_to_obj(r));
            }
        }

        // Ungrouped rooms.
        if !ungrouped.is_empty() {
            store.append(&RoomObject::new_header("Rooms"));
            for r in &ungrouped {
                store.append(&Self::room_to_obj(r));
            }
        }

        // Empty/cleanup rooms at the bottom.
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
