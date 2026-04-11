// BookmarksOverview — full-window saved-things overview.
//
// Two sections:
//   1. Favourite Rooms  — room cards with live unread badges (GObject bindings).
//   2. Saved Messages   — locally bookmarked message cards.
//
// Inspired by the Ptyxis tab overview: slides over the normal UI,
// dismisses back to chat on close or card click.

mod imp {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use std::cell::RefCell;

    pub struct BookmarksOverview {
        // ── Favourite rooms section ──────────────────────────────────────────
        pub rooms_section: gtk::Box,
        pub rooms_flow: gtk::FlowBox,
        /// GObject property bindings for room badge labels/visibility.
        /// Cleared and rebuilt on every set_favourite_rooms() call.
        pub room_bindings: RefCell<Vec<glib::Binding>>,

        // ── Saved messages section ───────────────────────────────────────────
        pub msgs_section: gtk::Box,
        pub msgs_flow: gtk::FlowBox,
        pub msgs_empty: adw::StatusPage,
        pub msgs_stack: gtk::Stack,

        // ── Separator between sections ───────────────────────────────────────
        pub section_sep: gtk::Separator,

        // ── Search ───────────────────────────────────────────────────────────
        pub search_entry: gtk::SearchEntry,
        pub search_query: std::rc::Rc<std::cell::RefCell<String>>,

        // ── Callbacks ────────────────────────────────────────────────────────
        /// Navigate to a saved message: (room_id, event_id).
        pub on_navigate: RefCell<Option<Box<dyn Fn(String, String)>>>,
        /// Navigate to a favourite room: (room_id, room_name).
        pub on_room_navigate: RefCell<Option<Box<dyn Fn(String, String)>>>,
        /// Fired when the user closes the overview.
        pub on_close: RefCell<Option<Box<dyn Fn()>>>,
    }

    impl Default for BookmarksOverview {
        fn default() -> Self {
            let make_flow = || {
                gtk::FlowBox::builder()
                    .selection_mode(gtk::SelectionMode::None)
                    .homogeneous(true)
                    .column_spacing(12)
                    .row_spacing(12)
                    .max_children_per_line(8)
                    .min_children_per_line(1)
                    .build()
            };

            let rooms_flow = make_flow();
            let msgs_flow = make_flow();

            // Rooms section wrapper (hidden when no favourites).
            let rooms_section = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .margin_start(24)
                .margin_end(24)
                .margin_top(20)
                .margin_bottom(8)
                .visible(false)
                .build();

            // Messages section wrapper.
            let msgs_section = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .margin_start(24)
                .margin_end(24)
                .margin_top(12)
                .margin_bottom(24)
                .build();

            let msgs_empty = adw::StatusPage::builder()
                .icon_name("starred-symbolic")
                .title("No Saved Messages")
                .description(
                    "Hover over a message and click the star icon to save it for later.",
                )
                .build();

            let msgs_stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::None)
                .build();

            let section_sep = gtk::Separator::builder()
                .orientation(gtk::Orientation::Horizontal)
                .margin_start(24)
                .margin_end(24)
                .visible(false)
                .build();

            Self {
                rooms_section,
                rooms_flow,
                room_bindings: RefCell::new(Vec::new()),
                msgs_section,
                msgs_flow,
                msgs_empty,
                msgs_stack,
                section_sep,
                search_entry: gtk::SearchEntry::builder()
                    .placeholder_text("Search bookmarks…")
                    .hexpand(true)
                    .build(),
                search_query: Default::default(),
                on_navigate: RefCell::new(None),
                on_room_navigate: RefCell::new(None),
                on_close: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BookmarksOverview {
        const NAME: &'static str = "MxBookmarksOverview";
        type Type = super::BookmarksOverview;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for BookmarksOverview {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);

            // ── Header bar ──────────────────────────────────────────────────
            let header = adw::HeaderBar::builder()
                .show_end_title_buttons(false)
                .build();
            header.set_title_widget(Some(&self.search_entry));

            let close_btn = gtk::Button::builder()
                .icon_name("go-previous-symbolic")
                .tooltip_text("Back to chat")
                .build();
            close_btn.add_css_class("flat");
            header.pack_start(&close_btn);

            let weak = obj.downgrade();
            close_btn.connect_clicked(move |_| {
                let Some(view) = weak.upgrade() else { return };
                let has_cb = view.imp().on_close.borrow().is_some();
                if has_cb {
                    let borrow = view.imp().on_close.borrow();
                    borrow.as_ref().unwrap()();
                }
            });

            // ── Search filter ────────────────────────────────────────────────
            // FlowBoxChild widget_name is "<id>\t<searchable_lowercase_text>".
            // The filter closure checks if the query appears anywhere after the tab.
            let query_for_rooms = self.search_query.clone();
            self.rooms_flow.set_filter_func(move |child| {
                let q = query_for_rooms.borrow();
                if q.is_empty() { return true; }
                let name = child.widget_name();
                name.split_once('\t').map_or(false, |(_, s)| s.contains(q.as_str()))
            });
            let query_for_msgs = self.search_query.clone();
            self.msgs_flow.set_filter_func(move |child| {
                let q = query_for_msgs.borrow();
                if q.is_empty() { return true; }
                let name = child.widget_name();
                name.split_once('\t').map_or(false, |(_, s)| s.contains(q.as_str()))
            });

            let query = self.search_query.clone();
            let rooms_flow = self.rooms_flow.clone();
            let msgs_flow = self.msgs_flow.clone();
            self.search_entry.connect_search_changed(move |entry| {
                *query.borrow_mut() = entry.text().to_lowercase();
                rooms_flow.invalidate_filter();
                msgs_flow.invalidate_filter();
            });

            // ── Section: Favourite Rooms ────────────────────────────────────
            let rooms_header = gtk::Label::builder()
                .label("Favourite Rooms")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build();
            self.rooms_section.append(&rooms_header);
            self.rooms_section.append(&self.rooms_flow);

            // ── Section: Saved Messages ─────────────────────────────────────
            let msgs_header = gtk::Label::builder()
                .label("Saved Messages")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build();
            self.msgs_stack.add_named(&self.msgs_empty, Some("empty"));
            self.msgs_stack.add_named(&self.msgs_flow, Some("cards"));
            self.msgs_stack.set_visible_child_name("empty");
            self.msgs_section.append(&msgs_header);
            self.msgs_section.append(&self.msgs_stack);

            // ── Scrollable body ─────────────────────────────────────────────
            let body = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            body.append(&self.rooms_section);
            body.append(&self.section_sep);
            body.append(&self.msgs_section);

            // min_content_height gives the sheet a usable floor without
            // setting height_request on the overview widget itself.
            // height_request acts as a minimum in GTK4 — if set to the window
            // height it prevents the window from resizing smaller while the
            // sheet is open.  min_content_height only constrains the scroll
            // viewport, so the window can always resize below this value
            // (the content scrolls instead of locking the window).
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .min_content_height(320)
                .child(&body)
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&header);
            toolbar.set_content(Some(&scroll));

            obj.append(&toolbar);
        }
    }

    impl WidgetImpl for BookmarksOverview {
        /// Override GTK's measure so that the vertical MINIMUM is always 0
        /// while the NATURAL height still reflects height_request (set to the
        /// window height when the sheet is open).
        ///
        /// GTK4 conflates minimum and natural into height_request: setting it
        /// to 800 makes the window minimum 800, permanently blocking downward
        /// resize.  By returning min=0 here the window manager never enforces
        /// a floor, so the user can resize freely.  The sheet still fills the
        /// window because the natural height (what BottomSheet uses to size the
        /// panel) equals height_request.
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (min, nat, min_bl, nat_bl) = self.parent_measure(orientation, for_size);
            if orientation == gtk::Orientation::Vertical {
                (0, nat, min_bl, nat_bl)
            } else {
                (min, nat, min_bl, nat_bl)
            }
        }
    }
    impl BoxImpl for BookmarksOverview {}
}

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;

use crate::bookmarks::{BookmarkEntry, BOOKMARK_STORE};
use crate::models::RoomObject;
use crate::widgets::message_row::format_timestamp;

glib::wrapper! {
    pub struct BookmarksOverview(ObjectSubclass<imp::BookmarksOverview>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::Orientable;
}

impl BookmarksOverview {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn connect_navigate<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_navigate.replace(Some(Box::new(f)));
    }

    pub fn connect_room_navigate<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_room_navigate.replace(Some(Box::new(f)));
    }

    pub fn connect_close<F: Fn() + 'static>(&self, f: F) {
        self.imp().on_close.replace(Some(Box::new(f)));
    }

    pub fn clear_search(&self) {
        let imp = self.imp();
        imp.search_entry.set_text("");
        imp.search_query.borrow_mut().clear();
        imp.rooms_flow.invalidate_filter();
        imp.msgs_flow.invalidate_filter();
    }

    // ── Favourite Rooms ──────────────────────────────────────────────────────

    /// Replace the favourite rooms section with the given RoomObjects.
    /// Uses GObject property bindings so unread badges update live.
    pub fn set_favourite_rooms(&self, rooms: &[RoomObject]) {
        let imp = self.imp();

        // Disconnect all previous badge bindings.
        for b in imp.room_bindings.take() {
            b.unbind();
        }

        // Clear existing room cards.
        while let Some(child) = imp.rooms_flow.first_child() {
            imp.rooms_flow.remove(&child);
        }

        if rooms.is_empty() {
            imp.rooms_section.set_visible(false);
            imp.section_sep.set_visible(false);
            return;
        }

        imp.rooms_section.set_visible(true);
        // Show separator only when there are also message cards.
        let has_msgs = imp.msgs_flow.first_child().is_some();
        imp.section_sep.set_visible(has_msgs);

        let mut bindings = Vec::new();
        for room in rooms {
            let card = self.make_room_card(room, &mut bindings);
            imp.rooms_flow.append(&card);
        }
        imp.room_bindings.replace(bindings);
    }

    fn make_room_card(
        &self,
        room: &RoomObject,
        bindings: &mut Vec<glib::Binding>,
    ) -> gtk::Widget {
        let room_id = room.room_id();
        let room_name = room.name();
        let room_name_lower = room_name.to_lowercase();

        let card_btn = gtk::Button::builder()
            .width_request(200)
            .build();
        card_btn.add_css_class("card");
        card_btn.add_css_class("bookmark-room-card");

        let hbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .margin_start(10)
            .margin_end(10)
            .margin_top(10)
            .margin_bottom(10)
            .build();

        // Avatar.
        let avatar = adw::Avatar::builder()
            .size(40)
            .text(&room_name)
            .show_initials(true)
            .build();
        if !room.avatar_path().is_empty() {
            if let Ok(tex) = gtk::gdk::Texture::from_filename(room.avatar_path()) {
                avatar.set_custom_image(Some(&tex));
            }
        }

        // Name + badge column.
        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();

        let name_lbl = gtk::Label::builder()
            .label(&room_name)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["body"])
            .build();

        // Unread badge row.
        let badge_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::Start)
            .build();

        let mention_icon = gtk::Image::builder()
            .icon_name("mention-symbolic")
            .pixel_size(12)
            .visible(false)
            .build();

        let unread_badge = gtk::Label::builder()
            .visible(false)
            .css_classes(["unread-badge", "caption"])
            .build();

        badge_row.append(&mention_icon);
        badge_row.append(&unread_badge);

        // Bind unread-count → badge label + visibility.
        bindings.push(
            room.bind_property("unread-count", &unread_badge, "visible")
                .transform_to(|_, count: u32| Some(count > 0))
                .sync_create()
                .build(),
        );
        bindings.push(
            room.bind_property("unread-count", &unread_badge, "label")
                .transform_to(|_, count: u32| {
                    Some(if count > 99 {
                        "99+".to_string()
                    } else {
                        count.to_string()
                    })
                })
                .sync_create()
                .build(),
        );
        bindings.push(
            room.bind_property("highlight-count", &mention_icon, "visible")
                .transform_to(|_, count: u32| Some(count > 0))
                .sync_create()
                .build(),
        );

        vbox.append(&name_lbl);
        vbox.append(&badge_row);
        hbox.append(&avatar);
        hbox.append(&vbox);
        card_btn.set_child(Some(&hbox));

        let weak = self.downgrade();
        card_btn.connect_clicked(move |_| {
            let Some(view) = weak.upgrade() else { return };
            let has_cb = view.imp().on_room_navigate.borrow().is_some();
            if has_cb {
                let borrow = view.imp().on_room_navigate.borrow();
                borrow.as_ref().unwrap()(room_id.clone(), room_name.clone());
            }
        });

        let fb_child = gtk::FlowBoxChild::new();
        fb_child.set_widget_name(&format!("{}\t{}", room.room_id(), room_name_lower));
        fb_child.set_child(Some(&card_btn));
        fb_child.upcast()
    }

    // ── Saved Messages ───────────────────────────────────────────────────────

    /// Reload all message bookmark cards from disk.
    pub fn reload_messages(&self) {
        let entries = BOOKMARK_STORE.load();
        let imp = self.imp();

        while let Some(child) = imp.msgs_flow.first_child() {
            imp.msgs_flow.remove(&child);
        }

        if entries.is_empty() {
            imp.msgs_stack.set_visible_child_name("empty");
            imp.section_sep.set_visible(false);
            return;
        }

        imp.msgs_stack.set_visible_child_name("cards");
        let has_rooms = imp.rooms_section.get_visible();
        imp.section_sep.set_visible(has_rooms);
        for entry in &entries {
            self.add_message_card(entry);
        }
    }

    /// Convenience: reload both sections (called when opening the overview).
    pub fn reload(&self) {
        self.reload_messages();
        // Room section is rebuilt by the window via set_favourite_rooms.
    }

    /// Add a single message bookmark card without reloading everything.
    pub fn add_message_card(&self, entry: &BookmarkEntry) {
        let imp = self.imp();
        imp.msgs_stack.set_visible_child_name("cards");
        let has_rooms = imp.rooms_section.get_visible();
        imp.section_sep.set_visible(has_rooms);

        let card = self.make_message_card(entry);
        imp.msgs_flow.append(&card);
    }

    /// Remove the message card for a given event_id.
    pub fn remove_message_card(&self, event_id: &str) {
        let imp = self.imp();
        let mut child = imp.msgs_flow.first_child();
        while let Some(w) = child {
            let next = w.next_sibling();
            if let Some(fb_child) = w.downcast_ref::<gtk::FlowBoxChild>() {
                let wname = fb_child.widget_name();
                let id = wname.split_once('\t').map(|(id, _)| id).unwrap_or(&wname);
                if id == event_id {
                    imp.msgs_flow.remove(fb_child);
                    break;
                }
            }
            child = next;
        }
        if imp.msgs_flow.first_child().is_none() {
            imp.msgs_stack.set_visible_child_name("empty");
            imp.section_sep.set_visible(false);
        }
    }

    fn make_message_card(&self, entry: &BookmarkEntry) -> gtk::Widget {
        let event_id = entry.event_id.clone();
        let room_id = entry.room_id.clone();
        let nav_event_id = entry.event_id.clone();

        let overlay = gtk::Overlay::new();

        let card_btn = gtk::Button::builder()
            .width_request(260)
            .build();
        card_btn.add_css_class("card");
        card_btn.add_css_class("bookmark-card");

        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(10)
            .margin_end(10)
            .margin_top(10)
            .margin_bottom(10)
            .build();

        let room_label = gtk::Label::builder()
            .label(&entry.room_name)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build();

        let meta_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build();
        let sender_label = gtk::Label::builder()
            .label(&entry.sender)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build();
        let time_label = gtk::Label::builder()
            .label(format_timestamp(entry.timestamp))
            .halign(gtk::Align::End)
            .css_classes(["dim-label", "caption"])
            .build();
        meta_box.append(&sender_label);
        meta_box.append(&time_label);

        let preview = gtk::Label::builder()
            .label(&entry.body_preview)
            .halign(gtk::Align::Start)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(3)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label"])
            .build();

        vbox.append(&room_label);
        vbox.append(&meta_box);
        vbox.append(&preview);
        card_btn.set_child(Some(&vbox));
        overlay.set_child(Some(&card_btn));

        // Delete button — floated top-right, visible on hover via CSS.
        let delete_btn = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Remove bookmark")
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .margin_top(4)
            .margin_end(4)
            .build();
        delete_btn.add_css_class("circular");
        delete_btn.add_css_class("flat");
        delete_btn.add_css_class("bookmark-delete-btn");
        overlay.add_overlay(&delete_btn);

        let weak = self.downgrade();
        card_btn.connect_clicked(move |_| {
            let Some(view) = weak.upgrade() else { return };
            let has_cb = view.imp().on_navigate.borrow().is_some();
            if has_cb {
                let borrow = view.imp().on_navigate.borrow();
                borrow.as_ref().unwrap()(room_id.clone(), nav_event_id.clone());
            }
        });

        let eid_del = event_id.clone();
        let weak = self.downgrade();
        delete_btn.connect_clicked(move |_| {
            BOOKMARK_STORE.remove(&eid_del);
            if let Some(view) = weak.upgrade() {
                view.remove_message_card(&eid_del);
            }
        });

        let searchable = format!("{} {} {}",
            entry.room_name.to_lowercase(),
            entry.sender.to_lowercase(),
            entry.body_preview.to_lowercase());
        let fb_child = gtk::FlowBoxChild::new();
        fb_child.set_widget_name(&format!("{}\t{}", event_id, searchable));
        fb_child.set_child(Some(&overlay));
        fb_child.upcast()
    }
}
