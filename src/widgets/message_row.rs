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
}

impl Default for RowContext {
    fn default() -> Self {
        Self {
            highlight_names: std::rc::Rc::from([]),
            my_user_id: String::new(),
            is_dm: false,
            no_media: false,
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
        /// Emoji chooser for reactions (created once per row).
        pub react_chooser: gtk::EmojiChooser,
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
        /// Cache key for the last rendered body: "{body}\0{formatted_body}".
        /// When unchanged, body widget recreation is skipped on rebind.
        pub last_body_key: std::cell::RefCell<String>,
        /// Cache key for the last rendered reactions JSON.
        pub last_reactions_key: std::cell::RefCell<String>,
        /// Signal handler IDs for notify connections on the currently bound
        /// MessageObject. Disconnected on unbind to prevent stale handlers
        /// accumulating as rows are recycled by the ListView factory.
        pub flash_handler: std::cell::RefCell<Option<(glib::Object, glib::SignalHandlerId)>>,
        pub new_message_handler: std::cell::RefCell<Option<(glib::Object, glib::SignalHandlerId)>>,
        /// Handler for notify::is-first-unread — shows/hides the divider bar above the row.
        pub unread_divider_handler: std::cell::RefCell<Option<(glib::Object, glib::SignalHandlerId)>>,
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

            self.action_bar.set_orientation(gtk::Orientation::Horizontal);
            self.action_bar.set_spacing(2);
            self.action_bar.append(&self.reply_button);
            self.action_bar.append(&dm_button);
            self.action_bar.append(&self.react_button);
            self.action_bar.append(&bookmark_button);
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

            // React button — use a MenuButton with the EmojiChooser as popover.
            let react_menu_btn = gtk::MenuButton::builder()
                .icon_name("face-smile-symbolic")
                .tooltip_text("React")
                .popover(&self.react_chooser)
                .build();
            react_menu_btn.add_css_class("flat");
            react_menu_btn.add_css_class("circular");
            // Replace the plain button with the menu button in the action bar.
            self.action_bar.remove(&self.react_button);
            self.action_bar.append(&react_menu_btn);

            let event_id = self.event_id.clone();
            let on_react_ref = self.on_react.clone();
            self.react_chooser.connect_emoji_picked(move |_chooser, emoji| {
                let eid = event_id.borrow().clone();
                if let Some(ref cb) = *on_react_ref.borrow() {
                    cb(eid, emoji.to_string());
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
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let event_time = UNIX_EPOCH + Duration::from_secs(ts);
    let now = SystemTime::now();

    let Ok(dt) = glib::DateTime::from_unix_local(ts as i64) else {
        return String::new();
    };

    let Ok(today) = glib::DateTime::now_local() else {
        return dt.format("%H:%M").map(|s: glib::GString| s.to_string()).unwrap_or_default();
    };

    let secs_ago = now.duration_since(event_time).unwrap_or_default().as_secs();
    let same_day = dt.year() == today.year()
        && dt.day_of_year() == today.day_of_year();

    // Select format string via computed time range — no if-else chain.
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
fn extract_image_url(body: &str) -> Option<String> {
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
fn parse_matrix_uri(uri: &str) -> Option<String> {
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

/// Create a text label with the standard message body styling.
fn make_text_label(markup: &str) -> gtk::Label {
    let lbl = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .selectable(true)
        .hexpand(true)
        .css_classes(["mx-message-body"])
        .build();
    lbl.set_markup(markup);

    // Intercept matrix.to / matrix: links — navigate in-app instead of opening a browser.
    lbl.connect_activate_link(|_lbl, uri| {
        if let Some(matrix_id) = parse_matrix_uri(uri) {
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

    lbl
}

/// Create a GtkSourceView widget for a syntax-highlighted code block.
fn make_code_view(code: &str, lang: &str) -> gtk::Widget {
    use gtk::prelude::*;
    use sourceview5::prelude::*;

    let lm = sourceview5::LanguageManager::default();

    // Look up language by its ID (e.g. "rust", "javascript", "python3").
    // Fall back to guessing via a fake filename when the ID doesn't match directly.
    let lang_obj = if lang.is_empty() {
        None
    } else {
        lm.language(lang).or_else(|| {
            // Code fences use names like "js" → try guessing from extension.
            let fake_name = format!("code.{lang}");
            lm.guess_language(Some(fake_name.as_str()), None)
        })
    };

    let buffer = sourceview5::Buffer::builder()
        .highlight_syntax(true)
        .build();

    buffer.set_language(lang_obj.as_ref());

    // Pick a style scheme based on dark/light mode.
    let is_dark = adw::StyleManager::default().is_dark();
    let scheme_name = if is_dark { "oblivion" } else { "classic" };
    let scheme = sourceview5::StyleSchemeManager::default().scheme(scheme_name);
    buffer.set_style_scheme(scheme.as_ref());

    // Set text after language + scheme are configured so the initial
    // tokenisation uses the correct rules.
    buffer.set_text(code);

    let view = sourceview5::View::with_buffer(&buffer);
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_show_line_numbers(false);
    view.set_monospace(true);
    view.add_css_class("code-block");
    view.upcast::<gtk::Widget>()
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
        let sender = msg.sender();
        let body = msg.body();
        let formatted_body = msg.formatted_body();
        let timestamp = msg.timestamp();
        let reply_to = msg.reply_to();
        let thread_root = msg.thread_root();
        let reactions_json = msg.reactions_json();
        let imp = self.imp();
        let highlight_names = &ctx.highlight_names;
        let my_user_id = ctx.my_user_id.as_str();
        let is_dm_room = ctx.is_dm;
        let show_media = !ctx.no_media;

        // System event row — join/leave/invite/kick/ban inline text.
        // Use pre-allocated system_event_label to avoid widget construction on scroll.
        if msg.is_system_event() {
            imp.sender_label.set_visible(false);
            imp.timestamp_label.set_visible(false);
            imp.body_label.set_visible(false);
            imp.body_box.set_visible(false);
            imp.divider_label.set_visible(false);
            imp.system_event_label.set_text(&body);
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

        // Divider row — "New messages" separator, not a real message.
        // Use pre-allocated divider_label to avoid widget construction on scroll.
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

        // Store current message data for action buttons.
        imp.event_id.replace(msg.event_id());
        imp.sender_text.replace(sender.clone());
        imp.sender_id_text.replace(msg.sender_id());
        imp.body_text.replace(body.clone());
        imp.reply_to.replace(reply_to.clone());
        imp.timestamp_val.replace(timestamp);

        // Reply indicator — show who they're replying to.
        if !reply_to.is_empty() {
            let reply_sender_name = msg.reply_to_sender();
            let label = if !reply_sender_name.is_empty() {
                format!("Replying to {reply_sender_name}")
            } else {
                // Fallback: try to extract from body quote format.
                body.lines()
                    .find(|l| l.starts_with("> <@"))
                    .and_then(|l| l.strip_prefix("> <"))
                    .and_then(|l| l.split('>').next())
                    .and_then(|uid| uid.strip_prefix('@'))
                    .and_then(|uid| uid.split(':').next())
                    .map(|local| format!("Replying to {local}"))
                    .unwrap_or_else(|| "Reply".to_string())
            };
            imp.reply_label.set_label(&label);
            imp.reply_box.set_visible(true);
        } else {
            imp.reply_box.set_visible(false);
        }

        // Thread indicator.
        imp.thread_icon.set_visible(!thread_root.is_empty());

        // Show edit/delete only on own messages.
        let msg_sender_id = msg.sender_id();
        let is_own = !my_user_id.is_empty() && msg_sender_id == my_user_id;
        if !msg_sender_id.is_empty() {
            tracing::debug!("Edit check: sender_id='{}' my_id='{}' is_own={}", msg_sender_id, my_user_id, is_own);
        }
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

        // Media attachment — skip entirely when the room has media previews disabled.
        let media_json = msg.media_json();
        if !show_media {
            imp.media_button.set_visible(false);
        } else if !media_json.is_empty() {
            if let Ok(media) = serde_json::from_str::<crate::matrix::MediaInfo>(&media_json) {
                use std::sync::LazyLock;
                static MEDIA_ICONS: LazyLock<std::collections::HashMap<&'static str, &'static str>> =
                    LazyLock::new(|| {
                        [
                            ("Image", "image-x-generic-symbolic"),
                            ("Video", "video-x-generic-symbolic"),
                            ("Audio", "audio-x-generic-symbolic"),
                            ("File", "text-x-generic-symbolic"),
                        ].into_iter().collect()
                    });
                let kind_str = match media.kind {
                    crate::matrix::MediaKind::Image => "Image",
                    crate::matrix::MediaKind::Video => "Video",
                    crate::matrix::MediaKind::Audio => "Audio",
                    crate::matrix::MediaKind::File => "File",
                };
                let icon = MEDIA_ICONS.get(kind_str).unwrap_or(&"text-x-generic-symbolic");
                imp.media_icon.set_icon_name(Some(icon));
                let size_str = media.size
                    .map(|s| {
                        if s > 1_048_576 { format!(" ({:.1} MB)", s as f64 / 1_048_576.0) }
                        else if s > 1024 { format!(" ({:.0} KB)", s as f64 / 1024.0) }
                        else { format!(" ({s} B)") }
                    })
                    .unwrap_or_default();
                imp.media_label.set_label(&format!("{}{size_str}", media.filename));
                imp.media_button.update_property(&[gtk::accessible::Property::Label(
                    &format!("{kind_str}: {}{size_str}", media.filename)
                )]);
                imp.media_button.set_visible(true);

                imp.media_url.replace(media.url.clone());
                imp.media_filename.replace(media.filename.clone());
                imp.media_source_json.replace(media.source_json.clone());
            } else {
                imp.media_button.set_visible(false);
            }
        } else {
            // Check if body contains an image/gif URL — show as media placeholder.
            if let Some(url) = extract_image_url(&body) {
                imp.media_icon.set_icon_name(Some("image-x-generic-symbolic"));
                let display = if url.contains("giphy.com") {
                    "GIF".to_string()
                } else {
                    url.split('/').last().unwrap_or("image").to_string()
                };
                imp.media_label.set_label(&display);
                imp.media_button.set_visible(true);
                imp.media_url.replace(url.clone());
                imp.media_filename.replace(display);
            } else {
                imp.media_button.set_visible(false);
            }
        }

        // Reactions — skip full rebuild if unchanged since last bind.
        if *imp.last_reactions_key.borrow() != reactions_json {
            imp.last_reactions_key.replace(reactions_json.clone());
            while let Some(child) = imp.reactions_box.first_child() {
                imp.reactions_box.remove(&child);
            }
        if let Ok(reactions) = serde_json::from_str::<Vec<(String, u64, Vec<String>)>>(&reactions_json) {
            if !reactions.is_empty() {
                for (emoji, count, names) in &reactions {
                    let label = if *count > 1 {
                        format!("{emoji} {count}")
                    } else {
                        emoji.clone()
                    };
                    // Tooltip shows who reacted.
                    let tooltip = names.join(", ");
                    // Accessible label gives screen readers a meaningful description
                    // instead of just the raw emoji + count string.
                    let a11y_label = if *count > 1 {
                        format!("{count} reactions: {emoji}. From: {tooltip}")
                    } else {
                        format!("Reaction: {emoji}. From: {tooltip}")
                    };
                    let pill = gtk::Label::builder()
                        .label(&label)
                        .tooltip_text(&tooltip)
                        .css_classes(["reaction-pill"])
                        .build();
                    pill.update_property(&[gtk::accessible::Property::Label(&a11y_label)]);
                    imp.reactions_box.append(&pill);
                }
                imp.reactions_box.set_visible(true);
            } else {
                imp.reactions_box.set_visible(false);
            }
        } else {
            imp.reactions_box.set_visible(false);
        }
        } // end reactions cache guard

        // Delegate to text rendering with highlights.
        // Reply fallback is already stripped at the GObject level (info_to_obj).
        let force_highlight = msg.is_highlight();
        self.render_body(&sender, &msg.sender_id(), &body, &formatted_body, timestamp, highlight_names, force_highlight);

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
    }

    fn render_body(
        &self,
        sender: &str,
        sender_id: &str,
        body: &str,
        formatted_body: &str,
        timestamp: u64,
        highlight_names: &[String],
        force_highlight: bool,
    ) {
        let imp = self.imp();

        // Sender label — always colorize by user ID for visual disambiguation.
        // Color is supplementary (never the sole identifier): the text name is
        // always present, so this is safe for color-blind users and screen readers.
        if !sender_id.is_empty() {
            let color = nick_color(sender_id);
            let escaped = glib::markup_escape_text(sender);
            imp.sender_label.set_markup(&format!("<span foreground=\"{color}\">{escaped}</span>"));
        } else {
            imp.sender_label.set_label(sender);
        }

        // Rolodex contact indicator: subtle glow on sender name.
        if !sender_id.is_empty() {
            let in_rolodex = crate::config::settings().rolodex.iter().any(|entry| {
                entry.split_once('|').map(|(_, uid)| uid.trim()) == Some(sender_id)
            });
            if in_rolodex { imp.sender_label.add_css_class("rolodex-contact"); }
            else { imp.sender_label.remove_css_class("rolodex-contact"); }
        }

        // Pre-lowercase names once for both the check and the highlight loop.
        let highlight_lower: Vec<String> = highlight_names.iter()
            .filter(|n| !n.is_empty())
            .map(|n| n.to_lowercase())
            .collect();
        let body_lower = body.to_lowercase();
        let has_highlight = force_highlight
            || highlight_lower.iter().any(|n| body_lower.contains(n.as_str()));

        if has_highlight { self.add_css_class("mention-row"); }
        else { self.remove_css_class("mention-row"); }

        // Skip body update if content is unchanged since last bind.
        // Reactions are handled separately; only body + formatted_body matter here.
        let body_key = format!("{body}\0{formatted_body}");
        if *imp.last_body_key.borrow() != body_key {
            imp.last_body_key.replace(body_key);

            if !formatted_body.is_empty() {
                if formatted_body.contains("<pre") {
                    // Contains code blocks — use body_box for syntax-highlighted views.
                    imp.body_label.set_visible(false);
                    clear_body_box(&imp.body_box);
                    imp.body_box.set_visible(true);
                    for seg in crate::markdown::html_to_segments(formatted_body) {
                        match seg {
                            crate::markdown::Segment::Text(markup) => {
                                imp.body_box.append(&make_text_label(&markup));
                            }
                            crate::markdown::Segment::Code { content, lang } => {
                                imp.body_box.append(&make_code_view(&content, &lang));
                            }
                        }
                    }
                } else {
                    // Simple HTML (bold, italic, links, etc.) — convert to Pango markup
                    // and use the pre-allocated body_label. Avoids all widget construction
                    // per bind, which is the main source of scroll lag.
                    imp.body_box.set_visible(false);
                    clear_body_box(&imp.body_box);
                    imp.body_label.set_markup(&crate::markdown::html_to_pango(formatted_body));
                    imp.body_label.set_visible(true);
                }
            } else {
                // Plain-text path — update the pre-allocated body_label in-place.
                // This avoids constructing a new GtkLabel (with its internal
                // GtkTextView for selectability) on every room switch.
                let mut escaped = glib::markup_escape_text(body).to_string();
                if has_highlight {
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
                }
                imp.body_box.set_visible(false);
                clear_body_box(&imp.body_box);
                imp.body_label.set_markup(&linkify_urls(&escaped));
                imp.body_label.set_visible(true);
            }
        }

        if timestamp > 0 {
            imp.timestamp_label.set_label(&format_timestamp(timestamp));
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
