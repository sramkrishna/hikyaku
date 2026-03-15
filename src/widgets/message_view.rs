// MessageView — displays messages for the selected room.
//
// A ListView of MessageObjects with a text input bar at the bottom.
// The ListView is inside a ScrolledWindow that auto-scrolls to the bottom
// when new messages arrive.

mod imp {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use std::cell::RefCell;

    use crate::models::MessageObject;
    use crate::widgets::message_row::MessageRow;

    pub struct MessageView {
        pub list_store: gio::ListStore,
        pub list_view: gtk::ListView,
        pub scrolled_window: gtk::ScrolledWindow,
        pub input_entry: gtk::Entry,
        pub send_button: gtk::Button,
        pub on_send: RefCell<Option<Box<dyn Fn(String)>>>,
    }

    impl Default for MessageView {
        fn default() -> Self {
            let list_store = gio::ListStore::new::<MessageObject>();

            let factory = gtk::SignalListItemFactory::new();

            factory.connect_setup(|_factory, list_item| {
                let list_item = list_item
                    .downcast_ref::<gtk::ListItem>()
                    .expect("ListItem expected");
                list_item.set_child(Some(&MessageRow::new()));
            });

            factory.connect_bind(|_factory, list_item| {
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

                row.set_message(&msg_obj.sender(), &msg_obj.body());
            });

            let no_selection = gtk::NoSelection::new(Some(list_store.clone()));
            let list_view = gtk::ListView::builder()
                .model(&no_selection)
                .factory(&factory)
                .build();

            let scrolled_window = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list_view)
                .build();

            Self {
                list_store,
                list_view,
                scrolled_window,
                input_entry: gtk::Entry::builder()
                    .placeholder_text("Send a message…")
                    .hexpand(true)
                    .build(),
                send_button: gtk::Button::builder()
                    .icon_name("go-up-symbolic")
                    .css_classes(["circular", "suggested-action"])
                    .build(),
                on_send: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageView {
        const NAME: &'static str = "MxMessageView";
        type Type = super::MessageView;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for MessageView {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);
            obj.set_spacing(0);

            // Input bar at the bottom.
            let input_bar = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .margin_start(8)
                .margin_end(8)
                .margin_top(8)
                .margin_bottom(8)
                .build();
            input_bar.append(&self.input_entry);
            input_bar.append(&self.send_button);

            obj.append(&self.scrolled_window);
            obj.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            obj.append(&input_bar);

            // Send on button click.
            let entry = self.input_entry.clone();
            let view = obj.clone();
            self.send_button.connect_clicked(move |_| {
                let text = entry.text().to_string();
                if !text.is_empty() {
                    let imp = view.imp();
                    if let Some(ref cb) = *imp.on_send.borrow() {
                        cb(text);
                    }
                    entry.set_text("");
                }
            });

            // Send on Enter key.
            let entry = self.input_entry.clone();
            let view = obj.clone();
            self.input_entry.connect_activate(move |_| {
                let text = entry.text().to_string();
                if !text.is_empty() {
                    let imp = view.imp();
                    if let Some(ref cb) = *imp.on_send.borrow() {
                        cb(text);
                    }
                    entry.set_text("");
                }
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

    pub fn connect_send_message<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_send.replace(Some(Box::new(f)));
    }

    /// Replace all messages (used when switching rooms).
    pub fn set_messages(&self, messages: &[(String, String, u64)]) {
        let store = &self.imp().list_store;
        store.remove_all();
        for (sender, body, ts) in messages {
            store.append(&MessageObject::new(sender, body, *ts));
        }
        self.scroll_to_bottom();
    }

    /// Append a single new message (used for live updates).
    pub fn append_message(&self, sender: &str, body: &str, timestamp: u64) {
        self.imp()
            .list_store
            .append(&MessageObject::new(sender, body, timestamp));
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
