// RolodexEntryObject — GObject wrapping a personal contact book entry.
//
// Follows the same imp-pattern as RoomObject and MessageObject.
// The backing store is a gio::ListStore<RolodexEntryObject> held in
// the Window imp; persistence to rolodex.json is triggered by the
// ListStore's items-changed signal.

mod imp {
    use glib::Properties;
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use std::cell::{Cell, RefCell};

    #[derive(Properties, Default)]
    #[properties(wrapper_type = super::RolodexEntryObject)]
    pub struct RolodexEntryObject {
        /// Matrix user ID, e.g. @alice:example.com
        #[property(get, set)]
        pub user_id: RefCell<String>,

        /// Human-readable display name as it appeared in chat.
        #[property(get, set)]
        pub display_name: RefCell<String>,

        /// Free-form personal notes (not sent to Matrix).
        #[property(get, set)]
        pub notes: RefCell<String>,

        /// Unix timestamp (seconds) when the entry was added.
        #[property(get, set)]
        pub added_at: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RolodexEntryObject {
        const NAME: &'static str = "MxRolodexEntryObject";
        type Type = super::RolodexEntryObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for RolodexEntryObject {}
}

use glib::Object;
use gtk::glib;

glib::wrapper! {
    pub struct RolodexEntryObject(ObjectSubclass<imp::RolodexEntryObject>);
}

impl RolodexEntryObject {
    pub fn new(user_id: &str, display_name: &str, notes: &str, added_at: u64) -> Self {
        Object::builder()
            .property("user-id", user_id)
            .property("display-name", display_name)
            .property("notes", notes)
            .property("added-at", added_at)
            .build()
    }

    /// Convert to the plain-Rust struct used for JSON serialisation.
    pub fn to_entry(&self) -> crate::plugins::rolodex::RolodexEntry {
        crate::plugins::rolodex::RolodexEntry {
            user_id: self.user_id(),
            display_name: self.display_name(),
            notes: self.notes(),
            added_at: self.added_at(),
        }
    }
}
