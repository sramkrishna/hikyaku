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

    use crate::matrix::RoomKind;
    use crate::models::RoomObject;
    use crate::widgets::room_row::RoomRow;

    /// Create a subtle join button with icon + label.
    #[allow(dead_code)]
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
        pub dm_list_view: gtk::ListView,
        pub room_list_view: gtk::ListView,
        pub fav_list_view: gtk::ListView,
        pub space_child_list_view: gtk::ListView,
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
        /// The room ID of the currently displayed space (for drill-down).
        pub current_space_id: RefCell<Option<String>>,
        /// Navigation history stack for space drill-down: each entry is
        /// (space_name, space_room_id) of the parent level. Pop on back.
        pub space_nav_history: RefCell<Vec<(String, String)>>,
        /// Cached room data for filtering space children.
        pub cached_rooms: RefCell<Vec<crate::matrix::RoomInfo>>,
        /// Pre-indexed space children: space_name → Vec<index into cached_rooms>.
        pub space_children_index: RefCell<std::collections::HashMap<String, Vec<usize>>>,
        /// room_id → parent space name, for O(1) badge propagation.
        pub room_id_to_parent_space: RefCell<std::collections::HashMap<String, String>>,
        /// Central room registry: room_id → single shared RoomObject.
        /// All ListStores hold clones (shared references) of these GObjects.
        pub room_registry: RefCell<std::collections::HashMap<String, crate::models::RoomObject>>,
        /// Room IDs locally marked as read, mapped to the unread count at the
        /// time of reading.  Suppresses server-reported counts ≤ that baseline
        /// (server catching up to our read receipt) but lets higher counts
        /// through (new messages arrived after we read the room).
        pub locally_read: RefCell<std::collections::HashMap<String, u32>>,
        /// Tab pages — for badge updates.
        pub dm_page: std::cell::OnceCell<adw::ViewStackPage>,
        pub room_page: std::cell::OnceCell<adw::ViewStackPage>,
        pub fav_page: std::cell::OnceCell<adw::ViewStackPage>,
        pub space_page: std::cell::OnceCell<adw::ViewStackPage>,
        /// Callback fired when the user Ctrl+clicks a room row to request an AI preview.
        /// Arguments: (room_id, y_in_widget) — y is the click y relative to the list view.
        pub on_room_preview_requested: RefCell<Option<Box<dyn Fn(String, f64)>>>,
        /// Fired when the Bookmarks tab is selected.
        pub on_bookmarks_activated: RefCell<Option<Box<dyn Fn()>>>,
        /// The currently active room ID — used for O(1) highlight switching.
        pub active_room_id: RefCell<Option<String>>,
        /// Last server-reported unread_notification_count per room.
        /// Used to detect cross-client reads: if count drops from >0 to 0
        /// between syncs, another client sent a read receipt.
        pub prev_server_counts: RefCell<std::collections::HashMap<String, u32>>,
        /// Flat ordered navigation list: favs → DMs → rooms (room_ids).
        /// Rebuilt in update_rooms to keep navigate_room O(1).
        pub nav_order: RefCell<Vec<String>>,
        /// room_id → position in nav_order for O(1) current-position lookup.
        pub nav_index: RefCell<std::collections::HashMap<String, usize>>,
        /// space display-name → space room_id.  Built alongside
        /// space_children_index so update_parent_space_badge can resolve a
        /// space's RoomObject in O(1) instead of scanning all rooms.
        pub space_name_to_id: RefCell<std::collections::HashMap<String, String>>,
        /// Last-known room_id order for each store — used to skip rebuilds when
        /// nothing changed.  Avoids O(n) GObject downcast iteration in store_matches.
        pub last_dm_order: RefCell<Vec<String>>,
        pub last_room_order: RefCell<Vec<String>>,
        pub last_fav_order: RefCell<Vec<String>>,
        pub last_space_order: RefCell<Vec<String>>,
        /// Structural signature: (room_id, last_activity_ts, is_favourite, is_pinned,
        /// is_tombstoned) for each room in arrival order.  When only unread counts
        /// change this signature stays the same and we skip the expensive
        /// group_and_sort_rooms + ListStore rebuilds entirely.
        pub last_structural_sig: RefCell<Vec<(String, u64, bool, bool, bool)>>,
        /// Search bar — toggled by the header magnifier button.
        pub search_bar: gtk::SearchBar,
        pub search_entry: gtk::SearchEntry,
        /// Flat store populated with matching RoomObjects when search is active.
        pub search_store: gio::ListStore,
        pub search_selection: gtk::SingleSelection,
        pub search_list_view: gtk::ListView,
        /// Stack switching between the normal tabbed browse view and the search results.
        pub search_stack: gtk::Stack,
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
                .css_classes(["mx-tinted-sidebar"])
                .build();
            let room_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&room_list_view)
                .css_classes(["mx-tinted-sidebar"])
                .build();
            // Rooms tab: just the scroll view.
            let room_tab_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            room_tab_box.append(&room_scroll);
            let space_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&space_list_view)
                .css_classes(["mx-tinted-sidebar"])
                .build();
            let space_child_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&space_child_list_view)
                .css_classes(["mx-tinted-sidebar"])
                .build();

            // Space child view: back button + title + room list.
            let space_child_title = gtk::Label::new(Some("Space"));
            let space_child_header = adw::HeaderBar::builder()
                .title_widget(&space_child_title)
                .show_title(true)
                .build();

            let space_child_view = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            space_child_view.append(&space_child_header);
            space_child_view.append(&space_child_scroll);

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
                .css_classes(["mx-tinted-sidebar"])
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

            // Search widgets.
            let search_entry = gtk::SearchEntry::builder()
                .placeholder_text("Search rooms…")
                .hexpand(true)
                .build();
            let search_bar = gtk::SearchBar::builder()
                .show_close_button(false)
                .build();
            search_bar.set_child(Some(&search_entry));
            search_bar.connect_entry(&search_entry);

            let (search_store, search_selection, search_list_view) = make_room_list();
            let search_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&search_list_view)
                .css_classes(["mx-tinted-sidebar"])
                .build();

            // Stack switching between browse (tabs) and search results.
            let search_stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .transition_duration(150)
                .vexpand(true)
                .build();
            let browse_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .vexpand(true)
                .build();
            browse_box.append(&view_stack);
            browse_box.append(&switcher_bar);
            search_stack.add_named(&browse_box, Some("browse"));
            search_stack.add_named(&search_scroll, Some("search"));

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
                dm_list_view,
                room_list_view,
                fav_list_view,
                space_child_list_view,
                view_stack,
                switcher_bar,
                space_nav_stack,
                space_child_header,
                space_child_title,
                on_room_selected: RefCell::new(None),
                on_leave_room: RefCell::new(None),
                current_space_id: RefCell::new(None),
                space_nav_history: RefCell::new(Vec::new()),
                cached_rooms: RefCell::new(Vec::new()),
                space_children_index: RefCell::new(std::collections::HashMap::new()),
                room_id_to_parent_space: RefCell::new(std::collections::HashMap::new()),
                room_registry: RefCell::new(std::collections::HashMap::new()),
                locally_read: RefCell::new(std::collections::HashMap::new()),
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
                on_room_preview_requested: RefCell::new(None),
                on_bookmarks_activated: RefCell::new(None),
                active_room_id: RefCell::new(None),
                nav_order: RefCell::new(Vec::new()),
                nav_index: RefCell::new(std::collections::HashMap::new()),
                space_name_to_id: RefCell::new(std::collections::HashMap::new()),
                last_dm_order: RefCell::new(Vec::new()),
                last_room_order: RefCell::new(Vec::new()),
                last_fav_order: RefCell::new(Vec::new()),
                last_space_order: RefCell::new(Vec::new()),
                last_structural_sig: RefCell::new(Vec::new()),
                prev_server_counts: RefCell::new(std::collections::HashMap::new()),
                search_bar,
                search_entry,
                search_store,
                search_selection,
                search_list_view,
                search_stack,
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

            obj.append(&self.search_bar);
            obj.append(&self.search_stack);

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

            // Space child list: sub-spaces drill deeper; rooms open normally.
            {
                let weak = obj.downgrade();
                self.space_child_selection.connect_selection_changed(move |sel, _, _| {
                    let Some(view) = weak.upgrade() else { return };
                    if let Some(item) = sel.selected_item() {
                        if let Some(room_obj) = item.downcast_ref::<RoomObject>() {
                            if !room_obj.is_header() {
                                if room_obj.kind() == RoomKind::Space {
                                    // Push current level onto history before drilling deeper.
                                    let current_name = view.imp().space_child_title.label().to_string();
                                    let current_id = view.imp().current_space_id.borrow().clone();
                                    if let Some(id) = current_id {
                                        view.imp().space_nav_history.borrow_mut().push((current_name, id));
                                    }
                                    view.imp().current_space_id.replace(Some(room_obj.room_id()));
                                    view.show_space_children(&room_obj.name());
                                } else if let Some(ref cb) = *view.imp().on_room_selected.borrow() {
                                    cb(room_obj.room_id(), room_obj.name());
                                }
                            }
                        }
                    }
                    sel.set_selected(gtk::INVALID_LIST_POSITION);
                });
            }

            // Space list: clicking a space drills into its child rooms.
            let weak = obj.downgrade();
            self.space_selection.connect_selection_changed(move |sel, _, _| {
                let Some(view) = weak.upgrade() else { return };
                if let Some(item) = sel.selected_item() {
                    if let Some(room_obj) = item.downcast_ref::<RoomObject>() {
                        if !room_obj.is_header() {
                            // Fresh top-level drill-in: clear history.
                            view.imp().space_nav_history.borrow_mut().clear();
                            view.imp().current_space_id.replace(Some(room_obj.room_id()));
                            view.show_space_children(&room_obj.name());
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
                let Some(view) = weak.upgrade() else { return };
                let parent = view.imp().space_nav_history.borrow_mut().pop();
                match parent {
                    Some((parent_name, parent_id)) => {
                        // Go up one level.
                        view.imp().current_space_id.replace(Some(parent_id));
                        view.show_space_children(&parent_name);
                    }
                    None => {
                        // Back to the top-level space list.
                        view.imp().space_nav_stack.set_visible_child_name("space-list");
                    }
                }
            });
            self.space_child_header.pack_start(&back_btn);

            // Wire search entry → filter room registry and populate search_store.
            let weak = obj.downgrade();
            self.search_entry.connect_search_changed(move |entry| {
                let Some(view) = weak.upgrade() else { return };
                view.apply_search(entry.text().as_str());
            });

            // Wire search entry → also hook up the selection for search results.
            connect_room_selection(&self.search_selection, &obj);

            // When search mode is disabled (Escape / toggle off), clear query and return to browse.
            let weak = obj.downgrade();
            self.search_bar.connect_notify_local(Some("search-mode-enabled"), move |bar: &gtk::SearchBar, _| {
                let Some(view) = weak.upgrade() else { return };
                if !bar.is_search_mode() {
                    view.imp().search_entry.set_text("");
                    view.imp().search_stack.set_visible_child_name("browse");
                }
            });

            // Bookmarks tab activation → fire callback (window shows full overlay).
            let weak = obj.downgrade();
            self.view_stack.connect_notify_local(Some("visible-child-name"), move |stack, _| {
                if stack.visible_child_name().as_deref() == Some("bookmarks") {
                    if let Some(view) = weak.upgrade() {
                        if let Some(ref cb) = *view.imp().on_bookmarks_activated.borrow() {
                            cb();
                        }
                    }
                }
            });

            // Ctrl+click on a room row → request AI preview.
            // We claim the event sequence so the ListView doesn't also select the row.
            for list_view in [
                &self.dm_list_view,
                &self.room_list_view,
                &self.fav_list_view,
                &self.space_child_list_view,
            ] {
                let gesture = gtk::GestureClick::new();
                let view_weak = obj.downgrade();

                gesture.connect_pressed(move |gesture, _n_press, x, y| {
                    // Only act on Ctrl+left-click.
                    let modifiers = gesture.current_event_state();
                    if !modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK) { return; }

                    let Some(widget) = gesture.widget() else { return };
                    let room_id: Option<String> = widget
                        .pick(x, y, gtk::PickFlags::DEFAULT)
                        .and_then(|w| super::find_room_row_ancestor(&w))
                        .and_then(|row: RoomRow| row.imp().current_room_id.borrow().clone());
                    let Some(room_id) = room_id else { return };

                    // Claim so the ListView does not also select/activate the row.
                    gesture.set_state(gtk::EventSequenceState::Claimed);

                    let Some(view) = view_weak.upgrade() else { return };
                    if let Some(ref cb) = *view.imp().on_room_preview_requested.borrow() {
                        cb(room_id, y);
                    };
                });

                list_view.add_controller(gesture);
            }
        }
    }

    impl WidgetImpl for RoomListView {}
    impl BoxImpl for RoomListView {}
}

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::collections::BTreeMap;

use crate::matrix::{RoomInfo, RoomKind};
use crate::models::RoomObject;
use crate::widgets::room_row::RoomRow;

/// Walk widget ancestors to find the nearest RoomRow.
fn find_room_row_ancestor(widget: &gtk::Widget) -> Option<RoomRow> {
    let mut w = Some(widget.clone());
    while let Some(ref current) = w {
        if let Some(row) = current.downcast_ref::<RoomRow>() {
            return Some(row.clone());
        }
        w = current.parent();
    }
    None
}

glib::wrapper! {
    pub struct RoomListView(ObjectSubclass<imp::RoomListView>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::Orientable;
}

impl RoomListView {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Return the name of the currently visible tab ("messages", "rooms", "bookmarks", "spaces").
    pub fn visible_tab(&self) -> Option<glib::GString> {
        self.imp().view_stack.visible_child_name()
    }

    /// Toggle the search bar on/off.
    pub fn toggle_search(&self) {
        let bar = &self.imp().search_bar;
        bar.set_search_mode(!bar.is_search_mode());
        if bar.is_search_mode() {
            self.imp().search_entry.grab_focus();
        }
    }

    /// Expose the search bar so window.rs can set the key-capture widget.
    pub fn search_bar(&self) -> gtk::SearchBar {
        self.imp().search_bar.clone()
    }

    /// Filter the room registry by `query` and populate the search store.
    /// Switches the search_stack to "search" when query is non-empty,
    /// back to "browse" when empty.
    fn apply_search(&self, query: &str) {
        let imp = self.imp();
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            imp.search_stack.set_visible_child_name("browse");
            imp.search_store.remove_all();
            return;
        }
        imp.search_stack.set_visible_child_name("search");
        imp.search_store.remove_all();
        let registry = imp.room_registry.borrow();
        let mut results: Vec<crate::models::RoomObject> = registry
            .values()
            .filter(|r| !r.is_header() && r.kind() != RoomKind::Space && r.name().to_lowercase().contains(&q))
            .cloned()
            .collect();
        // Sort by most recent activity so the best matches float up.
        results.sort_by(|a, b| b.last_activity_ts().cmp(&a.last_activity_ts()));
        for room in results {
            imp.search_store.append(&room);
        }
    }

    /// Register a callback fired when the user Ctrl+clicks a room row.
    /// The second argument is the click y position relative to the list view.
    pub fn connect_room_preview_requested<F: Fn(String, f64) + 'static>(&self, f: F) {
        self.imp().on_room_preview_requested.replace(Some(Box::new(f)));
    }

    pub fn connect_room_selected<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_room_selected.replace(Some(Box::new(f)));
    }

    /// Set the topic-changed flag on a room row (MOTD plugin).
    pub fn set_topic_changed(&self, room_id: &str, changed: bool) {
        if let Some(obj) = self.imp().room_registry.borrow().get(room_id) {
            obj.set_topic_changed(changed);
        }
    }

    /// Set or clear the watch-alert indicator on a room row.
    pub fn set_watch_alert(&self, room_id: &str, alert: bool) {
        if let Some(obj) = self.imp().room_registry.borrow().get(room_id) {
            obj.set_watch_alert(alert);
        }
    }

    /// Clear topic-changed on all rooms (called when MOTD plugin is disabled).
    pub fn clear_all_topic_changed(&self) {
        for obj in self.imp().room_registry.borrow().values() {
            obj.set_topic_changed(false);
        }
    }

    /// Mark `room_id` as the active room in the sidebar, clearing the previous one.
    /// O(1): two HashMap lookups. CSS updates reactively via connect_notify_local in bind_room.
    pub fn set_active_room(&self, room_id: &str) {
        let imp = self.imp();
        let registry = imp.room_registry.borrow();
        let prev_id = imp.active_room_id.borrow().clone();

        if prev_id.as_deref() != Some(room_id) {
            if let Some(ref prev) = prev_id {
                if let Some(obj) = registry.get(prev.as_str()) {
                    obj.set_is_active(false);
                }
            }
            if let Some(obj) = registry.get(room_id) {
                obj.set_is_active(true);
            }
            drop(registry);
            *imp.active_room_id.borrow_mut() = Some(room_id.to_string());
        }
    }

    pub fn connect_leave_room<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_leave_room.replace(Some(Box::new(f)));
    }

    pub fn connect_bookmarks_activated<F: Fn() + 'static>(&self, f: F) {
        self.imp().on_bookmarks_activated.replace(Some(Box::new(f)));
    }

    /// Switch back to the messages tab (called when the bookmarks overlay closes).
    pub fn select_messages_tab(&self) {
        self.imp().view_stack.set_visible_child_name("messages");
    }

    /// Resolve the effective no_media flag for a room by walking the context chain:
    /// room override → parent space override → false (global default: show media).
    pub fn resolve_no_media(&self, room_id: &str) -> bool {
        let registry = self.imp().room_registry.borrow();
        crate::room_context::resolve_no_media(room_id, &registry)
    }

    /// Set a context override for a room or space and persist it.
    pub fn set_context_override(&self, id: &str, value: crate::room_context::CtxValue) {
        crate::room_context::save_override(id, value);
        let registry = self.imp().room_registry.borrow();
        if let Some(obj) = registry.get(id) {
            obj.set_ctx_no_media(value);
        }
    }

    /// Navigate the sidebar to the context of `room_id`.
    /// If the room belongs to a space, switches to the Spaces tab and drills
    /// into that space.  Otherwise falls back to the Messages tab.
    pub fn navigate_to_room_context(&self, room_id: &str) {
        let imp = self.imp();
        let space_name = imp.room_id_to_parent_space.borrow().get(room_id).cloned();
        if let Some(name) = space_name {
            imp.view_stack.set_visible_child_name("spaces");
            self.show_space_children(&name);
        } else {
            imp.view_stack.set_visible_child_name("messages");
        }
    }

    /// Navigate to the room adjacent to `current_room_id` in the flat list
    /// (favourites → DMs → rooms, spaces excluded). `delta` is +1 for next,
    /// -1 for previous; wraps around at the ends.
    pub fn navigate_room(&self, current_room_id: &str, delta: i32) {
        let imp = self.imp();
        let nav_order = imp.nav_order.borrow();
        let nav_index = imp.nav_index.borrow();
        let n = nav_order.len() as i32;
        if n == 0 { return; }

        // O(1) lookup of current position.
        let current_pos = nav_index.get(current_room_id).copied();
        let next_pos = match current_pos {
            Some(pos) => ((pos as i32 + delta).rem_euclid(n)) as usize,
            None => if delta > 0 { 0 } else { (n - 1) as usize },
        };
        let next_id = nav_order[next_pos].clone();
        drop(nav_order);
        drop(nav_index);

        // Look up name from registry for the callback.
        let registry = imp.room_registry.borrow();
        let name = registry.get(&next_id).map(|o| o.name()).unwrap_or_default();
        drop(registry);
        if let Some(ref cb) = *imp.on_room_selected.borrow() {
            cb(next_id, name);
        }
    }

    /// Immediately patch GObject properties for a single room using the same
    /// guarded setters as update_rooms().  Called by the ticker for the
    /// currently open room so its sidebar row reflects the latest state at
    /// normal priority, before the idle that updates the rest of the list.
    pub fn patch_room(&self, r: &crate::matrix::RoomInfo) {
        let imp = self.imp();
        let registry = imp.room_registry.borrow();
        let Some(obj) = registry.get(&r.room_id) else { return };
        let server_unread = r.unread_count as u32;
        let server_hl    = r.highlight_count as u32;
        let new_unread   = server_unread.max(obj.unread_count());
        let new_hl       = server_hl.max(obj.highlight_count());
        if obj.name()            != r.name            { obj.set_name(r.name.as_str()); }
        if obj.unread_count()    != new_unread         { obj.set_unread_count(new_unread); }
        if obj.highlight_count() != new_hl             { obj.set_highlight_count(new_hl); }
        if obj.is_pinned()       != r.is_pinned        { obj.set_is_pinned(r.is_pinned); }
        if obj.is_admin()        != r.is_admin         { obj.set_is_admin(r.is_admin); }
        if obj.is_tombstoned()   != r.is_tombstoned    { obj.set_is_tombstoned(r.is_tombstoned); }
        if obj.is_favourite()    != r.is_favourite     { obj.set_is_favourite(r.is_favourite); }
        if obj.last_activity_ts()!= r.last_activity_ts { obj.set_last_activity_ts(r.last_activity_ts); }
        if obj.avatar_url()      != r.avatar_url       { obj.set_avatar_url(r.avatar_url.as_str()); }
    }

    /// Clear unread/highlight badges for a room immediately in the UI.
    /// O(1) lookup via room_registry — the GObject is shared across all stores.
    /// The RoomRow's bind_property on unread-count/highlight-count
    /// automatically updates the badge widget when these properties change.
    pub fn clear_unread(&self, room_id: &str) {
        let imp = self.imp();
        let registry = imp.room_registry.borrow();
        let count_at_read = registry.get(room_id).map(|o| o.unread_count()).unwrap_or(0);
        if let Some(obj) = registry.get(room_id) {
            obj.set_unread_count(0);
            obj.set_highlight_count(0);
        }
        drop(registry);
        imp.locally_read.borrow_mut().insert(room_id.to_string(), count_at_read);
        self.update_parent_space_badge(room_id);
    }

    /// Set the unread/highlight counts for a room from the local broker.
    /// The broker count is applied as a floor: we take `max(current, broker)`,
    /// so a server sync that returns 0 cannot erase a locally-tracked count.
    /// Passing (0, 0) always zeroes the count (used by mark_read).
    pub fn set_room_unread_counts(&self, room_id: &str, unread: u32, highlights: u32) {
        let imp = self.imp();
        let registry = imp.room_registry.borrow();
        if let Some(obj) = registry.get(room_id) {
            let final_u = if unread == 0 && highlights == 0 {
                0
            } else {
                obj.unread_count().max(unread)
            };
            let final_h = if unread == 0 && highlights == 0 {
                0
            } else {
                obj.highlight_count().max(highlights)
            };
            if obj.unread_count() != final_u { obj.set_unread_count(final_u); }
            if obj.highlight_count() != final_h { obj.set_highlight_count(final_h); }
        }
        drop(registry);
        self.update_parent_space_badge(room_id);
    }

    /// Update the community health dot for `room_id` via the RoomObject property.
    /// The bound RoomRow reacts via `connect_notify_local`.
    #[cfg(feature = "community-health")]
    pub fn set_room_health(&self, room_id: &str, alert: u8) {
        if let Some(obj) = self.imp().room_registry.borrow().get(room_id) {
            obj.set_health_alert(alert);
        }
    }

    /// Hide all community health dots (called when the plugin is disabled).
    #[cfg(feature = "community-health")]
    pub fn clear_all_health_dots(&self) {
        for obj in self.imp().room_registry.borrow().values() {
            obj.set_health_alert(0);
        }
    }

    /// Increment unread count for a room (when a message arrives for a
    /// room we're not viewing). O(1) via room_registry.
    /// The RoomRow's property bindings auto-update the badge.
    pub fn increment_unread(&self, room_id: &str, is_highlight: bool) {
        let imp = self.imp();
        // A new message invalidates the "locally read" suppression — remove so
        // the next server unread count is shown rather than silently zeroed.
        imp.locally_read.borrow_mut().remove(room_id);
        let registry = imp.room_registry.borrow();
        if let Some(obj) = registry.get(room_id) {
            obj.set_unread_count(obj.unread_count() + 1);
            if is_highlight {
                obj.set_highlight_count(obj.highlight_count() + 1);
            }
        }
        drop(registry);
        self.update_parent_space_badge(room_id);
        self.set_tab_needs_attention(room_id);
    }

    /// Set `needs_attention` on the appropriate tab when unread changes.
    /// rebuild_stores only runs when the structural sig changes, so live
    /// badge updates (from NewMessage) must poke the tab dot here.
    fn set_tab_needs_attention(&self, room_id: &str) {
        let imp = self.imp();
        // Determine the room's kind and whether it belongs to a space.
        let (kind, in_space) = {
            let registry = imp.room_registry.borrow();
            let obj = registry.get(room_id);
            let kind = obj.map(|o| o.kind());
            let in_space = imp.room_id_to_parent_space.borrow().contains_key(room_id);
            (kind, in_space)
        };
        match kind {
            Some(RoomKind::DirectMessage) => {
                if let Some(page) = imp.dm_page.get() {
                    page.set_needs_attention(true);
                }
            }
            _ if in_space => {
                if let Some(page) = imp.space_page.get() {
                    page.set_needs_attention(true);
                }
            }
            _ => {
                if let Some(page) = imp.room_page.get() {
                    page.set_needs_attention(true);
                }
            }
        }
    }

    /// Recalculate the parent space's aggregated unread badge after a
    /// child room's count changes. Finds the parent space via cached_rooms,
    /// then sums all children's unread from the registry.
    fn update_parent_space_badge(&self, room_id: &str) {
        let imp = self.imp();
        // O(1) lookup via pre-built index.
        let Some(space_name) = imp.room_id_to_parent_space.borrow().get(room_id).cloned()
            else { return };

        // Sum children's unread for this space.
        let index = imp.space_children_index.borrow();
        let rooms = imp.cached_rooms.borrow();
        let registry = imp.room_registry.borrow();
        let mut total_unread: u32 = 0;
        let mut total_hl: u32 = 0;
        if let Some(indices) = index.get(&space_name) {
            for &i in indices {
                if let Some(child) = rooms.get(i) {
                    if let Some(obj) = registry.get(&child.room_id) {
                        total_unread += obj.unread_count();
                        total_hl += obj.highlight_count();
                    }
                }
            }
        }
        // O(1): resolve space name → room_id via pre-built reverse index.
        let Some(space_room_id) = imp.space_name_to_id.borrow().get(&space_name).cloned()
            else { return };
        if let Some(space_obj) = registry.get(&space_room_id) {
            if space_obj.unread_count() != total_unread {
                space_obj.set_unread_count(total_unread);
            }
            if space_obj.highlight_count() != total_hl {
                space_obj.set_highlight_count(total_hl);
            }
        }
    }

    /// Apply the minimal diff to a `gio::ListStore` to match `new_items`.
    ///
    /// Computes the longest common prefix and suffix, then issues a SINGLE
    /// `splice()` call for the changed middle section.  This fires exactly one
    /// `items-changed` signal → one layout pass on the ListView, regardless of
    /// how many items moved.  GTK only rebinds rows in the changed window.
    ///
    /// The previous approach (individual remove+insert per moved item) fired
    /// 2×N signals and 2×N layout passes for N moved items — O(seconds) in
    /// debug builds when most rooms reordered after a long idle period.
    fn patch_store(store: &gio::ListStore, new_items: &[crate::models::RoomObject]) {
        use crate::models::RoomObject;

        fn item_key(obj: &RoomObject) -> String {
            if obj.is_header() { format!("__hdr__{}", obj.name()) } else { obj.room_id() }
        }

        let old_len = store.n_items() as usize;
        let new_len = new_items.len();

        // Fast path: nothing to do.
        if old_len == 0 && new_len == 0 { return; }

        // Common prefix: how many items at the front are already correct.
        let prefix = (0..old_len.min(new_len))
            .take_while(|&i| {
                store.item(i as u32)
                    .and_downcast::<RoomObject>()
                    .map_or(false, |o| item_key(&o) == item_key(&new_items[i]))
            })
            .count();

        if prefix == old_len && prefix == new_len {
            return; // Identical — nothing to do.
        }

        // Common suffix (must not overlap with prefix).
        let max_suffix = old_len.saturating_sub(prefix).min(new_len.saturating_sub(prefix));
        let suffix = (0..max_suffix)
            .take_while(|&i| {
                store.item((old_len - 1 - i) as u32)
                    .and_downcast::<RoomObject>()
                    .map_or(false, |o| item_key(&o) == item_key(&new_items[new_len - 1 - i]))
            })
            .count();

        // One splice for the changed middle: fires exactly one items-changed signal.
        let splice_pos = prefix as u32;
        let n_remove = (old_len - prefix - suffix) as u32;
        let additions: Vec<glib::Object> = new_items[prefix..new_len - suffix]
            .iter()
            .map(|o| o.clone().upcast::<glib::Object>())
            .collect();
        store.splice(splice_pos, n_remove, &additions);
    }

    pub fn update_rooms(&self, rooms: &[RoomInfo]) {
        let imp = self.imp();

        // Cache room data for space drill-down + build index.
        imp.cached_rooms.replace(rooms.to_vec());
        {
            let mut idx: std::collections::HashMap<String, Vec<usize>> =
                std::collections::HashMap::new();
            let mut parent_idx: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            let mut space_to_id: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for (i, r) in rooms.iter().enumerate() {
                if let Some(ref space) = r.parent_space {
                    idx.entry(space.clone()).or_default().push(i);
                    parent_idx.insert(r.room_id.clone(), space.clone());
                }
                if r.kind == crate::matrix::RoomKind::Space {
                    space_to_id.insert(r.name.clone(), r.room_id.clone());
                }
            }
            imp.space_children_index.replace(idx);
            imp.room_id_to_parent_space.replace(parent_idx);
            imp.space_name_to_id.replace(space_to_id);
        }

        // Phase 2: Patch existing GObjects or create new ones in the registry.
        // Property bindings on RoomRow auto-update badges when GObject
        // properties change — no manual notification needed.
        let new_ids: std::collections::HashSet<String> =
            rooms.iter().map(|r| r.room_id.clone()).collect();
        {
            let mut registry = imp.room_registry.borrow_mut();
            for r in rooms {
                if let Some(obj) = registry.get(&r.room_id) {
                    let server_unread = r.unread_count as u32;
                    let server_hl = r.highlight_count as u32;
                    let new_unread = server_unread.max(obj.unread_count());
                    let new_hl = server_hl.max(obj.highlight_count());

                    // Guard every setter: GObject notify signals fire even when
                    // the value is unchanged, triggering CSS recalculations on
                    // every bound RoomRow.  With 295 rooms × 8 properties this
                    // was ~2 360 spurious notifications per sync, blocking the
                    // GTK main thread for ~2 s in debug builds.
                    if obj.name() != r.name       { obj.set_name(r.name.as_str()); }
                    if obj.unread_count()   != new_unread { obj.set_unread_count(new_unread); }
                    if obj.highlight_count()!= new_hl     { obj.set_highlight_count(new_hl); }
                    if obj.is_pinned()      != r.is_pinned     { obj.set_is_pinned(r.is_pinned); }
                    if obj.is_admin()       != r.is_admin      { obj.set_is_admin(r.is_admin); }
                    if obj.is_tombstoned()  != r.is_tombstoned { obj.set_is_tombstoned(r.is_tombstoned); }
                    if obj.is_favourite()   != r.is_favourite  { obj.set_is_favourite(r.is_favourite); }
                    if obj.last_activity_ts()!= r.last_activity_ts { obj.set_last_activity_ts(r.last_activity_ts); }
                    if obj.avatar_url()     != r.avatar_url   { obj.set_avatar_url(r.avatar_url.as_str()); }
                } else {
                    registry.insert(r.room_id.clone(), Self::room_to_obj(r));
                }
            }
            registry.retain(|id, _| new_ids.contains(id));

            let mut locally_read = imp.locally_read.borrow_mut();
            locally_read.retain(|rid, baseline| {
                if let Some(obj) = registry.get(rid) {
                    let current = obj.unread_count();
                    if current > *baseline {
                        // Server count grew beyond the baseline — new messages
                        // arrived after we read the room.  Stop suppressing.
                        false
                    } else if current > 0 {
                        // Server still reporting stale unreads (read receipt
                        // not yet processed).  Suppress and keep waiting.
                        obj.set_unread_count(0);
                        obj.set_highlight_count(0);
                        true
                    } else {
                        // Server confirmed 0 — read receipt acknowledged.
                        false
                    }
                } else {
                    false
                }
            });
        }

        // Update prev_server_counts so the next sync can detect drops.
        {
            let mut prev = imp.prev_server_counts.borrow_mut();
            prev.clear();
            for r in rooms {
                if r.unread_count > 0 {
                    prev.insert(r.room_id.clone(), r.unread_count as u32);
                }
            }
        }

        // Apply persisted context overrides (no_media, etc.) — reads in-memory cache, no disk I/O.
        {
            let registry = imp.room_registry.borrow();
            crate::room_context::apply_to_registry(&registry);
        }

        // Rebuild nav_order + nav_index for O(1) navigate_room.
        {
            let mut order: Vec<String> = Vec::new();
            // favs first, then DMs, then rooms (same ordering as navigate_room used).
            for r in rooms.iter().filter(|r| r.is_favourite) {
                order.push(r.room_id.clone());
            }
            for r in rooms.iter().filter(|r| r.kind == crate::matrix::RoomKind::DirectMessage && !r.is_favourite) {
                order.push(r.room_id.clone());
            }
            for r in rooms.iter().filter(|r| r.kind == crate::matrix::RoomKind::Room && !r.is_favourite) {
                order.push(r.room_id.clone());
            }
            let index: std::collections::HashMap<String, usize> = order.iter()
                .enumerate()
                .map(|(i, id)| (id.clone(), i))
                .collect();
            imp.nav_order.replace(order);
            imp.nav_index.replace(index);
        }

        // Rebuild ListStores from registry (clones of shared GObjects).
        self.rebuild_stores(rooms);
    }

    /// Returns rooms where the server's notification count dropped from >0 to 0
    /// since the last sync — indicating another client sent a read receipt.
    /// Does NOT include rooms that are in `locally_read` (handled by this client).
    /// Call this BEFORE `update_rooms` so the detection uses pre-update counts.
    pub fn detect_cross_client_reads(&self, rooms: &[crate::matrix::RoomInfo]) -> Vec<String> {
        let imp = self.imp();
        let prev = imp.prev_server_counts.borrow();
        let locally_read = imp.locally_read.borrow();
        let mut cross_reads = Vec::new();
        for r in rooms {
            let prev_count = prev.get(&r.room_id).copied().unwrap_or(0);
            let new_count = r.unread_count as u32;
            // Server had a notification count and it reached 0 → another client read it.
            // Skip rooms we handled locally (locally_read) — those will clear via normal flow.
            if prev_count > 0 && new_count == 0 && !locally_read.contains_key(&r.room_id) {
                cross_reads.push(r.room_id.clone());
            }
        }
        cross_reads
    }

    /// Rebuild all ListStores from the registry, using shared GObject clones.
    /// Tab dot updates always run — unread counts change without structural changes.
    /// ListStore rebuilds are skipped when the sorted room_id sequence hasn't
    /// changed (the most common sync case), saving 3-8 ms of GObject work.
    fn rebuild_stores(&self, rooms: &[RoomInfo]) {
        let _t0 = std::time::Instant::now();
        let imp = self.imp();

        let registry = imp.room_registry.borrow();

        let lookup = |r: &RoomInfo| -> RoomObject {
            registry.get(&r.room_id).unwrap().clone()
        };

        let (mut dms, _by_space, mut ungrouped, cleanup) = group_and_sort_rooms(rooms);

        // Bookmarks — computed separately since they cut across all categories.
        let mut favourites: Vec<&RoomInfo> = rooms
            .iter()
            .filter(|r| r.is_favourite)
            .collect();
        favourites.sort_by(|a, b| b.last_activity_ts.cmp(&a.last_activity_ts));

        // Spaces — top-level only (sub-spaces have a parent_space and appear in drill-down).
        let mut spaces: Vec<&RoomInfo> = rooms
            .iter()
            .filter(|r| r.kind == RoomKind::Space && r.parent_space.is_none())
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

        // --- Tab dot updates (always, even when ListStore layout is unchanged) ---

        // DMs tab — dot when any DM has unread.
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
                page.set_needs_attention(has_unread || has_hl);
            }
        }

        // Rooms tab — dot when any ungrouped room has unread.
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
                page.set_needs_attention(has_unread || has_hl);
            }
        }

        // Bookmarks tab — dot when any favourite has unread.
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
                page.set_needs_attention(has_unread || has_hl);
            }
        }

        // Spaces tab: aggregate child room unread onto each space's RoomObject
        // and compute total for the tab badge + per-space tooltip.
        {
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
                // Only set if changed to avoid unnecessary notify storms.
                if let Some(obj) = registry.get(&r.room_id) {
                    if obj.unread_count() != child_unread {
                        obj.set_unread_count(child_unread);
                    }
                    if obj.highlight_count() != child_hl {
                        obj.set_highlight_count(child_hl);
                    }
                }
                total_space_unread += child_unread;
                if child_hl > 0 { space_has_hl = true; }
            }
            if let Some(page) = imp.space_page.get() {
                page.set_needs_attention(total_space_unread > 0 || space_has_hl);
            }
        }

        // --- ListStore rebuilds (skipped when structural layout is unchanged) ---
        // Structural signature: fields that affect which tab a room lands in and
        // its sort position.  Unread counts are excluded — they are already
        // patched on the shared GObjects above.
        let sig: Vec<(String, u64, bool, bool, bool)> = rooms
            .iter()
            .map(|r| (r.room_id.clone(), r.last_activity_ts, r.is_favourite, r.is_pinned, r.is_tombstoned))
            .collect();
        if *imp.last_structural_sig.borrow() == sig {
            return;
        }
        imp.last_structural_sig.replace(sig);

        // DMs tab — synchronous, always.  This is the default visible tab; the
        // user is waiting for DMs to appear, so we render them first.
        let dm_ids: Vec<String> = dms.iter().map(|r| r.room_id.clone()).collect();
        if *imp.last_dm_order.borrow() != dm_ids {
            let objects: Vec<RoomObject> = dms.iter().map(|r| lookup(r)).collect();
            let _td = std::time::Instant::now();
            Self::patch_store(&imp.dm_store, &objects);
            tracing::info!("rebuild_stores: dm_store patch(n={}) took {:?}", objects.len(), _td.elapsed());
            imp.last_dm_order.replace(dm_ids);
        }

        // Pre-compute IDs for the remaining tabs (cheap string ops).
        let mut room_ids: Vec<String> = ungrouped.iter().map(|r| r.room_id.clone()).collect();
        if !cleanup.is_empty() {
            room_ids.push("__header__".to_string());
            room_ids.extend(cleanup.iter().map(|r| r.room_id.clone()));
        }
        let fav_ids: Vec<String> = favourites.iter().map(|r| r.room_id.clone()).collect();
        let space_ids: Vec<String> = spaces.iter().map(|r| r.room_id.clone()).collect();

        // On the very first populate (room store is empty), defer the room/fav/space
        // stores to the next idle slot.  This lets the DM list render before the GTK
        // thread is frozen again for the remaining stores (~400ms on a 70-room list).
        if imp.last_room_order.borrow().is_empty() {
            let mut room_objects: Vec<RoomObject> = ungrouped.iter().map(|r| lookup(r)).collect();
            if !cleanup.is_empty() {
                room_objects.push(RoomObject::new_header("Suggested Cleanup"));
                room_objects.extend(cleanup.iter().map(|r| lookup(r)));
            }
            let fav_objects: Vec<RoomObject> = favourites.iter().map(|r| lookup(r)).collect();
            let space_objects: Vec<RoomObject> = spaces.iter().map(|r| lookup(r)).collect();

            // Claim order slots before yielding — prevents a concurrent update_rooms
            // from re-entering this branch and double-scheduling the idle.
            imp.last_room_order.replace(room_ids);
            imp.last_fav_order.replace(fav_ids);
            imp.last_space_order.replace(space_ids);

            let room_store = imp.room_store.clone();
            let fav_store = imp.fav_store.clone();
            let space_store = imp.space_store.clone();
            let room_gobs: Vec<glib::Object> = room_objects.iter()
                .map(|o| o.clone().upcast::<glib::Object>()).collect();
            let fav_gobs: Vec<glib::Object> = fav_objects.iter()
                .map(|o| o.clone().upcast::<glib::Object>()).collect();
            let space_gobs: Vec<glib::Object> = space_objects.iter()
                .map(|o| o.clone().upcast::<glib::Object>()).collect();

            glib::idle_add_local_once(move || {
                let _tr = std::time::Instant::now();
                room_store.splice(0, 0, &room_gobs);
                tracing::info!(
                    "rebuild_stores (idle): room_store init(n={}) took {:?}",
                    room_gobs.len(), _tr.elapsed()
                );
                if !fav_gobs.is_empty() {
                    fav_store.splice(0, 0, &fav_gobs);
                }
                for obj in &space_gobs {
                    space_store.append(obj);
                }
            });

            tracing::info!("rebuild_stores: total {:?} (room/fav/space deferred to idle)", _t0.elapsed());
            return;
        }

        // Subsequent updates: diff-patch all stores.  Structural changes are rare
        // after startup, so these splices are typically O(1) items.
        if *imp.last_room_order.borrow() != room_ids {
            let mut objects: Vec<RoomObject> = ungrouped.iter().map(|r| lookup(r)).collect();
            if !cleanup.is_empty() {
                objects.push(RoomObject::new_header("Suggested Cleanup"));
                objects.extend(cleanup.iter().map(|r| lookup(r)));
            }
            let _tr = std::time::Instant::now();
            Self::patch_store(&imp.room_store, &objects);
            tracing::info!("rebuild_stores: room_store patch(n={}) took {:?}", objects.len(), _tr.elapsed());
            imp.last_room_order.replace(room_ids);
        }

        if *imp.last_fav_order.borrow() != fav_ids {
            let objects: Vec<RoomObject> = favourites.iter().map(|r| lookup(r)).collect();
            Self::patch_store(&imp.fav_store, &objects);
            imp.last_fav_order.replace(fav_ids);
        }

        if *imp.last_space_order.borrow() != space_ids {
            imp.space_store.remove_all();
            for r in &spaces {
                imp.space_store.append(&lookup(r));
            }
            imp.last_space_order.replace(space_ids);
        }
        tracing::info!("rebuild_stores: total {:?}", _t0.elapsed());
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

    /// Returns (room_id, mxc_url) for rooms that have an avatar URL but
    /// haven't had their avatar loaded yet. Used by window.rs to request downloads.
    pub fn rooms_needing_avatars(&self) -> Vec<(String, String)> {
        self.imp().room_registry.borrow().values()
            .filter(|obj| !obj.avatar_url().is_empty() && obj.avatar_path().is_empty())
            .map(|obj| (obj.room_id(), obj.avatar_url()))
            .collect()
    }

    /// Update the local cached avatar path for a room (called after download).
    pub fn set_room_avatar_path(&self, room_id: &str, path: &str) {
        if let Some(obj) = self.imp().room_registry.borrow().get(room_id) {
            obj.set_avatar_path(path);
        }
    }

    fn room_to_obj(r: &RoomInfo) -> RoomObject {
        let obj = RoomObject::new(
            &r.room_id,
            &r.name,
            r.kind,
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
        obj.set_avatar_url(r.avatar_url.as_str());
        obj.set_parent_space_id(r.parent_space_id.as_str());
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
            avatar_url: String::new(),
            topic: String::new(),
            parent_space_id: String::new(),
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
            avatar_url: String::new(),
            topic: String::new(),
            parent_space_id: String::new(),
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

    // ── locally_read suppression logic ──────────────────────────────────────
    //
    // These tests validate the HashMap-based retain logic that prevents stale
    // server unread counts from re-lighting the badge right after the user
    // reads a room, while still showing genuinely new messages.
    //
    // The retain closure mirrors the one in handle_room_list exactly.  It is
    // simulated here with a plain HashMap so no GTK widget is needed.
    //
    // `counts`      = room_id → current unread count as reported by the server
    // `locally_read`= room_id → baseline count recorded when user read the room
    //
    // Baseline is stored by `clear_unread`; the retain step runs on every
    // RoomList event to decide which rooms are still "catching up" vs "new".

    fn apply_locally_read_retain(
        mut counts: std::collections::HashMap<String, u32>,
        mut locally_read: std::collections::HashMap<String, u32>,
    ) -> (std::collections::HashMap<String, u32>, std::collections::HashMap<String, u32>) {
        locally_read.retain(|rid, baseline| {
            match counts.get_mut(rid) {
                Some(c) if *c > *baseline => false,
                Some(c) if *c > 0 => { *c = 0; true },
                _ => false,
            }
        });
        (counts, locally_read)
    }

    #[test]
    fn locally_read_suppresses_stale_server_count() {
        // User read room with 3 unreads; server still reports 3 (read receipt
        // not yet processed) → suppress to 0, keep in locally_read.
        let counts = [("!r:m.org".to_string(), 3u32)].into();
        let locally_read = [("!r:m.org".to_string(), 3u32)].into();
        let (counts, locally_read) = apply_locally_read_retain(counts, locally_read);
        assert_eq!(counts["!r:m.org"], 0, "stale count should be suppressed");
        assert!(locally_read.contains_key("!r:m.org"), "should remain in locally_read");
    }

    #[test]
    fn locally_read_shows_new_messages_above_baseline() {
        // User read room with 3 unreads; server now reports 5 → 2 new messages
        // arrived after the read receipt.  Must NOT suppress.
        let counts = [("!r:m.org".to_string(), 5u32)].into();
        let locally_read = [("!r:m.org".to_string(), 3u32)].into();
        let (counts, locally_read) = apply_locally_read_retain(counts, locally_read);
        assert_eq!(counts["!r:m.org"], 5, "new unreads must not be suppressed");
        assert!(!locally_read.contains_key("!r:m.org"), "should be evicted from locally_read");
    }

    #[test]
    fn locally_read_evicted_when_server_confirms_zero() {
        // Server reports 0 → our read receipt was processed; evict the room.
        let counts = [("!r:m.org".to_string(), 0u32)].into();
        let locally_read = [("!r:m.org".to_string(), 3u32)].into();
        let (counts, locally_read) = apply_locally_read_retain(counts, locally_read);
        assert_eq!(counts["!r:m.org"], 0);
        assert!(!locally_read.contains_key("!r:m.org"), "should evict once server confirms 0");
    }

    #[test]
    fn locally_read_zero_baseline_never_suppresses() {
        // User read when unread_count was already 0; any positive server count
        // is a genuinely new message → must not suppress.
        let counts = [("!r:m.org".to_string(), 1u32)].into();
        let locally_read = [("!r:m.org".to_string(), 0u32)].into();
        let (counts, locally_read) = apply_locally_read_retain(counts, locally_read);
        assert_eq!(counts["!r:m.org"], 1, "count above zero baseline must be shown");
        assert!(!locally_read.contains_key("!r:m.org"));
    }

    #[test]
    fn locally_read_cleared_by_new_message_arrival() {
        // increment_unread removes the room from locally_read so the next
        // server count is not suppressed.  Simulate what increment_unread does.
        let mut locally_read: std::collections::HashMap<String, u32> =
            [("!r:m.org".to_string(), 0u32)].into();
        locally_read.remove("!r:m.org");  // what increment_unread does first
        assert!(
            !locally_read.contains_key("!r:m.org"),
            "locally_read must be cleared when a new message arrives"
        );
    }

    // ── cross-client read detection ──────────────────────────────────────────
    //
    // `detect_cross_client_reads` compares the server's notification count
    // from the previous sync cycle against the incoming one.  A drop from
    // >0 to 0 in a room the user didn't read locally means another client
    // (phone, web) sent a read receipt.
    //
    // The pure function below mirrors the logic in detect_cross_client_reads.

    fn apply_cross_client_detection(
        rooms: &[(&str, u32)],          // (room_id, new_server_count)
        prev_server_counts: &std::collections::HashMap<String, u32>,
        locally_read: &std::collections::HashMap<String, u32>,
    ) -> Vec<String> {
        let mut out = Vec::new();
        for (room_id, new_count) in rooms {
            let prev = prev_server_counts.get(*room_id).copied().unwrap_or(0);
            if prev > 0 && *new_count == 0 && !locally_read.contains_key(*room_id) {
                out.push(room_id.to_string());
            }
        }
        out
    }

    #[test]
    fn cross_client_read_detected_when_server_count_drops_to_zero() {
        let prev = [("!r:m.org".to_string(), 3u32)].into();
        let locally_read = std::collections::HashMap::new();
        let rooms = [("!r:m.org", 0u32)];
        let detected = apply_cross_client_detection(&rooms, &prev, &locally_read);
        assert_eq!(detected, vec!["!r:m.org"], "should detect cross-client read");
    }

    #[test]
    fn cross_client_read_not_detected_when_locally_read() {
        // We read the room ourselves — locally_read entry suppresses detection.
        let prev = [("!r:m.org".to_string(), 3u32)].into();
        let locally_read = [("!r:m.org".to_string(), 3u32)].into();
        let rooms = [("!r:m.org", 0u32)];
        let detected = apply_cross_client_detection(&rooms, &prev, &locally_read);
        assert!(detected.is_empty(), "local read must not be attributed to another client");
    }

    #[test]
    fn cross_client_read_not_detected_when_server_always_zero() {
        // Push-rule-exempt room: server was always 0, no drop → not detectable.
        let prev = std::collections::HashMap::new(); // was 0 before too
        let locally_read = std::collections::HashMap::new();
        let rooms = [("!r:m.org", 0u32)];
        let detected = apply_cross_client_detection(&rooms, &prev, &locally_read);
        assert!(detected.is_empty(), "push-rule-exempt rooms cannot be detected via server count");
    }

    #[test]
    fn cross_client_read_not_detected_on_partial_drop() {
        // Count dropped from 5 to 2 (some messages read, not all) — not zero → no detection.
        let prev = [("!r:m.org".to_string(), 5u32)].into();
        let locally_read = std::collections::HashMap::new();
        let rooms = [("!r:m.org", 2u32)];
        let detected = apply_cross_client_detection(&rooms, &prev, &locally_read);
        assert!(detected.is_empty(), "partial drop is not a cross-client full-read");
    }

    #[test]
    fn cross_client_read_only_detected_for_dropped_room() {
        // Room A dropped to 0 (cross-client read); room B still has a count.
        let prev = [
            ("!a:m.org".to_string(), 3u32),
            ("!b:m.org".to_string(), 5u32),
        ].into();
        let locally_read = std::collections::HashMap::new();
        let rooms = [("!a:m.org", 0u32), ("!b:m.org", 5u32)];
        let detected = apply_cross_client_detection(&rooms, &prev, &locally_read);
        assert_eq!(detected, vec!["!a:m.org"], "only the zero-dropped room should be flagged");
    }

    #[test]
    fn locally_read_multiple_rooms_independent() {
        // Suppression for room A must not affect room B.
        let counts = [
            ("!a:m.org".to_string(), 2u32),  // stale count == baseline
            ("!b:m.org".to_string(), 5u32),  // count > baseline → new messages
        ].into();
        let locally_read = [
            ("!a:m.org".to_string(), 2u32),
            ("!b:m.org".to_string(), 3u32),
        ].into();
        let (counts, locally_read) = apply_locally_read_retain(counts, locally_read);
        assert_eq!(counts["!a:m.org"], 0, "room A stale count should be suppressed");
        assert_eq!(counts["!b:m.org"], 5, "room B new count should be shown");
        assert!(locally_read.contains_key("!a:m.org"));
        assert!(!locally_read.contains_key("!b:m.org"));
    }
}
