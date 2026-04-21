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

        /// Display name of who this message replies to.
        #[property(get, set)]
        reply_to_sender: RefCell<String>,

        /// Thread root event ID (empty if not in a thread).
        #[property(get, set)]
        thread_root: RefCell<String>,

        /// Reactions as JSON string: [["👍", 3], ["❤️", 1]]
        #[property(get, set)]
        reactions_json: RefCell<String>,

        /// Media info as JSON: {"kind":"Image","filename":"photo.jpg","size":12345,"url":"mxc://..."}
        #[property(get, set)]
        media_json: RefCell<String>,

        /// HTML formatted body (Matrix formatted_body), empty if absent.
        #[property(get, set)]
        formatted_body: RefCell<String>,

        /// Transient: set to true while the flash animation is active.
        /// MessageRow connects notify::is-flashing to add/remove the CSS class.
        #[property(get, set)]
        is_flashing: Cell<bool>,

        /// True for messages that arrived since the user last read this room.
        /// Drives the configurable new-message background tint.
        #[property(get, set)]
        is_new_message: Cell<bool>,

        /// True for system events (join/leave/invite/kick/ban) rendered as
        /// compact inline text rather than full message bubbles.
        #[property(get, set)]
        is_system_event: Cell<bool>,

        /// True on the first unread message — causes the row to render a
        /// "New messages" divider bar above its content.  Avoids inserting a
        /// sentinel item into the list store (which would trigger expensive
        /// GTK items_changed position-tracking for all subsequent rows).
        #[property(get, set)]
        is_first_unread: Cell<bool>,

        /// Pre-formatted timestamp string, computed once in info_to_obj().
        /// Avoids calling glib::DateTime::from_unix_local on every row bind.
        pub(super) formatted_timestamp: RefCell<String>,

        /// Pre-rendered Pango markup for the message body, computed once in
        /// info_to_obj() and reused on every scroll bind.  Empty for code-block
        /// messages which still need dynamic body_box construction.
        pub(super) rendered_markup: RefCell<String>,

        /// FNV-1a hash of (body, formatted_body), used as a cheap O(1) cache key
        /// in MessageRow so bind can skip set_markup() when the row is recycled
        /// for a message it already rendered.
        pub(super) body_hash: Cell<u64>,
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
use gtk::subclass::prelude::ObjectSubclassIsExt;

glib::wrapper! {
    pub struct MessageObject(ObjectSubclass<imp::MessageObject>);
}

impl MessageObject {
    pub fn new(
        sender: &str,
        sender_id: &str,
        body: &str,
        formatted_body: &str,
        timestamp: u64,
        event_id: &str,
        reply_to: &str,
        thread_root: &str,
        reactions: &[(String, u64, Vec<String>)],
        media_json: &str,
    ) -> Self {
        let reactions_json = serde_json::to_string(reactions).unwrap_or_default();
        Object::builder()
            .property("sender", sender)
            .property("sender-id", sender_id)
            .property("body", body)
            .property("formatted-body", formatted_body)
            .property("timestamp", timestamp)
            .property("event-id", event_id)
            .property("reply-to", reply_to)
            .property("thread-root", thread_root)
            .property("reactions-json", reactions_json)
            .property("media-json", media_json)
            .build()
    }

    pub fn formatted_timestamp(&self) -> String {
        self.imp().formatted_timestamp.borrow().clone()
    }

    pub fn set_formatted_timestamp(&self, s: String) {
        self.imp().formatted_timestamp.replace(s);
    }

    pub fn rendered_markup(&self) -> String {
        self.imp().rendered_markup.borrow().clone()
    }

    pub fn set_rendered_markup(&self, s: String) {
        self.imp().rendered_markup.replace(s);
    }

    pub fn body_hash(&self) -> u64 {
        self.imp().body_hash.get()
    }

    pub fn set_body_hash(&self, h: u64) {
        self.imp().body_hash.set(h);
    }
}
