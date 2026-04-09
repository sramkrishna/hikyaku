// NotificationObject — GObject wrapping a single in-app notification entry.
//
// Stored in a gio::ListStore held by MxWindow.  Notifications are @mentions
// and DMs; they accumulate across rooms and persist for the session.

mod imp {
    use glib::Properties;
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use std::cell::{Cell, RefCell};

    #[derive(Properties, Default)]
    #[properties(wrapper_type = super::NotificationObject)]
    pub struct NotificationObject {
        #[property(get, set)]
        pub room_id: RefCell<String>,

        #[property(get, set)]
        pub event_id: RefCell<String>,

        #[property(get, set)]
        pub sender: RefCell<String>,

        #[property(get, set)]
        pub room_name: RefCell<String>,

        #[property(get, set)]
        pub body: RefCell<String>,

        /// Unix timestamp in seconds.
        #[property(get, set)]
        pub timestamp: Cell<u64>,

        /// False until the user clicks the notification row.
        #[property(get, set)]
        pub is_read: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NotificationObject {
        const NAME: &'static str = "MxNotificationObject";
        type Type = super::NotificationObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for NotificationObject {}
}

use glib::Object;
use gtk::glib;

glib::wrapper! {
    pub struct NotificationObject(ObjectSubclass<imp::NotificationObject>);
}

impl NotificationObject {
    pub fn new(
        room_id: &str,
        event_id: &str,
        sender: &str,
        room_name: &str,
        body: &str,
        timestamp: u64,
    ) -> Self {
        Object::builder()
            .property("room-id", room_id)
            .property("event-id", event_id)
            .property("sender", sender)
            .property("room-name", room_name)
            .property("body", body)
            .property("timestamp", timestamp)
            .property("is-read", false)
            .build()
    }
}
