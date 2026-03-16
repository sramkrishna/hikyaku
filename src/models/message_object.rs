mod imp {
    use glib::Properties;
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use std::cell::{Cell, RefCell};

    #[derive(Properties, Default)]
    #[properties(wrapper_type = super::MessageObject)]
    pub struct MessageObject {
        #[property(get, set)]
        sender: RefCell<String>,

        #[property(get, set)]
        sender_id: RefCell<String>,

        #[property(get, set)]
        body: RefCell<String>,

        #[property(get, set)]
        timestamp: Cell<u64>,

        #[property(get, set)]
        is_highlight: Cell<bool>,

        /// Matrix event ID for this message.
        #[property(get, set)]
        event_id: RefCell<String>,

        /// Event ID of the message this replies to (empty if not a reply).
        #[property(get, set)]
        reply_to: RefCell<String>,

        /// Thread root event ID (empty if not in a thread).
        #[property(get, set)]
        thread_root: RefCell<String>,

        /// Reactions as JSON string: [["👍", 3], ["❤️", 1]]
        #[property(get, set)]
        reactions_json: RefCell<String>,

        /// Media info as JSON: {"kind":"Image","filename":"photo.jpg","size":12345,"url":"mxc://..."}
        #[property(get, set)]
        media_json: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageObject {
        const NAME: &'static str = "MxMessageObject";
        type Type = super::MessageObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for MessageObject {}
}

use glib::Object;
use gtk::glib;

glib::wrapper! {
    pub struct MessageObject(ObjectSubclass<imp::MessageObject>);
}

impl MessageObject {
    pub fn new(
        sender: &str,
        sender_id: &str,
        body: &str,
        timestamp: u64,
        event_id: &str,
        reply_to: &str,
        thread_root: &str,
        reactions: &[(String, u64)],
        media_json: &str,
    ) -> Self {
        let reactions_json = serde_json::to_string(reactions).unwrap_or_default();
        Object::builder()
            .property("sender", sender)
            .property("sender-id", sender_id)
            .property("body", body)
            .property("timestamp", timestamp)
            .property("event-id", event_id)
            .property("reply-to", reply_to)
            .property("thread-root", thread_root)
            .property("reactions-json", reactions_json)
            .property("media-json", media_json)
            .build()
    }
}
