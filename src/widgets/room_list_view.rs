// RoomListView — the sidebar with tabbed views for Messages, Rooms, and Spaces.
//
// Uses AdwViewStack + AdwViewSwitcherBar to separate the three categories.
// The Spaces tab supports drill-down: clicking a space shows its child rooms
// in a sub-list with a back button.

mod imp {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use std::cell::RefCell;

    use crate::models::RoomObject;
    use crate::widgets::room_row::RoomRow;

    /// Create a subtle join button with icon + label.
    fn create_banner_button(icon_name: &str, label: &str, css_class: &str) -> gtk::Button {
        let button = gtk::Button::new();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Center)
            .build();
        content.append(&gtk::Image::from_icon_name(icon_name));
        let lbl = gtk::Label::new(Some(label));
        lbl.add_css_class("caption");
        content.append(&lbl);
        button.set_child(Some(&content));
        button.add_css_class("flat");
        button.add_css_class(css_class);
        button
    }

    /// Build a (ListStore, SingleSelection, ListView) triple for one tab.
    fn make_room_list() -> (gio::ListStore, gtk::SingleSelection, gtk::ListView) {
        let store = gio::ListStore::new::<RoomObject>();

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
            list_item.set_selectable(!room_obj.is_header());
            list_item.set_activatable(!room_obj.is_header());
        });

        factory.connect_unbind(|_factory, list_item| {
            let list_item = list_item
                .downcast_ref::<gtk::ListItem>()
                .expect("ListItem expected");
            if let Some(row) = list_item.child().and_downcast::<RoomRow>() {
                row.unbind_room();
            }
        });

        let selection = gtk::SingleSelection::new(Some(store.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        selection.set_selected(gtk::INVALID_LIST_POSITION);
        let list_view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .css_classes(["navigation-sidebar"])
            .build();

        (store, selection, list_view)
    }

    pub struct RoomListView {
        pub dm_store: gio::ListStore,
        pub room_store: gio::ListStore,
        pub fav_store: gio::ListStore,
        pub space_store: gio::ListStore,
        /// Store for child rooms when drilling into a space.
        pub space_child_store: gio::ListStore,
        pub dm_selection: gtk::SingleSelection,
        pub room_selection: gtk::SingleSelection,
        pub fav_selection: gtk::SingleSelection,
        pub space_selection: gtk::SingleSelection,
        pub space_child_selection: gtk::SingleSelection,
        pub view_stack: adw::ViewStack,
        pub switcher_bar: adw::ViewSwitcherBar,
        /// The spaces page uses a nested stack to switch between the space
        /// list and the child-room list (drill-down).
        pub space_nav_stack: gtk::Stack,
        /// Header bar for the space child view with back button and space name.
        pub space_child_header: adw::HeaderBar,
        pub space_child_title: gtk::Label,
        pub on_room_selected: RefCell<Option<Box<dyn Fn(String, String)>>>,
        pub on_leave_room: RefCell<Option<Box<dyn Fn(String, String)>>>,
        /// Callback for "Join Room" in space drill-down. Passes space room ID.
        pub on_browse_space: RefCell<Option<Box<dyn Fn(String)>>>,
        /// The room ID of the currently displayed space (for drill-down).
        pub current_space_id: RefCell<Option<String>>,
        /// "Join a Room" button at the bottom of the space child view.
        pub join_space_btn: gtk::Button,
        pub join_room_btn: gtk::Button,
        /// Callback for "Join Room" in Rooms tab. No argument (searches homeserver).
        pub on_browse_rooms: RefCell<Option<Box<dyn Fn()>>>,
        /// Cached room data for filtering space children.
        pub cached_rooms: RefCell<Vec<crate::matrix::RoomInfo>>,
        /// Pre-indexed space children: space_name → Vec<index into cached_rooms>.
        pub space_children_index: RefCell<std::collections::HashMap<String, Vec<usize>>>,
        /// Central room registry: room_id → single shared RoomObject.
        /// All ListStores hold clones (shared references) of these GObjects.
        pub room_registry: RefCell<std::collections::HashMap<String, crate::models::RoomObject>>,
        /// Room IDs locally marked as read — suppresses server unread counts
        /// until the server confirms (reports unread_count == 0).
        pub locally_read: RefCell<std::collections::HashSet<String>>,
        /// Tab pages — for badge updates.
        pub dm_page: std::cell::OnceCell<adw::ViewStackPage>,
        pub room_page: std::cell::OnceCell<adw::ViewStackPage>,
        pub fav_page: std::cell::OnceCell<adw::ViewStackPage>,
        pub space_page: std::cell::OnceCell<adw::ViewStackPage>,
    }

    impl Default for RoomListView {
        fn default() -> Self {
            let (dm_store, dm_selection, dm_list_view) = make_room_list();
            let (room_store, room_selection, room_list_view) = make_room_list();
            let (fav_store, fav_selection, fav_list_view) = make_room_list();
            let (space_store, space_selection, space_list_view) = make_room_list();
            let (space_child_store, space_child_selection, space_child_list_view) = make_room_list();

            // Wrap each list in a ScrolledWindow.
            let dm_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&dm_list_view)
                .build();
            let room_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&room_list_view)
                .build();
            // Rooms tab: scroll + pinned join banner at bottom.
            let room_join_banner = create_banner_button("list-add-symbolic", "Join a Room", "join-banner");
            let room_tab_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            room_tab_box.append(&room_scroll);
            room_tab_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            room_tab_box.append(&room_join_banner);
            let space_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&space_list_view)
                .build();
            let space_child_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&space_child_list_view)
                .build();

            // Space child view: back button + title + room list.
            let space_child_title = gtk::Label::new(Some("Space"));
            let space_child_header = adw::HeaderBar::builder()
                .title_widget(&space_child_title)
                .show_title(true)
                .build();

            // "Join Room" banner at the bottom of the space child list.
            let join_banner = create_banner_button("list-add-symbolic", "Join a Room", "join-banner");

            let space_child_view = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            space_child_view.append(&space_child_header);
            space_child_view.append(&space_child_scroll);
            space_child_view.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            space_child_view.append(&join_banner);

            // Nested stack for spaces: space list ↔ space child list.
            let space_nav_stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::SlideLeftRight)
                .build();
            space_nav_stack.add_named(&space_scroll, Some("space-list"));
            space_nav_stack.add_named(&space_child_view, Some("space-children"));

            let view_stack = adw::ViewStack::new();

            let dm_page = view_stack.add_titled(&dm_scroll, Some("messages"), "Messages");
            dm_page.set_icon_name(Some("chat-message-new-symbolic"));

            let room_page = view_stack.add_titled(&room_tab_box, Some("rooms"), "Rooms");
            room_page.set_icon_name(Some("system-users-symbolic"));

            let fav_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&fav_list_view)
                .build();
            let fav_page_ref = view_stack.add_titled(&fav_scroll, Some("bookmarks"), "Bookmarks");
            fav_page_ref.set_icon_name(Some("starred-symbolic"));

            let space_page = view_stack.add_titled(&space_nav_stack, Some("spaces"), "Spaces");
            space_page.set_icon_name(Some("view-grid-symbolic"));

            view_stack.set_vexpand(true);

            let switcher_bar = adw::ViewSwitcherBar::builder()
                .reveal(true)
                .stack(&view_stack)
                .build();

            Self {
                dm_store,
                room_store,
                fav_store,
                space_store,
                space_child_store,
                dm_selection,
                room_selection,
                fav_selection,
                space_selection,
                space_child_selection,
                view_stack,
                switcher_bar,
                space_nav_stack,
                space_child_header,
                space_child_title,
                on_room_selected: RefCell::new(None),
                on_leave_room: RefCell::new(None),
                on_browse_space: RefCell::new(None),
                current_space_id: RefCell::new(None),
                join_space_btn: join_banner,
                join_room_btn: room_join_banner,
                on_browse_rooms: RefCell::new(None),
                cached_rooms: RefCell::new(Vec::new()),
                space_children_index: RefCell::new(std::collections::HashMap::new()),
                room_registry: RefCell::new(std::collections::HashMap::new()),
                locally_read: RefCell::new(std::collections::HashSet::new()),
                dm_page: {
                    let cell = std::cell::OnceCell::new();
                    let _ = cell.set(dm_page);
                    cell
                },
                room_page: {
                    let cell = std::cell::OnceCell::new();
                    let _ = cell.set(room_page);
                    cell
                },
                fav_page: {
                    let cell = std::cell::OnceCell::new();
                    let _ = cell.set(fav_page_ref);
                    cell
                },
                space_page: {
                    let cell = std::cell::OnceCell::new();
                    let _ = cell.set(space_page);
                    cell
                },
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

            obj.append(&self.view_stack);
            obj.append(&self.switcher_bar);

            // Wire up room selection callbacks for DMs, rooms, and space children.
            fn connect_room_selection(
                selection: &gtk::SingleSelection,
                view: &super::RoomListView,
            ) {
                let weak = view.downgrade();
                selection.connect_selection_changed(move |sel, _, _| {
                    let Some(view) = weak.upgrade() else { return };
                    if let Some(item) = sel.selected_item() {
                        if let Some(room_obj) = item.downcast_ref::<RoomObject>() {
                            if !room_obj.is_header() {
                                if let Some(ref cb) = *view.imp().on_room_selected.borrow() {
                                    cb(room_obj.room_id(), room_obj.name());
                                }
                            }
                        }
                    }
                    // Reset selection so the same room can be clicked again.
                    sel.set_selected(gtk::INVALID_LIST_POSITION);
                });
            }

            connect_room_selection(&self.dm_selection, &obj);
            connect_room_selection(&self.room_selection, &obj);
            connect_room_selection(&self.fav_selection, &obj);
            connect_room_selection(&self.space_child_selection, &obj);

            // Space list: clicking a space drills into its child rooms.
            let weak = obj.downgrade();
            self.space_selection.connect_selection_changed(move |sel, _, _| {
                let Some(view) = weak.upgrade() else { return };
                if let Some(item) = sel.selected_item() {
                    if let Some(room_obj) = item.downcast_ref::<RoomObject>() {
                        if !room_obj.is_header() {
                            view.imp().current_space_id.replace(Some(room_obj.room_id()));
                            view.show_space_children(
                                &room_obj.name(),
                            );
                        }
                    }
                }
                // Reset so the same space can be re-entered after going back.
                sel.set_selected(gtk::INVALID_LIST_POSITION);
            });

            // Back button in the space child header.
            let back_btn = gtk::Button::builder()
                .icon_name("go-previous-symbolic")
                .build();
            let weak = obj.downgrade();
            back_btn.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.imp().space_nav_stack.set_visible_child_name("space-list");
                }
            });
            self.space_child_header.pack_start(&back_btn);

            // Wire the "Join a Room" banner in Rooms tab.
            let weak = obj.downgrade();
            self.join_room_btn.connect_clicked(move |_| {
                let Some(view) = weak.upgrade() else { return };
                let has_cb = view.imp().on_browse_rooms.borrow().is_some();
                if has_cb {
                    let borrow = view.imp().on_browse_rooms.borrow();
                    borrow.as_ref().unwrap()();
                }
            });

            // Wire the "Join a Room" banner in Space drill-down.
            let weak = obj.downgrade();
            self.join_space_btn.connect_clicked(move |_| {
                let Some(view) = weak.upgrade() else { return };
                let space_id = view.imp().current_space_id.borrow().clone();
                if let Some(id) = space_id {
                    if let Some(ref cb) = *view.imp().on_browse_space.borrow() {
                        cb(id);
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

    pub fn connect_leave_room<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_leave_room.replace(Some(Box::new(f)));
    }

    pub fn connect_browse_space<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_browse_space.replace(Some(Box::new(f)));
    }

    pub fn connect_browse_rooms<F: Fn() + 'static>(&self, f: F) {
        self.imp().on_browse_rooms.replace(Some(Box::new(f)));
    }

    /// Clear unread/highlight badges for a room immediately in the UI.
    /// O(1) lookup via room_registry — the GObject is shared across all stores.
    /// The RoomRow's connect_notify_local on unread-count/highlight-count
    /// automatically updates the badge widget when these properties change.
    pub fn clear_unread(&self, room_id: &str) {
        let imp = self.imp();
        let registry = imp.room_registry.borrow();
        if let Some(obj) = registry.get(room_id) {
            obj.set_unread_count(0);
            obj.set_highlight_count(0);
        }
        drop(registry);
        // Track locally-read so the next sync doesn't overwrite our zero.
        imp.locally_read.borrow_mut().insert(room_id.to_string());
    }

    /// Increment unread count for a room (when a message arrives for a
    /// room we're not viewing). O(1) via room_registry.
    /// The RoomRow's connect_notify_local auto-updates the badge.
    pub fn increment_unread(&self, room_id: &str, is_highlight: bool) {
        let imp = self.imp();
        let registry = imp.room_registry.borrow();
        if let Some(obj) = registry.get(room_id) {
            obj.set_unread_count(obj.unread_count() + 1);
            if is_highlight {
                obj.set_highlight_count(obj.highlight_count() + 1);
            }
        }
    }

    pub fn update_rooms(&self, rooms: &[RoomInfo]) {
        let imp = self.imp();

        // Cache room data for space drill-down + build index.
        imp.cached_rooms.replace(rooms.to_vec());
        {
            let mut idx: std::collections::HashMap<String, Vec<usize>> =
                std::collections::HashMap::new();
            for (i, r) in rooms.iter().enumerate() {
                if let Some(ref space) = r.parent_space {
                    idx.entry(space.clone()).or_default().push(i);
                }
            }
            imp.space_children_index.replace(idx);
        }

        // Phase 2: Patch existing GObjects or create new ones in the registry.
        // Freeze notifications during patching + rebuild to prevent
        // connect_notify_local callbacks from firing on partially-updated
        // widget trees (which causes double-free crashes).
        let new_ids: std::collections::HashSet<String> =
            rooms.iter().map(|r| r.room_id.clone()).collect();
        let mut freeze_guards: Vec<glib::object::PropertyNotificationFreezeGuard> = Vec::new();
        {
            let mut registry = imp.room_registry.borrow_mut();

            // Freeze all existing GObjects before patching.
            for obj in registry.values() {
                use glib::object::ObjectExt;
                freeze_guards.push(obj.freeze_notify());
            }

            for r in rooms {
                if let Some(obj) = registry.get(&r.room_id) {
                    let server_unread = r.unread_count as u32;
                    let server_hl = r.highlight_count as u32;
                    let new_unread = server_unread.max(obj.unread_count());
                    let new_hl = server_hl.max(obj.highlight_count());

                    obj.set_name(r.name.as_str());
                    obj.set_unread_count(new_unread);
                    obj.set_highlight_count(new_hl);
                    obj.set_is_pinned(r.is_pinned);
                    obj.set_is_admin(r.is_admin);
                    obj.set_is_tombstoned(r.is_tombstoned);
                    obj.set_is_favourite(r.is_favourite);
                    obj.set_last_activity_ts(r.last_activity_ts);
                } else {
                    registry.insert(r.room_id.clone(), Self::room_to_obj(r));
                }
            }
            registry.retain(|id, _| new_ids.contains(id));

            let mut locally_read = imp.locally_read.borrow_mut();
            locally_read.retain(|rid| {
                if let Some(obj) = registry.get(rid) {
                    if obj.unread_count() > 0 || obj.highlight_count() > 0 {
                        obj.set_unread_count(0);
                        obj.set_highlight_count(0);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            });
        }

        // Rebuild ListStores from registry (clones of shared GObjects).
        self.rebuild_stores(rooms);

        // Thaw all notifications — callbacks fire now with stable widget state.
        drop(freeze_guards);
    }

    /// Rebuild all ListStores from the registry, using shared GObject clones.
    /// Phase 5 optimization: skip ListStore manipulation when the sorted
    /// room_id sequence hasn't changed (most common case during incremental sync).
    fn rebuild_stores(&self, rooms: &[RoomInfo]) {
        let imp = self.imp();
        let registry = imp.room_registry.borrow();

        let lookup = |r: &RoomInfo| -> RoomObject {
            registry.get(&r.room_id).unwrap().clone()
        };

        let (dms, _by_space, ungrouped, cleanup) = group_and_sort_rooms(rooms);

        // Bookmarks — computed separately since they cut across all categories.
        let mut favourites: Vec<&RoomInfo> = rooms
            .iter()
            .filter(|r| r.is_favourite)
            .collect();
        favourites.sort_by(|a, b| b.last_activity_ts.cmp(&a.last_activity_ts));

        // Spaces — sorted by child activity.
        let mut spaces: Vec<&RoomInfo> = rooms
            .iter()
            .filter(|r| r.kind == RoomKind::Space)
            .collect();
        let mut space_activity: std::collections::HashMap<&str, u64> =
            std::collections::HashMap::new();
        for r in rooms {
            if let Some(ref space) = r.parent_space {
                let entry = space_activity.entry(space.as_str()).or_insert(0);
                if r.last_activity_ts > *entry {
                    *entry = r.last_activity_ts;
                }
            }
        }
        spaces.sort_by(|a, b| {
            let a_ts = space_activity.get(a.name.as_str()).copied()
                .unwrap_or(a.last_activity_ts);
            let b_ts = space_activity.get(b.name.as_str()).copied()
                .unwrap_or(b.last_activity_ts);
            b_ts.cmp(&a_ts)
        });

        // Helper: compare store contents against new id sequence.
        // If identical, skip rebuild — properties are already patched on the
        // shared GObjects, so the UI is up-to-date without ListStore churn.
        let store_matches = |store: &gio::ListStore, ids: &[&str]| -> bool {
            use gtk::prelude::Cast;
            let n = gio::prelude::ListModelExt::n_items(store) as usize;
            if n != ids.len() { return false; }
            for (i, &expected_id) in ids.iter().enumerate() {
                let Some(item) = gio::prelude::ListModelExt::item(store, i as u32) else { return false };
                let Some(obj) = item.downcast_ref::<crate::models::RoomObject>() else { return false };
                if obj.room_id() != expected_id { return false; }
            }
            true
        };

        // DMs tab.
        let dm_ids: Vec<&str> = dms.iter().map(|r| r.room_id.as_str()).collect();
        if !store_matches(&imp.dm_store, &dm_ids) {
            imp.dm_store.remove_all();
            for r in &dms {
                imp.dm_store.append(&lookup(r));
            }
        }
        // DMs tab — dot indicator only when there are unread messages.
        {
            let mut has_unread = false;
            let mut has_hl = false;
            for r in &dms {
                if let Some(obj) = registry.get(&r.room_id) {
                    if obj.unread_count() > 0 { has_unread = true; }
                    if obj.highlight_count() > 0 { has_hl = true; }
                }
            }
            if let Some(page) = imp.dm_page.get() {
                use adw::prelude::*;
                page.set_needs_attention(has_unread || has_hl);
            }
        }

        // Rooms tab (ungrouped + cleanup section).
        let mut room_ids: Vec<&str> = ungrouped.iter().map(|r| r.room_id.as_str()).collect();
        if !cleanup.is_empty() {
            room_ids.push("__header__");
            room_ids.extend(cleanup.iter().map(|r| r.room_id.as_str()));
        }
        let room_store_ids: Vec<String> = {
            let n = gio::prelude::ListModelExt::n_items(&imp.room_store);
            (0..n).filter_map(|i| {
                use gtk::prelude::Cast;
                let item = gio::prelude::ListModelExt::item(&imp.room_store, i)?;
                let obj = item.downcast_ref::<crate::models::RoomObject>()?;
                if obj.is_header() { Some("__header__".to_string()) }
                else { Some(obj.room_id()) }
            }).collect()
        };
        let room_ids_owned: Vec<String> = room_ids.iter().map(|s| s.to_string()).collect();
        if room_store_ids != room_ids_owned {
            imp.room_store.remove_all();
            for r in &ungrouped {
                imp.room_store.append(&lookup(r));
            }
            if !cleanup.is_empty() {
                imp.room_store.append(&RoomObject::new_header("Suggested Cleanup"));
                for r in &cleanup {
                    imp.room_store.append(&lookup(r));
                }
            }
        }
        // Rooms tab — dot indicator only.
        {
            let mut has_unread = false;
            let mut has_hl = false;
            for r in &ungrouped {
                if let Some(obj) = registry.get(&r.room_id) {
                    if obj.unread_count() > 0 { has_unread = true; }
                    if obj.highlight_count() > 0 { has_hl = true; }
                }
            }
            if let Some(page) = imp.room_page.get() {
                use adw::prelude::*;
                page.set_needs_attention(has_unread || has_hl);
            }
        }

        // Bookmarks tab.
        let fav_ids: Vec<&str> = favourites.iter().map(|r| r.room_id.as_str()).collect();
        if !store_matches(&imp.fav_store, &fav_ids) {
            imp.fav_store.remove_all();
            for r in &favourites {
                imp.fav_store.append(&lookup(r));
            }
        }
        // Bookmarks tab — dot indicator only.
        {
            let mut has_unread = false;
            let mut has_hl = false;
            for r in &favourites {
                if let Some(obj) = registry.get(&r.room_id) {
                    if obj.unread_count() > 0 { has_unread = true; }
                    if obj.highlight_count() > 0 { has_hl = true; }
                }
            }
            if let Some(page) = imp.fav_page.get() {
                use adw::prelude::*;
                page.set_needs_attention(has_unread || has_hl);
            }
        }

        // Spaces tab: aggregate child room unread onto each space's RoomObject
        // and compute total for the tab badge + per-space tooltip.
        let index = imp.space_children_index.borrow();
        let cached = imp.cached_rooms.borrow();
        let mut total_space_unread: u32 = 0;
        let mut space_has_hl = false;
        for r in &spaces {
            let mut child_unread: u32 = 0;
            let mut child_hl: u32 = 0;
            if let Some(indices) = index.get(&r.name) {
                for &i in indices {
                    if let Some(child) = cached.get(i) {
                        if let Some(obj) = registry.get(&child.room_id) {
                            child_unread += obj.unread_count();
                            child_hl += obj.highlight_count();
                        }
                    }
                }
            }
            // Set aggregated unread on the space's own RoomObject so the
            // badge renders on the space row in the Spaces tab.
            if let Some(obj) = registry.get(&r.room_id) {
                obj.set_unread_count(child_unread);
                obj.set_highlight_count(child_hl);
            }
            total_space_unread += child_unread;
            if child_hl > 0 { space_has_hl = true; }
        }
        drop(index);
        drop(cached);

        let space_ids: Vec<&str> = spaces.iter().map(|r| r.room_id.as_str()).collect();
        if !store_matches(&imp.space_store, &space_ids) {
            imp.space_store.remove_all();
            for r in &spaces {
                imp.space_store.append(&lookup(r));
            }
        }
        // Spaces tab — dot indicator only.
        if let Some(page) = imp.space_page.get() {
            use adw::prelude::*;
            page.set_needs_attention(total_space_unread > 0 || space_has_hl);
        }
    }

    /// Drill into a space: show its child rooms in the space child view.
    fn show_space_children(&self, space_name: &str) {
        let imp = self.imp();
        imp.space_child_store.remove_all();
        imp.space_child_title.set_label(space_name);

        let rooms = imp.cached_rooms.borrow();
        let index = imp.space_children_index.borrow();
        let registry = imp.room_registry.borrow();
        // O(1) lookup by space name instead of O(n) filter.
        let mut children: Vec<&RoomInfo> = index
            .get(space_name)
            .map(|indices| indices.iter().filter_map(|&i| rooms.get(i)).collect())
            .unwrap_or_default();

        children.sort_by(|a, b| {
            b.is_pinned
                .cmp(&a.is_pinned)
                .then(b.last_activity_ts.cmp(&a.last_activity_ts))
        });

        for r in &children {
            // Use shared GObject from registry so badge updates propagate.
            if let Some(obj) = registry.get(&r.room_id) {
                imp.space_child_store.append(&obj.clone());
            }
        }

        imp.space_nav_stack.set_visible_child_name("space-children");
    }

    fn room_to_obj(r: &RoomInfo) -> RoomObject {
        let kind_str = match r.kind {
            RoomKind::DirectMessage => "dm",
            RoomKind::Room => "room",
            RoomKind::Space => "space",
        };
        let obj = RoomObject::new(
            &r.room_id,
            &r.name,
            kind_str,
            r.is_encrypted,
            r.parent_space.as_deref().unwrap_or(""),
            r.is_pinned,
            r.is_admin,
            r.is_tombstoned,
            r.is_favourite,
        );
        obj.set_unread_count(r.unread_count as u32);
        obj.set_highlight_count(r.highlight_count as u32);
        obj.set_last_activity_ts(r.last_activity_ts);
        obj
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
            RoomKind::Space => {
                // Spaces themselves go to the Spaces tab, not grouped here.
            }
            RoomKind::Room => {
                if let Some(ref space) = r.parent_space {
                    by_space.entry(space.clone()).or_default().push(r);
                } else {
                    ungrouped.push(r);
                }
            }
        }
    }

    // Sort priority: has highlights → pinned → most recent activity.
    // Rooms where you were mentioned float to top. Pinned rooms come next.
    // Everything else sorts by recency. Unread count is a visual indicator
    // only — it doesn't affect sort order, otherwise stale rooms with
    // unread messages would outrank recently active ones.
    let sort_by_priority = |a: &&RoomInfo, b: &&RoomInfo| {
        let a_has_hl = a.highlight_count > 0;
        let b_has_hl = b.highlight_count > 0;
        b_has_hl
            .cmp(&a_has_hl)
            .then(b.is_pinned.cmp(&a.is_pinned))
            .then(b.last_activity_ts.cmp(&a.last_activity_ts))
    };

    dms.sort_by(sort_by_priority);
    ungrouped.sort_by(sort_by_priority);
    for rooms in by_space.values_mut() {
        rooms.sort_by(sort_by_priority);
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
            unread_count: 0,
            highlight_count: 0,
            is_admin: false,
            is_tombstoned: false,
            is_favourite: false,
        }
    }

    fn make_room_with_unread(
        name: &str,
        kind: RoomKind,
        last_activity_ts: u64,
        unread_count: u64,
        highlight_count: u64,
    ) -> RoomInfo {
        RoomInfo {
            room_id: format!("!{}:matrix.org", name.to_lowercase().replace(' ', "_")),
            name: name.to_string(),
            last_activity_ts,
            kind,
            is_encrypted: false,
            parent_space: None,
            is_pinned: false,
            unread_count,
            highlight_count,
            is_admin: false,
            is_tombstoned: false,
            is_favourite: false,
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
    fn test_space_children_excluded_from_ungrouped() {
        let rooms = vec![
            make_room("Dev Chat", RoomKind::Room, Some("Work"), false, 100),
            make_room("General", RoomKind::Room, None, false, 300),
        ];
        let (_, by_space, ungrouped, _) = group_and_sort_rooms(&rooms);
        assert_eq!(by_space.len(), 1);
        assert_eq!(by_space["Work"].len(), 1);
        // Only ungrouped rooms show in the Rooms tab.
        assert_eq!(ungrouped.len(), 1);
        assert_eq!(ungrouped[0].name, "General");
    }

    #[test]
    fn test_spaces_not_in_room_groups() {
        let rooms = vec![
            make_room("Work Space", RoomKind::Space, None, false, 100),
            make_room("General", RoomKind::Room, None, false, 200),
        ];
        let (dms, by_space, ungrouped, _) = group_and_sort_rooms(&rooms);
        assert!(dms.is_empty());
        assert!(by_space.is_empty());
        // Spaces are excluded from ungrouped.
        assert_eq!(ungrouped.len(), 1);
        assert_eq!(ungrouped[0].name, "General");
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

    #[test]
    fn test_highlights_sort_first() {
        // Highlighted rooms float to top regardless of timestamp.
        // Non-highlighted rooms sort by recency (unread doesn't matter).
        let rooms = vec![
            make_room_with_unread("Active", RoomKind::Room, 9999, 0, 0),
            make_room_with_unread("Mentioned", RoomKind::Room, 100, 3, 1),
            make_room_with_unread("Unread", RoomKind::Room, 500, 5, 0),
        ];
        let (_, _, ungrouped, _) = group_and_sort_rooms(&rooms);
        assert_eq!(ungrouped[0].name, "Mentioned");
        assert_eq!(ungrouped[1].name, "Active");
        assert_eq!(ungrouped[2].name, "Unread");
    }

    #[test]
    fn test_unread_does_not_affect_sort() {
        // Unread count is visual only — recency always wins.
        let rooms = vec![
            make_room_with_unread("Old Unread", RoomKind::DirectMessage, 100, 50, 0),
            make_room_with_unread("Recent Read", RoomKind::DirectMessage, 9999, 0, 0),
        ];
        let (dms, _, _, _) = group_and_sort_rooms(&rooms);
        assert_eq!(dms[0].name, "Recent Read");
        assert_eq!(dms[1].name, "Old Unread");
    }

    #[test]
    fn test_dead_rooms_sort_last() {
        let rooms = vec![
            make_room("Dead", RoomKind::Room, None, false, 0),
            make_room("Active", RoomKind::Room, None, false, 500),
        ];
        let (_, _, ungrouped, _) = group_and_sort_rooms(&rooms);
        assert_eq!(ungrouped[0].name, "Active");
        assert_eq!(ungrouped[1].name, "Dead");
    }
}
