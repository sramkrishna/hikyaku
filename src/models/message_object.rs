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

        /// Pre-rendered Pango markup for the message body, either synthesised
        /// synchronously in info_to_obj for plain-text messages or delivered
        /// asynchronously by the markup worker for HTML-bearing messages.
        /// Declared as a GObject property so the MessageRow bind path can
        /// listen on notify::rendered-markup and re-run set_markup when the
        /// worker replies after the initial bind.
        #[property(get, set)]
        rendered_markup: RefCell<String>,

        /// FNV-1a hash of (body, formatted_body), used as a cheap O(1) cache key
        /// in MessageRow so bind can skip set_markup() when the row is recycled
        /// for a message it already rendered.
        pub(super) body_hash: Cell<u64>,

        /// Pre-rendered sender label markup: `<span foreground="#rrggbb">Name</span>`.
        /// Computed once in info_to_obj() from sender + sender_id so bind() never
        /// calls nick_color / markup_escape_text / format! per row.
        pub(super) sender_markup: RefCell<String>,

        /// FNV-1a hash of reactions_json.  Updated whenever reactions change so
        /// bind() can skip the O(len) string comparison with an O(1) u64 check.
        pub(super) reactions_hash: Cell<u64>,

        /// Pre-extracted image/gif URL from the message body (empty if none).
        /// Avoids re-running extract_image_url() O(body) scan on every bind.
        pub(super) image_url: RefCell<String>,

        /// Pre-computed reply indicator label: "Replying to Name" or "Reply".
        /// Empty when the message is not a reply.  Avoids format! + body scan on bind.
        pub(super) reply_label: RefCell<String>,

        // Pre-computed media display strings — all empty for non-media messages.
        pub(super) media_icon_name: RefCell<String>,
        pub(super) media_display_label: RefCell<String>,
        pub(super) media_a11y_label: RefCell<String>,
        pub(super) media_url_str: RefCell<String>,
        pub(super) media_filename_str: RefCell<String>,
        pub(super) media_source_json_str: RefCell<String>,

        /// True when the msg has an HTML formatted_body that still needs
        /// the markup worker to produce Pango markup. Starts true for HTML
        /// msgs, set false once enqueued so bind doesn't queue duplicates
        /// when a row is rebound repeatedly to the same msg (scroll recycle).
        /// Also set false in apply_result once the worker delivers.
        pub(super) needs_markup: Cell<bool>,
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
use gtk::prelude::ObjectExt;
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

    pub fn body_hash(&self) -> u64 {
        self.imp().body_hash.get()
    }

    pub fn set_body_hash(&self, h: u64) {
        self.imp().body_hash.set(h);
    }

    pub fn needs_markup(&self) -> bool {
        self.imp().needs_markup.get()
    }

    pub fn set_needs_markup(&self, v: bool) {
        self.imp().needs_markup.set(v);
    }

    pub fn sender_markup(&self) -> String {
        self.imp().sender_markup.borrow().clone()
    }

    pub fn set_sender_markup(&self, s: String) {
        self.imp().sender_markup.replace(s);
    }

    pub fn reactions_hash(&self) -> u64 {
        self.imp().reactions_hash.get()
    }

    pub fn set_reactions_hash(&self, h: u64) {
        self.imp().reactions_hash.set(h);
    }

    /// Update reactions_json and its hash together.  Always use this instead of
    /// set_reactions_json() so the hash stays in sync.
    ///
    /// Discipline: non-notifying fields (Cell/RefCell) set BEFORE any notify-
    /// firing property setter. GObject property notifications fire
    /// synchronously off the setter, so any handler that reads related state
    /// off the notify callback must see the updated cells — otherwise the
    /// handler sees stale cache-equal-to-hash and skips work. Same rule
    /// applies to every multi-field update on MessageObject; see
    /// `update_body_and_markup` for the equivalent pattern.
    ///
    /// A `freeze_notify` guard defers any additional notifies in this
    /// method until the guard drops, so if in future we add more property
    /// setters here they'll fire in a single batch and handlers observe a
    /// single fully-consistent state transition.
    pub fn update_reactions_json(&self, json: String) {
        use gtk::prelude::ObjectExt;
        let _guard = self.freeze_notify();
        let h = fnv1a_str(&json);
        self.imp().reactions_hash.set(h);
        self.set_reactions_json(json);
    }

    /// Update body, formatted_body, rendered_markup, body_hash, and image_url
    /// atomically. All non-notifying Cell/RefCell fields set first; then a
    /// freeze_notify guard defers property notifies until every setter has
    /// run, so handlers listening on any of these properties fire once and
    /// observe fully-consistent state (body_hash matches body, image_url
    /// matches the new body, etc.).
    ///
    /// Any code that previously called msg.set_body / set_formatted_body /
    /// set_rendered_markup individually for a live edit should route through
    /// this method instead. Construction paths (info_to_obj, make_divider_obj)
    /// remain OK to set fields directly since no handlers are attached yet.
    pub fn update_body_and_markup(
        &self,
        body: String,
        formatted_body: String,
        rendered_markup: String,
        body_hash: u64,
        image_url: String,
    ) {
        use gtk::prelude::ObjectExt;
        let _guard = self.freeze_notify();
        // Non-notifying fields first.
        self.imp().body_hash.set(body_hash);
        self.imp().image_url.replace(image_url);
        // Property setters — notifies deferred until _guard drops.
        self.set_body(body);
        self.set_formatted_body(formatted_body);
        self.set_rendered_markup(rendered_markup);
    }

    /// Live-update rendered_markup only. Used by the markup worker's
    /// apply_result and by refresh_alias_references. Wrapped in freeze_notify
    /// for consistency with the other update helpers — currently it only
    /// touches one property but the guard reserves room for future related
    /// state (a rendered_markup_hash cache, etc.) without callers needing
    /// to re-audit ordering.
    pub fn update_rendered_markup(&self, markup: String) {
        use gtk::prelude::ObjectExt;
        let _guard = self.freeze_notify();
        self.set_rendered_markup(markup);
    }

    pub fn image_url(&self) -> String {
        self.imp().image_url.borrow().clone()
    }

    pub fn set_image_url(&self, s: String) {
        self.imp().image_url.replace(s);
    }

    pub fn reply_label(&self) -> String {
        self.imp().reply_label.borrow().clone()
    }

    pub fn set_reply_label(&self, s: String) {
        self.imp().reply_label.replace(s);
    }

    pub fn media_icon_name(&self) -> String { self.imp().media_icon_name.borrow().clone() }
    pub fn set_media_icon_name(&self, s: String) { self.imp().media_icon_name.replace(s); }
    pub fn media_display_label(&self) -> String { self.imp().media_display_label.borrow().clone() }
    pub fn set_media_display_label(&self, s: String) { self.imp().media_display_label.replace(s); }
    pub fn media_a11y_label(&self) -> String { self.imp().media_a11y_label.borrow().clone() }
    pub fn set_media_a11y_label(&self, s: String) { self.imp().media_a11y_label.replace(s); }
    pub fn media_url_str(&self) -> String { self.imp().media_url_str.borrow().clone() }
    pub fn set_media_url_str(&self, s: String) { self.imp().media_url_str.replace(s); }
    pub fn media_filename_str(&self) -> String { self.imp().media_filename_str.borrow().clone() }
    pub fn set_media_filename_str(&self, s: String) { self.imp().media_filename_str.replace(s); }
    pub fn media_source_json_str(&self) -> String { self.imp().media_source_json_str.borrow().clone() }
    pub fn set_media_source_json_str(&self, s: String) { self.imp().media_source_json_str.replace(s); }
}

fn fnv1a_str(s: &str) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    s.bytes().fold(FNV_OFFSET, |h, b| h.wrapping_mul(FNV_PRIME) ^ b as u64)
}
