// MessageRow — a single message bubble in the message view.

/// Per-timeline context passed to every row bind.
/// Owned by MessageView; cloning is cheap — highlight_names is Rc (pointer copy).
#[derive(Clone)]
pub struct RowContext {
    /// Rc so cloning the context is O(1) regardless of how many names are tracked.
    pub highlight_names: std::rc::Rc<[String]>,
    pub my_user_id: String,
    pub is_dm: bool,
    pub no_media: bool,
    /// Set of Matrix user IDs in the rolodex — pre-built once per room switch so
    /// bind_message_object can do an O(1) lookup instead of scanning GSettings.
    pub rolodex_ids: std::rc::Rc<std::collections::HashSet<String>>,
}

impl Default for RowContext {
    fn default() -> Self {
        Self {
            highlight_names: std::rc::Rc::from([]),
            my_user_id: String::new(),
            is_dm: false,
            no_media: false,
            rolodex_ids: std::rc::Rc::new(std::collections::HashSet::new()),
        }
    }
}

mod imp {
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::CompositeTemplate;

    #[derive(CompositeTemplate, Default)]
    #[template(file = "src/widgets/message_row.blp")]
    pub struct MessageRow {
        /// Current event ID — updated on each bind for action buttons.
        /// Uses Rc<RefCell> so closures in constructed() share the same cell.
        pub event_id: std::rc::Rc<std::cell::RefCell<String>>,
        pub sender_text: std::rc::Rc<std::cell::RefCell<String>>,
        pub body_text: std::rc::Rc<std::cell::RefCell<String>>,
        /// Callback: reply clicked → (event_id, sender, body).
        pub on_reply: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String, String)>>>>,
        /// Callback: reaction emoji picked → (event_id, emoji).
        pub on_react: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String)>>>>,
        /// Callback: request the shared emoji chooser → (event_id, button).
        /// Fires when the row's react button is clicked; MessageView owns a
        /// single EmojiChooser and reparents it onto `button` for the popup.
        /// This avoids allocating ~1.5GB of per-row chooser widget trees
        /// (see heaptrack evidence: populate_emoji_chooser dominates).
        pub on_show_react_picker:
            std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, gtk::Button)>>>>,
        /// Callback: edit clicked → (event_id, body).
        pub on_edit: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String)>>>>,
        /// Callback: delete clicked → (event_id).
        pub on_delete: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String)>>>>,
        /// Callback: jump to replied-to message → (event_id).
        pub on_jump_to_reply: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String)>>>>,
        /// Current reply-to event ID — updated on each bind.
        pub reply_to: std::rc::Rc<std::cell::RefCell<String>>,
        /// Callback: DM clicked → (sender_id).
        pub on_dm: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String)>>>>,
        /// Callback: open thread → (thread_root_event_id).
        pub on_open_thread: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String)>>>>,
        /// Sender's Matrix user ID (e.g. @user:server).
        pub sender_id_text: std::rc::Rc<std::cell::RefCell<String>>,
        /// Callback: media click → (mxc_url, filename, source_json).
        pub on_media_click: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String, String)>>>>,
        /// Current media URL, filename, and source JSON.
        pub media_url: std::rc::Rc<std::cell::RefCell<String>>,
        pub media_filename: std::rc::Rc<std::cell::RefCell<String>>,
        pub media_source_json: std::rc::Rc<std::cell::RefCell<String>>,
        /// Cached local file path after download.
        pub media_cached_path: std::rc::Rc<std::cell::RefCell<Option<String>>>,
        /// Callback: bookmark clicked → (event_id, sender, body, timestamp).
        pub on_bookmark: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String, String, u64)>>>>,
        /// Callback: unbookmark clicked → (event_id).
        pub on_unbookmark: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String)>>>>,
        /// Callback: add sender to rolodex → (user_id, display_name).
        pub on_add_to_rolodex: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String)>>>>,
        /// Callback: remove sender from rolodex → (user_id).
        pub on_remove_from_rolodex: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String)>>>>,
        /// Callback: fetch notes for a contact → (user_id) → Option<notes>.
        pub on_get_rolodex_notes: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String) -> Option<String>>>>>,
        /// Callback: save updated notes for a contact → (user_id, notes).
        pub on_save_rolodex_notes: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String, String)>>>>,
        /// Current message timestamp — updated on each bind.
        pub timestamp_val: std::rc::Rc<std::cell::RefCell<u64>>,
        /// Whether this row is currently bookmarked — drives icon + CSS class.
        pub is_bookmarked: std::cell::Cell<bool>,
        /// Edit, delete, DM, and bookmark buttons — visibility/icon toggled per message.
        pub edit_button: std::cell::RefCell<Option<gtk::Button>>,
        pub delete_button: std::cell::RefCell<Option<gtk::Button>>,
        pub dm_button: std::cell::RefCell<Option<gtk::Button>>,
        pub bookmark_button: std::cell::RefCell<Option<gtk::Button>>,
        #[template_child]
        pub unread_divider_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub sender_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub sender_flag_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub timestamp_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub body_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub system_event_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub divider_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub body_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub reply_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub reply_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub thread_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub media_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub media_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub media_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub reactions_box: TemplateChild<gtk::Box>,
        /// Floating action bar (not in template — created programmatically).
        pub action_bar: gtk::Box,
        pub reply_button: gtk::Button,
        pub react_button: gtk::Button,
        /// FNV-1a hash of the last rendered (body, formatted_body).
        /// Compared against MessageObject::body_hash() — O(1) u64 comparison with
        /// no allocation. When equal, set_markup() is skipped on rebind.
        pub last_body_hash: std::cell::Cell<u64>,
        /// FNV-1a hash of the last rendered reactions JSON.
        /// Compared against MessageObject::reactions_hash() — O(1) with no allocation.
        pub last_reactions_hash: std::cell::Cell<u64>,
        /// Pool of reaction pill labels reused across rebinds. Hide unused ones
        /// instead of removing + recreating them. Widget construction is the
        /// costly path per CLAUDE.md §3, so we pay the allocation once.
        pub reaction_pills: std::cell::RefCell<Vec<gtk::Label>>,
        /// Signal handler IDs for notify connections on the currently bound
        /// MessageObject. Disconnected on unbind to prevent stale handlers
        /// accumulating as rows are recycled by the ListView factory.
        pub flash_handler: std::cell::RefCell<Option<(glib::Object, glib::SignalHandlerId)>>,
        pub new_message_handler: std::cell::RefCell<Option<(glib::Object, glib::SignalHandlerId)>>,
        /// Handler for notify::is-first-unread — shows/hides the divider bar above the row.
        pub unread_divider_handler: std::cell::RefCell<Option<(glib::Object, glib::SignalHandlerId)>>,
        /// Handler for notify::rendered-markup — applies the Pango markup
        /// delivered by the background markup worker after the row has
        /// already been bound (the initial bind showed the plain-text
        /// fallback). Disconnected in clear_flash_handler alongside the
        /// other per-bind notify handlers.
        pub markup_handler: std::cell::RefCell<Option<(glib::Object, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageRow {
        const NAME: &'static str = "MxMessageRow";
        type Type = super::MessageRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MessageRow {
        fn constructed(&self) {
            self.parent_constructed();

            // Announce this widget as a list item so screen readers
            // (via AT-SPI) know it represents a single message in a list.
            use gtk::prelude::AccessibleExt;
            self.obj().set_accessible_role(gtk::AccessibleRole::ListItem);

            // Build floating action bar — appears on hover without
            // affecting layout. Uses a Popover for zero-layout-impact.
            self.reply_button.set_icon_name("mail-reply-sender-symbolic");
            self.reply_button.set_tooltip_text(Some("Reply"));
            self.reply_button.add_css_class("flat");
            self.reply_button.add_css_class("circular");

            self.react_button.set_icon_name("face-smile-symbolic");
            self.react_button.set_tooltip_text(Some("React"));
            self.react_button.add_css_class("flat");
            self.react_button.add_css_class("circular");

            let edit_button = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .tooltip_text("Edit")
                .build();
            edit_button.add_css_class("flat");
            edit_button.add_css_class("circular");

            let delete_button = gtk::Button::builder()
                .icon_name("edit-delete-symbolic")
                .tooltip_text("Delete")
                .build();
            delete_button.add_css_class("flat");
            delete_button.add_css_class("circular");

            let dm_button = gtk::Button::builder()
                .icon_name("avatar-default-symbolic")
                .tooltip_text("Direct Message")
                .build();
            dm_button.add_css_class("flat");
            dm_button.add_css_class("circular");

            let bookmark_button = gtk::Button::builder()
                .icon_name("non-starred-symbolic")
                .tooltip_text("Save for later")
                .build();
            bookmark_button.add_css_class("flat");
            bookmark_button.add_css_class("circular");
            self.bookmark_button.replace(Some(bookmark_button.clone()));

            // Copy: writes the stored plain-text body to the system
            // clipboard. Separate from making body_label selectable,
            // which would backfill a GtkTextView per row and regress
            // scroll perf (see CLAUDE.md §1). Clipboard path reads the
            // cached body_text Rc<RefCell<String>> — no allocation on
            // scroll, cost paid only on explicit user click.
            let copy_button = gtk::Button::builder()
                .icon_name("edit-copy-symbolic")
                .tooltip_text("Copy message")
                .build();
            copy_button.add_css_class("flat");
            copy_button.add_css_class("circular");

            // Select: flips body_label.set_selectable(true) on demand so
            // the user can drag-select a specific phrase or URL. The
            // selectable flag backfills a GtkTextView (~10-20ms per row),
            // so we pay that cost only for the single row the user
            // activated. On focus-out of the body label, flip back to
            // false so no row in the factory pool stays selectable after
            // the user has moved on.
            let select_button = gtk::Button::builder()
                .icon_name("edit-select-all-symbolic")
                .tooltip_text("Select text in message")
                .build();
            select_button.add_css_class("flat");
            select_button.add_css_class("circular");

            // Community-safety plugin: toggle a local "caution" flag on
            // this message's sender. One-click toggles (no dialog for
            // v1; category defaults to "caution", reason left empty).
            // The pill next to the sender label refreshes immediately
            // via a re-bind.
            #[cfg(feature = "community-safety")]
            let flag_button = gtk::Button::builder()
                .icon_name("dialog-warning-symbolic")
                .tooltip_text("Flag sender as problematic")
                .build();
            #[cfg(feature = "community-safety")]
            {
                flag_button.add_css_class("flat");
                flag_button.add_css_class("circular");
            }

            self.action_bar.set_orientation(gtk::Orientation::Horizontal);
            self.action_bar.set_spacing(2);
            self.action_bar.append(&self.reply_button);
            self.action_bar.append(&dm_button);
            self.action_bar.append(&self.react_button);
            self.action_bar.append(&bookmark_button);
            self.action_bar.append(&copy_button);
            self.action_bar.append(&select_button);
            #[cfg(feature = "community-safety")]
            self.action_bar.append(&flag_button);
            self.action_bar.append(&edit_button);
            self.action_bar.append(&delete_button);
            self.edit_button.replace(Some(edit_button.clone()));
            self.delete_button.replace(Some(delete_button.clone()));
            self.dm_button.replace(Some(dm_button.clone()));

            // Keep the action bar in the normal vbox flow so there is no
            // layout shift on hover. It's always present but invisible
            // (opacity 0) when not hovered. Compact CSS keeps its height
            // small so the reserved space is minimal.
            self.action_bar.add_css_class("msg-action-bar");
            let obj = self.obj();
            if let Some(vbox) = obj.first_child().and_downcast::<gtk::Box>() {
                vbox.append(&self.action_bar);
            }

            // Fade in/out on hover via CSS class — no layout change.
            let action_bar = self.action_bar.clone();
            let hover = gtk::EventControllerMotion::new();
            let ab_enter = action_bar.clone();
            hover.connect_enter(move |_, _, _| {
                ab_enter.add_css_class("msg-action-bar-visible");
            });
            let ab_leave = action_bar;
            hover.connect_leave(move |_| {
                ab_leave.remove_css_class("msg-action-bar-visible");
            });
            obj.add_controller(hover);

            // "row.copy" action — copies the message body to the clipboard.
            // Registered here in constructed() so the action is wired once per
            // pool slot, not on every bind.
            let copy_body = self.body_text.clone();
            let copy_action = gio::SimpleAction::new("copy", None);
            copy_action.connect_activate(move |_, _| {
                let text = copy_body.borrow().clone();
                if !text.is_empty() {
                    if let Some(display) = gtk::gdk::Display::default() {
                        display.clipboard().set_text(&text);
                    }
                }
            });
            let action_group = gio::SimpleActionGroup::new();
            action_group.add_action(&copy_action);
            obj.insert_action_group("row", Some(&action_group));

            // Right-click on the row shows a clipboard context menu.
            let body_click = gtk::GestureClick::builder().button(3).build();
            let obj_weak = obj.downgrade();
            body_click.connect_released(move |gesture, _, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(row) = obj_weak.upgrade() else { return };
                if row.imp().body_text.borrow().is_empty() { return; }
                let menu = gio::Menu::new();
                menu.append(Some("Copy"), Some("row.copy"));
                let popover = gtk::PopoverMenu::from_model(Some(&menu));
                popover.set_parent(&row);
                popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                popover.set_has_arrow(false);
                popover.connect_closed(|p| p.unparent());
                popover.popup();
            });
            obj.add_controller(body_click);

            // Reply button — reads current event_id/sender/body from row state.
            let event_id = self.event_id.clone();
            let sender_text = self.sender_text.clone();
            let body_text = self.body_text.clone();
            let on_reply_ref = self.on_reply.clone();
            self.reply_button.connect_clicked(move |_| {
                let eid = event_id.borrow().clone();
                let sender = sender_text.borrow().clone();
                let body = body_text.borrow().clone();
                if let Some(ref cb) = *on_reply_ref.borrow() {
                    cb(eid, sender, body);
                }
            });

            // DM button — open/create DM with message sender.
            let sender_id = self.sender_id_text.clone();
            let on_dm_ref = self.on_dm.clone();
            dm_button.connect_clicked(move |_| {
                let uid = sender_id.borrow().clone();
                if !uid.is_empty() {
                    if let Some(ref cb) = *on_dm_ref.borrow() {
                        cb(uid);
                    }
                }
            });

            // Bookmark button — toggles add/remove based on current bookmarked state.
            let obj_weak = self.obj().downgrade();
            let ev = self.event_id.clone();
            let st = self.sender_text.clone();
            let bt = self.body_text.clone();
            let ts_val = self.timestamp_val.clone();
            let on_bm = self.on_bookmark.clone();
            let on_ubm = self.on_unbookmark.clone();
            bookmark_button.connect_clicked(move |_| {
                let Some(obj) = obj_weak.upgrade() else { return };
                let eid = ev.borrow().clone();
                if eid.is_empty() { return; }
                if obj.imp().is_bookmarked.get() {
                    if let Some(ref cb) = *on_ubm.borrow() {
                        cb(eid);
                    }
                } else {
                    let sender = st.borrow().clone();
                    let body = bt.borrow().clone();
                    let ts = *ts_val.borrow();
                    if let Some(ref cb) = *on_bm.borrow() {
                        cb(eid, sender, body, ts);
                    }
                }
            });

            // Left-click sender name to start DM.
            let sender_id = self.sender_id_text.clone();
            let on_dm_ref = self.on_dm.clone();
            let sender_click = gtk::GestureClick::builder().button(1).build();
            sender_click.connect_released(move |_, _, _, _| {
                let uid = sender_id.borrow().clone();
                if !uid.is_empty() {
                    if let Some(ref cb) = *on_dm_ref.borrow() {
                        cb(uid);
                    }
                }
            });
            self.sender_label.add_controller(sender_click);
            self.sender_label.set_cursor_from_name(Some("pointer"));

            // Right-click sender name: contact card (if in rolodex) or "Add to contacts".
            let sender_id = self.sender_id_text.clone();
            let sender_name = self.sender_text.clone();
            let on_add = self.on_add_to_rolodex.clone();
            let on_remove = self.on_remove_from_rolodex.clone();
            let on_get_notes = self.on_get_rolodex_notes.clone();
            let on_save_notes = self.on_save_rolodex_notes.clone();
            let right_click = gtk::GestureClick::builder().button(3).build();
            let label_ref = self.sender_label.clone();
            right_click.connect_released(move |gesture, _, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let uid = sender_id.borrow().clone();
                let name = sender_name.borrow().clone();
                if uid.is_empty() { return; }

                let in_rolodex = label_ref.has_css_class("rolodex-contact");
                let popover = gtk::Popover::new();
                popover.set_parent(&label_ref as &gtk::Label);
                let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                popover.set_pointing_to(Some(&rect));

                if in_rolodex {
                    // Contact card: name, id, editable notes, remove button.
                    let notes_text = on_get_notes.borrow().as_ref()
                        .and_then(|cb| cb(uid.clone()))
                        .unwrap_or_default();

                    let vbox = gtk::Box::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .spacing(6)
                        .margin_top(8).margin_bottom(8)
                        .margin_start(10).margin_end(10)
                        .build();

                    let name_label = gtk::Label::builder()
                        .label(&name)
                        .halign(gtk::Align::Start)
                        .build();
                    name_label.add_css_class("heading");

                    let id_label = gtk::Label::builder()
                        .label(&uid)
                        .halign(gtk::Align::Start)
                        .build();
                    id_label.add_css_class("dim-label");
                    id_label.add_css_class("caption");

                    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);

                    let notes_label = gtk::Label::builder()
                        .label("Notes")
                        .halign(gtk::Align::Start)
                        .build();
                    notes_label.add_css_class("caption");

                    let notes_entry = gtk::Entry::builder()
                        .text(&notes_text)
                        .placeholder_text("Add a note…")
                        .hexpand(true)
                        .build();

                    let btn_row = gtk::Box::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .spacing(6)
                        .homogeneous(true)
                        .build();

                    let save_btn = gtk::Button::builder().label("Save").build();
                    save_btn.add_css_class("flat");
                    save_btn.add_css_class("suggested-action");

                    let remove_btn = gtk::Button::builder().label("Remove contact").build();
                    remove_btn.add_css_class("flat");
                    remove_btn.add_css_class("destructive-action");

                    btn_row.append(&save_btn);
                    btn_row.append(&remove_btn);

                    vbox.append(&name_label);
                    vbox.append(&id_label);
                    vbox.append(&sep);
                    vbox.append(&notes_label);
                    vbox.append(&notes_entry);
                    vbox.append(&btn_row);
                    popover.set_child(Some(&vbox));

                    // Save notes.
                    let save_uid = uid.clone();
                    let on_save2 = on_save_notes.clone();
                    let notes_ref = notes_entry.clone();
                    let popover_weak = popover.downgrade();
                    save_btn.connect_clicked(move |_| {
                        let text = notes_ref.text().to_string();
                        if let Some(ref cb) = *on_save2.borrow() { cb(save_uid.clone(), text); }
                        if let Some(p) = popover_weak.upgrade() { p.popdown(); }
                    });

                    // Remove from contacts.
                    let rm_uid = uid.clone();
                    let on_remove2 = on_remove.clone();
                    let label_weak: glib::WeakRef<gtk::Label> = label_ref.downgrade();
                    let popover_weak2 = popover.downgrade();
                    remove_btn.connect_clicked(move |_| {
                        if let Some(ref cb) = *on_remove2.borrow() { cb(rm_uid.clone()); }
                        if let Some(l) = label_weak.upgrade() { l.remove_css_class("rolodex-contact"); }
                        if let Some(p) = popover_weak2.upgrade() { p.popdown(); }
                    });
                } else {
                    // Non-contact: simple "Add to contacts" button.
                    let btn = gtk::Button::builder().label("Add to contacts").build();
                    btn.add_css_class("flat");
                    popover.set_child(Some(&btn));
                    let on_add2 = on_add.clone();
                    let label_weak: glib::WeakRef<gtk::Label> = label_ref.downgrade();
                    let popover_weak = popover.downgrade();
                    btn.connect_clicked(move |_| {
                        if let Some(ref cb) = *on_add2.borrow() { cb(uid.clone(), name.clone()); }
                        if let Some(l) = label_weak.upgrade() { l.add_css_class("rolodex-contact"); }
                        if let Some(p) = popover_weak.upgrade() { p.popdown(); }
                    });
                }

                popover.popup();
            });
            self.sender_label.add_controller(right_click);

            // Click thread icon to open thread sidebar.
            let event_id = self.event_id.clone();
            let reply_to = self.reply_to.clone();
            let on_thread_ref = self.on_open_thread.clone();
            let thread_click = gtk::GestureClick::new();
            thread_click.connect_released(move |_, _, _, _| {
                // The thread root is either the reply_to (for thread replies)
                // or the event_id itself (for the thread root message).
                let thread_root = {
                    let rt = reply_to.borrow().clone();
                    if rt.is_empty() { event_id.borrow().clone() } else { rt }
                };
                if !thread_root.is_empty() {
                    if let Some(ref cb) = *on_thread_ref.borrow() {
                        cb(thread_root);
                    }
                }
            });
            self.thread_icon.add_controller(thread_click);
            self.thread_icon.set_cursor_from_name(Some("pointer"));

            // React button — plain Button; on click we ask MessageView to
            // reparent its single shared EmojiChooser onto this button and
            // pop it up. Previous architecture built one EmojiChooser per
            // MessageRow and each populated a full emoji widget tree (~1.5GB
            // aggregate per heaptrack Top-Down view).
            let react_btn = gtk::Button::builder()
                .icon_name("face-smile-symbolic")
                .tooltip_text("React")
                .build();
            react_btn.add_css_class("flat");
            react_btn.add_css_class("circular");
            self.action_bar.remove(&self.react_button);
            self.action_bar.append(&react_btn);

            let event_id = self.event_id.clone();
            let on_show_picker = self.on_show_react_picker.clone();
            react_btn.connect_clicked(move |btn| {
                let eid = event_id.borrow().clone();
                if eid.is_empty() { return; }
                if let Some(ref cb) = *on_show_picker.borrow() {
                    cb(eid, btn.clone());
                }
            });

            // Edit button — enter edit mode with current body.
            let event_id = self.event_id.clone();
            let body_text = self.body_text.clone();
            let on_edit = self.on_edit.clone();
            edit_button.connect_clicked(move |_| {
                let eid = event_id.borrow().clone();
                let body = body_text.borrow().clone();
                if let Some(ref cb) = *on_edit.borrow() {
                    cb(eid, body);
                }
            });

            // Copy button — write body_text to the system clipboard.
            let body_text = self.body_text.clone();
            copy_button.connect_clicked(move |btn| {
                let body = body_text.borrow().clone();
                if body.is_empty() { return; }
                btn.display().clipboard().set_text(&body);
            });

            // Select-text button — flip body_label selectable on demand,
            // grab focus so drag-selection starts immediately.
            let body_label = self.body_label.clone();
            select_button.connect_clicked(move |_| {
                body_label.set_selectable(true);
                body_label.grab_focus();
            });

            // Flag button — toggle local community-safety flag on the
            // message's sender. One-click toggles; the pill next to
            // the sender label refreshes on the current row via a
            // manual re-paint of sender_flag_label. Other rows from
            // the same sender will update on their next bind.
            #[cfg(feature = "community-safety")]
            {
                let sender_id = self.sender_id_text.clone();
                let flag_label = self.sender_flag_label.clone();
                flag_button.connect_clicked(move |_| {
                    let uid = sender_id.borrow().clone();
                    if uid.is_empty() { return; }
                    let store = &crate::plugins::community_safety::FLAGGED_STORE;
                    if store.get(&uid).is_some() {
                        store.unflag(&uid);
                        flag_label.set_visible(false);
                    } else {
                        let entry = store.flag(&uid, "caution", "");
                        let category_escaped = glib::markup_escape_text(&entry.category);
                        let markup = format!(
                            "<span foreground=\"#e5a50a\" background=\"#e5a50a26\"> ⚠ {category_escaped} </span>"
                        );
                        flag_label.set_markup(&markup);
                        flag_label.set_tooltip_text(Some(&format!("Flagged as {}", entry.category)));
                        flag_label.set_visible(true);
                    }
                });
            }

            // When the body label loses focus (user clicked away or
            // pressed Escape) flip selectable back off. This keeps the
            // GtkTextView backing store cost out of the scroll path for
            // every row except the one the user is actively selecting in.
            let focus_ctrl = gtk::EventControllerFocus::new();
            let body_label_leave = self.body_label.clone();
            focus_ctrl.connect_leave(move |_| {
                body_label_leave.set_selectable(false);
            });
            self.body_label.add_controller(focus_ctrl);

            // Delete button — redact the message.
            let event_id = self.event_id.clone();
            let on_delete = self.on_delete.clone();
            delete_button.connect_clicked(move |_| {
                let eid = event_id.borrow().clone();
                if let Some(ref cb) = *on_delete.borrow() {
                    cb(eid);
                }
            });

            // Reaction pill click — toggle via gesture on the reactions box.
            let event_id = self.event_id.clone();
            let on_react = self.on_react.clone();
            let gesture = gtk::GestureClick::new();
            gesture.connect_released(move |gesture, _, x, y| {
                let Some(widget) = gesture.widget() else { return };
                if let Some(child) = widget.pick(x, y, gtk::PickFlags::DEFAULT) {
                    // Walk up to find a Label (the reaction pill).
                    let mut w: Option<gtk::Widget> = Some(child);
                    while let Some(ref current) = w {
                        if let Ok(label) = current.clone().downcast::<gtk::Label>() {
                            let text = label.text().to_string();
                            let emoji = text.split_whitespace().next()
                                .unwrap_or(&text).to_string();
                            let eid = event_id.borrow().clone();
                            if let Some(ref cb) = *on_react.borrow() {
                                cb(eid, emoji);
                            }
                            break;
                        }
                        w = current.parent();
                    }
                }
            });
            self.reactions_box.add_controller(gesture);

            // Reply box click — jump to the original message.
            let reply_to = self.reply_to.clone();
            let on_jump = self.on_jump_to_reply.clone();
            let reply_gesture = gtk::GestureClick::new();
            reply_gesture.connect_released(move |_, _, _, _| {
                let target = reply_to.borrow().clone();
                if !target.is_empty() {
                    if let Some(ref cb) = *on_jump.borrow() {
                        cb(target);
                    }
                }
            });
            self.reply_box.add_controller(reply_gesture);
            // Make reply box look clickable.
            self.reply_box.set_cursor(gtk::gdk::Cursor::from_name("pointer", None).as_ref());

            // body_label matrix.to link handler — connected once at construction
            // so the label itself doesn't need to be re-created on every bind.
            self.body_label.connect_activate_link(|_lbl, uri| {
                if let Some(matrix_id) = super::parse_matrix_uri(uri) {
                    if let Some(app) = gio::Application::default() {
                        if let Some(gtk_app) = app.downcast_ref::<gtk::Application>() {
                            if let Some(window) = gtk_app.active_window() {
                                if let Some(win) = window.downcast_ref::<crate::widgets::MxWindow>() {
                                    win.handle_matrix_link(&matrix_id);
                                    return glib::Propagation::Stop;
                                }
                            }
                        }
                    }
                }
                glib::Propagation::Proceed
            });

            // Media button click — download and show preview.
            let media_url = self.media_url.clone();
            let media_filename = self.media_filename.clone();
            let media_src = self.media_source_json.clone();
            let on_media = self.on_media_click.clone();
            self.media_button.connect_clicked(move |_| {
                let url = media_url.borrow().clone();
                let filename = media_filename.borrow().clone();
                let source_json = media_src.borrow().clone();
                if !url.is_empty() {
                    if let Some(ref cb) = *on_media.borrow() {
                        cb(url, filename, source_json);
                    }
                }
            });
        }
    }
    impl WidgetImpl for MessageRow {}

    impl BoxImpl for MessageRow {}
}

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk::subclass::prelude::*;

/// Pre-render the sender label markup string for use in MessageObject.
/// Computes `<span foreground="#rrggbb">Escaped Name</span>` once at load
/// time so bind() never calls nick_color/markup_escape_text/format! per row.
pub(crate) fn prerender_sender_markup(sender: &str, sender_id: &str) -> String {
    if sender_id.is_empty() {
        sender.to_string()
    } else {
        let color = nick_color(sender_id);
        let escaped = gtk::glib::markup_escape_text(sender);
        format!("<span foreground=\"{color}\">{escaped}</span>")
    }
}

/// Derive a stable display color from a Matrix user ID.
/// Result is cached so each unique user ID is computed only once per session.
fn nick_color(user_id: &str) -> String {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|cache| {
        let mut c = cache.borrow_mut();
        if let Some(color) = c.get(user_id) {
            return color.clone();
        }
        let hash = user_id.bytes().fold(5381u32, |h, b| h.wrapping_mul(33).wrapping_add(b as u32));
        let hue = (hash % 360) as f64;
        let color = hsl_to_hex(hue, 0.65, 0.50);
        c.insert(user_id.to_string(), color.clone());
        color
    })
}

/// Convert HSL (h in [0,360], s and l in [0,1]) to a CSS hex color.
fn hsl_to_hex(h: f64, s: f64, l: f64) -> String {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match (h as u32) / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let r = ((r1 + m) * 255.0) as u8;
    let g = ((g1 + m) * 255.0) as u8;
    let b = ((b1 + m) * 255.0) as u8;
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Format a Unix timestamp (seconds) into a human-readable string.
/// Shows "HH:MM" for today, "Yesterday HH:MM", or "Mon DD HH:MM" for older.
pub(crate) fn format_timestamp(ts: u64) -> String {
    let today = glib::DateTime::now_local().ok();
    format_timestamp_with_today(ts, today.as_ref())
}

/// Like `format_timestamp` but accepts a pre-computed `today` so callers
/// processing a batch can call `glib::DateTime::now_local()` exactly once.
pub(crate) fn format_timestamp_with_today(ts: u64, today: Option<&glib::DateTime>) -> String {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let Ok(dt) = glib::DateTime::from_unix_local(ts as i64) else {
        return String::new();
    };

    let Some(today) = today else {
        return dt.format("%H:%M").map(|s: glib::GString| s.to_string()).unwrap_or_default();
    };

    let event_time = UNIX_EPOCH + Duration::from_secs(ts);
    let secs_ago = SystemTime::now().duration_since(event_time).unwrap_or_default().as_secs();
    let same_day = dt.year() == today.year()
        && dt.day_of_year() == today.day_of_year();

    let fmt = match () {
        _ if same_day => "%H:%M",
        _ if secs_ago < 86400 * 2 => "Yesterday %H:%M",
        _ if dt.year() == today.year() => "%b %e %H:%M",
        _ => "%b %e, %Y %H:%M",
    };
    dt.format(fmt)
        .map(|s: glib::GString| s.to_string())
        .unwrap_or_default()
}

/// Strip Matrix reply fallback lines from message body.
/// These are lines starting with "> " at the beginning of the body,
/// followed by an optional blank line.
pub fn strip_reply_fallback(body: &str) -> String {
    let mut lines = body.lines().peekable();
    // Skip all leading "> " lines.
    while let Some(line) = lines.peek() {
        if line.starts_with("> ") {
            lines.next();
        } else {
            break;
        }
    }
    // Skip one blank line after the quote block.
    if let Some(line) = lines.peek() {
        if line.is_empty() {
            lines.next();
        }
    }
    let result: String = lines.collect::<Vec<_>>().join("\n");
    if result.is_empty() { body.to_string() } else { result }
}

/// Extract an image/gif URL from message body text, if present.
/// Converts Giphy page URLs to direct media URLs.
pub(crate) fn extract_image_url(body: &str) -> Option<String> {
    let body_trimmed = body.trim();
    for word in body_trimmed.split_whitespace() {
        if !(word.starts_with("https://") || word.starts_with("http://")) {
            continue;
        }
        // Strip query params/fragments for extension check.
        let lower = word.to_lowercase();
        let path_part = lower.split('?').next().unwrap_or(&lower);
        let path_part = path_part.split('#').next().unwrap_or(path_part);
        // Any URL ending in an image extension.
        if path_part.ends_with(".gif")
            || path_part.ends_with(".png")
            || path_part.ends_with(".jpg")
            || path_part.ends_with(".jpeg")
            || path_part.ends_with(".webp")
        {
            return Some(word.to_string());
        }
        // Giphy page URLs → convert to direct media URL.
        // https://giphy.com/gifs/NAME-ID → https://media.giphy.com/media/ID/giphy.gif
        if lower.contains("giphy.com/gifs/") {
            if let Some(slug) = word.rsplit('/').next() {
                // The ID is the last part after the last dash, or the whole slug.
                let id = slug.rsplit('-').next().unwrap_or(slug);
                return Some(format!("https://media.giphy.com/media/{id}/giphy.gif"));
            }
        }
        // media.giphy.com URLs are already direct.
        if lower.contains("media.giphy.com") {
            return Some(word.to_string());
        }
        // Tenor media URLs — media.tenor.com serves GIFs/videos directly.
        if lower.contains("media.tenor.com") || lower.contains("c.tenor.com") {
            return Some(word.to_string());
        }
        // Tenor page URLs.
        if lower.contains("tenor.com/view/") {
            return Some(word.to_string());
        }
    }
    None
}

/// Convert URLs in already-escaped markup text into clickable <a> links.
fn linkify_urls(text: &str) -> String {
    crate::markdown::linkify_urls(text)
}

/// Remove all children from the body_box before repopulating it.
fn clear_body_box(body_box: &gtk::Box) {
    while let Some(child) = body_box.first_child() {
        body_box.remove(&child);
    }
}

/// Extract a Matrix room ID or alias from a matrix.to or matrix: URI.
/// Returns `Some("!roomid:server")`, `Some("#alias:server")`, or `None`.
pub(crate) fn parse_matrix_uri(uri: &str) -> Option<String> {
    // https://matrix.to/#/!roomid:server or https://matrix.to/#/#alias:server
    if let Some(rest) = uri.strip_prefix("https://matrix.to/#/") {
        // URL-decode the first component.
        let id = rest.split('?').next().unwrap_or(rest);
        let id = percent_decode(id);
        if id.starts_with('!') || id.starts_with('#') {
            return Some(id);
        }
    }
    // matrix:r/alias.server  or  matrix:roomid/!room:server
    if let Some(rest) = uri.strip_prefix("matrix:r/") {
        let alias = rest.split('?').next().unwrap_or(rest);
        return Some(format!("#{}", alias.replacen('/', ":", 1)));
    }
    if let Some(rest) = uri.strip_prefix("matrix:roomid/") {
        let id = rest.split('?').next().unwrap_or(rest);
        return Some(format!("!{}", id.replacen('/', ":", 1)));
    }
    None
}

fn percent_decode(s: &str) -> String {
    // Minimal percent-decoding for %21 (!) and %23 (#) common in matrix.to links.
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(std::str::from_utf8(&bytes[i+1..i+3]).unwrap_or(""), 16) {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}


glib::wrapper! {
    pub struct MessageRow(ObjectSubclass<imp::MessageRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::Orientable;
}

impl MessageRow {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn set_on_reply<F: Fn(String, String, String) + 'static>(&self, f: F) {
        self.imp().on_reply.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_react<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_react.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_show_react_picker<F: Fn(String, gtk::Button) + 'static>(&self, f: F) {
        self.imp().on_show_react_picker.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_edit<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_edit.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_delete<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_delete.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_jump_to_reply<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_jump_to_reply.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_media_click<F: Fn(String, String, String) + 'static>(&self, f: F) {
        self.imp().on_media_click.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_dm<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_dm.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_open_thread<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_open_thread.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_bookmark<F: Fn(String, String, String, u64) + 'static>(&self, f: F) {
        self.imp().on_bookmark.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_unbookmark<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_unbookmark.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_add_to_rolodex<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_add_to_rolodex.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_remove_from_rolodex<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_remove_from_rolodex.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_get_rolodex_notes<F: Fn(String) -> Option<String> + 'static>(&self, f: F) {
        self.imp().on_get_rolodex_notes.borrow_mut().replace(Box::new(f));
    }

    pub fn set_on_save_rolodex_notes<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_save_rolodex_notes.borrow_mut().replace(Box::new(f));
    }

    /// Update the bookmarked visual state: CSS class + button icon.
    pub fn set_bookmarked(&self, bookmarked: bool) {
        self.imp().is_bookmarked.set(bookmarked);
        if bookmarked {
            self.add_css_class("bookmarked-message");
        } else {
            self.remove_css_class("bookmarked-message");
        }
        if let Some(ref btn) = *self.imp().bookmark_button.borrow() {
            if bookmarked {
                btn.set_icon_name("starred-symbolic");
                btn.set_tooltip_text(Some("Remove bookmark"));
            } else {
                btn.set_icon_name("non-starred-symbolic");
                btn.set_tooltip_text(Some("Save for later"));
            }
        }
    }

    /// Bind a MessageObject to this row.
    /// `ctx` is the timeline-level context owned by MessageView.
    pub fn bind_message_object(
        &self,
        msg: &crate::models::MessageObject,
        ctx: &crate::widgets::MessageRowContext,
    ) {
        let _g = crate::perf::scope("bind_message_object");
        let imp = self.imp();
        // Reset the on-demand selectable flag if it somehow leaked from
        // a prior bind (focus-leave on body_label normally handles this,
        // but a new message arriving into the same row while the label
        // still has focus would otherwise keep the GtkTextView backing
        // store alive on every recycled row).
        imp.body_label.set_selectable(false);
        let highlight_names = &ctx.highlight_names;
        let my_user_id = ctx.my_user_id.as_str();
        let is_dm_room = ctx.is_dm;
        let show_media = !ctx.no_media;

        // System event row — join/leave/invite/kick/ban inline text.
        // body is read only here so we defer the clone of other properties.
        if msg.is_system_event() {
            imp.sender_label.set_visible(false);
            imp.timestamp_label.set_visible(false);
            imp.body_label.set_visible(false);
            imp.body_box.set_visible(false);
            imp.divider_label.set_visible(false);
            imp.system_event_label.set_text(&msg.body());
            imp.system_event_label.set_visible(true);
            imp.reply_box.set_visible(false);
            imp.thread_icon.set_visible(false);
            imp.reactions_box.set_visible(false);
            imp.media_button.set_visible(false);
            imp.action_bar.set_visible(false);
            self.remove_css_class("message-divider");
            self.add_css_class("message-system-event");
            return;
        }
        // Reset system event styling.
        self.remove_css_class("message-system-event");
        imp.system_event_label.set_visible(false);

        // Defer property clones until after early returns.
        let sender = msg.sender();
        let body = msg.body();
        let formatted_body = msg.formatted_body();
        let timestamp = msg.timestamp();
        let formatted_ts = msg.formatted_timestamp();
        let reply_to = msg.reply_to();
        let thread_root = msg.thread_root();
        let sender_id = msg.sender_id();  // read once — used in 3 places below

        // Divider row — "New messages" separator, not a real message.
        if sender.is_empty() && msg.event_id().is_empty() && body.contains("──") {
            imp.sender_label.set_visible(false);
            imp.timestamp_label.set_visible(false);
            imp.body_label.set_visible(false);
            imp.body_box.set_visible(false);
            imp.divider_label.set_text(&body);
            imp.divider_label.set_visible(true);
            imp.reply_box.set_visible(false);
            imp.thread_icon.set_visible(false);
            imp.reactions_box.set_visible(false);
            imp.media_button.set_visible(false);
            imp.action_bar.set_visible(false);
            self.add_css_class("message-divider");
            return;
        }
        // Reset divider styling if this row was previously a divider.
        imp.divider_label.set_visible(false);
        self.remove_css_class("message-divider");
        imp.sender_label.set_visible(true);
        imp.timestamp_label.set_visible(true);
        imp.action_bar.set_visible(true);

        // Store current message data for action buttons (click handlers read these).
        imp.event_id.replace(msg.event_id());
        imp.sender_text.replace(sender.clone());
        imp.sender_id_text.replace(sender_id.clone());
        imp.body_text.replace(body.clone());
        imp.reply_to.replace(reply_to.clone());
        imp.timestamp_val.replace(timestamp);

        // Reply indicator — pre-computed in info_to_obj, no format!/scan here.
        let reply_label = msg.reply_label();
        if !reply_to.is_empty() {
            imp.reply_label.set_label(&reply_label);
            imp.reply_box.set_visible(true);
        } else {
            imp.reply_box.set_visible(false);
        }

        // Thread indicator.
        imp.thread_icon.set_visible(!thread_root.is_empty());

        // Show edit/delete only on own messages.
        let is_own = !my_user_id.is_empty() && sender_id == my_user_id;
        if let Some(ref btn) = *imp.edit_button.borrow() {
            btn.set_visible(is_own);
        }
        if let Some(ref btn) = *imp.delete_button.borrow() {
            btn.set_visible(is_own);
        }
        // Hide DM button on own messages and in DM rooms (already a DM).
        if let Some(ref btn) = *imp.dm_button.borrow() {
            btn.set_visible(!is_own && !is_dm_room);
        }

        // Media attachment — all display strings pre-computed in info_to_obj.
        if !show_media {
            imp.media_button.set_visible(false);
        } else {
            let media_icon = msg.media_icon_name();
            if !media_icon.is_empty() {
                // Structured attachment (image, video, audio, file).
                imp.media_icon.set_icon_name(Some(&media_icon));
                imp.media_label.set_label(&msg.media_display_label());
                imp.media_button.update_property(&[gtk::accessible::Property::Label(
                    &msg.media_a11y_label()
                )]);
                imp.media_url.replace(msg.media_url_str());
                imp.media_filename.replace(msg.media_filename_str());
                imp.media_source_json.replace(msg.media_source_json_str());
                imp.media_button.set_visible(true);
            } else {
                // Plain-text message — check pre-extracted image URL.
                let image_url = msg.image_url();
                if !image_url.is_empty() {
                    imp.media_icon.set_icon_name(Some("image-x-generic-symbolic"));
                    let display = if image_url.contains("giphy.com") {
                        "GIF".to_string()
                    } else {
                        image_url.split('/').last().unwrap_or("image").to_string()
                    };
                    imp.media_label.set_label(&display);
                    imp.media_url.replace(image_url);
                    imp.media_filename.replace(display);
                    imp.media_button.set_visible(true);
                } else {
                    imp.media_button.set_visible(false);
                }
            }
        }

        // Reactions — O(1) hash check, full rebuild only when reactions changed.
        // Rebuild reuses pooled gtk::Label widgets in-place (see reaction_pills
        // in imp) rather than constructing new ones; GTK widget allocation is
        // the expensive path, so we grow the pool only when a row needs more
        // pills than any previous bind demanded.
        let reactions_hash = msg.reactions_hash();
        if imp.last_reactions_hash.get() != reactions_hash {
            let _g = crate::perf::scope("bind::reactions_rebuild");
            imp.last_reactions_hash.set(reactions_hash);
            let reactions = serde_json::from_str::<Vec<(String, u64, Vec<String>)>>(
                &msg.reactions_json()
            ).unwrap_or_default();

            let mut pills = imp.reaction_pills.borrow_mut();
            let need = reactions.len();
            while pills.len() < need {
                let pill = gtk::Label::builder()
                    .css_classes(["reaction-pill"])
                    .build();
                imp.reactions_box.append(&pill);
                pills.push(pill);
            }

            for (i, (emoji, count, names)) in reactions.iter().enumerate() {
                let label = if *count > 1 {
                    format!("{emoji} {count}")
                } else {
                    emoji.clone()
                };
                let tooltip = names.join(", ");
                let a11y_label = if *count > 1 {
                    format!("{count} reactions: {emoji}. From: {tooltip}")
                } else {
                    format!("Reaction: {emoji}. From: {tooltip}")
                };
                let pill = &pills[i];
                pill.set_label(&label);
                pill.set_tooltip_text(Some(&tooltip));
                pill.update_property(&[gtk::accessible::Property::Label(&a11y_label)]);
                pill.set_visible(true);
            }
            for pill in pills.iter().skip(need) {
                pill.set_visible(false);
            }
            imp.reactions_box.set_visible(need > 0);
        }

        // Delegate to text rendering with highlights.
        // Reply fallback is already stripped at the GObject level (info_to_obj).
        let force_highlight = msg.is_highlight();
        self.render_body(msg, &sender, &sender_id, &body, &formatted_body, &formatted_ts, highlight_names, force_highlight, &ctx.rolodex_ids);

        // Community-safety plugin: show an amber "Caution" pill next to
        // the sender label for flagged senders. Stored entirely locally —
        // see src/plugins/community_safety.rs.
        #[cfg(feature = "community-safety")]
        {
            if sender_id.is_empty() {
                imp.sender_flag_label.set_visible(false);
            } else if let Some(entry) = crate::plugins::community_safety::FLAGGED_STORE.get(&sender_id) {
                let category_escaped = glib::markup_escape_text(&entry.category);
                let markup = format!(
                    "<span foreground=\"#e5a50a\" background=\"#e5a50a26\"> ⚠ {category_escaped} </span>"
                );
                imp.sender_flag_label.set_markup(&markup);
                let tooltip = if entry.reason.is_empty() {
                    format!("Flagged as {}", entry.category)
                } else {
                    format!("Flagged as {}: {}", entry.category, entry.reason)
                };
                imp.sender_flag_label.set_tooltip_text(Some(&tooltip));
                imp.sender_flag_label.set_visible(true);
            } else {
                imp.sender_flag_label.set_visible(false);
            }
        }

        // Disconnect old flash handler before connecting to the new object.
        self.clear_flash_handler();
        // Reflect initial is_flashing state (row may be rebound while flashing).
        if msg.is_flashing() {
            self.add_css_class("message-flash");
        } else {
            self.remove_css_class("message-flash");
        }
        // Disconnect any previous is-new-message handler before rebinding.
        if let Some((obj, id)) = self.imp().new_message_handler.borrow_mut().take() {
            obj.disconnect(id);
        }
        // Apply new-message tint immediately, then track changes reactively.
        if msg.is_new_message() {
            self.add_css_class("new-message");
        } else {
            self.remove_css_class("new-message");
        }
        let nm_id = msg.connect_notify_local(Some("is-new-message"), {
            let row_weak = self.downgrade();
            move |obj, _| {
                use crate::models::MessageObject;
                if let (Some(row), Some(msg)) = (row_weak.upgrade(), obj.downcast_ref::<MessageObject>()) {
                    if msg.is_new_message() { row.add_css_class("new-message"); }
                    else { row.remove_css_class("new-message"); }
                }
            }
        });
        *self.imp().new_message_handler.borrow_mut() = Some((msg.clone().upcast(), nm_id));

        // Show/hide the pre-allocated "New messages" divider bar above this row.
        // Avoids inserting a sentinel list-store item for the divider, which would
        // fire items_changed and invalidate GTK's height cache for all following rows.
        if let Some((obj, id)) = self.imp().unread_divider_handler.borrow_mut().take() {
            obj.disconnect(id);
        }
        self.imp().unread_divider_box.set_visible(msg.is_first_unread());
        let div_id = msg.connect_notify_local(Some("is-first-unread"), {
            let row_weak = self.downgrade();
            move |obj, _| {
                use crate::models::MessageObject;
                if let (Some(row), Some(msg)) = (row_weak.upgrade(), obj.downcast_ref::<MessageObject>()) {
                    row.imp().unread_divider_box.set_visible(msg.is_first_unread());
                }
            }
        });
        *self.imp().unread_divider_handler.borrow_mut() = Some((msg.clone().upcast(), div_id));

        // Connect reactive flash handler to this MessageObject.
        let row_weak = self.downgrade();
        let id = msg.connect_notify_local(Some("is-flashing"), move |obj, _| {
            use crate::models::MessageObject;
            if let (Some(row), Some(msg)) = (row_weak.upgrade(), obj.downcast_ref::<MessageObject>()) {
                if msg.is_flashing() {
                    row.add_css_class("message-flash");
                } else {
                    row.remove_css_class("message-flash");
                }
            }
        });
        *self.imp().flash_handler.borrow_mut() = Some((msg.clone().upcast(), id));

        // Connect handler for async markup delivery. When the background
        // worker finishes html_to_pango and calls set_rendered_markup on
        // this MessageObject, the row swaps its body_label text from the
        // plain-text fallback (shown during the initial bind) to the
        // properly-rendered Pango markup. If the row has been recycled for
        // a different message in the interim, the weak-upgrade + event_id
        // check guards against writing to the wrong row.
        let row_weak = self.downgrade();
        let bound_eid = msg.event_id();
        let mu_id = msg.connect_notify_local(Some("rendered-markup"), move |obj, _| {
            use crate::models::MessageObject;
            let (Some(row), Some(msg)) = (row_weak.upgrade(), obj.downcast_ref::<MessageObject>())
                else { return };
            // Same-message check — the row may have been bound to a
            // different message by the time the worker returned.
            if *row.imp().event_id.borrow() != bound_eid { return; }
            let markup = msg.rendered_markup();
            if markup.is_empty() { return; }
            row.imp().body_label.set_markup(&markup);
            row.imp().body_label.set_visible(true);
        });
        *self.imp().markup_handler.borrow_mut() = Some((msg.clone().upcast(), mu_id));
    }

    /// Disconnect and clear the `notify::is-flashing` handler from the bound MessageObject.
    pub fn clear_flash_handler(&self) {
        if let Some((obj, id)) = self.imp().flash_handler.borrow_mut().take() {
            obj.disconnect(id);
        }
        if let Some((obj, id)) = self.imp().new_message_handler.borrow_mut().take() {
            obj.disconnect(id);
        }
        if let Some((obj, id)) = self.imp().unread_divider_handler.borrow_mut().take() {
            obj.disconnect(id);
        }
        if let Some((obj, id)) = self.imp().markup_handler.borrow_mut().take() {
            obj.disconnect(id);
        }
    }

    fn render_body(
        &self,
        msg: &crate::models::MessageObject,
        sender: &str,
        sender_id: &str,
        body: &str,
        formatted_body: &str,
        formatted_ts: &str,
        highlight_names: &[String],
        force_highlight: bool,
        rolodex_ids: &std::collections::HashSet<String>,
    ) {
        let _g = crate::perf::scope("render_body");
        let imp = self.imp();

        // Sender label — use pre-computed markup (nick_color + markup_escape_text
        // + format! all happened once in info_to_obj, never again on scroll bind).
        let sender_markup = msg.sender_markup();
        if sender_id.is_empty() {
            imp.sender_label.set_label(sender);
        } else if sender_markup.is_empty() {
            // Fallback for objects not created via info_to_obj.
            imp.sender_label.set_label(sender);
        } else {
            imp.sender_label.set_markup(&sender_markup);
        }

        // Rolodex contact indicator: subtle glow on sender name.
        if !sender_id.is_empty() {
            if rolodex_ids.contains(sender_id) {
                imp.sender_label.add_css_class("rolodex-contact");
            } else {
                imp.sender_label.remove_css_class("rolodex-contact");
            }
        }

        // O(1) cache check — body_hash is an FNV-1a digest of (body, formatted_body)
        // computed once at MessageObject construction.  Saves all markup processing
        // when a row is recycled for a message it previously rendered.
        let body_hash = msg.body_hash();
        if imp.last_body_hash.get() != body_hash {
            imp.last_body_hash.set(body_hash);

            // Highlight detection — only runs when body changed.
            let highlight_lower: Vec<String> = highlight_names.iter()
                .filter(|n| !n.is_empty())
                .map(|n| n.to_lowercase())
                .collect();
            let body_lower = body.to_lowercase();
            let has_highlight = force_highlight
                || highlight_lower.iter().any(|n| body_lower.contains(n.as_str()));

            if has_highlight { self.add_css_class("mention-row"); }
            else { self.remove_css_class("mention-row"); }

            let rendered_markup = msg.rendered_markup();

            if !rendered_markup.is_empty() {
                // Fast path: use pre-computed Pango markup (HTML→pango or
                // plain-text escape+linkify), computed once at load time.
                imp.body_box.set_visible(false);
                clear_body_box(&imp.body_box);
                if has_highlight && !formatted_body.is_empty() {
                    // HTML message with mention — use pre-rendered markup as-is
                    // (per-word bolding on HTML bodies is skipped to avoid
                    // injecting <b> inside existing Pango/anchor tags).
                    imp.body_label.set_markup(&rendered_markup);
                } else if has_highlight && !highlight_lower.is_empty() {
                    // Plain-text mention — apply per-word bold on the original
                    // escaped text then linkify, same as before.
                    let mut escaped = glib::markup_escape_text(body).to_string();
                    for name_lower in &highlight_lower {
                        let escaped_lower = escaped.to_lowercase();
                        let mut result = String::new();
                        let mut pos = 0;
                        while let Some(idx) = escaped_lower[pos..].find(name_lower.as_str()) {
                            result.push_str(&escaped[pos..pos + idx]);
                            let end = pos + idx + name_lower.len();
                            result.push_str("<b>");
                            result.push_str(&escaped[pos + idx..end]);
                            result.push_str("</b>");
                            pos = end;
                        }
                        result.push_str(&escaped[pos..]);
                        escaped = result;
                    }
                    imp.body_label.set_markup(&linkify_urls(&escaped));
                } else {
                    imp.body_label.set_markup(&rendered_markup);
                }
                imp.body_label.set_visible(true);
            } else {
                // Fallback for objects created outside info_to_obj (e.g. echoes).
                let escaped = glib::markup_escape_text(body).to_string();
                imp.body_box.set_visible(false);
                clear_body_box(&imp.body_box);
                imp.body_label.set_markup(&linkify_urls(&escaped));
                imp.body_label.set_visible(true);
            }
        }

        if !formatted_ts.is_empty() {
            imp.timestamp_label.set_label(formatted_ts);
            imp.timestamp_label.set_visible(true);
        } else {
            imp.timestamp_label.set_visible(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_zero_epoch() {
        let result = format_timestamp(0);
        // 1970-01-01 — should produce a date format, not crash.
        assert!(!result.is_empty());
        assert!(result.contains(':'), "should contain HH:MM, got: {result}");
    }

    #[test]
    fn test_timestamp_recent_shows_time_only() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 30 seconds ago — same day.
        let result = format_timestamp(now - 30);
        // Should be "HH:MM" format (5 chars), no date prefix.
        assert!(
            result.len() <= 5,
            "same-day timestamp should be short, got: {result}"
        );
        assert!(!result.contains("Yesterday"));
    }

    #[test]
    fn test_timestamp_yesterday() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 25 hours ago.
        let result = format_timestamp(now - 25 * 3600);
        // Might say "Yesterday" or a date depending on exact time-of-day,
        // but should not be just "HH:MM".
        assert!(result.len() > 5, "yesterday should have a prefix, got: {result}");
    }

    #[test]
    fn test_timestamp_old_shows_date() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 10 days ago.
        let result = format_timestamp(now - 10 * 86400);
        assert!(result.len() > 5, "old date should include month/day, got: {result}");
        assert!(!result.contains("Yesterday"));
    }
}
