// MessageView — displays messages for the selected room.
//
// A ListView of MessageObjects with a text input bar at the bottom.
// The ListView is inside a ScrolledWindow that auto-scrolls to the bottom
// when new messages arrive.

mod imp {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use gtk::CompositeTemplate;
    use std::cell::{Cell, RefCell};

    use crate::models::MessageObject;
    use crate::widgets::message_row::MessageRow;

    #[derive(CompositeTemplate)]
    #[template(file = "src/widgets/message_view.blp")]
    pub struct MessageView {
        pub list_store: gio::ListStore,
        #[template_child]
        pub view_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub scrolled_window: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub list_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub attach_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub input_entry: TemplateChild<gtk::Entry>,
        #[template_child]
        pub emoji_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub emoji_chooser: TemplateChild<gtk::EmojiChooser>,
        #[template_child]
        pub send_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub info_banner: TemplateChild<gtk::Box>,
        #[template_child]
        pub info_separator: TemplateChild<gtk::Separator>,
        #[template_child]
        pub topic_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub tombstone_banner: TemplateChild<gtk::Box>,
        #[template_child]
        pub tombstone_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub pinned_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub reply_preview: TemplateChild<gtk::Box>,
        #[template_child]
        pub reply_preview_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub reply_cancel_button: TemplateChild<gtk::Button>,
        /// The event ID we're replying to (None = not replying).
        pub reply_to_event: RefCell<Option<String>>,
        /// Callback for sending a message: (body, reply_to_event_id).
        pub on_send: RefCell<Option<Box<dyn Fn(String, Option<String>)>>>,
        /// Callback for sending a reaction: (event_id, emoji).
        pub on_react: RefCell<Option<Box<dyn Fn(String, String)>>>,
        /// Callback for editing a message: (event_id, body).
        pub on_edit: RefCell<Option<Box<dyn Fn(String, String)>>>,
        /// Callback for deleting a message: (event_id).
        pub on_delete: RefCell<Option<Box<dyn Fn(String)>>>,
        /// Callback for media hover: (mxc_url, filename, anchor widget).
        pub on_media_hover: RefCell<Option<Box<dyn Fn(String, String, gtk::Widget)>>>,
        /// Callback for sending a file: (file_path).
        pub on_attach: RefCell<Option<Box<dyn Fn(String)>>>,
        /// Callback for replying — sets up the reply preview.
        pub on_reply: RefCell<Option<Box<dyn Fn(String, String, String)>>>,
        pub on_scroll_top: RefCell<Option<Box<dyn Fn()>>>,
        pub prev_batch_token: RefCell<Option<String>>,
        pub fetching_older: Cell<bool>,
        /// Names to highlight in message bodies (user's own name + friends).
        pub highlight_names: RefCell<Vec<String>>,
        /// Current user's Matrix ID for showing edit/delete on own messages.
        pub user_id: RefCell<String>,
        /// Room members for nick completion: (lowercase_name, display_name, user_id).
        /// Sorted by lowercase_name for binary search prefix matching.
        pub room_members: RefCell<Vec<(String, String, String)>>,
        /// Nick completion popover.
        pub nick_popover: gtk::Popover,
        pub nick_list: gtk::ListBox,
        /// Original prefix and @ position when nick completion started.
        pub nick_completion_state: RefCell<Option<(usize, String, String)>>, // (at_pos, prefix, text_after)
    }

    impl Default for MessageView {
        fn default() -> Self {
            Self {
                list_store: gio::ListStore::new::<MessageObject>(),
                view_stack: Default::default(),
                scrolled_window: Default::default(),
                list_view: Default::default(),
                attach_button: Default::default(),
                input_entry: Default::default(),
                emoji_button: Default::default(),
                emoji_chooser: Default::default(),
                send_button: Default::default(),
                info_banner: Default::default(),
                info_separator: Default::default(),
                topic_label: Default::default(),
                tombstone_banner: Default::default(),
                tombstone_label: Default::default(),
                pinned_box: Default::default(),
                reply_preview: Default::default(),
                reply_preview_label: Default::default(),
                reply_cancel_button: Default::default(),
                reply_to_event: RefCell::new(None),
                on_send: RefCell::new(None),
                on_react: RefCell::new(None),
                on_edit: RefCell::new(None),
                on_delete: RefCell::new(None),
                on_media_hover: RefCell::new(None),
                on_attach: RefCell::new(None),
                on_reply: RefCell::new(None),
                on_scroll_top: RefCell::new(None),
                prev_batch_token: RefCell::new(None),
                fetching_older: Cell::new(false),
                highlight_names: RefCell::new(Vec::new()),
                user_id: RefCell::new(String::new()),
                room_members: RefCell::new(Vec::new()),
                nick_popover: {
                    let popover = gtk::Popover::new();
                    popover.set_autohide(false);
                    popover.set_has_arrow(false);
                    popover
                },
                nick_list: gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::Single)
                    .build(),
                nick_completion_state: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageView {
        const NAME: &'static str = "MxMessageView";
        type Type = super::MessageView;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MessageView {
        fn constructed(&self) {
            self.parent_constructed();

            // Set up the factory and model programmatically since
            // ListView factories with custom widgets don't work in Blueprint.
            let factory = gtk::SignalListItemFactory::new();

            let setup_view_weak = self.obj().downgrade();
            factory.connect_setup(move |_factory, list_item| {
                let list_item = list_item
                    .downcast_ref::<gtk::ListItem>()
                    .expect("ListItem expected");
                let row = MessageRow::new();

                // Set reply/react callbacks once per row (not per bind).
                {
                    let view_weak = setup_view_weak.clone();
                    row.set_on_reply(move |eid, sender, body| {
                        if let Some(v) = view_weak.upgrade() {
                            v.start_reply(&eid, &sender, &body);
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_edit(move |eid, body| {
                        if let Some(v) = view_weak.upgrade() {
                            v.start_edit(&eid, &body);
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_delete(move |eid| {
                        if let Some(v) = view_weak.upgrade() {
                            if let Some(ref cb) = *v.imp().on_delete.borrow() {
                                cb(eid);
                            }
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_media_hover(move |url, filename, widget| {
                        if let Some(v) = view_weak.upgrade() {
                            let has_cb = v.imp().on_media_hover.borrow().is_some();
                            if has_cb {
                                let borrow = v.imp().on_media_hover.borrow();
                                borrow.as_ref().unwrap()(url, filename, widget);
                            }
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_react(move |eid, emoji| {
                        if let Some(v) = view_weak.upgrade() {
                            let has_cb = v.imp().on_react.borrow().is_some();
                            if has_cb {
                                let borrow = v.imp().on_react.borrow();
                                borrow.as_ref().unwrap()(eid, emoji);
                            }
                        }
                    });
                }

                list_item.set_child(Some(&row));
            });

            let obj_weak = self.obj().downgrade();
            factory.connect_bind(move |_factory, list_item| {
                let list_item = list_item
                    .downcast_ref::<gtk::ListItem>()
                    .expect("ListItem expected");
                let msg_obj = list_item
                    .item()
                    .and_downcast::<MessageObject>()
                    .expect("MessageObject expected");
                let row = list_item
                    .child()
                    .and_downcast::<MessageRow>()
                    .expect("MessageRow expected");

                let view = obj_weak.upgrade();
                let names = view.as_ref()
                    .map(|o| o.imp().highlight_names.borrow().clone())
                    .unwrap_or_default();
                let my_id = view.as_ref()
                    .map(|o| o.imp().user_id.borrow().clone())
                    .unwrap_or_default();
                row.bind_message_object(
                    &msg_obj,
                    &names,
                    &my_id,
                );
            });

            let no_selection = gtk::NoSelection::new(Some(self.list_store.clone()));
            self.list_view.set_model(Some(&no_selection));
            self.list_view.set_factory(Some(&factory));

            // Send on button click.
            let obj = self.obj();
            let entry = self.input_entry.clone();
            let view = obj.clone();
            self.send_button.connect_clicked(move |_| {
                let text = entry.text().to_string();
                if !text.is_empty() {
                    let imp = view.imp();
                    let reply_to = imp.reply_to_event.borrow().clone();
                    if let Some(ref cb) = *imp.on_send.borrow() {
                        cb(text, reply_to);
                    }
                    entry.set_text("");
                    imp.reply_to_event.replace(None);
                    imp.reply_preview.set_visible(false);
                }
            });

            // Detect scroll-to-top for pagination.
            let view_for_scroll = obj.clone();
            self.scrolled_window.vadjustment().connect_value_notify(move |adj| {
                // Trigger when scrolled near the top (within 50px).
                if adj.value() < 50.0 {
                    let imp = view_for_scroll.imp();
                    if !imp.fetching_older.get() {
                        if imp.prev_batch_token.borrow().is_some() {
                            imp.fetching_older.set(true);
                            if let Some(ref cb) = *imp.on_scroll_top.borrow() {
                                cb();
                            }
                        }
                    }
                }
            });

            // Send on Enter key — includes reply_to if replying.
            let entry = self.input_entry.clone();
            let view = obj.clone();
            self.input_entry.connect_activate(move |_| {
                let text = entry.text().to_string();
                if !text.is_empty() {
                    let imp = view.imp();
                    let reply_to = imp.reply_to_event.borrow().clone();
                    if let Some(ref cb) = *imp.on_send.borrow() {
                        cb(text, reply_to);
                    }
                    entry.set_text("");
                    // Clear reply state.
                    imp.reply_to_event.replace(None);
                    imp.reply_preview.set_visible(false);
                }
            });

            // Attach button — open file chooser.
            let view_for_attach = obj.clone();
            self.attach_button.connect_clicked(move |btn| {
                let dialog = gtk::FileDialog::builder()
                    .title("Attach a file")
                    .build();

                let btn_widget = btn.clone().upcast::<gtk::Widget>();
                let root = btn_widget.root();
                let window = root.and_then(|r| r.downcast::<gtk::Window>().ok());
                let view = view_for_attach.clone();
                dialog.open(
                    window.as_ref(),
                    gio::Cancellable::NONE,
                    move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let path_str = path.to_string_lossy().to_string();
                                let imp = view.imp();
                                if let Some(ref cb) = *imp.on_attach.borrow() {
                                    cb(path_str);
                                }
                            }
                        }
                    },
                );
            });

            // Cancel reply button.
            let view_for_cancel = obj.clone();
            self.reply_cancel_button.connect_clicked(move |_| {
                let imp = view_for_cancel.imp();
                imp.reply_to_event.replace(None);
                imp.reply_preview.set_visible(false);
            });

            // Set up nick completion popover.
            let nick_scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .max_content_height(200)
                .propagate_natural_height(true)
                .child(&self.nick_list)
                .build();
            self.nick_popover.set_child(Some(&nick_scroll));
            self.nick_popover.set_parent(&*self.input_entry);
            self.nick_popover.set_position(gtk::PositionType::Top);

            // When a nick is selected from the list, insert it.
            let entry_for_nick = self.input_entry.clone();
            let popover_for_nick = self.nick_popover.clone();
            self.nick_list.connect_row_activated(move |_, row| {
                if let Some(label) = row.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                    let nick = label.text().to_string();
                    let text = entry_for_nick.text().to_string();
                    // Find the last '@' and replace from there.
                    if let Some(at_pos) = text.rfind('@') {
                        let before = &text[..at_pos];
                        let new_text = format!("{before}{nick} ");
                        entry_for_nick.set_text(&new_text);
                        entry_for_nick.set_position(new_text.len() as i32);
                    }
                    popover_for_nick.popdown();
                    entry_for_nick.grab_focus();
                }
            });

            // Tab/Arrow nick completion.
            let view_for_tab = obj.clone();
            let key_controller = gtk::EventControllerKey::new();
            key_controller.connect_key_pressed(move |_, key, _, _| {
                use gtk::gdk::Key as K;
                let imp = view_for_tab.imp();

                // Classify key into an action. Using match avoids serial
                // if-else and gives O(1) dispatch via compiler jump table.
                enum NickAction { Escape, Navigate(bool), Tab, Other }
                let action = match key {
                    K::Escape => NickAction::Escape,
                    K::Down => NickAction::Navigate(false),
                    K::Up => NickAction::Navigate(true),
                    K::Tab => NickAction::Tab,
                    _ => NickAction::Other,
                };

                match action {
                    NickAction::Escape if imp.nick_popover.is_visible() => {
                        imp.nick_popover.popdown();
                        imp.nick_completion_state.replace(None);
                        return glib::Propagation::Stop;
                    }
                    NickAction::Other | NickAction::Escape => {
                        // Any non-completion key — close popover if open.
                        if imp.nick_popover.is_visible()
                            && key != K::Shift_L && key != K::Shift_R
                        {
                            imp.nick_popover.popdown();
                            imp.nick_completion_state.replace(None);
                        }
                        return glib::Propagation::Proceed;
                    }
                    _ => {} // Navigate/Tab — handled below.
                }

                // Navigate or Tab with popover visible — cycle through matches.
                let is_up = matches!(action, NickAction::Navigate(true));
                if imp.nick_popover.is_visible()
                    && matches!(action, NickAction::Navigate(_) | NickAction::Tab)
                {
                    let state = imp.nick_completion_state.borrow();
                    let Some((at_pos, _, ref text_after)) = *state else {
                        return glib::Propagation::Proceed;
                    };
                    let text_after = text_after.clone();
                    drop(state);

                    let current = imp.nick_list.selected_row();
                    let current_idx = current.as_ref().map(|r| r.index()).unwrap_or(-1);
                    let next_idx = if is_up {
                        if current_idx <= 0 {
                            let mut i = 0;
                            while imp.nick_list.row_at_index(i + 1).is_some() { i += 1; }
                            i
                        } else {
                            current_idx - 1
                        }
                    } else {
                        current_idx + 1
                    };

                    let row = imp.nick_list.row_at_index(next_idx)
                        .or_else(|| imp.nick_list.row_at_index(0));
                    if let Some(row) = row {
                        imp.nick_list.select_row(Some(&row));
                        if let Some(label) = row.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                            let nick = label.text().to_string();
                            let text = imp.input_entry.text().to_string();
                            let before = &text[..at_pos];
                            let preview = format!("{before}@{nick}{text_after}");
                            imp.input_entry.set_text(&preview);
                            imp.input_entry.set_position((at_pos + 1 + nick.len()) as i32);
                        }
                    }
                    return glib::Propagation::Stop;
                }

                // Not visible — only Tab triggers completion.
                if !matches!(action, NickAction::Tab) {
                    return glib::Propagation::Proceed;
                }

                let text = imp.input_entry.text().to_string();
                let cursor = imp.input_entry.position() as usize;
                let before_cursor = &text[..cursor.min(text.len())];

                let Some(at_pos) = before_cursor.rfind('@') else {
                    return glib::Propagation::Proceed;
                };
                let prefix = &before_cursor[at_pos + 1..];
                if prefix.is_empty() || prefix.contains(' ') {
                    return glib::Propagation::Proceed;
                }

                let text_after = text[cursor.min(text.len())..].to_string();
                let prefix_lower = prefix.to_lowercase();
                let members = imp.room_members.borrow();
                // Binary search to find the start of matching prefix, then
                // collect consecutive matches. O(log n + k) where k = matches.
                let start = members.partition_point(|(lower, _, _)| lower.as_str() < prefix_lower.as_str());
                let matches: Vec<&(String, String, String)> = members[start..]
                    .iter()
                    .take_while(|(lower, _, _)| lower.starts_with(&prefix_lower))
                    .take(10)
                    .collect();

                if matches.is_empty() {
                    return glib::Propagation::Stop;
                }

                // Single match — insert directly, no popover.
                if matches.len() == 1 {
                    let before = &text[..at_pos];
                    let new_text = format!("{before}@{}{text_after}", matches[0].1);
                    imp.input_entry.set_text(&new_text);
                    imp.input_entry.set_position((at_pos + 1 + matches[0].1.len()) as i32);
                    return glib::Propagation::Stop;
                }

                // Multiple matches — store state and show popover.
                imp.nick_completion_state.replace(Some((at_pos, prefix.to_string(), text_after.clone())));

                while let Some(row) = imp.nick_list.first_child() {
                    imp.nick_list.remove(&row);
                }
                for (_, name, _) in &matches {
                    let label = gtk::Label::builder()
                        .label(name.as_str())
                        .halign(gtk::Align::Start)
                        .margin_start(8)
                        .margin_end(8)
                        .margin_top(4)
                        .margin_bottom(4)
                        .build();
                    imp.nick_list.append(&label);
                }
                // Select first and preview.
                if let Some(first) = imp.nick_list.row_at_index(0) {
                    imp.nick_list.select_row(Some(&first));
                    if let Some(label) = first.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                        let nick = label.text().to_string();
                        let before = &text[..at_pos];
                        let preview = format!("{before}@{nick}{text_after}");
                        imp.input_entry.set_text(&preview);
                        imp.input_entry.set_position((at_pos + 1 + nick.len()) as i32);
                    }
                }
                imp.nick_popover.popup();
                glib::Propagation::Stop
            });
            self.input_entry.add_controller(key_controller);

            // Insert emoji at cursor position when picked.
            let entry_for_emoji = self.input_entry.clone();
            self.emoji_chooser.connect_emoji_picked(move |_, emoji| {
                let pos = entry_for_emoji.position();
                entry_for_emoji.insert_text(emoji, &mut pos.clone());
                entry_for_emoji.set_position(pos + emoji.len() as i32);
                entry_for_emoji.grab_focus();
            });
        }
    }

    impl WidgetImpl for MessageView {}
    impl BoxImpl for MessageView {}
}

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::models::MessageObject;

glib::wrapper! {
    pub struct MessageView(ObjectSubclass<imp::MessageView>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::Orientable;
}

impl MessageView {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Set names to highlight in message bodies (user's own name + friends).
    pub fn set_highlight_names(&self, names: &[&str]) {
        self.imp()
            .highlight_names
            .replace(names.iter().map(|s| s.to_string()).collect());
    }

    /// Add a name to highlight.
    pub fn add_highlight_name(&self, name: &str) {
        self.imp().highlight_names.borrow_mut().push(name.to_string());
    }

    pub fn connect_send_message<F: Fn(String, Option<String>) + 'static>(&self, f: F) {
        self.imp().on_send.replace(Some(Box::new(f)));
    }

    pub fn connect_react<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_react.replace(Some(Box::new(f)));
    }

    pub fn connect_edit<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_edit.replace(Some(Box::new(f)));
    }

    pub fn connect_delete<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_delete.replace(Some(Box::new(f)));
    }

    /// Enter edit mode — populate compose box with old text.
    pub fn start_edit(&self, event_id: &str, body: &str) {
        let imp = self.imp();
        // Use reply_to_event to store the event being edited.
        // The send handler checks if this is an edit or new message.
        imp.reply_to_event.replace(Some(format!("edit:{event_id}")));
        imp.reply_preview_label.set_label(&format!("Editing message"));
        imp.reply_preview.set_visible(true);
        imp.input_entry.set_text(body);
        imp.input_entry.grab_focus();
    }

    pub fn set_user_id(&self, user_id: &str) {
        self.imp().user_id.replace(user_id.to_string());
    }

    pub fn connect_attach<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_attach.replace(Some(Box::new(f)));
    }

    pub fn connect_media_hover<F: Fn(String, String, gtk::Widget) + 'static>(&self, f: F) {
        self.imp().on_media_hover.replace(Some(Box::new(f)));
    }

    /// Add a reaction locally to a message (immediate visual feedback).
    pub fn add_local_reaction(&self, event_id: &str, emoji: &str) {
        if event_id.is_empty() {
            return;
        }
        let imp = self.imp();
        let n = gio::prelude::ListModelExt::n_items(&imp.list_store);
        for i in 0..n {
            let Some(obj) = gio::prelude::ListModelExt::item(&imp.list_store, i) else { continue };
            let Some(msg) = obj.downcast_ref::<MessageObject>() else { continue };
            if msg.event_id() == event_id {
                // Toggle: if already present decrement/remove, else add.
                let mut reactions: Vec<(String, u64)> = serde_json::from_str(&msg.reactions_json())
                    .unwrap_or_default();
                if let Some(pos) = reactions.iter().position(|(e, _)| e == emoji) {
                    if reactions[pos].1 <= 1 {
                        reactions.remove(pos);
                    } else {
                        reactions[pos].1 -= 1;
                    }
                } else {
                    reactions.push((emoji.to_string(), 1));
                }
                // Update the GObject properties directly — the factory's
                // bind already has a reference to this object, so changing
                // the property triggers the UI update via GObject notify.
                // No remove/insert needed.
                msg.set_reactions_json(
                    serde_json::to_string(&reactions).unwrap_or_default(),
                );

                // Find the row widget for this position and rebind it
                // directly, bypassing the ListStore entirely.
                let list_view = &imp.list_view;
                // Walk the ListView's children to find the row at position i.
                let mut child = list_view.first_child();
                let mut idx = 0u32;
                while let Some(ref widget) = child {
                    if idx == i {
                        // Found the ListItem widget — find our MessageRow inside.
                        if let Some(row) = Self::find_message_row(widget) {
                            let names = imp.highlight_names.borrow().clone();
                            let my_id = imp.user_id.borrow().clone();
                            row.bind_message_object(&msg, &names, &my_id);
                        }
                        break;
                    }
                    child = widget.next_sibling();
                    idx += 1;
                }
                return;
            }
        }
    }

    /// Enter reply mode — show preview and store the target event ID.
    pub fn start_reply(&self, event_id: &str, sender: &str, body: &str) {
        let imp = self.imp();
        imp.reply_to_event.replace(Some(event_id.to_string()));
        imp.reply_preview_label.set_label(&format!("{sender}: {body}"));
        imp.reply_preview.set_visible(true);
        imp.input_entry.grab_focus();
    }

    /// Replace all messages (used when switching rooms).
    pub fn set_messages(&self, messages: &[crate::matrix::MessageInfo], prev_batch: Option<String>) {
        let imp = self.imp();
        imp.list_store.remove_all();
        for m in messages {
            imp.list_store.append(&Self::info_to_obj(m));
        }
        imp.prev_batch_token.replace(prev_batch);
        imp.fetching_older.set(false);
        imp.view_stack.set_visible_child_name("messages");
        self.scroll_to_bottom();
    }

    /// Prepend older messages at the top (pagination).
    pub fn prepend_messages(&self, messages: &[crate::matrix::MessageInfo], prev_batch: Option<String>) {
        let imp = self.imp();
        for (i, m) in messages.iter().enumerate() {
            imp.list_store.insert(i as u32, &Self::info_to_obj(m));
        }
        imp.prev_batch_token.replace(prev_batch);
        imp.fetching_older.set(false);
    }

    /// Walk a widget tree to find a MessageRow child.
    fn find_message_row(widget: &gtk::Widget) -> Option<crate::widgets::message_row::MessageRow> {
        use crate::widgets::message_row::MessageRow;
        if let Some(row) = widget.downcast_ref::<MessageRow>() {
            return Some(row.clone());
        }
        let mut child = widget.first_child();
        while let Some(ref w) = child {
            if let Some(row) = Self::find_message_row(w) {
                return Some(row);
            }
            child = w.next_sibling();
        }
        None
    }

    fn info_to_obj(m: &crate::matrix::MessageInfo) -> MessageObject {
        let media_json = m.media.as_ref()
            .and_then(|media| serde_json::to_string(media).ok())
            .unwrap_or_default();
        MessageObject::new(
            &m.sender,
            &m.sender_id,
            &m.body,
            m.timestamp,
            &m.event_id,
            m.reply_to.as_deref().unwrap_or(""),
            m.thread_root.as_deref().unwrap_or(""),
            &m.reactions,
            &media_json,
        )
    }

    /// Get the current pagination token.
    pub fn prev_batch_token(&self) -> Option<String> {
        self.imp().prev_batch_token.borrow().clone()
    }

    /// Clear messages and show placeholder (used when switching rooms before new data arrives).
    pub fn clear(&self) {
        let imp = self.imp();
        imp.list_store.remove_all();
        imp.prev_batch_token.replace(None);
        imp.fetching_older.set(false);
    }

    /// Connect a callback for when the user scrolls to the top (load older messages).
    pub fn connect_scroll_top<F: Fn() + 'static>(&self, f: F) {
        self.imp().on_scroll_top.replace(Some(Box::new(f)));
    }

    /// Update the room info banner with metadata (topic, tombstone, pinned).
    pub fn set_room_meta(&self, meta: &crate::matrix::RoomMeta) {
        let imp = self.imp();
        let mut show_banner = false;

        // Topic.
        if !meta.topic.is_empty() {
            imp.topic_label.set_label(&meta.topic);
            imp.topic_label.set_visible(true);
            show_banner = true;
        } else {
            imp.topic_label.set_visible(false);
        }

        // Tombstone — apply background to entire message view.
        if meta.is_tombstoned {
            let msg = match (&meta.replacement_room_name, &meta.replacement_room) {
                (Some(name), _) => format!("This room has been upgraded to: {name}"),
                (None, Some(id)) => format!("This room has been upgraded. New room: {id}"),
                _ => "This room has been upgraded to a new room.".to_string(),
            };
            imp.tombstone_label.set_label(&msg);
            imp.tombstone_banner.set_visible(true);
            self.add_css_class("tombstone-view");
            show_banner = true;
        } else {
            imp.tombstone_banner.set_visible(false);
            self.remove_css_class("tombstone-view");
        }

        // Pinned messages — remove old entries, add fresh ones with sender.
        let pinned = &imp.pinned_box;
        // Remove all children except the header label.
        while let Some(child) = pinned.last_child() {
            if child.downcast_ref::<gtk::Label>().map_or(false, |l| {
                l.css_classes().iter().any(|c| c == "heading")
            }) {
                break;
            }
            pinned.remove(&child);
        }
        if !meta.pinned_messages.is_empty() {
            for (sender, body) in &meta.pinned_messages {
                let row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(2)
                    .css_classes(["pinned-message"])
                    .build();
                let sender_label = gtk::Label::builder()
                    .label(&format!("{sender}:"))
                    .halign(gtk::Align::Start)
                    .css_classes(["caption", "heading"])
                    .build();
                let body_label = gtk::Label::builder()
                    .label(body)
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .css_classes(["caption"])
                    .build();
                row.append(&sender_label);
                row.append(&body_label);
                pinned.append(&row);
            }
            pinned.set_visible(true);
            show_banner = true;
        } else {
            pinned.set_visible(false);
        }

        imp.info_banner.set_visible(show_banner);
        imp.info_separator.set_visible(show_banner);

        // Store members for nick completion, sorted by lowercase name
        // for O(log n) binary search prefix matching.
        let mut members: Vec<(String, String, String)> = meta.members
            .iter()
            .map(|(uid, name)| (name.to_lowercase(), name.clone(), uid.clone()))
            .collect();
        members.sort_by(|a, b| a.0.cmp(&b.0));
        imp.room_members.replace(members);
    }

    /// Append a single new message (used for live updates).
    pub fn append_message(&self, msg: &crate::matrix::MessageInfo) {
        self.imp()
            .list_store
            .append(&Self::info_to_obj(msg));
        self.scroll_to_bottom();
    }

    fn scroll_to_bottom(&self) {
        let adj = self.imp().scrolled_window.vadjustment();
        // Schedule scroll after the layout pass so upper bound is updated.
        glib::idle_add_local_once(move || {
            adj.set_value(adj.upper());
        });
    }
}
