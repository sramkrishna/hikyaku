// RoomObject — a GObject representing a Matrix room in the UI.
//
// GObject subclassing in Rust uses the "imp pattern": you write an inner
// module (`imp`) containing the struct and trait implementations, then a
// public wrapper type that delegates to it. This separation exists because
// GObject has its own type system with reference counting, properties, and
// signals — the imp module is where you teach GObject about your type.

mod imp {
    use glib::Properties;
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use std::cell::{Cell, RefCell};

    // The #[derive(Properties)] macro generates GObject property descriptors
    // from annotated fields. Each #[property(...)] attribute tells GObject
    // "this struct field is a property that the UI can bind to."
    #[derive(Properties, Default)]
    #[properties(wrapper_type = super::RoomObject)]
    pub struct RoomObject {
        #[property(get, set)]
        room_id: RefCell<String>,

        #[property(get, set)]
        name: RefCell<String>,

        #[property(get, set)]
        unread_count: Cell<u32>,

        #[property(get, set)]
        last_activity_ts: Cell<u64>,

        #[property(get, set)]
        topic_summary: RefCell<String>,

        /// "dm", "room", or "space"
        #[property(get, set)]
        kind: RefCell<String>,

        #[property(get, set)]
        is_encrypted: Cell<bool>,

        /// Parent space name, empty string if none.
        #[property(get, set)]
        parent_space: RefCell<String>,

        /// Whether this room is pinned by the user.
        #[property(get, set)]
        is_pinned: Cell<bool>,

        /// If true, this item is a section header, not a real room.
        #[property(get, set)]
        is_header: Cell<bool>,
    }

    // These trait impls register our type with GObject's type system.
    // ObjectSubclass: "this is a new GObject type"
    // ObjectImpl: "here's how to construct it and handle property get/set"
    #[glib::object_subclass]
    impl ObjectSubclass for RoomObject {
        const NAME: &'static str = "MxRoomObject";
        type Type = super::RoomObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for RoomObject {}
}

use glib::Object;
use gtk::glib;

// This macro creates the public wrapper type. It's a thin handle (like Rc)
// that points to the GObject instance containing our imp::RoomObject data.
glib::wrapper! {
    pub struct RoomObject(ObjectSubclass<imp::RoomObject>);
}

impl RoomObject {
    pub fn new(room_id: &str, name: &str, kind: &str, is_encrypted: bool, parent_space: &str, is_pinned: bool) -> Self {
        Object::builder()
            .property("room-id", room_id)
            .property("name", name)
            .property("kind", kind)
            .property("is-encrypted", is_encrypted)
            .property("parent-space", parent_space)
            .property("is-pinned", is_pinned)
            .build()
    }

    /// Create a section header pseudo-item.
    pub fn new_header(title: &str) -> Self {
        Object::builder()
            .property("name", title)
            .property("is-header", true)
            .build()
    }
}
