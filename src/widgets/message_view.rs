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
    use std::cell::RefCell;

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
        pub input_entry: TemplateChild<gtk::Entry>,
        #[template_child]
        pub send_button: TemplateChild<gtk::Button>,
        pub on_send: RefCell<Option<Box<dyn Fn(String)>>>,
    }

    impl Default for MessageView {
        fn default() -> Self {
            Self {
                list_store: gio::ListStore::new::<MessageObject>(),
                view_stack: Default::default(),
                scrolled_window: Default::default(),
                list_view: Default::default(),
                input_entry: Default::default(),
                send_button: Default::default(),
                on_send: RefCell::new(None),
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

                row.set_message(&msg_obj.sender(), &msg_obj.body(), msg_obj.timestamp());
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
        let imp = self.imp();
        imp.list_store.remove_all();
        for (sender, body, ts) in messages {
            imp.list_store.append(&MessageObject::new(sender, body, *ts));
        }
        imp.view_stack.set_visible_child_name("messages");
        self.scroll_to_bottom();
    }

    /// Clear messages and show placeholder (used when switching rooms before new data arrives).
    pub fn clear(&self) {
        self.imp().list_store.remove_all();
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
