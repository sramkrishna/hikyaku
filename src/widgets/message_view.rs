// MessageView — displays messages for the selected room.
//
// A ListView of MessageObjects with a text input bar at the bottom.
// The ListView is inside a ScrolledWindow that auto-scrolls to the bottom
// when new messages arrive.

// Diagnostic counters reset before each splice, read after, to measure how
// many bind calls happen per splice and their total time.
pub(crate) static BIND_COUNT: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);
pub(crate) static BIND_TOTAL_US: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Return a prefix of `s` containing at most `n` Unicode scalar values.
/// Safe for log statements — never panics on multi-byte characters.
fn body_preview(s: &str) -> &str {
    match s.char_indices().nth(40) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

mod imp {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::pango;
    use gtk::gdk;
    use gtk::subclass::prelude::*;
    use gtk::CompositeTemplate;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use crate::models::MessageObject;
    use crate::widgets::message_row::MessageRow;

    #[derive(CompositeTemplate)]
    #[template(file = "src/widgets/message_view.blp")]
    pub struct MessageView {
        // ── Per-room ListView + GtkStack (O(1) room switch) ──────────────────
        /// Single shared EmojiChooser for reaction picking, lazily created on
        /// the first reaction-button click. Reparented onto whichever row's
        /// react Button requested it. Before this was per-row; heaptrack
        /// showed populate_emoji_chooser accounted for 1.5GB peak across
        /// 60M allocations because every MessageRow built its own tree.
        pub react_chooser: std::cell::OnceCell<gtk::EmojiChooser>,
        /// Event id currently targeted by react_chooser. Stored in a cell
        /// because the chooser has one persistent emoji-picked handler that
        /// reads this at fire time — avoids disconnect/reconnect per show.
        pub react_target_event_id: RefCell<String>,
        /// Stack containing one ScrolledWindow per visited room.
        /// set_visible_child_name(room_id) on switch — no items_changed, no splice.
        pub room_view_stack: std::cell::OnceCell<gtk::Stack>,
        /// room_id → (ScrolledWindow, ListView) — widget tree kept alive.
        pub room_view_cache: RefCell<HashMap<String, (gtk::ScrolledWindow, gtk::ListView)>>,
        /// MRU ordering for room_view_cache. Front = most recently visited.
        /// Cold entries at the back are evicted when the cache exceeds
        /// MAX_CACHED_ROOMS, dropping their ListView + ListStore + per-room
        /// saved state. Without this, every visited room retains ~O(pool_size)
        /// GObject widget tree forever — heap grows linearly with rooms seen.
        pub recent_rooms: RefCell<std::collections::VecDeque<String>>,
        /// Shared factory — one instance, reused for every per-room ListView.
        pub factory: std::cell::OnceCell<gtk::SignalListItemFactory>,
        /// Dedicated store for the seek context window — never the live store.
        pub seek_store: gio::ListStore,
        /// Blueprint placeholder where room_view_stack is inserted.
        #[template_child]
        pub room_list_placeholder: TemplateChild<gtk::Box>,
        /// room_id → gio::ListStore (lightweight GObjects, no widget overhead).
        pub list_store_cache: RefCell<HashMap<String, gio::ListStore>>,
        /// Normalized scroll position (0.0–1.0) saved per room on switch-away.
        /// Restored in an idle after model swap so users return to where they were.
        pub saved_scroll_frac: RefCell<HashMap<String, f64>>,
        /// Currently visible room id (empty string = no room selected).
        pub current_room_id: RefCell<String>,
        /// Current room's list_store — updated by switch_room() for O(1) access.
        pub cur_list_store: RefCell<gio::ListStore>,
        /// Saved event_index per room — restored on return visit so bg_refresh
        /// can detect "nothing changed" without a full splice.
        pub saved_event_indices: RefCell<HashMap<String, HashMap<String, crate::models::MessageObject>>>,
        /// Saved messages_loaded flag per room — restored so return visits
        /// don't trigger first-load auto-scroll.
        pub saved_messages_loaded: RefCell<HashMap<String, bool>>,
        // ── Template children ────────────────────────────────────────────────
        #[template_child]
        pub view_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub room_loading_overlay: TemplateChild<gtk::Box>,
        #[template_child]
        pub attach_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub input_view: TemplateChild<gtk::TextView>,
        #[template_child]
        pub input_placeholder: TemplateChild<gtk::Label>,
        #[template_child]
        pub markdown_button: TemplateChild<gtk::MenuButton>,
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
        pub unread_banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub refresh_banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub topic_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub tombstone_banner: TemplateChild<gtk::Box>,
        #[template_child]
        pub tombstone_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub pinned_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub typing_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub reply_preview: TemplateChild<gtk::Box>,
        #[template_child]
        pub reply_preview_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub reply_cancel_button: TemplateChild<gtk::Button>,
        /// The event ID we're replying to (None = not replying).
        pub reply_to_event: RefCell<Option<String>>,
        /// Quote sender + body for the reply (for fallback format).
        pub reply_quote: RefCell<Option<(String, String)>>,
        /// Callback for sending a message: (body, reply_to_event_id, quote_text, formatted_body, mentioned_user_ids).
        pub on_send: RefCell<Option<Box<dyn Fn(String, Option<String>, Option<(String, String)>, Option<String>, Vec<String>)>>>,
        /// display_name → user_id for mentions inserted via nick completion.
        /// Cleared after each send and on room switch.
        pub pending_mentions: RefCell<std::collections::HashMap<String, String>>,
        /// Callback for sending a reaction: (event_id, emoji).
        pub on_react: RefCell<Option<Box<dyn Fn(String, String)>>>,
        /// Callback for editing a message: (event_id, body).
        pub on_edit: RefCell<Option<Box<dyn Fn(String, String)>>>,
        /// Callback for deleting a message: (event_id).
        pub on_delete: RefCell<Option<Box<dyn Fn(String)>>>,
        /// Callback for media hover: (mxc_url, filename, anchor widget).
        pub on_media_click: RefCell<Option<Box<dyn Fn(String, String, String)>>>,
        /// Callback for sending a file: (file_path).
        pub on_attach: RefCell<Option<Box<dyn Fn(String)>>>,
        /// Callback for DM: (user_id).
        pub on_dm: RefCell<Option<Box<dyn Fn(String)>>>,
        /// Callback for opening a thread: (thread_root_event_id).
        pub on_open_thread: RefCell<Option<Box<dyn Fn(String)>>>,
        /// Callback: bookmark clicked → (event_id, sender, body, timestamp).
        pub on_bookmark: RefCell<Option<Box<dyn Fn(String, String, String, u64)>>>,
        /// Callback: unbookmark clicked → (event_id).
        pub on_unbookmark: RefCell<Option<Box<dyn Fn(String)>>>,
        /// Callback: add contact to rolodex → (user_id, display_name).
        pub on_add_to_rolodex: RefCell<Option<Box<dyn Fn(String, String)>>>,
        /// Callback: remove contact from rolodex → (user_id).
        pub on_remove_from_rolodex: RefCell<Option<Box<dyn Fn(String)>>>,
        /// Callback: fetch notes for a contact → (user_id) → Option<notes>.
        pub on_get_rolodex_notes: RefCell<Option<Box<dyn Fn(String) -> Option<String>>>>,
        /// Callback: save notes for a contact → (user_id, notes).
        pub on_save_rolodex_notes: RefCell<Option<Box<dyn Fn(String, String)>>>,
        /// MessageObjects currently marked as new — cleared by remove_dividers.
        /// Kept separate so removal is O(unread_count) not O(total messages).
        pub new_message_objs: RefCell<Vec<MessageObject>>,
        /// The MessageObject whose is_first_unread = true (the property-based divider).
        /// Only set by insert_divider_by_count / insert_divider_after_event.
        /// None when the live-message sentinel is used instead (insert_divider).
        pub divider_obj: RefCell<Option<MessageObject>>,
        /// Bookmarked event IDs for the current room — drives row highlight + button icon.
        pub bookmarked_ids: RefCell<std::collections::HashSet<String>>,
        /// Callback for typing notice: (typing: bool).
        pub on_typing: RefCell<Option<Box<dyn Fn(bool)>>>,
        /// Pending debounce timer for typing notices.
        pub typing_debounce: RefCell<Option<glib::SourceId>>,
        /// Pending debounce timer for spell-check (runs 400 ms after last keystroke).
        pub spell_debounce: RefCell<Option<glib::SourceId>>,
        /// Last typing state sent — avoids redundant network calls.
        pub last_typing_sent: Cell<bool>,
        /// Callback for replying — sets up the reply preview.
        pub on_reply: RefCell<Option<Box<dyn Fn(String, String, String)>>>,
        pub on_scroll_top: RefCell<Option<Box<dyn Fn()>>>,
        /// Fired when the user scrolls back to the bottom after prepend_messages()
        /// evicted the newest messages.  window.rs uses this to trigger a bg_refresh.
        pub on_scroll_bottom: RefCell<Option<Box<dyn Fn()>>>,
        pub prev_batch_token: RefCell<Option<String>>,
        pub fetching_older: Cell<bool>,
        /// Names to highlight in message bodies (user's own name + friends).
        /// Rc<[String]> so row_context() clones only the pointer, not the data.
        pub highlight_names: RefCell<std::rc::Rc<[String]>>,
        /// Current user's Matrix ID for showing edit/delete on own messages.
        pub user_id: RefCell<String>,
        /// Whether the current room is a DM (hides DM button on messages).
        pub is_dm_room: Cell<bool>,
        /// When true, media buttons and URL image previews are hidden for this room.
        pub is_no_media: Cell<bool>,
        /// True after the first set_messages() call for the current room.
        /// Suppresses auto-scroll on subsequent bg_refresh calls so the user
        /// is not yanked away from their reading position.
        pub messages_loaded: Cell<bool>,
        /// Server unread count at room-load time — show divider + banner when > 0.
        pub room_unread_count: Cell<u32>,
        /// Room members for nick completion: (lowercase_name, display_name, user_id).
        /// Sorted by lowercase_name for binary search prefix matching.
        pub room_members: RefCell<Vec<(String, String, String)>>,
        /// user_id → avatar mxc URL for members of the current room.
        /// Populated in set_room_meta from meta.member_avatars. Used by
        /// the nick-picker popover to enqueue FetchAvatar commands for
        /// members whose avatar image hasn't been downloaded yet.
        pub member_avatar_mxc: RefCell<HashMap<String, String>>,
        /// Nick completion popover.
        pub nick_popover: gtk::Popover,
        pub nick_list: gtk::ListBox,
        /// Original prefix and @ position when nick completion started.
        pub nick_completion_state: RefCell<Option<(usize, String, String)>>, // (at_pos, prefix, text_after)
        /// O(1) event_id → MessageObject index. Kept in sync with list_store.
        /// Eliminates linear scans in update/scroll/remove/has_event.
        pub event_index: RefCell<HashMap<String, MessageObject>>,
        /// Count of local-echo MessageObjects (event_id == "") currently in the
        /// list_store. Bumps when append_message adds an echo; decrements when
        /// patch_echo_event_id successfully replaces the empty id with a real
        /// one. When zero, patch_echo_event_id short-circuits to O(1) instead
        /// of doing a backwards scan — important in busy rooms where dozens of
        /// remote-sender messages arrive and each one would otherwise trigger
        /// a full list_store walk. Approximate: room switches reset it; stale
        /// echoes that never get patched would cause slight drift. Acceptable
        /// because the counter is a hint for the fast path, never for
        /// correctness. (See issue: perf: guard patch_echo_event_id scan.)
        pub pending_echo_count: Cell<u32>,
        /// The event_id stored in m.fully_read for this room — used for precise
        /// divider placement.  None until set_room_meta is called.
        pub fully_read_event_id: RefCell<Option<String>>,
        /// Sent message history for Up/Down recall (capped at 100 entries).
        /// Each entry is (body, event_id) — event_id is patched in by MessageSent.
        pub send_history: RefCell<Vec<(String, String)>>,
        /// Current position in send_history; equal to history.len() when not navigating.
        pub history_cursor: Cell<usize>,
        /// Draft saved when the user first presses Up to navigate history.
        pub history_draft: RefCell<String>,
        /// Pending timer to show the "loading" view after a delay.
        /// Cancelled when RoomMessages arrives so warm-cache rooms never flash.
        pub loading_timer: RefCell<Option<glib::SourceId>>,
        /// When Some, the timeline shows a historical context window (seek mode).
        /// Contains the before_token for loading older messages from the seek position.
        pub seek_before_token: RefCell<Option<String>>,
        /// The event_id we seeked to — used to scroll after the seek result loads.
        pub seek_target_event_id: RefCell<Option<String>>,
        /// Callback fired when the user clicks "Jump to latest" in seek mode.
        pub on_seek_cancelled: RefCell<Option<Box<dyn Fn()>>>,
        /// The live event_index saved while seek mode is active (restored on cancel).
        pub seek_saved_event_index: RefCell<Option<std::collections::HashMap<String, crate::models::MessageObject>>>,
        /// Inline banner shown when the timeline is in seek (historical context) mode.
        pub seek_banner: gtk::Box,
        /// Spinner inside seek_banner — spinning while the seek is in flight.
        pub seek_spinner: gtk::Spinner,
        /// Label inside seek_banner — text changes between "Finding…" and "Historical context".
        pub seek_banner_label: gtk::Label,
        /// True while a scroll_to_bottom idle is already queued.
        /// Deduplicates the flood of idles that arrive when many messages come in
        /// while the user is near the bottom — prevents repeated vadj.set_value()
        /// calls from breaking GTK's kinetic scroll gesture state machine.
        pub scroll_to_bottom_pending: Cell<bool>,
        /// Set to true when prepend_messages() evicts tail (newest) messages to
        /// maintain the store cap.  The vadjustment scroll handler uses this to
        /// trigger a bg_refresh when the user scrolls back to the bottom.
        pub tail_evicted: Cell<bool>,
        /// Cached row-binding context — rebuilt once per room switch by the setters
        /// (set_highlight_names, set_user_id, set_is_dm_room, set_no_media).
        /// The bind callback reads this instead of calling config::settings() per recycle.
        pub cached_row_ctx: RefCell<crate::widgets::MessageRowContext>,
        /// Pending objects from append_message() calls waiting to be flushed to
        /// the list_store in a single splice.  Multiple NewMessage events that arrive
        /// in a burst (e.g. after a sync reconnect) accumulate here so GTK sees one
        /// items_changed signal instead of N separate ones.
        pub pending_appends: RefCell<Vec<crate::models::MessageObject>>,
        /// True while a flush idle is already queued for pending_appends.
        pub append_flush_pending: Cell<bool>,
    }

    impl Default for MessageView {
        fn default() -> Self {
            Self {
                react_chooser: std::cell::OnceCell::new(),
                react_target_event_id: RefCell::new(String::new()),
                room_view_stack: std::cell::OnceCell::new(),
                room_view_cache: RefCell::new(HashMap::new()),
                recent_rooms: RefCell::new(std::collections::VecDeque::new()),
                factory: std::cell::OnceCell::new(),
                seek_store: gio::ListStore::new::<crate::models::MessageObject>(),
                room_list_placeholder: Default::default(),
                list_store_cache: RefCell::new(HashMap::new()),
                saved_scroll_frac: RefCell::new(HashMap::new()),
                current_room_id: RefCell::new(String::new()),
                cur_list_store: RefCell::new(gio::ListStore::new::<MessageObject>()),
                saved_event_indices: RefCell::new(HashMap::new()),
                saved_messages_loaded: RefCell::new(HashMap::new()),
                view_stack: Default::default(),
                room_loading_overlay: Default::default(),
                attach_button: Default::default(),
                input_view: Default::default(),
                input_placeholder: Default::default(),
                markdown_button: Default::default(),
                emoji_button: Default::default(),
                emoji_chooser: Default::default(),
                send_button: Default::default(),
                info_banner: Default::default(),
                info_separator: Default::default(),
                unread_banner: Default::default(),
                refresh_banner: Default::default(),
                topic_label: Default::default(),
                tombstone_banner: Default::default(),
                tombstone_label: Default::default(),
                pinned_box: Default::default(),
                reply_preview: Default::default(),
                reply_preview_label: Default::default(),
                reply_cancel_button: Default::default(),
                reply_to_event: RefCell::new(None),
                reply_quote: RefCell::new(None),
                on_send: RefCell::new(None),
                on_react: RefCell::new(None),
                on_edit: RefCell::new(None),
                on_delete: RefCell::new(None),
                on_media_click: RefCell::new(None),
                on_attach: RefCell::new(None),
                on_dm: RefCell::new(None),
                on_open_thread: RefCell::new(None),
                on_bookmark: RefCell::new(None),
                on_unbookmark: RefCell::new(None),
                on_add_to_rolodex: RefCell::new(None),
                on_remove_from_rolodex: RefCell::new(None),
                on_get_rolodex_notes: RefCell::new(None),
                on_save_rolodex_notes: RefCell::new(None),
                new_message_objs: RefCell::new(Vec::new()),
                divider_obj: RefCell::new(None),
                bookmarked_ids: RefCell::new(std::collections::HashSet::new()),
                on_typing: RefCell::new(None),
                typing_debounce: RefCell::new(None),
                spell_debounce: RefCell::new(None),
                last_typing_sent: Cell::new(false),
                typing_label: Default::default(),
                is_dm_room: Cell::new(false),
                is_no_media: Cell::new(false),
                messages_loaded: Cell::new(false),
                room_unread_count: Cell::new(0),
                on_reply: RefCell::new(None),
                on_scroll_top: RefCell::new(None),
                on_scroll_bottom: RefCell::new(None),
                prev_batch_token: RefCell::new(None),
                fetching_older: Cell::new(false),
                highlight_names: RefCell::new(std::rc::Rc::from([])),
                user_id: RefCell::new(String::new()),
                room_members: RefCell::new(Vec::new()),
                member_avatar_mxc: RefCell::new(HashMap::new()),
                nick_popover: {
                    let popover = gtk::Popover::new();
                    popover.set_autohide(false);
                    popover.set_has_arrow(false);
                    popover
                },
                nick_list: gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::Single)
                    .build(),
                pending_mentions: RefCell::new(std::collections::HashMap::new()),
                nick_completion_state: RefCell::new(None),
                event_index: RefCell::new(HashMap::new()),
                pending_echo_count: Cell::new(0),
                fully_read_event_id: RefCell::new(None),
                send_history: RefCell::new(Vec::new()),
                history_cursor: Cell::new(0),
                history_draft: RefCell::new(String::new()),
                loading_timer: RefCell::new(None),
                seek_before_token: RefCell::new(None),
                seek_target_event_id: RefCell::new(None),
                on_seek_cancelled: RefCell::new(None),
                seek_saved_event_index: RefCell::new(None),
                seek_spinner: gtk::Spinner::builder().visible(false).build(),
                seek_banner_label: gtk::Label::builder()
                    .label("Finding message…")
                    .hexpand(true)
                    .xalign(0.0)
                    .build(),
                seek_banner: gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .visible(false)
                    .css_classes(["osd", "toolbar"])
                    .build(),
                scroll_to_bottom_pending: Cell::new(false),
                tail_evicted: Cell::new(false),
                cached_row_ctx: RefCell::new(crate::widgets::MessageRowContext::default()),
                pending_appends: RefCell::new(Vec::new()),
                append_flush_pending: Cell::new(false),
            }
        }
    }

    impl MessageView {
        /// Current room's list_store — O(1), returns a cloned GObject handle.
        pub fn list_store(&self) -> gio::ListStore {
            self.cur_list_store.borrow().clone()
        }

        /// ListView for the currently visible room.
        pub fn list_view(&self) -> gtk::ListView {
            let rid = self.current_room_id.borrow().clone();
            self.room_view_cache.borrow()
                .get(&rid)
                .map(|(_, lv)| lv.clone())
                .expect("list_view() called before room is set up")
        }

        /// ScrolledWindow for the currently visible room.
        pub fn scrolled_window(&self) -> gtk::ScrolledWindow {
            let rid = self.current_room_id.borrow().clone();
            self.room_view_cache.borrow()
                .get(&rid)
                .map(|(sw, _)| sw.clone())
                .expect("scrolled_window() called before room is set up")
        }

        /// Get or create the per-room gio::ListStore.  Only stores data — no
        /// widgets created here; the widget tree stays constant size regardless
        /// of how many rooms are visited.
        pub fn ensure_room_store(&self, room_id: &str) -> gio::ListStore {
            if let Some(store) = self.list_store_cache.borrow().get(room_id) {
                return store.clone();
            }
            let store = gio::ListStore::new::<crate::models::MessageObject>();
            self.list_store_cache.borrow_mut().insert(room_id.to_string(), store.clone());
            store
        }

        /// Get or create the per-room ScrolledWindow+ListView in room_view_stack.
        ///
        /// On first call for a room: creates the widgets, connects the scroll-to-top
        /// signal (for backpagination), sets the model, and adds to room_view_stack.
        /// On subsequent calls: returns the cached widgets without touching the model.
        ///
        /// The factory is shared across all per-room ListViews — setup/bind closures
        /// capture a reference to the MessageView, not to a specific ListView, so
        /// multiple ListViews using the same factory work correctly.
        pub fn ensure_room_view(&self, room_id: &str) {
            if self.room_view_cache.borrow().contains_key(room_id) {
                return;
            }

            let store = self.ensure_room_store(room_id);
            let no_sel = gtk::NoSelection::new(Some(store));

            let lv = gtk::ListView::builder().build();
            lv.set_factory(self.factory.get());
            lv.set_model(Some(&no_sel));

            let sw = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&lv)
                .css_classes(["mx-tinted-bg"])
                .build();

            // Scroll handler: backpagination (top) and tail-eviction refresh (bottom).
            // Guard on current_room_id so hidden rooms don't accidentally trigger this.
            let view_weak = self.obj().downgrade();
            let guard_rid = room_id.to_string();
            sw.vadjustment().connect_value_notify(move |adj| {
                let Some(view) = view_weak.upgrade() else { return };
                let imp = view.imp();
                if *imp.current_room_id.borrow() != guard_rid { return; }
                // Backpagination: near the top → load older messages.
                if adj.value() < 50.0 {
                    if !imp.fetching_older.get() && imp.prev_batch_token.borrow().is_some() {
                        imp.fetching_older.set(true);
                        if let Some(ref cb) = *imp.on_scroll_top.borrow() {
                            cb();
                        }
                    }
                }
                // Tail-refresh: user scrolled back to bottom after prepend_messages()
                // evicted the newest messages — fire on_scroll_bottom so window.rs
                // can trigger a bg_refresh to reload the live tail.
                let slack = 150.0_f64;
                let near_bottom = adj.upper() - adj.page_size() - adj.value() < slack;
                if near_bottom && imp.tail_evicted.get() {
                    imp.tail_evicted.set(false);
                    if let Some(ref cb) = *imp.on_scroll_bottom.borrow() {
                        cb();
                    }
                }
            });

            self.room_view_stack.get()
                .expect("room_view_stack must be set in constructed()")
                .add_named(&sw, Some(room_id));

            self.room_view_cache.borrow_mut()
                .insert(room_id.to_string(), (sw, lv));
        }

        /// Record a room as most-recently-used and evict cold entries.
        ///
        /// Each retained (ScrolledWindow, ListView, ListStore, saved
        /// scroll/event_index/loaded-flag) for an unvisited room costs
        /// ~O(pool_size) live gtk::Label + widget tree allocations that GTK
        /// never reclaims. Heaptrack showed linear 4GB growth over 4 min as a
        /// result. Cap at MAX_CACHED_ROOMS and drop the LRU.
        pub fn touch_recent_room(&self, room_id: &str) {
            // Cap retained per-room widget trees. Originally tightened to 2
            // when heaptrack showed 4GB peak; after the shared-EmojiChooser
            // fix (populate_emoji_chooser dropped from 1.5GB to ~0), peak is
            // ~70MB and the cap can be relaxed for fast return visits.
            // Each evicted room triggers one first_load splice on return —
            // measured at ~40µs, so eviction is cheap to reverse.
            const MAX_CACHED_ROOMS: usize = 8;
            let mut recent = self.recent_rooms.borrow_mut();
            recent.retain(|r| r != room_id);
            recent.push_front(room_id.to_string());
            while recent.len() > MAX_CACHED_ROOMS {
                let Some(evicted) = recent.pop_back() else { break };
                self.evict_room_widgets(&evicted);
            }
        }

        /// Drop a cold room's widget tree and associated caches.
        /// The next visit to this room will re-create the ListView from
        /// scratch (one-time cost; first_load splice already measures ~40µs).
        fn evict_room_widgets(&self, room_id: &str) {
            if let Some((sw, _lv)) = self.room_view_cache.borrow_mut().remove(room_id) {
                if let Some(stack) = self.room_view_stack.get() {
                    stack.remove(&sw);
                }
            }
            self.list_store_cache.borrow_mut().remove(room_id);
            self.saved_event_indices.borrow_mut().remove(room_id);
            self.saved_messages_loaded.borrow_mut().remove(room_id);
            self.saved_scroll_frac.borrow_mut().remove(room_id);
            tracing::info!("evicted cold room widgets: room={room_id}");
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

            // Tombstone banner link — click the replacement-room anchor to
            // route through the window's matrix-link handler (same path
            // used by message body links, so join / navigate state is
            // handled uniformly).
            self.tombstone_label.connect_activate_link(|_lbl, uri| {
                if let Some(app) = gtk::gio::Application::default() {
                    if let Some(gtk_app) = app.downcast_ref::<gtk::Application>() {
                        if let Some(window) = gtk_app.active_window() {
                            if let Some(win) = window
                                .downcast_ref::<crate::widgets::MxWindow>()
                            {
                                if let Some(matrix_id) =
                                    crate::widgets::parse_matrix_uri(uri)
                                {
                                    win.handle_matrix_link(&matrix_id);
                                    return glib::Propagation::Stop;
                                }
                            }
                        }
                    }
                }
                glib::Propagation::Proceed
            });

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
                    row.set_on_media_click(move |url, filename, source_json| {
                        if let Some(v) = view_weak.upgrade() {
                            let has_cb = v.imp().on_media_click.borrow().is_some();
                            if has_cb {
                                let borrow = v.imp().on_media_click.borrow();
                                borrow.as_ref().unwrap()(url, filename, source_json);
                            }
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_jump_to_reply(move |event_id| {
                        if let Some(v) = view_weak.upgrade() {
                            v.scroll_to_event(&event_id);
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

                    let view_weak = setup_view_weak.clone();
                    row.set_on_show_react_picker(move |eid, btn| {
                        if let Some(v) = view_weak.upgrade() {
                            v.show_react_chooser_at(&btn, eid);
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_dm(move |user_id| {
                        if let Some(v) = view_weak.upgrade() {
                            if let Some(ref cb) = *v.imp().on_dm.borrow() {
                                cb(user_id);
                            }
                        }
                    });

                    // Sender-name click → walk to MxWindow and open the
                    // user-info dialog. Uses root-walk rather than a
                    // callback-through-MessageView so the dialog code
                    // stays co-located with other window dialogs.
                    row.set_on_user_info(move |user_id| {
                        if user_id.is_empty() { return; }
                        use gtk::prelude::*;
                        if let Some(app) = gtk::gio::Application::default() {
                            if let Some(gtk_app) = app.downcast_ref::<gtk::Application>() {
                                if let Some(window) = gtk_app.active_window() {
                                    if let Some(win) = window
                                        .downcast_ref::<crate::widgets::MxWindow>()
                                    {
                                        win.show_user_info_dialog(&user_id);
                                    }
                                }
                            }
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_open_thread(move |thread_root_id| {
                        if let Some(v) = view_weak.upgrade() {
                            if let Some(ref cb) = *v.imp().on_open_thread.borrow() {
                                cb(thread_root_id);
                            }
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_bookmark(move |eid, sender, body, ts| {
                        if let Some(v) = view_weak.upgrade() {
                            if let Some(ref cb) = *v.imp().on_bookmark.borrow() {
                                cb(eid, sender, body, ts);
                            }
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_unbookmark(move |eid| {
                        if let Some(v) = view_weak.upgrade() {
                            if let Some(ref cb) = *v.imp().on_unbookmark.borrow() {
                                cb(eid);
                            }
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_add_to_rolodex(move |uid, name| {
                        if let Some(v) = view_weak.upgrade() {
                            if let Some(ref cb) = *v.imp().on_add_to_rolodex.borrow() {
                                cb(uid, name);
                            }
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_remove_from_rolodex(move |uid| {
                        if let Some(v) = view_weak.upgrade() {
                            if let Some(ref cb) = *v.imp().on_remove_from_rolodex.borrow() {
                                cb(uid);
                            }
                        }
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_get_rolodex_notes(move |uid| {
                        view_weak.upgrade().and_then(|v| {
                            v.imp().on_get_rolodex_notes.borrow().as_ref().and_then(|cb| cb(uid))
                        })
                    });

                    let view_weak = setup_view_weak.clone();
                    row.set_on_save_rolodex_notes(move |uid, notes| {
                        if let Some(v) = view_weak.upgrade() {
                            if let Some(ref cb) = *v.imp().on_save_rolodex_notes.borrow() {
                                cb(uid, notes);
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
                let ctx = view.as_ref()
                    .map(|v| v.imp().cached_row_ctx.borrow().clone())
                    .unwrap_or_default();
                let _tb = std::time::Instant::now();
                row.bind_message_object(&msg_obj, &ctx);
                let is_bm = view.as_ref()
                    .map(|v| v.imp().bookmarked_ids.borrow().contains(&msg_obj.event_id()))
                    .unwrap_or(false);
                row.set_bookmarked(is_bm);
                crate::widgets::message_view::BIND_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                crate::widgets::message_view::BIND_TOTAL_US.fetch_add(
                    _tb.elapsed().as_micros() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
            });

            // Disconnect flash handler when a row is recycled for a different item.
            factory.connect_unbind(|_factory, list_item| {
                let list_item = list_item
                    .downcast_ref::<gtk::ListItem>()
                    .expect("ListItem expected");
                if let Some(row) = list_item.child().and_downcast::<MessageRow>() {
                    row.clear_flash_handler();
                }
            });

            // Store factory; per-room ListViews are created on demand in ensure_room_view().
            self.factory.set(factory).expect("factory already initialised");

            // Stack that holds one ScrolledWindow+ListView per visited room.
            // set_visible_child_name(room_id) on switch is O(1) — no items_changed,
            // no set_model(), no GTK layout work.
            let stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::None)
                .vexpand(true)
                .build();
            self.room_view_stack.set(stack.clone()).ok();
            self.room_list_placeholder.append(&stack);

            // Assemble the seek banner: [spinner] [label] [Jump to latest btn]
            let seek_btn = gtk::Button::builder().label("Jump to latest").build();
            self.seek_banner.append(&self.seek_spinner);
            self.seek_banner.append(&self.seek_banner_label);
            self.seek_banner.append(&seek_btn);

            // Insert above the main ScrolledWindow.
            self.room_list_placeholder.prepend(&self.seek_banner);

            let view_weak2 = self.obj().downgrade();
            seek_btn.connect_clicked(move |_| {
                let Some(view) = view_weak2.upgrade() else { return };
                view.cancel_seek();
            });

            // Helper: get full text from the TextView buffer.
            fn buf_text(buf: &gtk::TextBuffer) -> String {
                buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string()
            }

            // Helper: push text to send history, reset cursor to end.
            fn push_history(imp: &MessageView, text: &str) {
                let mut history = imp.send_history.borrow_mut();
                // Avoid consecutive duplicates.
                if history.last().map(|(b, _)| b.as_str()) != Some(text) {
                    history.push((text.to_string(), String::new()));
                    if history.len() > 100 {
                        history.remove(0);
                    }
                }
                let len = history.len();
                drop(history);
                imp.history_cursor.set(len);
                imp.history_draft.borrow_mut().clear();
            }

            // Helper: build (expanded_text, formatted_body, mentioned_ids) from raw input.
            fn prepare_send(imp: &MessageView, raw: &str) -> (String, Option<String>, Vec<String>) {
                let members = imp.room_members.borrow();
                let mut pending = imp.pending_mentions.borrow().clone();
                // Auto-resolve bare @word patterns not already in pending.
                let (text, auto) = super::auto_resolve_mentions(raw, &members, &pending);
                pending.extend(auto);
                drop(members);
                imp.pending_mentions.borrow_mut().clear();
                let html = crate::markdown::md_to_html(&text);
                let (html_with_pills, mentioned_ids) = super::inject_mention_pills(&html, &pending);
                (text, Some(html_with_pills), mentioned_ids)
            }

            // Send on button click.
            let obj = self.obj();
            let tv = self.input_view.clone();
            let view = obj.clone();
            self.send_button.connect_clicked(move |_| {
                let buf = tv.buffer();
                let raw = buf_text(&buf);
                if !raw.trim().is_empty() {
                    let imp = view.imp();
                    let reply_to = imp.reply_to_event.borrow().clone();
                    let quote = imp.reply_quote.borrow().clone();
                    let (text, formatted, mentioned_ids) = prepare_send(imp, &raw);
                    push_history(imp, &raw);
                    if let Some(ref cb) = *imp.on_send.borrow() {
                        cb(text, reply_to, quote, formatted, mentioned_ids);
                    }
                    buf.set_text("");
                    imp.reply_to_event.replace(None);
                    imp.reply_quote.replace(None);
                    imp.reply_preview.set_visible(false);
                }
            });

            // Scroll-to-top for pagination is connected once in constructed() on
            // the single shared ScrolledWindow.

            // Enter = send, Shift+Enter = newline.
            let send_key_ctrl = gtk::EventControllerKey::new();
            let view_for_enter = obj.clone();
            send_key_ctrl.connect_key_pressed(move |_, key, _, mods| {
                use gtk::gdk::Key as K;
                if key != K::Return && key != K::KP_Enter {
                    return glib::Propagation::Proceed;
                }
                if mods.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                    return glib::Propagation::Proceed;
                }
                let imp = view_for_enter.imp();
                // If the nick-complete popover is open, block the send entirely.
                // key_controller handles Enter to confirm the selected nick.
                if imp.nick_popover.is_visible() {
                    return glib::Propagation::Stop;
                }
                let buf = imp.input_view.buffer();
                let raw = buf_text(&buf);
                if !raw.trim().is_empty() {
                    let reply_to = imp.reply_to_event.borrow().clone();
                    let quote = imp.reply_quote.borrow().clone();
                    let (text, formatted, mentioned_ids) = prepare_send(imp, &raw);
                    push_history(imp, &raw);
                    if let Some(ref cb) = *imp.on_send.borrow() {
                        cb(text, reply_to, quote, formatted, mentioned_ids);
                    }
                    buf.set_text("");
                    imp.reply_to_event.replace(None);
                    imp.reply_quote.replace(None);
                    imp.reply_preview.set_visible(false);
                }
                glib::Propagation::Stop
            });
            self.input_view.add_controller(send_key_ctrl);

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

            // "Jump to new messages" banner button.
            let view_for_banner = obj.clone();
            self.unread_banner.connect_button_clicked(move |banner| {
                view_for_banner.scroll_to_event("__unread_divider__");
                banner.set_revealed(false);
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
            self.nick_popover.set_parent(&*self.input_view);
            self.nick_popover.set_position(gtk::PositionType::Top);

            // When a nick is selected from the list, insert it.
            // Row layout: ListBoxRow[widget_name=uid] → hbox → [
            //   adw::Avatar,
            //   vbox → [
            //     Label[css:body]       ← display name (used for insert),
            //     Label[css:dim-label+caption] ← @mxid (disambiguator),
            //   ],
            // ]
            let view_for_row = obj.downgrade();
            self.nick_list.connect_row_activated(move |_, row| {
                let Some(view) = view_for_row.upgrade() else { return; };
                let imp = view.imp();
                let uid = row.widget_name().to_string();
                let Some(nick) = row
                    .child()
                    .and_then(|c| c.downcast::<gtk::Box>().ok())      // hbox
                    .and_then(|b| b.last_child())                     // vbox (after avatar)
                    .and_then(|c| c.downcast::<gtk::Box>().ok())
                    .and_then(|b| b.first_child())                    // name Label
                    .and_then(|c| c.downcast::<gtk::Label>().ok())
                    .map(|l| l.text().to_string())
                else { return; };
                let buf = imp.input_view.buffer();
                let text = buf_text(&buf);
                if let Some(at_pos) = text.rfind('@') {
                    let before = &text[..at_pos];
                    let new_text = format!("{before}@{nick} ");
                    buf.set_text(&new_text);
                    buf.place_cursor(&buf.end_iter());
                }
                if !uid.is_empty() {
                    imp.pending_mentions.borrow_mut().insert(nick, uid);
                }
                imp.nick_popover.popdown();
                imp.input_view.grab_focus();
            });

            // Tab/Arrow nick completion.
            let view_for_tab = obj.clone();
            let key_controller = gtk::EventControllerKey::new();
            key_controller.connect_key_pressed(move |_, key, _, _| {
                use gtk::gdk::Key as K;
                let imp = view_for_tab.imp();

                // Classify key into an action. Using match avoids serial
                // if-else and gives O(1) dispatch via compiler jump table.
                enum NickAction { Escape, Navigate(bool), Tab, Confirm, Other }
                let popover_open = imp.nick_popover.is_visible();
                let action = match key {
                    K::Escape => NickAction::Escape,
                    K::Down => NickAction::Navigate(false),
                    K::Up => NickAction::Navigate(true),
                    K::Tab => NickAction::Tab,
                    // Enter confirms the selected nick — but only when the popover
                    // is open. Outside the popover, Enter is handled by send_key_ctrl.
                    K::Return | K::KP_Enter if popover_open => NickAction::Confirm,
                    _ => NickAction::Other,
                };

                match action {
                    NickAction::Escape if popover_open => {
                        imp.nick_popover.popdown();
                        imp.nick_completion_state.replace(None);
                        return glib::Propagation::Stop;
                    }
                    NickAction::Confirm => {
                        // Enter with popover open: activate the selected row (or
                        // first row if none highlighted) and close the popover.
                        // send_key_ctrl already returned Stop so no message is sent.
                        let row = imp.nick_list.selected_row()
                            .or_else(|| imp.nick_list.row_at_index(0));
                        if let Some(row) = row {
                            imp.nick_list.activate_action(
                                "list.select-item",
                                Some(&glib::Variant::from((row.index() as u32, false, false))),
                            ).ok();
                            imp.nick_list.emit_by_name::<()>("row-activated", &[&row]);
                        } else {
                            imp.nick_popover.popdown();
                            imp.nick_completion_state.replace(None);
                        }
                        return glib::Propagation::Stop;
                    }
                    NickAction::Other | NickAction::Escape => {
                        // Any non-completion key — close popover if open.
                        if popover_open
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
                        // Walk ListBoxRow → hbox → vbox (last child) → name Label.
                        if let Some(label) = row
                            .child()
                            .and_then(|c| c.downcast::<gtk::Box>().ok())
                            .and_then(|b| b.last_child())
                            .and_then(|c| c.downcast::<gtk::Box>().ok())
                            .and_then(|b| b.first_child())
                            .and_then(|c| c.downcast::<gtk::Label>().ok())
                        {
                            let nick = label.text().to_string();
                            let buf = imp.input_view.buffer();
                            let text = buf_text(&buf);
                            let before = &text[..at_pos];
                            let preview = format!("{before}@{nick}{text_after}");
                            buf.set_text(&preview);
                            let char_offset = (at_pos + 1 + nick.len()) as i32;
                            buf.place_cursor(&buf.iter_at_offset(char_offset));
                        }
                    }
                    return glib::Propagation::Stop;
                }

                // History navigation — Up/Down when popover is closed.
                if let NickAction::Navigate(is_up) = action {
                    let buf = imp.input_view.buffer();
                    let cursor = buf.iter_at_mark(&buf.get_insert());
                    let on_boundary = if is_up {
                        cursor.line() == 0
                    } else {
                        cursor.line() == buf.line_count() - 1
                    };
                    if on_boundary {
                        let history = imp.send_history.borrow();
                        let len = history.len();
                        if len == 0 {
                            return glib::Propagation::Proceed;
                        }
                        let cur = imp.history_cursor.get();
                        if is_up {
                            if cur == len {
                                *imp.history_draft.borrow_mut() = buf_text(&buf);
                            }
                            if cur > 0 {
                                let next = cur - 1;
                                let (text, event_id) = history[next].clone();
                                drop(history);
                                imp.history_cursor.set(next);
                                buf.set_text(&text);
                                buf.place_cursor(&buf.end_iter());
                                // Activate edit mode if event_id is known.
                                if event_id.is_empty() {
                                    imp.reply_to_event.replace(None);
                                } else {
                                    imp.reply_to_event.replace(Some(format!("edit:{event_id}")));
                                }
                            }
                        } else if cur < len {
                            let next = cur + 1;
                            let (text, event_id) = if next == len {
                                (imp.history_draft.borrow().clone(), String::new())
                            } else {
                                history[next].clone()
                            };
                            drop(history);
                            imp.history_cursor.set(next);
                            buf.set_text(&text);
                            buf.place_cursor(&buf.end_iter());
                            if event_id.is_empty() || next == len {
                                imp.reply_to_event.replace(None);
                                imp.reply_preview.set_visible(false);
                            } else {
                                imp.reply_to_event.replace(Some(format!("edit:{event_id}")));
                            }
                        }
                        return glib::Propagation::Stop;
                    }
                    return glib::Propagation::Proceed;
                }

                // Not visible — only Tab triggers completion.
                if !matches!(action, NickAction::Tab) {
                    return glib::Propagation::Proceed;
                }

                let buf = imp.input_view.buffer();
                let text = buf_text(&buf);
                // cursor_position() is a char count; convert to byte offset.
                let cursor_char = buf.cursor_position() as usize;
                let cursor_byte = text.char_indices()
                    .nth(cursor_char)
                    .map(|(i, _)| i)
                    .unwrap_or(text.len());
                let before_cursor = &text[..cursor_byte];

                // Empty entry → let Tab move focus.
                if before_cursor.trim().is_empty() {
                    return glib::Propagation::Proceed;
                }

                // Find insert_pos and prefix.
                // @-mode:  `@prefix`  → insert_pos = byte offset of @
                // IRC-mode: bare word → insert_pos = byte offset of word start
                let (insert_pos, prefix) = if let Some(at) = before_cursor.rfind('@') {
                    (at, &before_cursor[at + 1..])
                } else {
                    let ws = before_cursor
                        .rfind(|c: char| c.is_whitespace())
                        .map(|i| i + before_cursor[i..].chars().next().unwrap().len_utf8())
                        .unwrap_or(0);
                    (ws, &before_cursor[ws..])
                };

                let text_after = text[cursor_byte..].to_string();

                // Build rolodex entries as owned tuples (lowercase, display, user_id)
                // so they can be prepended to the match list.
                let rolodex_raw: Vec<(String, String, String)> =
                    crate::config::parse_rolodex(&crate::config::settings().rolodex)
                        .into_iter()
                        .map(|(name, uid)| (name.to_lowercase(), name, uid))
                        .collect();

                let members = imp.room_members.borrow();
                // Empty prefix (@-alone) → show all members. Otherwise match
                // in two phases: prefix first (O(log n + k) via binary
                // search) then substring over the rest (linear) to fill
                // remaining capacity. This preserves "starts-with wins"
                // ordering so typing @ali still surfaces alice first, while
                // also finding mali and kailani when prefix alone misses.
                let prefix_lower = prefix.to_lowercase();
                let rolodex_matches: Vec<(String, String, String)> = rolodex_raw.into_iter()
                    .filter(|(lower, _, _)| prefix.is_empty() || lower.contains(&prefix_lower))
                    .take(5)
                    .collect();
                let room_matches: Vec<&(String, String, String)> = if prefix.is_empty() {
                    members.iter().take(10).collect()
                } else {
                    const MATCH_CAP: usize = 10;
                    let start = members.partition_point(|(lower, _, _)| lower.as_str() < prefix_lower.as_str());
                    let mut collected: Vec<&(String, String, String)> = members[start..]
                        .iter()
                        .take_while(|(lower, _, _)| lower.starts_with(&prefix_lower))
                        .take(MATCH_CAP)
                        .collect();
                    if collected.len() < MATCH_CAP {
                        let seen: std::collections::HashSet<String> = collected
                            .iter()
                            .map(|(_, _, uid)| uid.clone())
                            .collect();
                        let needed = MATCH_CAP - collected.len();
                        let extras: Vec<&(String, String, String)> = members
                            .iter()
                            .filter(|(lower, _, uid)| {
                                !seen.contains(uid)
                                    && !lower.starts_with(prefix_lower.as_str())
                                    && lower.contains(prefix_lower.as_str())
                            })
                            .take(needed)
                            .collect();
                        collected.extend(extras);
                    }
                    collected
                };
                // Combine: rolodex first, then room members not already in rolodex.
                let rolodex_ids: std::collections::HashSet<String> =
                    rolodex_matches.iter().map(|(_, _, uid)| uid.clone()).collect();
                let mut matches: Vec<(String, String, String)> = rolodex_matches;
                for m in room_matches {
                    if !rolodex_ids.contains(&m.2) {
                        matches.push(m.clone());
                    }
                }

                if matches.is_empty() {
                    return glib::Propagation::Stop;
                }

                // Single match — insert directly, no popover.
                if matches.len() == 1 {
                    let before = &text[..insert_pos];
                    let new_text = format!("{before}@{}{text_after}", matches[0].1);
                    buf.set_text(&new_text);
                    let char_offset = (insert_pos + 1 + matches[0].1.len()) as i32;
                    buf.place_cursor(&buf.iter_at_offset(char_offset));
                    // Record mention so the send path can inject a pill link.
                    imp.pending_mentions.borrow_mut()
                        .insert(matches[0].1.clone(), matches[0].2.clone());
                    return glib::Propagation::Stop;
                }

                // Multiple matches — store state and show popover.
                imp.nick_completion_state.replace(Some((insert_pos, prefix.to_string(), text_after.clone())));

                while let Some(row) = imp.nick_list.first_child() {
                    imp.nick_list.remove(&row);
                }
                // Walk up to the enclosing MxWindow for avatar-cache reads
                // and command_tx (to enqueue FetchAvatar for unknown
                // members). Widget::ancestor returns Option<Widget> which
                // is downcastable; Widget::root returns an opaque Root
                // trait object that does not expose our subclass.
                use gtk::prelude::*;
                let window = view_for_tab
                    .ancestor(crate::widgets::MxWindow::static_type())
                    .and_then(|w| w.downcast::<crate::widgets::MxWindow>().ok());
                let mxc_map = imp.member_avatar_mxc.borrow();
                for (_, name, uid) in &matches {
                    // 24px avatar + vertical [display_name, @mxid] layout.
                    // The avatar uses the cached on-disk path if the window
                    // has one (populated by MatrixEvent::AvatarReady). If
                    // we know the mxc but not the local path, enqueue a
                    // FetchAvatar command — the row will update on next
                    // popover open with the real image.
                    let avatar = adw::Avatar::builder()
                        .size(24)
                        .text(name.as_str())
                        .show_initials(true)
                        .build();
                    if let Some(ref win) = window {
                        let cache = win.imp().avatar_cache.borrow();
                        if let Some(path) = cache.get(uid) {
                            if !path.is_empty() {
                                if let Ok(tex) = gtk::gdk::Texture::from_filename(path) {
                                    avatar.set_custom_image(Some(&tex));
                                }
                            }
                        } else if let Some(mxc) = mxc_map.get(uid) {
                            // Fire FetchAvatar so the next popover open
                            // picks up the downloaded image from the cache.
                            if let Some(tx) = win.imp().command_tx.get().cloned() {
                                let user_id = uid.clone();
                                let mxc = mxc.clone();
                                glib::spawn_future_local(async move {
                                    let _ = tx.send(crate::matrix::MatrixCommand::FetchAvatar {
                                        user_id, mxc_url: mxc,
                                    }).await;
                                });
                            }
                        }
                    }

                    let text_box = gtk::Box::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .spacing(0)
                        .build();
                    let name_label = gtk::Label::builder()
                        .label(name.as_str())
                        .halign(gtk::Align::Start)
                        .css_classes(["body"])
                        .build();
                    let mxid_label = gtk::Label::builder()
                        .label(uid.as_str())
                        .halign(gtk::Align::Start)
                        .css_classes(["dim-label", "caption"])
                        .build();
                    text_box.append(&name_label);
                    text_box.append(&mxid_label);

                    let row_box = gtk::Box::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .spacing(8)
                        .margin_start(8).margin_end(8)
                        .margin_top(4).margin_bottom(4)
                        .build();
                    row_box.append(&avatar);
                    row_box.append(&text_box);

                    let list_row = gtk::ListBoxRow::builder()
                        .activatable(true)
                        .child(&row_box)
                        .build();
                    // uid lives on the ListBoxRow so the activation handler
                    // can retrieve it without walking into the Box.
                    list_row.set_widget_name(uid.as_str());
                    imp.nick_list.append(&list_row);
                }
                drop(mxc_map);
                // Select first and preview.
                if let Some(first) = imp.nick_list.row_at_index(0) {
                    imp.nick_list.select_row(Some(&first));
                    // Walk ListBoxRow → hbox → vbox (last child) → name Label.
                    if let Some(name_label) = first
                        .child()
                        .and_then(|c| c.downcast::<gtk::Box>().ok())
                        .and_then(|b| b.last_child())
                        .and_then(|c| c.downcast::<gtk::Box>().ok())
                        .and_then(|b| b.first_child())
                        .and_then(|c| c.downcast::<gtk::Label>().ok())
                    {
                        let nick = name_label.text().to_string();
                        let before = &text[..insert_pos];
                        let preview = format!("{before}@{nick}{text_after}");
                        buf.set_text(&preview);
                        let char_offset = (insert_pos + 1 + nick.len()) as i32;
                        buf.place_cursor(&buf.iter_at_offset(char_offset));
                    }
                }
                imp.nick_popover.popup();
                glib::Propagation::Stop
            });
            key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
            self.input_view.add_controller(key_controller);

            // Markdown is always active — show a cheat sheet popover for reference.
            {
                let cheat = gtk::Label::builder()
                    .label(
                        "<b>**bold**</b>   <i>*italic*</i>   <tt>`code`</tt>   <s>~~strike~~</s>\n\
                         <a href=\"\">\\[text](url)</a>   # Heading   &gt; Blockquote\n\
                         ```block``` — fenced code block\n\
                         Shift+Enter — new line"
                    )
                    .use_markup(true)
                    .halign(gtk::Align::Start)
                    .margin_top(8)
                    .margin_bottom(8)
                    .margin_start(8)
                    .margin_end(8)
                    .build();
                let popover = gtk::Popover::new();
                popover.set_child(Some(&cheat));
                self.markdown_button.set_popover(Some(&popover));
            }

            // Insert emoji at cursor position when picked.
            let tv_for_emoji = self.input_view.clone();
            self.emoji_chooser.connect_emoji_picked(move |_, emoji| {
                tv_for_emoji.buffer().insert_at_cursor(emoji);
                tv_for_emoji.grab_focus();
            });

            // Spell-check: create the underline tag and wire up live checking
            // and a right-click suggestion popover.
            {
                let buf = self.input_view.buffer();
                // Create the "misspelled" tag once; check_buffer uses it by name.
                let tag = buf.create_tag(
                    Some("misspelled"),
                    &[
                        ("underline", &pango::Underline::Error),
                        ("underline-rgba", &gdk::RGBA::new(1.0, 0.2, 0.2, 1.0)),
                    ],
                );
                drop(tag);

                // Re-check spelling 400 ms after the last keystroke to avoid
                // blocking the GTK main loop on every character typed.
                let view_for_spell = obj.downgrade();
                buf.connect_changed(move |_buf| {
                    let Some(view) = view_for_spell.upgrade() else { return };
                    let imp = view.imp();
                    // Cancel any previously scheduled check.
                    if let Some(id) = imp.spell_debounce.borrow_mut().take() {
                        id.remove();
                    }
                    let view_weak = view.downgrade();
                    *imp.spell_debounce.borrow_mut() = Some(glib::timeout_add_local_once(
                        std::time::Duration::from_millis(400),
                        move || {
                            let Some(v) = view_weak.upgrade() else { return };
                            let imp = v.imp();
                            *imp.spell_debounce.borrow_mut() = None;
                            crate::spell_check::check_buffer(&imp.input_view.buffer());
                        },
                    ));
                });

                // Right-click over a misspelled word → show suggestion popover.
                let input_weak = self.input_view.downgrade();
                let gesture = gtk::GestureClick::new();
                gesture.set_button(3); // right mouse button
                gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
                gesture.connect_pressed(move |gesture, _n_press, x, y| {
                    let Some(tv) = input_weak.upgrade() else { return };
                    let buf = tv.buffer();

                    // Convert widget coords → buffer coords.
                    let (bx, by) = tv.window_to_buffer_coords(
                        gtk::TextWindowType::Widget, x as i32, y as i32,
                    );
                    let Some(iter) = tv.iter_at_location(bx, by) else { return };

                    // Only intercept if the click is over a misspelled word.
                    let tag_table = buf.tag_table();
                    let Some(tag) = tag_table.lookup("misspelled") else { return };
                    if !iter.has_tag(&tag) { return; }

                    // Find the word boundaries via tag toggles.
                    let mut word_start = iter.clone();
                    if !word_start.starts_tag(Some(&tag)) {
                        word_start.backward_to_tag_toggle(Some(&tag));
                    }
                    let mut word_end = iter.clone();
                    word_end.forward_to_tag_toggle(Some(&tag));
                    let word = buf.text(&word_start, &word_end, false).to_string();
                    if word.is_empty() { return; }

                    // Claim the sequence so the default context menu doesn't appear.
                    gesture.set_state(gtk::EventSequenceState::Claimed);

                    // Build suggestion popover.
                    let popover = gtk::Popover::new();
                    popover.set_has_arrow(true);
                    let vbox = gtk::Box::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .spacing(2)
                        .margin_top(4).margin_bottom(4)
                        .margin_start(4).margin_end(4)
                        .build();

                    let sugs = crate::spell_check::suggestions(&word);
                    if sugs.is_empty() {
                        let lbl = gtk::Label::new(Some("No suggestions"));
                        lbl.add_css_class("dim-label");
                        vbox.append(&lbl);
                    } else {
                        // Capture char offsets (plain integers), not TextIter
                        // structs. TextIter copies become stale after any buffer
                        // modification; offsets remain valid and we reconstruct
                        // fresh iters from them at click time.
                        let char_start = word_start.offset();
                        let char_end = word_end.offset();
                        for sug in sugs.iter().take(8) {
                            let btn = gtk::Button::with_label(sug);
                            btn.set_has_frame(false);
                            btn.add_css_class("flat");
                            let buf2 = buf.clone();
                            let sug2 = sug.clone();
                            let pop = popover.clone();
                            btn.connect_clicked(move |_| {
                                // Reconstruct fresh iters — stale iters cause
                                // gtk_text_buffer_insert assertion failures.
                                let mut s = buf2.iter_at_offset(char_start);
                                let mut e = buf2.iter_at_offset(char_end);
                                buf2.delete(&mut s, &mut e);
                                // After delete, `s` is re-initialised to the
                                // deletion point — use it directly for insert.
                                buf2.insert(&mut s, &sug2);
                                pop.popdown();
                            });
                            vbox.append(&btn);
                        }
                    }

                    // Separator + "Add to dictionary".
                    vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
                    let add_btn = gtk::Button::with_label(
                        &format!("Add \"{word}\" to dictionary"),
                    );
                    add_btn.set_has_frame(false);
                    add_btn.add_css_class("flat");
                    let word2 = word.clone();
                    let buf3 = buf.clone();
                    let pop2 = popover.clone();
                    add_btn.connect_clicked(move |_| {
                        crate::spell_check::add_to_dictionary(&word2);
                        pop2.popdown();
                        // Re-check so the underline disappears immediately.
                        crate::spell_check::check_buffer(&buf3);
                    });
                    vbox.append(&add_btn);

                    popover.set_child(Some(&vbox));
                    popover.set_parent(&tv);
                    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                    popover.popup();
                });
                self.input_view.add_controller(gesture);
            }

            // Typing indicator. Also dismiss nick popover on text change
            // (unless the change was from Tab completion itself).
            let view_for_typing = obj.clone();
            // Pre-warm the spell-check dictionary on the first idle cycle after
            // the widget is realized so the 100-200ms first-use cost doesn't
            // happen on the user's first keystroke.
            glib::idle_add_local_once(crate::spell_check::init);

            self.input_view.buffer().connect_changed(move |buf| {
                let imp = view_for_typing.imp();
                if imp.nick_popover.is_visible() && imp.nick_completion_state.borrow().is_none() {
                    imp.nick_popover.popdown();
                }
                // Show/hide placeholder — char_count() is O(1), no String alloc.
                let empty = buf.char_count() == 0;
                imp.input_placeholder.set_visible(empty);
                let is_typing = !empty;
                // Cancel any pending debounce timer.
                if let Some(id) = imp.typing_debounce.borrow_mut().take() {
                    id.remove();
                }
                if !is_typing {
                    // Send "not typing" immediately when entry is cleared.
                    if imp.last_typing_sent.get() {
                        imp.last_typing_sent.set(false);
                        if let Some(ref cb) = *imp.on_typing.borrow() {
                            cb(false);
                        }
                    }
                } else {
                    // Debounce "typing" — only send after 400ms of no input.
                    // Avoids flooding the server with a notice per keypress.
                    let view_weak = view_for_typing.downgrade();
                    *imp.typing_debounce.borrow_mut() = Some(glib::timeout_add_local_once(
                        std::time::Duration::from_millis(400),
                        move || {
                            let Some(view) = view_weak.upgrade() else { return };
                            let imp = view.imp();
                            *imp.typing_debounce.borrow_mut() = None;
                            if !imp.last_typing_sent.get() {
                                imp.last_typing_sent.set(true);
                                if let Some(ref cb) = *imp.on_typing.borrow() {
                                    cb(true);
                                }
                            }
                        },
                    ));
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

// Result of attempting to place the "New messages" divider at a known event.

glib::wrapper! {
    pub struct MessageView(ObjectSubclass<imp::MessageView>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::Orientable;
}

/// Post-process `html` to replace `@DisplayName` with Matrix pill links.
///
/// For each entry in `mentions` (display_name → user_id) whose `@name`
/// pattern appears in `html`, replaces the first occurrence with
/// `<a href="https://matrix.to/#/{uid}">@name</a>` and returns the list of
/// user IDs that were actually found. pulldown-cmark HTML-escapes `&`, `<`,
/// `>` in text, so we search for the escaped form.
/// Scan `text` for bare `@word` patterns not already covered by `pending`.
/// For each, try a case-insensitive prefix lookup in the sorted `members` list.
/// Returns the text with matched tokens expanded to `@DisplayName` plus the
/// newly resolved `display_name → user_id` pairs to merge into pending_mentions.
fn auto_resolve_mentions(
    text: &str,
    members: &[(String, String, String)], // (lowercase, display, uid), sorted
    pending: &std::collections::HashMap<String, String>, // display_name → uid
) -> (String, std::collections::HashMap<String, String>) {
    let mut result = String::with_capacity(text.len() + 32);
    let mut new_mentions = std::collections::HashMap::new();
    // Use index-based iteration so we can roll back over-consumed words.
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] != '@' {
            result.push(chars[i]);
            i += 1;
            continue;
        }
        i += 1; // consume '@'

        // Collect first word (stop at whitespace or '@').
        let word_start = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '@' {
            i += 1;
        }
        let word: String = chars[word_start..i].iter().collect();

        if word.is_empty() {
            result.push('@');
            continue;
        }
        // Already a full MXID (@user:server) — leave alone.
        if word.contains(':') {
            result.push('@');
            result.push_str(&word);
            continue;
        }
        let lower = word.to_lowercase();
        // Already resolved by nick-completion — skip.
        if pending.keys().any(|dn| dn.to_lowercase().starts_with(&lower)) {
            result.push('@');
            result.push_str(&word);
            continue;
        }

        // Binary-search for prefix matches in the sorted member list.
        let start = members.partition_point(|(ln, _, _)| ln.as_str() < lower.as_str());
        let mut candidates: Vec<&(String, String, String)> = members[start..]
            .iter()
            .take_while(|(ln, _, _)| ln.starts_with(&lower))
            .collect();

        if candidates.is_empty() {
            result.push('@');
            result.push_str(&word);
            continue;
        }

        // Greedily extend with subsequent words to handle multi-word display
        // names (e.g. "John Smith").  Only extends while ambiguous — a unique
        // single-word match is accepted immediately.
        let after_first_word = i; // save for rollback on no-match
        let mut extended_lower = lower.clone();
        let mut consumed_end = i;

        while candidates.len() > 1 && consumed_end < chars.len() && chars[consumed_end] == ' ' {
            let next_start = consumed_end + 1;
            let mut next_end = next_start;
            while next_end < chars.len() && !chars[next_end].is_whitespace() && chars[next_end] != '@' {
                next_end += 1;
            }
            if next_start == next_end { break; } // trailing space, stop
            let next_word: String = chars[next_start..next_end].iter().collect();
            let trial = format!("{} {}", extended_lower, next_word.to_lowercase());
            let ts = members.partition_point(|(ln, _, _)| ln.as_str() < trial.as_str());
            let trial_cands: Vec<&(String, String, String)> = members[ts..]
                .iter()
                .take_while(|(ln, _, _)| ln.starts_with(&trial))
                .collect();
            if trial_cands.is_empty() { break; } // no improvement — stop
            extended_lower = trial;
            consumed_end = next_end;
            candidates = trial_cands;
        }

        let resolved = if candidates.len() == 1 {
            Some((&candidates[0].1, &candidates[0].2))
        } else {
            // Ambiguous prefix — prefer exact match.
            candidates.iter().find(|(ln, _, _)| *ln == extended_lower).map(|(_, dn, uid)| (dn, uid))
        };

        match resolved {
            Some((display, uid)) => {
                // If the resolved display name has words beyond what we consumed
                // during the extension loop (unique match on first word only, but
                // display = "John Smith"), try to consume those words from the
                // input to avoid duplicating them in the output.
                let display_lower = display.to_lowercase();
                if display_lower.len() > extended_lower.len() {
                    let suffix: Vec<char> = display_lower[extended_lower.len()..].chars().collect();
                    let mut j = consumed_end;
                    let mut k = 0;
                    while k < suffix.len() && j < chars.len() {
                        if chars[j].to_lowercase().next() == Some(suffix[k]) {
                            j += 1; k += 1;
                        } else {
                            break;
                        }
                    }
                    if k == suffix.len() {
                        consumed_end = j; // full suffix matched — consume it
                    }
                }
                new_mentions.insert(display.to_string(), uid.to_string());
                result.push('@');
                result.push_str(display);
                i = consumed_end;
            }
            None => {
                result.push('@');
                result.push_str(&word);
                i = after_first_word; // roll back any words over-consumed during extension
            }
        }
    }
    (result, new_mentions)
}

fn inject_mention_pills(
    html: &str,
    mentions: &std::collections::HashMap<String, String>,
) -> (String, Vec<String>) {
    let mut result = html.to_string();
    let mut used_ids = Vec::new();
    for (name, uid) in mentions {
        let escaped = name
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let at_name = format!("@{escaped}");
        if result.contains(&at_name) {
            let pill = format!(r#"<a href="https://matrix.to/#/{uid}">@{escaped}</a>"#);
            result = result.replacen(&at_name, &pill, 1);
            used_ids.push(uid.clone());
        }
    }
    (result, used_ids)
}

impl MessageView {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Set names to highlight in message bodies (user's own name + friends).
    pub fn set_highlight_names(&self, names: &[&str]) {
        let rc: std::rc::Rc<[String]> = names.iter().map(|s| s.to_string()).collect();
        self.imp().highlight_names.replace(rc);
        self.rebuild_row_context_cache();
    }

    /// Add a name to highlight.  Replaces the Rc with a new one containing the added name.
    pub fn add_highlight_name(&self, name: &str) {
        let imp = self.imp();
        let mut v: Vec<String> = imp.highlight_names.borrow().iter().cloned().collect();
        v.push(name.to_string());
        imp.highlight_names.replace(v.into());
        self.rebuild_row_context_cache();
    }

    pub fn connect_send_message<F: Fn(String, Option<String>, Option<(String, String)>, Option<String>, Vec<String>) + 'static>(&self, f: F) {
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
        imp.input_view.buffer().set_text(body);
        imp.input_view.grab_focus();
    }

    pub fn set_user_id(&self, user_id: &str) {
        self.imp().user_id.replace(user_id.to_string());
        self.rebuild_row_context_cache();
    }

    pub fn set_is_dm_room(&self, is_dm: bool) {
        self.imp().is_dm_room.set(is_dm);
        self.rebuild_row_context_cache();
    }

    pub fn set_no_media(&self, no_media: bool) {
        self.imp().is_no_media.set(no_media);
        self.rebuild_row_context_cache();
    }

    /// Rebuild cached_row_ctx from current imp state.  Called by each setter so
    /// the bind callback never has to touch GSettings.
    fn rebuild_row_context_cache(&self) {
        let ctx = self.row_context();
        self.imp().cached_row_ctx.replace(ctx);
    }

    /// Build the per-timeline context for row binding.
    /// Cloning is O(1): highlight_names is an Rc (pointer copy).
    fn row_context(&self) -> crate::widgets::MessageRowContext {
        let imp = self.imp();
        let rolodex_ids: std::collections::HashSet<String> = crate::config::settings()
            .rolodex
            .iter()
            .filter_map(|entry| entry.split_once('|').map(|(_, uid)| uid.trim().to_string()))
            .collect();
        crate::widgets::MessageRowContext {
            highlight_names: imp.highlight_names.borrow().clone(), // Rc clone = pointer copy
            my_user_id: imp.user_id.borrow().clone(),
            is_dm: imp.is_dm_room.get(),
            no_media: imp.is_no_media.get(),
            rolodex_ids: std::rc::Rc::new(rolodex_ids),
        }
    }

    pub fn connect_attach<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_attach.replace(Some(Box::new(f)));
    }

    pub fn connect_dm<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_dm.replace(Some(Box::new(f)));
    }

    pub fn connect_open_thread<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_open_thread.replace(Some(Box::new(f)));
    }

    pub fn connect_bookmark<F: Fn(String, String, String, u64) + 'static>(&self, f: F) {
        self.imp().on_bookmark.replace(Some(Box::new(f)));
    }

    pub fn connect_unbookmark<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_unbookmark.replace(Some(Box::new(f)));
    }

    pub fn connect_add_to_rolodex<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_add_to_rolodex.replace(Some(Box::new(f)));
    }

    pub fn connect_remove_from_rolodex<F: Fn(String) + 'static>(&self, f: F) {
        self.imp().on_remove_from_rolodex.replace(Some(Box::new(f)));
    }

    pub fn connect_get_rolodex_notes<F: Fn(String) -> Option<String> + 'static>(&self, f: F) {
        self.imp().on_get_rolodex_notes.replace(Some(Box::new(f)));
    }

    pub fn connect_save_rolodex_notes<F: Fn(String, String) + 'static>(&self, f: F) {
        self.imp().on_save_rolodex_notes.replace(Some(Box::new(f)));
    }

    /// Load bookmarked event IDs for `room_id` from the store into the in-memory set.
    /// Call after `set_messages` so rows are highlighted on the first bind pass.
    /// Refresh the visible nick-picker popover's avatar widget for a
    /// specific user id. Called by window.rs when a MatrixEvent::AvatarReady
    /// arrives for a member whose row is currently in the popover — without
    /// this, the user only sees the real avatar after closing and reopening
    /// the popover (download is async; initial render uses the initials
    /// fallback until the cache is populated).
    pub fn refresh_nick_avatar(&self, user_id: &str, path: &str) {
        let imp = self.imp();
        if !imp.nick_popover.is_visible() { return; }
        if user_id.is_empty() || path.is_empty() { return; }
        let Ok(texture) = gtk::gdk::Texture::from_filename(path) else { return };
        let mut child = imp.nick_list.first_child();
        while let Some(node) = child {
            let next = node.next_sibling();
            if let Some(row) = node.downcast_ref::<gtk::ListBoxRow>() {
                if row.widget_name().as_str() == user_id {
                    if let Some(avatar) = row.child()
                        .and_then(|c| c.downcast::<gtk::Box>().ok())
                        .and_then(|b| b.first_child())
                        .and_then(|c| c.downcast::<adw::Avatar>().ok())
                    {
                        avatar.set_custom_image(Some(&texture));
                    }
                    break;
                }
            }
            child = next;
        }
    }

    pub fn load_bookmarks(&self, room_id: &str) {
        let _g = crate::perf::scope("load_bookmarks");
        let entries = crate::bookmarks::BOOKMARK_STORE.load();
        let ids: std::collections::HashSet<String> = entries.into_iter()
            .filter(|e| e.room_id == room_id)
            .map(|e| e.event_id)
            .collect();
        *self.imp().bookmarked_ids.borrow_mut() = ids;
    }

    /// Update a single event's bookmarked state in the set and on its visible row.
    pub fn set_message_bookmarked(&self, event_id: &str, bookmarked: bool) {
        let imp = self.imp();
        if bookmarked {
            imp.bookmarked_ids.borrow_mut().insert(event_id.to_string());
        } else {
            imp.bookmarked_ids.borrow_mut().remove(event_id);
        }
        // Update the visible row if it's currently rendered.
        let eid = event_id.to_string();
        let mut child = imp.list_view().first_child();
        while let Some(ref widget) = child {
            if let Some(row) = Self::find_message_row(widget) {
                if *row.imp().event_id.borrow() == eid {
                    row.set_bookmarked(bookmarked);
                    break;
                }
            }
            child = widget.next_sibling();
        }
    }

    pub fn connect_typing<F: Fn(bool) + 'static>(&self, f: F) {
        self.imp().on_typing.replace(Some(Box::new(f)));
    }

    /// Update the typing indicator label.
    pub fn set_typing_users(&self, names: &[String]) {
        let imp = self.imp();
        if names.is_empty() {
            if imp.typing_label.is_visible() {
                imp.typing_label.set_visible(false);
            }
        } else {
            let text = match names.len() {
                1 => format!("{} is typing…", names[0]),
                2 => format!("{} and {} are typing…", names[0], names[1]),
                n => format!("{}, {} and {} others are typing…", names[0], names[1], n - 2),
            };
            if imp.typing_label.label() != text {
                imp.typing_label.set_label(&text);
            }
            if !imp.typing_label.is_visible() {
                imp.typing_label.set_visible(true);
            }
        }
    }

    pub fn connect_media_click<F: Fn(String, String, String) + 'static>(&self, f: F) {
        self.imp().on_media_click.replace(Some(Box::new(f)));
    }

    /// Update a message in the timeline by event_id. The `mutate` closure
    /// modifies the MessageObject's properties, then the row is rebound
    /// in-place without scrolling or flashing. This is the single entry
    /// point for all local timeline updates (reactions, edits, deletes, etc.).
    fn update_message_in_place(
        &self,
        event_id: &str,
        mutate: impl FnOnce(&MessageObject),
    ) {
        if event_id.is_empty() { return; }
        let imp = self.imp();
        // O(1) lookup via event_index — no list_store scan.
        let msg = match imp.event_index.borrow().get(event_id).cloned() {
            Some(m) => m,
            None => {
                tracing::debug!("update_message_in_place: {event_id} not in event_index (room={})",
                    imp.current_room_id.borrow());
                return;
            }
        };
        mutate(&msg);
        // Walk only the currently-visible rows (typically ~10-20 widgets, not
        // the full list_store). Identify the right row by its stored event_id
        // rather than by absolute position, which is correct across virtual scroll.
        let eid = event_id.to_string();
        let mut child = imp.list_view().first_child();
        while let Some(ref widget) = child {
            if let Some(row) = Self::find_message_row(widget) {
                if *row.imp().event_id.borrow() == eid {
                    row.bind_message_object(&msg, &self.row_context());
                    break;
                }
            }
            child = widget.next_sibling();
        }
    }

    /// Toggle an emoji reaction on a message. If "You" already reacted,
    /// remove your reaction. Otherwise add it.
    pub fn toggle_reaction(&self, event_id: &str, emoji: &str) {
        let emoji = emoji.to_string();
        self.update_message_in_place(event_id, |msg| {
            let mut reactions: Vec<(String, u64, Vec<String>)> =
                serde_json::from_str(&msg.reactions_json()).unwrap_or_default();
            // O(1) emoji lookup via position index.
            let pos_by_emoji: std::collections::HashMap<&str, usize> = reactions
                .iter().enumerate().map(|(i, (e, _, _))| (e.as_str(), i)).collect();
            match pos_by_emoji.get(emoji.as_str()) {
                Some(&i) => {
                    // O(1) "You" check via HashSet.
                    let senders: std::collections::HashSet<&str> =
                        reactions[i].2.iter().map(|s| s.as_str()).collect();
                    if senders.contains("You") {
                        reactions[i].2.retain(|n| n != "You");
                        reactions[i].1 = reactions[i].1.saturating_sub(1);
                        if reactions[i].1 == 0 { reactions.remove(i); }
                    } else {
                        reactions[i].1 += 1;
                        reactions[i].2.push("You".to_string());
                    }
                }
                None => reactions.push((emoji.clone(), 1, vec!["You".to_string()])),
            }
            msg.update_reactions_json(serde_json::to_string(&reactions).unwrap_or_default());
        });
    }

    /// Add a reaction from a specific sender (used for live sync updates).
    pub fn add_reaction(&self, event_id: &str, emoji: &str, sender: &str) {
        let emoji = emoji.to_string();
        let sender = sender.to_string();
        self.update_message_in_place(event_id, |msg| {
            let mut reactions: Vec<(String, u64, Vec<String>)> =
                serde_json::from_str(&msg.reactions_json()).unwrap_or_default();
            // O(1) emoji lookup via position index.
            let pos_by_emoji: std::collections::HashMap<&str, usize> = reactions
                .iter().enumerate().map(|(i, (e, _, _))| (e.as_str(), i)).collect();
            match pos_by_emoji.get(emoji.as_str()) {
                Some(&i) => {
                    // O(1) duplicate sender check via HashSet.
                    let senders: std::collections::HashSet<&str> =
                        reactions[i].2.iter().map(|s| s.as_str()).collect();
                    if !senders.contains(sender.as_str()) {
                        reactions[i].1 += 1;
                        reactions[i].2.push(sender);
                    }
                }
                None => reactions.push((emoji, 1, vec![sender])),
            }
            msg.update_reactions_json(serde_json::to_string(&reactions).unwrap_or_default());
        });
    }

    /// Remove an emoji reaction from a message (decrement count).
    pub fn remove_reaction(&self, event_id: &str, emoji: &str) {
        let emoji = emoji.to_string();
        self.update_message_in_place(event_id, |msg| {
            let mut reactions: Vec<(String, u64, Vec<String>)> =
                serde_json::from_str(&msg.reactions_json()).unwrap_or_default();
            // O(1) emoji lookup via position index.
            let pos_by_emoji: std::collections::HashMap<&str, usize> = reactions
                .iter().enumerate().map(|(i, (e, _, _))| (e.as_str(), i)).collect();
            if let Some(&i) = pos_by_emoji.get(emoji.as_str()) {
                if reactions[i].1 <= 1 { reactions.remove(i); }
                else { reactions[i].1 -= 1; }
            }
            msg.update_reactions_json(serde_json::to_string(&reactions).unwrap_or_default());
        });
    }

    /// Patch the event_id into the most recent history entry that matches body.
    /// Called when MessageSent confirms the server assigned an event_id to our echo.
    pub fn update_history_event_id(&self, body: &str, event_id: &str) {
        let mut history = self.imp().send_history.borrow_mut();
        // Search from the end — the matching entry is almost always the last one.
        if let Some(entry) = history.iter_mut().rev().find(|(b, _)| b == body) {
            entry.1 = event_id.to_string();
        }
    }

    /// Update a message's body and formatted body (for edits).
    pub fn update_message_body(&self, event_id: &str, new_body: &str, formatted: Option<&str>) {
        let new_body = new_body.to_string();
        let new_formatted = formatted.unwrap_or("").to_string();
        let (markup, hash) = prerender_body(&new_body, &new_formatted);
        let image_url = crate::widgets::message_row::extract_image_url(&new_body).unwrap_or_default();
        self.update_message_in_place(event_id, |msg| {
            msg.set_body(new_body.clone());
            msg.set_formatted_body(new_formatted.clone());
            msg.set_rendered_markup(markup.clone());
            msg.set_body_hash(hash);
            msg.set_image_url(image_url.clone());
        });
    }

    /// Scroll to a message by event_id and briefly flash it. Returns true if found.
    pub fn scroll_to_event(&self, event_id: &str) -> bool {
        if event_id.is_empty() { return false; }
        let imp = self.imp();
        // O(1) lookup — no list_store scan.
        let msg = match imp.event_index.borrow().get(event_id).cloned() {
            Some(m) => m,
            None => return false,
        };
        // The single shared ListView's model is already seek_store in seek mode
        // and the live room store otherwise — always use imp.list_view().
        let in_seek = imp.seek_saved_event_index.borrow().is_some();
        let (store, list_view): (gio::ListStore, gtk::ListView) = if in_seek {
            (imp.seek_store.clone(), imp.list_view())
        } else {
            (imp.list_store(), imp.list_view())
        };
        let Some(i) = store.find(&msg) else { return false };
        list_view.scroll_to(i, gtk::ListScrollFlags::NONE, None);
        // Trigger flash via GObject property — the notify::is-flashing handler
        // in bind_message_object applies the CSS class reactively, no widget walk.
        msg.set_is_flashing(true);
        let msg_weak = msg.downgrade();
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(900),
            move || {
                if let Some(m) = msg_weak.upgrade() {
                    m.set_is_flashing(false);
                }
            },
        );
        true
    }

    /// Remove a message from the timeline (for deletes).
    pub fn remove_message(&self, event_id: &str) {
        if event_id.is_empty() { return; }
        let imp = self.imp();
        // O(1) lookup via event_index.
        let msg = match imp.event_index.borrow_mut().remove(event_id) {
            Some(m) => m,
            None => return,
        };
        if let Some(i) = imp.list_store().find(&msg) {
            imp.list_store().remove(i);
        }
    }

    /// Reparent the single shared react EmojiChooser onto a row's react
    /// button and pop it up. Called by MessageRow when its react button is
    /// clicked; ownership of the chooser lives here so only one emoji widget
    /// tree is ever built for the entire MessageView.
    pub fn show_react_chooser_at(&self, btn: &gtk::Button, event_id: String) {
        let imp = self.imp();
        *imp.react_target_event_id.borrow_mut() = event_id;
        let chooser = imp.react_chooser.get_or_init(|| {
            let c = gtk::EmojiChooser::new();
            let view_weak = self.downgrade();
            c.connect_emoji_picked(move |_, emoji| {
                let Some(view) = view_weak.upgrade() else { return };
                let imp = view.imp();
                let eid = imp.react_target_event_id.borrow().clone();
                if eid.is_empty() { return; }
                let has_cb = imp.on_react.borrow().is_some();
                if has_cb {
                    let borrow = imp.on_react.borrow();
                    borrow.as_ref().unwrap()(eid, emoji.to_string());
                }
            });
            c
        });
        if chooser.parent().is_some() {
            chooser.unparent();
        }
        chooser.set_parent(btn);
        chooser.popup();
    }

    /// Enter reply mode — show preview and store the target event ID.
    pub fn start_reply(&self, event_id: &str, sender: &str, body: &str) {
        let imp = self.imp();
        imp.reply_to_event.replace(Some(event_id.to_string()));
        imp.reply_quote.replace(Some((sender.to_string(), body.to_string())));
        imp.reply_preview_label.set_label(&format!("{sender}: {body}"));
        imp.reply_preview.set_visible(true);
        imp.input_view.grab_focus();
    }

    /// Replace all messages (used when switching rooms).
    /// Scrolls to the m.fully_read marker if set, otherwise to the bottom.
    pub fn set_messages(&self, messages: &[crate::matrix::MessageInfo], prev_batch: Option<String>) {
        let imp = self.imp();

        let first_load = !imp.messages_loaded.get();
        imp.messages_loaded.set(true);

        // Empty placeholder from a bg_refresh timeout — dismiss the banner but
        // keep whatever messages are already displayed.
        if should_skip_empty_splice(messages.is_empty(), first_load) {
            return;
        }

        if !first_load {
            // bg_refresh: always do incremental updates — never a full-replace splice.
            //
            // Full splice (splice(0, N, &objs)) on an occupied list_store takes 300ms–3s
            // regardless of bind cost.  Instead:
            //   • New messages  → append to end (server always returns latest-first).
            //   • Edits/UTD     → update GObject properties in-place via update_message_body.
            //   • Redactions    → NOT detected here; handled by MatrixEvent::MessageRedacted.
            //     (The server fetch window may return fewer events than the disk cache holds —
            //      treating that difference as redactions was the original false-positive source.)

            // Restore the pagination token if it was cleared by a room switch (clear()
            // resets it to None).  We accept the server's latest-window token only when
            // ours is None — if the user has already scrolled back via prepend_messages,
            // their deeper token is kept so pagination can continue from where they are.
            if imp.prev_batch_token.borrow().is_none() {
                imp.prev_batch_token.replace(prev_batch);
            }

            // UTD → decrypted patch: when bg_refresh returns a real body for a
            // message that is currently shown as "🔒 Unable to decrypt", update it
            // in-place.  This is the only path that heals live-sync UTD events
            // after their session key arrives — the normal sync handler never
            // re-fires for already-displayed events, so without this pass those
            // messages stay locked forever.
            const UTD_BODY: &str = "\u{1f512} Unable to decrypt message";
            for m in messages.iter().filter(|m| !m.event_id.is_empty() && m.body != UTD_BODY) {
                let currently_utd = imp.event_index.borrow()
                    .get(&m.event_id)
                    .map(|obj| obj.body() == UTD_BODY)
                    .unwrap_or(false);
                if currently_utd {
                    tracing::info!("set_messages: healing UTD for event_id={}", m.event_id);
                    self.update_message_body(&m.event_id, &m.body, m.formatted_body.as_deref());
                }
            }

            // Collect new messages (not yet in the displayed list).
            let new_msgs: Vec<&crate::matrix::MessageInfo> = messages.iter()
                .filter(|m| !m.event_id.is_empty()
                    && !imp.event_index.borrow().contains_key(&m.event_id))
                .collect();

            // NOTE: we deliberately do NOT apply body/formatted_body/reaction diffs here.
            //
            // Live edits and reactions arrive via MatrixEvent::MessageEdited /
            // MatrixEvent::ReactionAdded through the sync handler — those paths are
            // authoritative and always reflect the latest state.
            //
            // Applying diffs here would create a race condition: if the bg_refresh
            // fetch completes AFTER the user applies a local edit (but before the
            // server echo arrives), the stale server body would overwrite the local
            // edit.  The live-sync path handles all correctness; bg_refresh is only
            // responsible for gap-filling new messages.

            // Insert new messages at the correct timestamp position.
            // new_msgs is already sorted oldest→newest (inherited from the sorted `messages`
            // parameter).  For each one, binary-search the list_store to find where its
            // timestamp belongs so that gap-fill messages (older than the newest in the
            // list) land in the right place rather than at the end.
            // Common case: all new messages are newer → every insert is an O(1) append.
            if !new_msgs.is_empty() {
                let list_store = imp.list_store();
                let mut any_at_end = false;
                let my_id = imp.user_id.borrow().clone();
                let today = glib::DateTime::now_local().ok();

                // Pass 1: echo patches — update existing GObjects in place, no new rows.
                // Kept separate so patch_echo_event_id can freely borrow event_index
                // without the drop/reborrow dance that the old single-pass loop needed.
                let mut echo_patched: std::collections::HashSet<String> = Default::default();
                for m in &new_msgs {
                    tracing::debug!(
                        "set_messages incremental: new msg event_id={} sender={} body={:?}",
                        m.event_id, m.sender_id, body_preview(&m.body)
                    );
                    let is_own = !m.event_id.is_empty()
                        && !my_id.is_empty()
                        && m.sender_id == my_id;
                    if is_own && self.patch_echo_event_id(&m.body, &m.event_id) {
                        echo_patched.insert(m.event_id.clone());
                        any_at_end = true;
                    }
                }

                // Pass 2: pre-compute insert positions on the ORIGINAL list (before any
                // insertions) so all end-appends can be batched into a single
                // splice(orig_n, 0, &batch) — one items_changed signal instead of N.
                // Gap-fills (older messages inserted mid-list) are rare; they get
                // individual splices inserted high-to-low so pre-computed positions
                // stay valid after each insertion.
                let orig_n = list_store.n_items();
                let mut end_appends: Vec<MessageObject> = Vec::new();
                let mut gap_fills: Vec<(u32, MessageObject)> = Vec::new();

                for m in &new_msgs {
                    if echo_patched.contains(&m.event_id) { continue; }
                    tracing::info!(
                        "set_messages incremental: inserting event_id={} (no echo found)",
                        m.event_id
                    );
                    let obj = Self::info_to_obj(m, today.as_ref());
                    let pos = Self::sorted_insert_pos(&list_store, m.timestamp);
                    if pos >= orig_n {
                        end_appends.push(obj);
                    } else {
                        gap_fills.push((pos, obj));
                    }
                }

                // One splice for all end-appends → one items_changed signal.
                if !end_appends.is_empty() {
                    let upcast: Vec<glib::Object> =
                        end_appends.iter().map(|o| o.clone().upcast()).collect();
                    list_store.splice(orig_n, 0, &upcast);
                    any_at_end = true;
                }

                // Gap-fills high-to-low: each insert only shifts items above it,
                // so positions computed on the original list remain correct.
                gap_fills.sort_by_key(|(pos, _)| *pos);
                for (pos, obj) in gap_fills.iter().rev() {
                    list_store.splice(*pos, 0, &[obj.clone().upcast::<glib::Object>()]);
                }

                // Update event_index once for all newly inserted objects.
                {
                    let mut idx = imp.event_index.borrow_mut();
                    for obj in end_appends.iter()
                        .chain(gap_fills.iter().map(|(_, o)| o))
                    {
                        if !obj.event_id().is_empty() {
                            idx.insert(obj.event_id(), obj.clone());
                        }
                    }
                }

                // Only scroll to bottom when new messages arrived at the end AND
                // the user is already near the bottom.  If they are scrolled up
                // reading history, append silently without disturbing their position.
                if any_at_end && self.is_near_bottom() {
                    let view_weak = self.downgrade();
                    glib::idle_add_local_once(move || {
                        let Some(view) = view_weak.upgrade() else { return };
                        view.scroll_to_bottom();
                    });
                }
            }

            tracing::info!(
                "set_messages: incremental new={} room={}",
                new_msgs.len(),
                imp.current_room_id.borrow()
            );

            // Insert divider if needed (same logic as the no-change path).
            let unread = imp.room_unread_count.get();
            let divider_in_list = imp.event_index.borrow().contains_key("__unread_divider__");
            if !divider_in_list && unread > 0 {
                let fully_read = imp.fully_read_event_id.borrow().clone();
                tracing::info!(
                    "Divider check (incremental): unread={unread}, fully_read={fully_read:?}"
                );
                let placed = fully_read
                    .as_deref()
                    .map(|eid| self.insert_divider_after_event(eid))
                    .unwrap_or(false);
                if !placed {
                    self.insert_divider_by_count(unread);
                }
            }
            return; // Never fall through to the full-replace splice below.
        }

        let _t1 = std::time::Instant::now();
        let today = glib::DateTime::now_local().ok();
        let objs: Vec<MessageObject> = messages.iter().map(|m| Self::info_to_obj(m, today.as_ref())).collect();
        let _t2 = std::time::Instant::now();
        let n = gio::prelude::ListModelExt::n_items(&imp.list_store());
        use std::sync::atomic::Ordering;
        crate::widgets::message_view::BIND_COUNT.store(0, Ordering::Relaxed);
        crate::widgets::message_view::BIND_TOTAL_US.store(0, Ordering::Relaxed);
        // For first_load the room's list_store is empty (n=0).  Detaching the
        // model before the splice prevents GTK from processing items-changed for
        // each item — it reads the final N items in one shot when the model is
        // re-attached below.  set_model(None) on an empty list is O(0) (instant),
        // unlike set_model(None) on a non-empty list which is O(N_content).
        if first_load {
            // Detach model so GTK reads all N items in one shot on re-attach.
            imp.list_view().set_model(gtk::SelectionModel::NONE);
        }
        imp.list_store().splice(0, n, &objs);
        let _t3 = std::time::Instant::now();
        let bc = crate::widgets::message_view::BIND_COUNT.load(Ordering::Relaxed);
        let bt = crate::widgets::message_view::BIND_TOTAL_US.load(Ordering::Relaxed);
        tracing::info!("set_messages: info_to_obj(n={})={:?} splice(prev={n},first_load={first_load})={:?} binds={bc} bind_total={}µs",
            objs.len(), _t2-_t1, _t3-_t2, bt);
        // Rebuild event_index from scratch for the new room.
        // Also clear divider_obj — the splice removed the old room's messages,
        // so any is_first_unread property on them is now stale.
        *imp.divider_obj.borrow_mut() = None;
        // Echoes in the old list are gone; reset the guard counter.
        imp.pending_echo_count.set(0);
        let mut idx = imp.event_index.borrow_mut();
        idx.clear();
        for obj in &objs {
            let eid = obj.event_id();
            if !eid.is_empty() {
                idx.insert(eid, obj.clone());
            }
        }
        drop(idx);
        imp.prev_batch_token.replace(prev_batch);
        imp.fetching_older.set(false);

        // When the loaded batch is small (can fit on-screen), the scroll
        // threshold (adj.value < 50) never fires because the list is fully
        // visible and the user has no surface to scroll.  Auto-fetch older
        // messages immediately so the view fills with history.
        if imp.list_store().n_items() < 20 && imp.prev_batch_token.borrow().is_some() {
            if !imp.fetching_older.get() {
                if let Some(ref cb) = *imp.on_scroll_top.borrow() {
                    imp.fetching_older.set(true);
                    cb();
                }
            }
        }

        // Insert (or re-insert) the "New messages" divider whenever it's absent from
        // the list and the server reports unread messages.  The splice above always
        // removes the divider because it replaces the full message list with fresh SDK
        // data — so we re-run this check on every load, not just first_load.
        //
        // Three cases for where to place it:
        //   A) fully_read event is in the window with messages after it → insert there.
        //   B) fully_read event is the LAST message in the window (the unread messages
        //      are newer than this window) → fall back to count-based placement.
        //   C) fully_read event is not in the window at all, or no marker → fall back
        //      to count-based placement.
        let unread = imp.room_unread_count.get();
        // Place the divider whenever the list has no divider and the server
        // reports unread messages.  This includes both the initial disk-cache
        // load (first_load=true) AND bg_refresh calls that splice new messages
        // into the list (first_load=false) — without this, the divider would
        // silently disappear any time a bg_refresh delivered new messages
        // because the splice clears event_index (removing the divider sentinel).
        // Auto-scroll is still gated on first_load below so the user is not
        // yanked away from their reading position.
        let divider_in_list = imp.event_index.borrow().contains_key("__unread_divider__");
        let divider_inserted = if !divider_in_list && unread > 0 {
            let fully_read = imp.fully_read_event_id.borrow().clone();
            tracing::info!("Divider check: unread={unread}, fully_read={fully_read:?}");
            let placed = fully_read
                .as_deref()
                .map(|eid| self.insert_divider_after_event(eid))
                .unwrap_or(false);
            if !placed {
                // Cases B and C: fall back to count-based placement.
                self.insert_divider_by_count(unread);
            }
            true
        } else {
            tracing::debug!(
                "Divider skipped: divider_in_list={divider_in_list}, unread={unread}"
            );
            false
        };

        // Ensure "messages" is visible (handles placeholder→messages transition)
        // and hide the loading overlay.
        imp.view_stack.set_visible_child_name("messages");
        imp.room_loading_overlay.set_visible(false);

        if first_load {
            // Re-attach the model and scroll in one idle so set_messages returns
            // immediately without blocking the GTK thread.  set_model(Some)
            // triggers height estimation for all visible rows (~129ms for 80+
            // variable-height rows); deferring it keeps the GTK frame budget.
            let store = imp.list_store().clone();
            let lv = imp.list_view();
            let view_weak = self.downgrade();
            glib::idle_add_local_once(move || {
                let no_sel = gtk::NoSelection::new(Some(store));
                lv.set_model(Some(&no_sel));
                let Some(view) = view_weak.upgrade() else { return };
                if divider_inserted {
                    view.scroll_to_event("__unread_divider__");
                } else {
                    view.scroll_to_bottom();
                }
            });
        }
    }

    /// Prepend older messages at the top (pagination).
    pub fn prepend_messages(&self, messages: &[crate::matrix::MessageInfo], prev_batch: Option<String>) {
        const MAX_STORE_SIZE: u32 = 400;
        let imp = self.imp();
        let today = glib::DateTime::now_local().ok();
        let objs: Vec<MessageObject> = messages.iter().map(|m| Self::info_to_obj(m, today.as_ref())).collect();
        imp.list_store().splice(0, 0, &objs);
        // Add new objects to the event_index.
        {
            let mut idx = imp.event_index.borrow_mut();
            for obj in &objs {
                let eid = obj.event_id();
                if !eid.is_empty() {
                    idx.insert(eid, obj.clone());
                }
            }
        }
        // Cap the store at MAX_STORE_SIZE to keep GTK height-tracking bounded.
        // Items at the tail (high indices = newest messages) are evicted first
        // when the user loads deep history via prepend.
        // IMPORTANT: never evict unconfirmed echoes (empty event_id) — they are
        // in-flight messages the user sent, and evicting them would cause the message
        // to vanish and then reappear at a different position when the server confirms it.
        let store = imp.list_store();
        let n = store.n_items();
        if n > MAX_STORE_SIZE {
            // Scan forward from MAX_STORE_SIZE; stop before the first echo.
            let mut evict_end = MAX_STORE_SIZE;
            while evict_end < n {
                let is_echo = store.item(evict_end)
                    .and_downcast::<MessageObject>()
                    .map(|o| o.event_id().is_empty())
                    .unwrap_or(false);
                if is_echo { break; }
                evict_end += 1;
            }
            let remove_count = evict_end - MAX_STORE_SIZE;
            if remove_count > 0 {
                {
                    let mut idx = imp.event_index.borrow_mut();
                    for i in MAX_STORE_SIZE..evict_end {
                        if let Some(obj) = store.item(i).and_downcast::<MessageObject>() {
                            idx.remove(&obj.event_id());
                        }
                    }
                }
                store.splice(MAX_STORE_SIZE, remove_count, &[] as &[MessageObject]);
                // Signal that newest messages were evicted; the scroll handler
                // will trigger a bg_refresh when the user returns to the bottom.
                imp.tail_evicted.set(true);
            }
        }
        imp.prev_batch_token.replace(prev_batch);
        imp.fetching_older.set(false);
    }

    /// Show the seek banner immediately in "Finding…" state while the server
    /// round-trip is in flight.  Call this when SeekToEvent is dispatched.
    pub fn start_seek_loading(&self) {
        let imp = self.imp();
        imp.seek_banner_label.set_text("Finding message…");
        imp.seek_spinner.set_spinning(true);
        imp.seek_spinner.set_visible(true);
        // Hide "Jump to latest" until we have results.
        if let Some(btn) = imp.seek_banner.last_child() { btn.set_visible(false); }
        imp.seek_banner.set_visible(true);
    }

    /// Load a seek (historical context) result: replace the timeline with the
    /// context window around `target_event_id` and show the seek banner.
    pub fn load_seek_result(
        &self,
        messages: &[crate::matrix::MessageInfo],
        target_event_id: &str,
        before_token: Option<String>,
    ) {
        let imp = self.imp();

        let today = glib::DateTime::now_local().ok();
        let objs: Vec<MessageObject> = messages.iter().map(|m| Self::info_to_obj(m, today.as_ref())).collect();

        // Populate the dedicated seek_store (never touches the live store).
        let n_old = imp.seek_store.n_items();
        imp.seek_store.splice(0, n_old, &objs);

        // Build seek event_index; save the live one for restore on cancel.
        let mut seek_idx = std::collections::HashMap::new();
        for obj in &objs {
            let eid = obj.event_id();
            if !eid.is_empty() { seek_idx.insert(eid, obj.clone()); }
        }
        let live_idx = imp.event_index.borrow().clone();
        *imp.seek_saved_event_index.borrow_mut() = Some(live_idx);
        *imp.event_index.borrow_mut() = seek_idx;

        // Store seek state (don't overwrite prev_batch_token — live store's token untouched).
        *imp.seek_before_token.borrow_mut() = before_token;
        *imp.seek_target_event_id.borrow_mut() = Some(target_event_id.to_string());

        // Swap model to seek_store — same ListView, different data.
        let seek_no_sel = gtk::NoSelection::new(Some(imp.seek_store.clone()));
        imp.list_view().set_model(Some(&seek_no_sel));

        // Transition seek banner from "Finding…" → "Historical context" state.
        imp.seek_spinner.set_spinning(false);
        imp.seek_spinner.set_visible(false);
        imp.seek_banner_label.set_text("Viewing historical context");
        if let Some(btn) = imp.seek_banner.last_child() { btn.set_visible(true); }
        imp.seek_banner.set_visible(true);

        // Ensure message view is showing, overlay hidden.
        imp.view_stack.set_visible_child_name("messages");
        imp.room_loading_overlay.set_visible(false);

        tracing::info!(
            "load_seek_result: {} messages, target={target_event_id}",
            messages.len()
        );

        // Scroll to target event after GTK lays out the list.
        let view_weak = self.downgrade();
        let eid = target_event_id.to_string();
        glib::idle_add_local_once(move || {
            let Some(view) = view_weak.upgrade() else { return };
            view.scroll_to_event(&eid);
        });
    }

    /// Exit seek mode: swap the ListView model back to the live room store,
    /// restore the event_index, and clear the seek store.
    pub fn cancel_seek(&self) {
        let imp = self.imp();

        // Swap model back to the live room store.
        let room_id = imp.current_room_id.borrow().clone();
        let live_store = imp.ensure_room_store(&room_id);
        let no_sel = gtk::NoSelection::new(Some(live_store));
        imp.list_view().set_model(Some(&no_sel));

        // Restore the live event_index.
        if let Some(saved) = imp.seek_saved_event_index.borrow_mut().take() {
            *imp.event_index.borrow_mut() = saved;
        }

        // Clear the seek store so its memory is released.
        imp.seek_store.remove_all();

        // Reset seek state and banner.
        imp.seek_spinner.set_spinning(false);
        imp.seek_spinner.set_visible(false);
        imp.seek_banner_label.set_text("Viewing historical context");
        if let Some(btn) = imp.seek_banner.last_child() { btn.set_visible(true); }
        imp.seek_banner.set_visible(false);
        *imp.seek_before_token.borrow_mut() = None;
        *imp.seek_target_event_id.borrow_mut() = None;

        if let Some(ref cb) = *imp.on_seek_cancelled.borrow() {
            cb();
        }
    }

    /// Register a callback for when the user clicks "Jump to latest" in seek mode.
    pub fn connect_seek_cancelled<F: Fn() + 'static>(&self, f: F) {
        *self.imp().on_seek_cancelled.borrow_mut() = Some(Box::new(f));
    }

    /// Serialize all messages currently in the list store to JSON Lines and
    /// write them to `path`.  Each line is:
    ///   {"sender":"Display Name","sender_id":"@user:server","body":"..."}
    /// System events (joins, leaves) are skipped.
    /// Returns the number of messages written.
    pub fn export_messages_jsonl(&self, path: &std::path::Path) -> std::io::Result<usize> {
        use std::io::Write as _;
        use gio::prelude::ListModelExt as _;

        let imp = self.imp();
        let n = imp.list_store().n_items();
        let mut file = std::fs::File::create(path)?;
        let mut count = 0usize;

        for i in 0..n {
            let Some(obj) = imp.list_store().item(i) else { continue };
            let Some(msg) = obj.downcast_ref::<MessageObject>() else { continue };
            if msg.is_system_event() { continue; }
            let body = msg.body();
            if body.is_empty() { continue; }
            // Minimal JSON — escape only what serde_json would escape.
            let line = serde_json::json!({
                "sender":    msg.sender(),
                "sender_id": msg.sender_id(),
                "body":      body,
            });
            writeln!(file, "{line}")?;
            count += 1;
        }
        Ok(count)
    }

    /// Walk a widget tree to find a MessageRow child.
    /// Binary-search the list_store (sorted oldest→newest by timestamp) for the
    /// first position whose item's timestamp is strictly greater than `ts`.
    /// Inserting at this position keeps the list sorted.
    /// Returns list_store.n_items() when `ts` is ≥ everything (append case).
    fn sorted_insert_pos(list_store: &gio::ListStore, ts: u64) -> u32 {
        use gio::prelude::ListModelExt;
        let n = list_store.n_items();
        if n == 0 { return 0; }
        sorted_insert_pos_in(n, ts, |mid| {
            list_store.item(mid)
                .and_downcast::<crate::models::MessageObject>()
                .map(|o| o.timestamp())
                .unwrap_or(0)
        })
    }

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

    fn info_to_obj(m: &crate::matrix::MessageInfo, today: Option<&glib::DateTime>) -> MessageObject {
        let _g = crate::perf::scope_gt("info_to_obj", 200);
        let media_json = m.media.as_ref()
            .and_then(|media| serde_json::to_string(media).ok())
            .unwrap_or_default();
        // Strip Matrix reply fallback ("> <@user> ..." lines) from body
        // since we show "Replying to {name}" as a visual indicator.
        let body = if m.reply_to.is_some() {
            crate::widgets::message_row::strip_reply_fallback(&m.body)
        } else {
            m.body.clone()
        };
        let formatted_body = m.formatted_body.as_deref().unwrap_or("");
        let reactions_json = serde_json::to_string(&m.reactions).unwrap_or_default();
        let obj = MessageObject::new(
            &m.sender,
            &m.sender_id,
            &body,
            formatted_body,
            m.timestamp,
            &m.event_id,
            m.reply_to.as_deref().unwrap_or(""),
            m.thread_root.as_deref().unwrap_or(""),
            &m.reactions,
            &media_json,
        );
        obj.set_is_highlight(m.is_highlight);
        obj.set_is_system_event(m.is_system_event);
        let reply_to_sender = m.reply_to_sender.as_deref().unwrap_or("");
        if !reply_to_sender.is_empty() {
            obj.set_reply_to_sender(reply_to_sender.to_string());
        }
        if m.timestamp > 0 {
            obj.set_formatted_timestamp(
                crate::widgets::message_row::format_timestamp_with_today(m.timestamp, today));
        }
        // Pre-compute the body_hash synchronously (cheap FNV-1a over the
        // strings) and the plain-text fallback markup. For messages with a
        // non-empty formatted_body (Matrix HTML) we enqueue the expensive
        // html_to_pango parse onto the background markup worker; the row
        // shows the plain-text fallback in the interim and swaps in the
        // rendered Pango markup via notify::rendered-markup when the
        // worker delivers. This keeps info_to_obj bounded regardless of
        // how pathological a single formatted_body is.
        let hash = prerender_body_hash(&body, formatted_body);
        obj.set_body_hash(hash);
        if formatted_body.is_empty() {
            let escaped = gtk::glib::markup_escape_text(&body).to_string();
            obj.set_rendered_markup(crate::markdown::linkify_urls(&escaped));
        } else {
            // Fallback visible until the worker replies — keeps the message
            // readable rather than empty during the parse window.
            let escaped = gtk::glib::markup_escape_text(&body).to_string();
            obj.set_rendered_markup(crate::markdown::linkify_urls(&escaped));
            crate::markup_worker::try_enqueue(&obj, formatted_body.to_string());
        }
        obj.set_sender_markup(crate::widgets::message_row::prerender_sender_markup(&m.sender, &m.sender_id));
        obj.set_reactions_hash(fnv1a_str(&reactions_json));
        obj.set_image_url(
            crate::widgets::message_row::extract_image_url(&body)
                .unwrap_or_default()
        );
        obj.set_reply_label(prerender_reply_label(
            m.reply_to.is_some(), reply_to_sender, &body
        ));
        if let Some(ref media) = m.media {
            let (icon, label, a11y) = prerender_media_display(media);
            obj.set_media_icon_name(icon);
            obj.set_media_display_label(label);
            obj.set_media_a11y_label(a11y);
            obj.set_media_url_str(media.url.clone());
            obj.set_media_filename_str(media.filename.clone());
            obj.set_media_source_json_str(media.source_json.clone());
        }
        obj
    }

    /// Get the current pagination token.
    pub fn prev_batch_token(&self) -> Option<String> {
        self.imp().prev_batch_token.borrow().clone()
    }

    /// Prepare for a room switch to `room_id`.
    ///
    /// Swaps the single shared ListView's model to the room's gio::ListStore.
    /// The widget tree stays constant size regardless of rooms visited — only
    /// the data model pointer changes.  On return visits the existing messages
    /// are visible immediately; bg_refresh splices only if content changed.
    pub fn clear(&self, room_id: &str) {
        let imp = self.imp();
        // Cancel any previous deferred-loading timer.
        if let Some(id) = imp.loading_timer.borrow_mut().take() {
            id.remove();
        }
        self.set_refreshing(false);

        // ── Save outgoing room state ─────────────────────────────────────────
        let old_room_id = imp.current_room_id.borrow().clone();
        if !old_room_id.is_empty() {
            // Save normalized scroll position so we can restore it on return.
            let sw = imp.scrolled_window();
            let adj = sw.vadjustment();
            let frac = scroll_save_frac(adj.value(), adj.upper(), adj.page_size());
            imp.saved_scroll_frac.borrow_mut().insert(old_room_id.clone(), frac);

            let idx = imp.event_index.borrow().clone();
            imp.saved_event_indices.borrow_mut().insert(old_room_id.clone(), idx);
            imp.saved_messages_loaded.borrow_mut()
                .insert(old_room_id, imp.messages_loaded.get());
        }

        // ── Set up incoming room view and store ──────────────────────────────
        let is_return_visit = imp.room_view_cache.borrow().contains_key(room_id);
        // Create per-room ListView+ScrolledWindow on first visit (O(1) setup).
        // On return visits ensure_room_view is a no-op.
        imp.ensure_room_view(room_id);
        // Mark this room as most-recently-used and evict the coldest entries
        // from the per-room widget cache. Without this, every visited room
        // keeps its MessageRow pool (gtk::Labels, gestures, CSS styles) alive
        // forever; heap grows linearly with rooms-visited. Must run after
        // ensure_room_view so the incoming room is not itself evicted.
        imp.touch_recent_room(room_id);
        let list_store = imp.ensure_room_store(room_id);
        *imp.cur_list_store.borrow_mut() = list_store;
        *imp.current_room_id.borrow_mut() = room_id.to_string();

        // Restore per-room state for return visits so set_messages can skip an
        // unnecessary splice when the server returns the same data we already show.
        if let Some(saved_idx) = imp.saved_event_indices.borrow_mut().remove(room_id) {
            *imp.event_index.borrow_mut() = saved_idx;
        } else {
            imp.event_index.borrow_mut().clear();
        }
        let was_loaded = imp.saved_messages_loaded.borrow_mut()
            .remove(room_id).unwrap_or(false);
        imp.messages_loaded.set(was_loaded);

        // ── Reset non-persisted per-room state ───────────────────────────────
        imp.bookmarked_ids.borrow_mut().clear();
        imp.new_message_objs.borrow_mut().clear();
        imp.pending_appends.borrow_mut().clear();
        imp.append_flush_pending.set(false);
        imp.tail_evicted.set(false);
        imp.prev_batch_token.replace(None);
        imp.fetching_older.set(false);
        imp.room_unread_count.set(0);
        imp.fully_read_event_id.replace(None);
        imp.scroll_to_bottom_pending.set(false);
        // pending_echo_count is a hint; reset on room switch so stale echoes
        // in a swapped-away room don't keep the new room's counter elevated.
        imp.pending_echo_count.set(0);

        // Clear info banners.
        imp.unread_banner.set_revealed(false);
        imp.info_banner.set_visible(false);
        imp.info_separator.set_visible(false);
        imp.topic_label.set_visible(false);
        imp.tombstone_banner.set_visible(false);
        imp.pinned_box.set_visible(false);
        self.remove_css_class("tombstone-view");

        // Clear reply/typing state.
        imp.reply_to_event.replace(None);
        imp.reply_quote.replace(None);
        imp.pending_mentions.borrow_mut().clear();
        imp.last_typing_sent.set(false);
        if let Some(id) = imp.typing_debounce.borrow_mut().take() { id.remove(); }
        if let Some(id) = imp.spell_debounce.borrow_mut().take() { id.remove(); }
        imp.reply_preview.set_visible(false);
        imp.typing_label.set_visible(false);

        // ── Switch to this room's ListView — O(1), no items_changed ──────────
        // ensure_room_view (called above) created the per-room widgets on first
        // visit; set_visible_child_name just unhides the existing tree.
        // No set_model(), no splice, no GTK layout work on room switch.
        imp.room_view_stack.get().unwrap().set_visible_child_name(room_id);

        // Always show "messages" so the ListView gets a real size allocation —
        // this lets the factory pool warm up behind the loading overlay.
        imp.view_stack.set_visible_child_name("messages");
        if should_show_loading_after_switch(is_return_visit, was_loaded) {
            imp.room_loading_overlay.set_visible(true);
        } else {
            // Return visit with loaded messages: reveal immediately (no overlay).
            imp.room_loading_overlay.set_visible(false);
            // Scroll restore deferred one idle so GTK has computed row heights.
            if let Some(frac) = imp.saved_scroll_frac.borrow_mut().remove(room_id) {
                let view_weak = self.downgrade();
                let room_id_owned = room_id.to_string();
                glib::idle_add_local_once(move || {
                    let Some(view) = view_weak.upgrade() else { return };
                    let imp = view.imp();
                    if *imp.current_room_id.borrow() != room_id_owned { return };
                    let adj = imp.scrolled_window().vadjustment();
                    adj.set_value(scroll_restore_value(frac, adj.upper(), adj.page_size()));
                });
            }
        }

        tracing::info!(
            "clear: room={room_id} return={is_return_visit} was_loaded={was_loaded}"
        );
    }

    /// Connect a callback for when the user scrolls to the top (load older messages).
    pub fn connect_scroll_top<F: Fn() + 'static>(&self, f: F) {
        self.imp().on_scroll_top.replace(Some(Box::new(f)));
    }

    pub fn connect_scroll_bottom<F: Fn() + 'static>(&self, f: F) {
        self.imp().on_scroll_bottom.replace(Some(Box::new(f)));
    }

    /// Update the room info banner with metadata (topic, tombstone, pinned).
    pub fn set_room_meta(&self, meta: &crate::matrix::RoomMeta) {
        let _g = crate::perf::scope("set_room_meta");
        let imp = self.imp();
        let mut show_banner = false;

        // Topic — treat as markdown (Matrix topic is plain text but users write markdown).
        {
            let _t = crate::perf::scope_gt("set_room_meta::topic", 200);
            if !meta.topic.is_empty() {
                imp.topic_label.set_markup(&crate::markdown::md_to_pango(&meta.topic));
                imp.topic_label.set_visible(true);
                show_banner = true;
            } else {
                imp.topic_label.set_visible(false);
            }
        }

        // Tombstone — apply background to entire message view.
        // The replacement_room is rendered as a Pango anchor so the user can
        // click to join; label is selectable so the room id can at least be
        // copy-pasted when the click path fails (e.g. invite-only replacement).
        {
            let _t = crate::perf::scope_gt("set_room_meta::tombstone", 200);
            if meta.is_tombstoned {
                let msg = match (&meta.replacement_room_name, &meta.replacement_room) {
                    (Some(name), Some(id)) => format!(
                        "This room has been upgraded to: {}",
                        tombstone_link_markup(id, name),
                    ),
                    (None, Some(id)) => format!(
                        "This room has been upgraded. New room: {}",
                        tombstone_link_markup(id, id),
                    ),
                    (Some(name), None) => format!(
                        "This room has been upgraded to: {}",
                        glib::markup_escape_text(name),
                    ),
                    (None, None) => "This room has been upgraded to a new room.".to_string(),
                };
                imp.tombstone_label.set_markup(&msg);
                imp.tombstone_banner.set_visible(true);
                self.add_css_class("tombstone-view");
                show_banner = true;
            } else {
                imp.tombstone_banner.set_visible(false);
                self.remove_css_class("tombstone-view");
            }
        }

        // Pinned messages — remove old entries, add fresh ones with sender.
        // This section constructs gtk::Box + two gtk::Labels + parses markup
        // per pinned message; for rooms with many pinned messages it is the
        // dominant cost of set_room_meta. Count is logged as ctx so the log
        // tells us "n={how many pinned messages were rebuilt}".
        {
            let pinned_count = meta.pinned_messages.len();
            let _t = crate::perf::scope_with("set_room_meta::pinned", format!("n={pinned_count}"));
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
                for (sender, body, formatted) in &meta.pinned_messages {
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
                    let pango = match formatted {
                        Some(html) => crate::markdown::html_to_pango(html),
                        None => crate::markdown::md_to_pango(body),
                    };
                    let body_label = gtk::Label::builder()
                        .halign(gtk::Align::Start)
                        .wrap(true)
                        .wrap_mode(gtk::pango::WrapMode::WordChar)
                        .css_classes(["caption"])
                        .build();
                    body_label.set_markup(&pango);
                    row.append(&sender_label);
                    row.append(&body_label);
                    pinned.append(&row);
                }
                pinned.set_visible(true);
                show_banner = true;
            } else {
                pinned.set_visible(false);
            }
        }

        imp.info_banner.set_visible(show_banner);
        imp.info_separator.set_visible(show_banner);

        // Store members for nick completion, sorted by lowercase name
        // for O(log n) binary search prefix matching.
        {
            let member_count = meta.members.len();
            let _t = crate::perf::scope_with("set_room_meta::members", format!("n={member_count}"));
            let mut members: Vec<(String, String, String)> = meta.members
                .iter()
                .map(|(uid, name)| (name.to_lowercase(), name.clone(), uid.clone()))
                .collect();
            members.sort_by(|a, b| a.0.cmp(&b.0));
            imp.room_members.replace(members);
            // Stash mxc URLs for each member so the nick popover can look
            // them up and fire FetchAvatar on demand.
            let mut mxc_map: std::collections::HashMap<String, String> =
                std::collections::HashMap::with_capacity(meta.member_avatars.len());
            for (uid, mxc) in &meta.member_avatars {
                if !mxc.is_empty() {
                    mxc_map.insert(uid.clone(), mxc.clone());
                }
            }
            imp.member_avatar_mxc.replace(mxc_map);
        }

        // Update fully_read marker and unread count. messages_loaded is
        // intentionally NOT reset here — only clear() resets it on room switch
        // so bg_refresh doesn't snap the user back to an earlier position.
        tracing::info!(
            "set_room_meta: unread={}, fully_read={:?}",
            meta.unread_count,
            meta.fully_read_event_id
        );
        // Preserve a non-zero unread count that was set at initial load.
        // A bg_refresh completing after the 15-second read-receipt timer fires
        // would otherwise send unread_count=0 and erase the divider before the
        // user has actually scrolled to those messages.
        // Rule: only lower the count to zero when clear() resets it (room switch)
        // or dismiss_unread() resets it (user sends a message).
        let new_count = effective_unread_count(
            imp.messages_loaded.get(),
            imp.room_unread_count.get(),
            meta.unread_count,
        );
        imp.room_unread_count.set(new_count);
        imp.fully_read_event_id.replace(meta.fully_read_event_id.clone());
    }

    /// Called when the current user sends a message.  Clears the "New messages"
    /// divider and resets the unread count — if you're actively typing you've
    /// read everything in this room.  Prevents the user's own sent messages from
    /// appearing below the divider on the next bg_refresh.
    pub fn dismiss_unread(&self) {
        self.imp().room_unread_count.set(0);
        self.remove_dividers();
    }

    /// Remove all "New messages" divider lines from the timeline.
    pub fn remove_dividers(&self) {
        let imp = self.imp();
        // Clear new-message tint only on tracked objects — O(unread) not O(n).
        for obj in imp.new_message_objs.borrow_mut().drain(..) {
            obj.set_is_new_message(false);
        }
        // Property-based divider (insert_divider_by_count / insert_divider_after_event):
        // clear is_first_unread on the tracked object — no list-store removal, O(1).
        if let Some(obj) = imp.divider_obj.borrow_mut().take() {
            obj.set_is_first_unread(false);
            imp.event_index.borrow_mut().remove("__unread_divider__");
        } else {
            // Sentinel divider (insert_divider for live messages at end of list):
            // must be removed from the list store.
            if let Some(divider) = imp.event_index.borrow_mut().remove("__unread_divider__") {
                if let Some(i) = imp.list_store().find(&divider) {
                    imp.list_store().remove(i);
                }
            }
        }
        imp.unread_banner.set_revealed(false);
    }

    /// Build a "New messages" divider MessageObject with the sentinel event_id.
    fn make_divider_obj() -> MessageObject {
        let body = "── New messages ──";
        let divider = MessageObject::new(
            "",
            "",
            body,
            "",
            0,
            "__unread_divider__",
            "",
            "",
            &[],
            "",
        );
        divider.set_is_highlight(true);
        let (markup, hash) = prerender_body(body, "");
        divider.set_rendered_markup(markup);
        divider.set_body_hash(hash);
        divider.set_sender_markup(String::new());
        divider.set_reactions_hash(fnv1a_str("[]"));
        divider
    }

    /// Insert a "New messages" divider by counting `unread_count` items back
    /// from the end of the list.  Uses is_first_unread property on the target
    /// MessageObject instead of inserting a sentinel item into the list store —
    /// this avoids triggering items_changed which would invalidate GTK's height
    /// cache for all subsequent rows (causing slow scrolling for large unread counts).
    fn insert_divider_by_count(&self, unread_count: u32) {
        let imp = self.imp();
        let my_id = imp.user_id.borrow().clone();
        let n = gio::prelude::ListModelExt::n_items(&imp.list_store());
        let nominal_pos = n.saturating_sub(unread_count);
        // Clamp past the user's last own-message. An own-message means the
        // user engaged with the room past that point — the divider must
        // never appear above it. If no unread content follows the user's
        // last own-message, don't insert a divider at all.
        let pos = clamp_divider_past_own_messages(&imp.list_store(), nominal_pos, &my_id);
        if pos >= n {
            return;
        }
        // Mark all messages after the divider position as new and track them
        // so remove_dividers can clear them in O(unread) not O(total).
        // Never mark own messages as new — the user doesn't need to be notified
        // about messages they sent themselves.
        let mut new_objs = imp.new_message_objs.borrow_mut();
        new_objs.clear();
        for i in pos..n {
            if let Some(obj) = gio::prelude::ListModelExt::item(&imp.list_store(), i)
                .and_downcast::<MessageObject>()
            {
                if !divider_should_mark(&obj.sender_id(), &my_id) {
                    continue;
                }
                obj.set_is_new_message(true);
                new_objs.push(obj);
            }
        }
        drop(new_objs);
        // Set is_first_unread on the message at pos — it renders the divider
        // bar above itself.  No list-store insert → no items_changed → no GTK
        // height-cache invalidation for the following rows.
        if let Some(first_obj) = gio::prelude::ListModelExt::item(&imp.list_store(), pos)
            .and_downcast::<MessageObject>()
        {
            first_obj.set_is_first_unread(true);
            *imp.divider_obj.borrow_mut() = Some(first_obj.clone());
            imp.event_index.borrow_mut().insert("__unread_divider__".to_string(), first_obj);
        }
        // Actual new count = messages from pos to end.
        let actual_new = n - pos;
        self.show_unread_banner(actual_new);
    }

    /// Insert a "New messages" divider after the given event_id.
    ///
    /// Returns `true` if the event was found in the list and the divider was
    /// placed.  Returns `false` (case B/C) so the caller can fall back to
    /// `insert_divider_by_count`.
    ///
    /// Uses is_first_unread property on the message at insert_pos — no list-store
    /// insert, no items_changed, no GTK height-cache invalidation.
    fn insert_divider_after_event(&self, event_id: &str) -> bool {
        let imp = self.imp();
        let my_id = imp.user_id.borrow().clone();
        let Some(marker_obj) = imp.event_index.borrow().get(event_id).cloned() else {
            return false; // Event not in the current window (case C).
        };
        let Some(marker_pos) = imp.list_store().find(&marker_obj) else {
            return false;
        };
        let nominal_insert = marker_pos + 1;
        let n = gio::prelude::ListModelExt::n_items(&imp.list_store());
        if nominal_insert >= n {
            return false; // Event is the last item — unread messages not yet loaded (case B).
        }
        // Clamp past the user's last own-message: the server's fully_read can
        // lag behind the user's actual engagement (they sent messages but the
        // 15-second read-receipt timer hadn't fired when bg_refresh landed),
        // so a raw marker-based placement routinely puts the divider above
        // own replies the user clearly already read. Shift the divider to
        // after the user's last own-message. If that puts us past the end of
        // the list, don't insert a divider — the user is caught up.
        let insert_pos = clamp_divider_past_own_messages(&imp.list_store(), nominal_insert, &my_id);
        if insert_pos >= n {
            return true; // Caught-up: treat as handled, no divider needed.
        }
        // Mark messages after the (clamped) divider position as new.
        let mut new_objs = imp.new_message_objs.borrow_mut();
        new_objs.clear();
        for i in insert_pos..n {
            if let Some(obj) = gio::prelude::ListModelExt::item(&imp.list_store(), i)
                .and_downcast::<MessageObject>()
            {
                if !divider_should_mark(&obj.sender_id(), &my_id) {
                    continue;
                }
                obj.set_is_new_message(true);
                new_objs.push(obj);
            }
        }
        drop(new_objs);
        // Mark the first unread message — it renders the divider bar above itself.
        if let Some(first_obj) = gio::prelude::ListModelExt::item(&imp.list_store(), insert_pos)
            .and_downcast::<MessageObject>()
        {
            first_obj.set_is_first_unread(true);
            *imp.divider_obj.borrow_mut() = Some(first_obj.clone());
            imp.event_index.borrow_mut().insert("__unread_divider__".to_string(), first_obj);
        }
        let new_count = n - insert_pos;
        self.show_unread_banner(new_count);
        true
    }

    /// Insert a "New messages" divider line at the end of the timeline
    /// (used for live messages arriving while the room is unfocused).
    /// Removes any existing dividers first to avoid duplicates.
    pub fn insert_divider(&self) {
        self.remove_dividers();
        let divider = Self::make_divider_obj();
        self.imp().event_index.borrow_mut().insert("__unread_divider__".to_string(), divider.clone());
        self.imp().list_store().append(&divider);
        // Count starts at 1; window.rs calls set_unseen_count() as more messages arrive.
        self.show_unread_banner(1);
        self.scroll_to_bottom();
    }

    /// Update the "New messages" banner title with a fresh count.
    ///
    /// Called by window.rs each time `unseen_while_unfocused` increments so the
    /// banner always shows an accurate live count.
    pub fn set_unseen_count(&self, count: u32) {
        self.imp().unread_banner.set_title(&unread_label(count));
    }

    /// Append a single new message (used for live updates).
    /// `mark_as_new` tints the row blue and adds it to `new_message_objs` so
    /// `remove_dividers` can clear the tint when the user reads the room.
    pub fn append_message(&self, msg: &crate::matrix::MessageInfo, mark_as_new: bool) {
        // Dedup: if the event is already displayed (e.g. sync reconnect re-delivers it),
        // skip silently rather than appending a duplicate row.
        if !msg.event_id.is_empty() && self.imp().event_index.borrow().contains_key(&msg.event_id) {
            tracing::debug!("append_message: dedup skip event_id={} body={:?}", msg.event_id, body_preview(&msg.body));
            return;
        }
        // Echo dedup: if there is an unpatched local echo with the same body, patch
        // its event_id instead of appending a duplicate row.  This is the fallback
        // path for the race where is_self detection failed (e.g. user_id not yet set)
        // and the NewMessage handler took the non-self branch, bypassing the normal
        // patch_echo_event_id call.
        //
        // Guard: only attempt the O(n) scan for own messages — local echoes are
        // always from ourselves, so scanning for other senders is pure wasted work.
        // Without this guard, every message arriving on focus-return (potentially
        // dozens) triggers an O(list_store) backwards scan on the GTK thread.
        let my_id = self.imp().user_id.borrow().clone();
        if !msg.event_id.is_empty()
            && !my_id.is_empty()
            && msg.sender_id == my_id
            && self.patch_echo_event_id(&msg.body, &msg.event_id)
        {
            tracing::info!("append_message: echo patch fallback for event_id={} body={:?}", msg.event_id, body_preview(&msg.body));
            return;
        }
        // When the user is scrolled back reading history and the store is at cap,
        // skip the append — no event_index insert either.  The message will appear
        // on the next bg_refresh incremental pass; nothing is permanently lost.
        const MAX_STORE_SIZE: u32 = 400;
        if !msg.event_id.is_empty()
            && gio::prelude::ListModelExt::n_items(&self.imp().list_store()) >= MAX_STORE_SIZE
            && !self.is_near_bottom()
        {
            tracing::debug!(
                "append_message: list at cap ({}), not near bottom — deferring to bg_refresh",
                MAX_STORE_SIZE
            );
            return;
        }
        tracing::debug!("append_message: adding event_id={:?} sender={} body={:?}", msg.event_id, msg.sender_id, body_preview(&msg.body));
        let obj = Self::info_to_obj(msg, glib::DateTime::now_local().ok().as_ref());
        let eid = obj.event_id();
        if !eid.is_empty() {
            // Pre-insert into event_index immediately so subsequent dedup checks
            // within the same burst (before the flush idle fires) work correctly.
            self.imp().event_index.borrow_mut().insert(eid, obj.clone());
        } else {
            // Local echo (empty event_id). Bump the echo counter so
            // patch_echo_event_id's backwards scan is only taken when there
            // is actually an unpatched echo to look for.
            let imp = self.imp();
            imp.pending_echo_count.set(imp.pending_echo_count.get().saturating_add(1));
        }
        if mark_as_new {
            obj.set_is_new_message(true);
            self.imp().new_message_objs.borrow_mut().push(obj.clone());
        }
        // Defer the list_store mutation to an idle so bursts of NewMessage events
        // (e.g. 8 messages after a sync reconnect) produce one items_changed signal
        // instead of N separate ones.
        let imp = self.imp();
        imp.pending_appends.borrow_mut().push(obj);
        if !imp.append_flush_pending.get() {
            imp.append_flush_pending.set(true);
            let obj_weak = self.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(view) = obj_weak.upgrade() {
                    view.flush_pending_appends();
                }
            });
        }
    }

    /// Flush all pending appends to the list_store in a single splice call.
    /// Called from the idle scheduled by append_message().
    fn flush_pending_appends(&self) {
        const MAX_STORE_SIZE: u32 = 400;
        let imp = self.imp();
        imp.append_flush_pending.set(false);
        let objs: Vec<MessageObject> = imp.pending_appends.borrow_mut().drain(..).collect();
        if objs.is_empty() {
            return;
        }
        let store = imp.list_store();
        let n = store.n_items();
        // Single splice appends all queued objects — one items_changed signal.
        store.splice(n, 0, &objs);
        // Evict oldest messages from the front to maintain the cap.
        let new_n = store.n_items();
        if new_n > MAX_STORE_SIZE {
            let target = new_n - MAX_STORE_SIZE;
            let mut evict: u32 = 0;
            for i in 0..target {
                match store.item(i).and_downcast::<MessageObject>() {
                    Some(o) if !o.event_id().is_empty() => evict += 1,
                    _ => break,
                }
            }
            if evict > 0 {
                {
                    let mut idx = imp.event_index.borrow_mut();
                    for i in 0..evict {
                        if let Some(o) = store.item(i).and_downcast::<MessageObject>() {
                            idx.remove(&o.event_id());
                        }
                    }
                }
                store.splice(0, evict, &[] as &[MessageObject]);
                tracing::debug!("flush_pending_appends: evicted {} from front, store={}", evict, new_n - evict);
            }
        }
        if self.is_near_bottom() {
            self.scroll_to_bottom();
        }
    }

    /// Patch the event_id on a local echo message once the server confirms it.
    /// Searches backwards for a MessageObject with empty event_id and matching body.
    /// Returns true if an echo was found and patched, false otherwise.
    pub fn patch_echo_event_id(&self, echo_body: &str, event_id: &str) -> bool {
        let _g = crate::perf::scope_gt("patch_echo_event_id", 200);
        let imp = self.imp();
        // Previously guarded with `imp.pending_echo_count.get() == 0 →
        // return false` as a fast-path. The counter is approximate
        // (it can drift between its expected value and reality when
        // echoes are evicted by MAX_STORE_SIZE cap, when a message is
        // patched via MessageSent and then NewMessage arrives with a
        // different body variant, or when the user double-taps send)
        // and the short-circuit caused visible duplicate own-messages.
        // Correctness over speculative perf: always run the backwards
        // scan here — in a typical session it exits at the first
        // non-empty-event_id row near the tail (O(1) in practice).
        // Normalise the incoming body by stripping any Matrix reply fallback
        // ("> <@sender> quoted\n\nreal body"). The stored body on the local
        // echo MessageObject was stripped in info_to_obj; the server echo
        // still carries the fallback lines for reply messages. Without this
        // normalisation, reply echoes never match and the user sees their
        // local echo row + the server copy as two distinct rows (observed
        // intermittently across rooms — bug repro correlates with replies).
        // strip_reply_fallback is a no-op on bodies that don't start with
        // "> ", so this is cheap for the common non-reply case.
        let normalized = crate::widgets::message_row::strip_reply_fallback(echo_body);
        let echo_body: &str = &normalized;
        let n = gio::prelude::ListModelExt::n_items(&imp.list_store());
        tracing::debug!("patch_echo_event_id: searching n={} for body={:?} event_id={}", n, body_preview(echo_body), event_id);
        // Local echo search — echos have empty event_id and are near the end.
        // This is a backwards scan over items to process (finding the echo),
        // not a lookup of a known key — acceptable per the no-loops policy.
        for i in (0..n).rev() {
            let Some(obj) = gio::prelude::ListModelExt::item(&imp.list_store(), i) else { continue };
            let Some(msg) = obj.downcast_ref::<MessageObject>() else { continue };
            if msg.event_id().is_empty() && msg.body() == echo_body {
                tracing::info!("patch_echo_event_id: patched echo at pos={} body={:?} → {}", i, body_preview(echo_body), event_id);
                msg.set_event_id(event_id.to_string());
                // The echo now has a real id, so decrement the guard counter.
                imp.pending_echo_count.set(imp.pending_echo_count.get().saturating_sub(1));
                // Add to event_index now that it has a real ID.
                imp.event_index.borrow_mut().insert(event_id.to_string(), msg.clone());
                // Rebind the visible row so MessageRow's Rc cells get updated.
                let eid_str = event_id.to_string();
                let mut child = imp.list_view().first_child();
                while let Some(ref widget) = child {
                    if let Some(row) = Self::find_message_row(widget) {
                        // Echo row had empty event_id — match by checking if row
                        // still has empty event_id (it hasn't been reused yet).
                        if row.imp().event_id.borrow().is_empty()
                            || *row.imp().event_id.borrow() == eid_str
                        {
                            row.bind_message_object(msg, &self.row_context());
                            break;
                        }
                    }
                    child = widget.next_sibling();
                }
                return true;
            } else if msg.event_id().is_empty() {
                tracing::debug!("patch_echo_event_id: found echo at pos={} with DIFFERENT body={:?} (wanted {:?})", i, body_preview(&msg.body()), body_preview(echo_body));
            }
        }
        tracing::warn!("patch_echo_event_id: NO echo found for body={:?} event_id={}", body_preview(echo_body), event_id);
        false
    }

    /// Check if a message with the given event_id already exists in the timeline.
    pub fn has_event(&self, event_id: &str) -> bool {
        if event_id.is_empty() { return false; }
        // O(1) lookup via event_index.
        self.imp().event_index.borrow().contains_key(event_id)
    }

    /// True when the scroll position is within `threshold` pixels of the bottom.
    /// Used to decide whether incoming messages should auto-scroll the view.
    fn is_near_bottom(&self) -> bool {
        let sw = self.imp().scrolled_window();
        let vadj = sw.vadjustment();
        // At the very bottom: value == upper - page_size.
        // We use a 150 px slack so that a message arriving while the last row
        // is only partially visible still triggers auto-scroll.
        let slack = 150.0_f64;
        vadj.upper() - vadj.page_size() - vadj.value() < slack
    }

    fn scroll_to_bottom(&self) {
        let imp = self.imp();
        if gio::prelude::ListModelExt::n_items(&imp.list_store()) == 0 { return; }
        // Do NOT call list_view.scroll_to() — with GTK_LIST_SCROLL_NONE it has
        // undefined behaviour when the item is already partially visible and can
        // scroll the last row to the *top* of the viewport, overriding us.
        //
        // Instead, drive the vadjustment directly.  GTK layout runs at priority
        // RESIZE (-10); our idle at DEFAULT_IDLE (200) is guaranteed to fire
        // after the layout has updated vadj.upper for the new row.  We fire a
        // second idle inside the first to catch any second layout pass (e.g.
        // variable-height rows that get remeasured after realisation).
        //
        // Deduplication: when many messages arrive rapidly while near the bottom
        // (busy room), each one calls scroll_to_bottom().  Without dedup that
        // queues N outer idles + N inner idles — a flood of vadj.set_value()
        // calls that corrupts GTK's kinetic scroll gesture state, breaking
        // touchpad scrolling until the room is switched.  The pending flag
        // ensures at most one outer idle is in the GLib queue at a time; the
        // single inner idle (per outer) handles the second layout pass.
        if imp.scroll_to_bottom_pending.get() { return; }
        imp.scroll_to_bottom_pending.set(true);
        let sw = imp.scrolled_window().clone();
        let view_weak = self.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(view) = view_weak.upgrade() {
                view.imp().scroll_to_bottom_pending.set(false);
            }
            let vadj = sw.vadjustment();
            vadj.set_value(vadj.upper() - vadj.page_size());
            let sw2 = sw.clone();
            glib::idle_add_local_once(move || {
                let vadj = sw2.vadjustment();
                vadj.set_value(vadj.upper() - vadj.page_size());
            });
        });
    }

    /// Show or hide the banner that indicates a background refresh is in
    /// progress while stale cached messages are displayed.
    pub fn is_loading(&self) -> bool {
        self.imp().room_loading_overlay.is_visible()
    }

    pub fn set_refreshing(&self, refreshing: bool) {
        // Only show "Updating messages" when there are no messages yet (first
        // load of the room).  Background re-fetches triggered by SyncGap on
        // active rooms run silently so the banner doesn't flash every 30 s.
        let show = should_show_refresh_banner(refreshing, self.imp().messages_loaded.get());
        self.imp().refresh_banner.set_revealed(show);
    }

    /// Reveal the unread banner with an accurate count label and schedule
    /// auto-dismiss after 10 seconds.
    fn show_unread_banner(&self, count: u32) {
        let imp = self.imp();
        imp.unread_banner.set_title(&unread_label(count));
        imp.unread_banner.set_revealed(true);
        let view_weak = self.downgrade();
        glib::timeout_add_seconds_local_once(10, move || {
            let Some(view) = view_weak.upgrade() else { return };
            view.imp().unread_banner.set_revealed(false);
        });
    }
}

/// Pre-render a message body into Pango markup and compute a body hash for
/// O(1) cache checks in the bind callback.  Called once per MessageObject
/// Build a Pango anchor for a tombstone replacement room. The href is a
/// matrix.to URL so our existing `parse_matrix_uri` / `handle_matrix_link`
/// pipeline can route the click the same way it routes body-text room
/// links. The visible text is the human-readable name when we have one,
/// falling back to the room id so the user can still see and copy it.
fn tombstone_link_markup(room_id: &str, display: &str) -> String {
    // Percent-encode just the `!` → `%21` so the matrix.to fragment stays
    // canonical; the rest of the id (server part after `:`) is URL-safe.
    let href = if let Some(rest) = room_id.strip_prefix('!') {
        format!("https://matrix.to/#/%21{rest}")
    } else {
        format!("https://matrix.to/#/{room_id}")
    };
    let href_esc = glib::markup_escape_text(&href);
    let text_esc = glib::markup_escape_text(display);
    format!("<a href=\"{href_esc}\">{text_esc}</a>")
}

/// construction so the expensive work is paid at load time, not on every scroll.
///
/// Returns `(markup, hash)`:
/// - `markup`: ready-to-pass-to-set_markup() string, empty for code-block messages
///   (those still need dynamic body_box widget construction in bind).
/// - `hash`: FNV-1a hash of (body, formatted_body) — used as cache key in
///   MessageRow.last_body_hash to skip set_markup() when rebinding the same msg.
/// FNV-1a hash of (body, formatted_body). Used as the MessageRow bind cache
/// key so a row recycled for the same message skips set_markup. Cheap —
/// allocation-free O(n) on the input strings.
pub(crate) fn prerender_body_hash(body: &str, formatted_body: &str) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut hash = FNV_OFFSET;
    for b in body.bytes().chain(std::iter::once(0)).chain(formatted_body.bytes()) {
        hash = hash.wrapping_mul(FNV_PRIME) ^ b as u64;
    }
    hash
}

pub(crate) fn prerender_body(body: &str, formatted_body: &str) -> (String, u64) {
    let _g = crate::perf::scope_gt("prerender_body", 200);
    // FNV-1a hash — allocation-free O(n) on input strings.
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut hash = FNV_OFFSET;
    for b in body.bytes().chain(std::iter::once(0)).chain(formatted_body.bytes()) {
        hash = hash.wrapping_mul(FNV_PRIME) ^ b as u64;
    }

    let markup = if !formatted_body.is_empty() {
        // html_to_pango handles all HTML including <pre>/<code> blocks,
        // converting them to <tt>…</tt> inline Pango markup.  This means
        // rendered_markup is always non-empty for HTML messages, so bind
        // never falls through to the GtkSourceView dynamic-widget path.
        crate::markdown::html_to_pango(formatted_body)
    } else {
        let escaped = gtk::glib::markup_escape_text(body).to_string();
        crate::markdown::linkify_urls(&escaped)
    };
    (markup, hash)
}

/// FNV-1a hash of a string — allocation-free O(n).
pub(crate) fn fnv1a_str(s: &str) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    s.bytes().fold(FNV_OFFSET, |h, b| h.wrapping_mul(FNV_PRIME) ^ b as u64)
}

/// Pre-compute the reply indicator label string for a MessageObject.
/// Returns "Replying to Name", a local-part fallback, or empty for non-replies.
pub(crate) fn prerender_reply_label(is_reply: bool, reply_sender: &str, body: &str) -> String {
    if !is_reply { return String::new(); }
    if !reply_sender.is_empty() {
        return format!("Replying to {reply_sender}");
    }
    // Fallback: parse "> <@local:server> body" Matrix reply quote in the body.
    body.lines()
        .find(|l| l.starts_with("> <@"))
        .and_then(|l| l.strip_prefix("> <"))
        .and_then(|l| l.split('>').next())
        .and_then(|uid| uid.strip_prefix('@'))
        .and_then(|uid| uid.split(':').next())
        .map(|local| format!("Replying to {local}"))
        .unwrap_or_else(|| "Reply".to_string())
}

/// Pre-compute media button display strings (icon name, label, a11y label).
/// Returns ("", "", "") for messages without attachments.
pub(crate) fn prerender_media_display(
    media: &crate::matrix::MediaInfo,
) -> (String, String, String) {
    let kind_str = match media.kind {
        crate::matrix::MediaKind::Image => "Image",
        crate::matrix::MediaKind::Video => "Video",
        crate::matrix::MediaKind::Audio => "Audio",
        crate::matrix::MediaKind::File  => "File",
    };
    let icon = match media.kind {
        crate::matrix::MediaKind::Image => "image-x-generic-symbolic",
        crate::matrix::MediaKind::Video => "video-x-generic-symbolic",
        crate::matrix::MediaKind::Audio => "audio-x-generic-symbolic",
        crate::matrix::MediaKind::File  => "text-x-generic-symbolic",
    };
    let size_str = media.size
        .map(|s| {
            if s > 1_048_576 { format!(" ({:.1} MB)", s as f64 / 1_048_576.0) }
            else if s > 1024  { format!(" ({:.0} KB)", s as f64 / 1024.0) }
            else               { format!(" ({s} B)") }
        })
        .unwrap_or_default();
    let label   = format!("{}{size_str}", media.filename);
    let a11y    = format!("{kind_str}: {}{size_str}", media.filename);
    (icon.to_string(), label, a11y)
}

/// Format the unread banner title for `n` new messages.
pub(crate) fn unread_label(n: u32) -> String {
    match n {
        1 => "1 new message".to_string(),
        _ => format!("{n} new messages"),
    }
}

/// Pure helper: resolve the effective `room_unread_count` when `set_room_meta` is called.
///
/// Once messages are loaded (initial load done), a bg_refresh that returns
/// `new_count = 0` must NOT erase the divider — it means the 15-second
/// read-receipt timer fired while the refresh was still in flight.  Keep
/// the existing count so the divider stays until the user explicitly reads
/// (sends a message → `dismiss_unread`, or switches rooms → `clear`).
///
/// Rules:
///   - Initial load (`!messages_loaded`): always use `new_count` (whatever
///     the server reported, including 0 for a room the user has read).
///   - Background refresh (`messages_loaded`): use `max(current, new_count)`.
///     This preserves the count if the timer fired mid-refresh (new=0 < current),
///     and still updates it if new messages arrived (new > current).
pub(crate) fn effective_unread_count(messages_loaded: bool, current: u32, new_count: u32) -> u32 {
    if messages_loaded { current.max(new_count) } else { new_count }
}

/// Pure binary-search helper: given `n` items and a function `ts_at(i)` that
/// returns the timestamp at position `i`, return the insertion index for `ts`
/// such that timestamps remain sorted oldest→newest.  Duplicates are inserted
/// AFTER existing equal timestamps (stable ordering).
///
/// Extracted from `sorted_insert_pos` so the algorithm can be unit-tested
/// without a live GtkListStore.
pub(crate) fn sorted_insert_pos_in<F>(n: u32, ts: u64, ts_at: F) -> u32
where
    F: Fn(u32) -> u64,
{
    let mut lo = 0u32;
    let mut hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if ts_at(mid) <= ts {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Normalize the current scroll position to a [0,1] fraction for save/restore.
/// `upper` and `page_size` come from `vadjustment()`.
pub(crate) fn scroll_save_frac(value: f64, upper: f64, page_size: f64) -> f64 {
    let max = (upper - page_size).max(1.0);
    (value / max).clamp(0.0, 1.0)
}

/// Restore a saved scroll fraction to an absolute `vadjustment` value.
pub(crate) fn scroll_restore_value(frac: f64, upper: f64, page_size: f64) -> f64 {
    let max = (upper - page_size).max(0.0);
    frac * max
}

/// Pure helper: should `set_messages` skip the splice when the payload is empty?
/// An empty placeholder from a bg_refresh timeout must not clear messages that
/// are already displayed — only the first-load path may process an empty slice.
pub(crate) fn should_skip_empty_splice(messages_empty: bool, first_load: bool) -> bool {
    messages_empty && !first_load
}

/// After a room switch, should the view show the loading spinner?
///
/// Only return visits where messages were previously loaded get to show
/// messages immediately.  Everything else shows the spinner and waits for
/// RoomMessages to arrive via bg_refresh.
///
/// This is the invariant that ensures:
///  - First visit → spinner (cold cache, wait for data)
///  - Return visit, was_loaded=true  → messages immediately (O(1) stack switch)
///  - Return visit, was_loaded=false → spinner (previous visit didn't finish loading)
pub(crate) fn should_show_loading_after_switch(
    is_return_visit: bool,
    was_loaded: bool,
) -> bool {
    !(is_return_visit && was_loaded)
}

/// How many items to evict from the front of the store after an append.
///
/// Scans `event_ids[0..target]` and stops before the first unconfirmed echo
/// (empty event_id). Echoes are in-flight sent messages; removing them would
/// make the message vanish and reappear when the server confirms it.
///
/// `event_ids` — event IDs of the store items in order (oldest first).
/// `max_size`  — the cap to maintain (typically MAX_STORE_SIZE = 400).
pub(crate) fn front_evict_count(event_ids: &[&str], max_size: usize) -> usize {
    let n = event_ids.len();
    if n <= max_size { return 0; }
    let target = n - max_size;
    let mut count = 0;
    while count < target {
        if event_ids[count].is_empty() { break; }
        count += 1;
    }
    count
}


/// Pure helper: should a live-appended message be tinted as "new"?
/// Only tint when the window is not focused AND the message is not from
/// the current user — own messages never need a "new" highlight.
#[cfg(test)]
pub(crate) fn should_mark_as_new(window_focused: bool) -> bool {
    !window_focused
}

/// Pure helper: should a divider-placement loop mark this message as new?
/// Skips messages sent by the local user so their own messages are never
/// highlighted as unread, whether on initial room load or incremental update.
pub(crate) fn divider_should_mark(sender_id: &str, my_id: &str) -> bool {
    my_id.is_empty() || sender_id != my_id
}

/// Slide the "New messages" divider position past the user's last own-message.
///
/// The server's `m.fully_read` marker can lag behind the user's actual
/// engagement — they've sent messages in the room but the 15-second read
/// receipt timer hadn't fired before the next bg_refresh landed, so the
/// server-reported fully_read_event_id points to an earlier event. Using
/// that raw marker to place the divider puts it above the user's own
/// replies, which is confusing: any time you see your own message above a
/// "New messages" bar it is a bug. Treat any own-message as hard evidence
/// that the user has read everything up to and including it.
///
/// Returns the clamped position. Returns `n` (past-the-end) when the user's
/// own-message is the last message in the window — meaning they are caught
/// up and no divider should be drawn; callers check for `>= n` and skip.
pub(crate) fn clamp_divider_past_own_messages(
    list_store: &gio::ListStore,
    nominal_pos: u32,
    my_id: &str,
) -> u32 {
    if my_id.is_empty() {
        return nominal_pos;
    }
    let n = gio::prelude::ListModelExt::n_items(list_store);
    // Scan backward from the end — the first own-message we hit is the
    // user's most recent engagement. Short-circuit on the first hit since
    // anything earlier is irrelevant to the clamp.
    for i in (nominal_pos..n).rev() {
        if let Some(obj) = gio::prelude::ListModelExt::item(list_store, i)
            .and_downcast::<MessageObject>()
        {
            if obj.sender_id() == my_id {
                return (i + 1).min(n);
            }
        }
    }
    nominal_pos
}

/// Pure helper: given a list of pending echo bodies (messages appended locally
/// with an empty event_id) and an incoming server message body, returns true if
/// the server message is a duplicate of a pending echo and should be skipped.
///
/// Used in the bg_refresh incremental path to suppress the race where a server
/// confirmation arrives before MessageSent has patched the local echo's event_id.
#[cfg(test)]
pub(crate) fn is_echo_duplicate(pending_echo_bodies: &[&str], incoming_body: &str) -> bool {
    pending_echo_bodies.contains(&incoming_body)
}

/// Pure helper: should `append_message` skip adding this event because it is
/// already displayed?  Mirrors the dedup guard at the top of `append_message`.
///
/// Returns `true` (skip) when:
///   - `event_id` is non-empty (i.e. not a local echo), AND
///   - the event is already in the timeline's event_index.
///
/// Local echoes (empty event_id) always pass through so they can be appended
/// as optimistic placeholders before the server assigns a real ID.
#[cfg(test)]
pub(crate) fn should_skip_append(event_id: &str, in_index: bool) -> bool {
    !event_id.is_empty() && in_index
}

/// Pure helper: given a slice of event_ids (ordered oldest→newest, matching
/// list_store indices) and a `max_size` cap, return the exclusive upper bound
/// of the range that should be evicted: [max_size, boundary).
///
/// Items at index ≥ max_size are candidates for eviction, but we stop before
/// the first item whose event_id is empty — that is an unconfirmed echo that
/// must not be removed from the timeline.
///
/// Returns `max_size` (evict nothing) when:
///   - n ≤ max_size (no overflow), or
///   - the first overflow item is already an echo.
///
/// Returns `n` when no echo exists in the overflow range (evict all overflow).
#[cfg(test)]
pub(crate) fn compute_eviction_boundary(event_ids: &[&str], max_size: usize) -> usize {
    let n = event_ids.len();
    if n <= max_size {
        return max_size; // nothing to evict
    }
    let mut boundary = max_size;
    while boundary < n {
        if event_ids[boundary].is_empty() {
            break; // stop before echo
        }
        boundary += 1;
    }
    boundary
}

/// Pure helper: should the "Updating messages" banner be shown?
/// The banner is only useful on first load (empty timeline).  Background
/// re-fetches (SyncGap) run silently to avoid flashing the banner every
/// 30 s in active rooms.
pub(crate) fn should_show_refresh_banner(refreshing: bool, messages_loaded: bool) -> bool {
    refreshing && !messages_loaded
}

/// Pure helper: given an ordered slice of event IDs and room metadata, return
/// the insert position (0-based) for the "New messages" divider, or `None` if
/// no divider should be placed (unread_count == 0).
///
/// Rules (mirrors the GTK methods above so they can be unit-tested without GTK):
///   A) `fully_read` is Some, the event is in the list, AND it is NOT the last
///      item → insert immediately after it.
///   B) `fully_read` is Some but the event IS the last item (new messages are
///      beyond the current window) → fall back to count-based placement.
///   C) `fully_read` is Some but the event is not in the list at all
///      (paged out) → fall back to count-based placement.
///   D) `fully_read` is None → count-based placement.
///
/// Count-based placement: `len.saturating_sub(unread_count)`.
///
/// # Deciding when to insert (see `divider_decision`)
///
/// `compute_divider_pos` only answers *where*.  Call `divider_decision` to
/// also answer *whether* — it gates on `divider_present` and `unread_count`
/// and returns the banner count alongside the insert position.
#[cfg(test)]
pub(crate) fn compute_divider_pos(
    event_ids: &[&str],
    fully_read: Option<&str>,
    unread_count: u32,
) -> Option<usize> {
    if unread_count == 0 {
        return None;
    }
    let n = event_ids.len();
    if let Some(eid) = fully_read {
        if let Some(pos) = event_ids.iter().position(|&id| id == eid) {
            let insert_pos = pos + 1;
            if insert_pos < n {
                // Case A: event found and is not the last item.
                return Some(insert_pos);
            }
            // Case B: event is the last item — fall through.
        }
        // Case C: event not found — fall through.
    }
    // Cases B, C, D: count-based.
    Some(n.saturating_sub(unread_count as usize))
}

/// Combined "where AND how many" divider decision.
///
/// Returns `Some((insert_position, banner_count))` when a divider should be
/// inserted, or `None` when it should be skipped.  The caller inserts the
/// divider object at `insert_position` and passes `banner_count` to
/// `show_unread_banner`.
///
/// * `divider_present` — pass `true` when a divider is already in the list.
///   The function returns `None` in that case (no duplicate).
/// * `banner_count` is derived from `insert_position`, not from `unread_count`
///   directly, so it reflects the actual number of messages after the divider
///   in the current window rather than the (possibly stale) server count.
#[cfg(test)]
pub(crate) fn divider_decision(
    event_ids: &[&str],
    fully_read: Option<&str>,
    unread_count: u32,
    divider_present: bool,
) -> Option<(usize, u32)> {
    if divider_present {
        return None;
    }
    let insert_pos = compute_divider_pos(event_ids, fully_read, unread_count)?;
    let n = event_ids.len();
    let banner_count = (n.saturating_sub(insert_pos)) as u32;
    Some((insert_pos, banner_count.max(1)))
}

/// Pure helper: decide the resulting prev_batch_token after a set_messages
/// incremental call.
///
/// Rule: accept the server token only when ours was cleared by a room switch
/// (None).  If the user has already paginated back via prepend_messages their
/// deeper token is preserved so pagination can continue from where they are.
#[cfg(test)]
pub(crate) fn incremental_prev_batch(
    current: Option<&str>,
    incoming: Option<&str>,
) -> Option<String> {
    if current.is_none() {
        incoming.map(str::to_string)
    } else {
        current.map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_divider_pos, divider_decision, divider_should_mark, front_evict_count,
        incremental_prev_batch, is_echo_duplicate, sorted_insert_pos_in, unread_label,
        effective_unread_count, prerender_body, prerender_reply_label, fnv1a_str,
        should_mark_as_new, should_show_refresh_banner, should_skip_empty_splice,
    };

    #[test]
    fn prerender_plain_text_linkifies() {
        let (markup, hash) = prerender_body("check https://example.com for info", "");
        assert!(markup.contains("<a href="), "plain text should have linkified URL");
        assert_ne!(hash, 0, "hash should be non-zero");
    }

    #[test]
    fn prerender_html_converts_to_pango() {
        let (markup, hash) = prerender_body("hello", "<b>hello</b>");
        assert!(markup.contains("<b>") || markup.contains("bold") || !markup.is_empty(),
            "HTML should produce non-empty pango markup");
        assert_ne!(hash, 0);
    }

    #[test]
    fn prerender_code_block_returns_tt_markup() {
        let (markup, hash) = prerender_body("code", "<pre><code>x = 1</code></pre>");
        assert!(!markup.is_empty(), "code blocks should produce <tt> Pango markup");
        assert!(markup.contains("x = 1"), "markup should include code content");
        assert_ne!(hash, 0);
    }

    #[test]
    fn prerender_hash_differs_for_different_bodies() {
        let (_, h1) = prerender_body("hello world", "");
        let (_, h2) = prerender_body("goodbye world", "");
        assert_ne!(h1, h2, "different bodies must produce different hashes");
    }

    #[test]
    fn prerender_hash_differs_for_body_vs_formatted() {
        let (_, h1) = prerender_body("hello", "");
        let (_, h2) = prerender_body("hello", "<b>hello</b>");
        assert_ne!(h1, h2, "body vs formatted_body should produce different hashes");
    }

    /// Build a timeline split at `read_count`: the first `read_count` events are
    /// read, the rest are new.  Returns `(all_ids, fully_read_id, unread_count,
    /// expected_divider_pos)` ready to feed into `compute_divider_pos`.
    ///
    /// `expected_divider_pos` is the index of the first new message (i.e. the
    /// position a divider inserted *before* it would separate read from new).
    #[test]
    fn reply_label_with_known_sender() {
        assert_eq!(prerender_reply_label(true, "Alice", ""), "Replying to Alice");
    }

    #[test]
    fn reply_label_empty_when_not_reply() {
        assert_eq!(prerender_reply_label(false, "Alice", "body"), "");
    }

    #[test]
    fn reply_label_fallback_from_body() {
        let body = "> <@alice:matrix.org> original message\nthe reply";
        let label = prerender_reply_label(true, "", body);
        assert_eq!(label, "Replying to alice");
    }

    #[test]
    fn fnv1a_str_is_stable() {
        let h1 = fnv1a_str("hello");
        let h2 = fnv1a_str("hello");
        assert_eq!(h1, h2);
        assert_ne!(fnv1a_str("hello"), fnv1a_str("world"));
    }

    fn make_timeline(
        total: usize,
        read_count: usize,
    ) -> (Vec<String>, Option<String>, u32, Option<usize>) {
        let events: Vec<String> = (0..total).map(|i| format!("$ev{i}")).collect();
        let unread = (total.saturating_sub(read_count)) as u32;
        let fully_read = if read_count > 0 {
            Some(events[read_count - 1].clone())
        } else {
            None
        };
        // If all messages are read, no divider; otherwise divider at the first new msg.
        let expected = if unread == 0 { None } else { Some(read_count) };
        (events, fully_read, unread, expected)
    }

    /// Convenience: run `compute_divider_pos` from a `make_timeline` result and
    /// assert the output matches the expected position.
    fn assert_divider(total: usize, read_count: usize) {
        let (events, fully_read, unread, expected) = make_timeline(total, read_count);
        let ids: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
        let got = compute_divider_pos(&ids, fully_read.as_deref(), unread);
        assert_eq!(
            got, expected,
            "total={total} read={read_count}: wanted divider at {expected:?}, got {got:?}"
        );
    }

    // ── Basic arrive scenarios ───────────────────────────────────────────────

    #[test]
    fn one_new_message_arrives() {
        // 9 read, 1 new → divider before the last message.
        assert_divider(10, 9);
    }

    #[test]
    fn three_new_messages_arrive() {
        // 7 read, 3 new → divider before the 8th message (index 7).
        assert_divider(10, 7);
    }

    #[test]
    fn all_messages_are_new() {
        // No fully_read at all (read_count=0) → count-based at index 0.
        assert_divider(5, 0);
    }

    #[test]
    fn all_messages_are_read_no_divider() {
        // All read → no divider.
        assert_divider(8, 8);
    }

    #[test]
    fn single_new_message_in_single_item_list() {
        // Only one message in the window and it's new.
        assert_divider(1, 0);
    }

    // ── Fully-read event present but in different positions ──────────────────

    #[test]
    fn fully_read_is_first_message_rest_are_new() {
        // Alice read only the very first message; 9 more arrived since.
        // fully_read = $ev0, so divider at index 1.
        assert_divider(10, 1);
    }

    #[test]
    fn fully_read_is_second_to_last() {
        // fully_read = second-to-last, 1 new at the end.
        assert_divider(6, 5);
    }

    // ── Fallback scenarios (fully_read outside current window) ───────────────

    #[test]
    fn fully_read_event_not_in_window_falls_back_to_count() {
        // The server says fully_read = "$old_event" which has scrolled out of
        // the current 25-message window.  We must fall back to count-based.
        //
        // Window: $ev0..$ev9 (10 messages), fully_read = "$old_event" (absent),
        // server says 3 unread → divider at 10 - 3 = 7.
        let events: Vec<String> = (0..10).map(|i| format!("$ev{i}")).collect();
        let ids: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
        let got = compute_divider_pos(&ids, Some("$old_event"), 3);
        assert_eq!(got, Some(7), "count-based fallback: expected index 7");
    }

    #[test]
    fn fully_read_is_last_in_window_new_messages_not_loaded_yet() {
        // The user read up to the last event in the current batch.  The actual
        // new messages haven't been fetched yet (they'll arrive via bg_refresh).
        // fully_read = last event → insert_divider_after_event returns false →
        // fall back to count-based.
        //
        // Window: $ev0..$ev4, fully_read = $ev4 (last), 2 new → 5 - 2 = 3.
        let events: Vec<&str> = vec!["$ev0", "$ev1", "$ev2", "$ev3", "$ev4"];
        let got = compute_divider_pos(&events, Some("$ev4"), 2);
        assert_eq!(got, Some(3), "last-item fallback: expected index 3");
    }

    // ── Edge cases ───────────────────────────────────────────────────────────

    #[test]
    fn no_unreads_never_places_divider() {
        let events = ["$a", "$b", "$c"];
        assert_eq!(compute_divider_pos(&events, Some("$a"), 0), None);
        assert_eq!(compute_divider_pos(&events, None, 0), None);
    }

    #[test]
    fn unread_count_larger_than_window_clamps_to_start() {
        // Server reports 20 unread but only 5 messages in the window →
        // divider at index 0 (show everything as new).
        let events: Vec<String> = (0..5).map(|i| format!("$ev{i}")).collect();
        let ids: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
        let got = compute_divider_pos(&ids, None, 20);
        assert_eq!(got, Some(0));
    }

    #[test]
    fn empty_window_with_unreads_gives_position_zero() {
        // Room has unread messages but they haven't loaded yet.
        let ids: &[&str] = &[];
        assert_eq!(compute_divider_pos(ids, None, 5), Some(0));
    }

    #[test]
    fn large_window_few_unreads() {
        // 50-message window (typical batch size), last 2 are new.
        assert_divider(50, 48);
    }

    #[test]
    fn large_window_many_unreads() {
        // 50-message window, user hasn't opened in a while — 20 new messages.
        assert_divider(50, 30);
    }

    #[test]
    fn fully_read_consistent_with_first_new_event() {
        // Verify that the event-ID path and the count path agree when both are
        // available and the server counts match the position of fully_read.
        // 10 messages, 4 new — fully_read = $ev5 (index 5) → divider at 6.
        let (events, fully_read, unread, expected) = make_timeline(10, 6);
        let ids: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
        // Event-ID path.
        let via_event_id = compute_divider_pos(&ids, fully_read.as_deref(), unread);
        // Count-only path (no fully_read).
        let via_count = compute_divider_pos(&ids, None, unread);
        // Both must land at the same index since count matches actual position.
        assert_eq!(via_event_id, expected);
        assert_eq!(via_count, expected);
    }

    // ── Banner label formatting ───────────────────────────────────────────────

    #[test]
    fn banner_label_singular() {
        assert_eq!(unread_label(1), "1 new message");
    }

    #[test]
    fn banner_label_plural() {
        assert_eq!(unread_label(2), "2 new messages");
        assert_eq!(unread_label(99), "99 new messages");
    }

    #[test]
    fn banner_label_zero_is_plural_form() {
        // Zero unreads is a degenerate case (banner shouldn't be shown) but
        // the label function should not panic.
        assert_eq!(unread_label(0), "0 new messages");
    }

    // ── divider_decision: when to insert and how many to show ────────────────

    /// Helper: build an event-ID vec and run `divider_decision`.
    fn decision(total: usize, read_count: usize, divider_present: bool) -> Option<(usize, u32)> {
        let events: Vec<String> = (0..total).map(|i| format!("$ev{i}")).collect();
        let ids: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
        let unread = (total.saturating_sub(read_count)) as u32;
        let fully_read = if read_count > 0 {
            Some(events[read_count - 1].clone())
        } else {
            None
        };
        divider_decision(&ids, fully_read.as_deref(), unread, divider_present)
    }

    // ── Initial room load (first_load=true) ──────────────────────────────────

    #[test]
    fn first_load_3_unreads_returns_correct_pos_and_count() {
        // 10 messages, 7 read, 3 new → divider at 7, banner "3 new messages".
        let result = decision(10, 7, false);
        assert_eq!(result, Some((7, 3)));
    }

    #[test]
    fn first_load_no_unreads_returns_none() {
        // All read → no divider.
        assert_eq!(decision(10, 10, false), None);
    }

    #[test]
    fn first_load_all_new_returns_pos_zero_full_count() {
        // Opened a room with zero previous reads → divider at 0, count=5.
        assert_eq!(decision(5, 0, false), Some((0, 5)));
    }

    // ── bg_refresh path: divider already present ─────────────────────────────

    #[test]
    fn divider_not_duplicated_when_already_present() {
        // A divider is in the list from the initial disk-cache load.  bg_refresh
        // fetches the same window → skip insertion.
        assert_eq!(decision(10, 7, true), None);
    }

    #[test]
    fn divider_present_even_with_unreads_returns_none() {
        // Even if unread_count changed, don't insert a second divider.
        assert_eq!(decision(10, 5, true), None);
    }

    // ── bg_refresh path: divider absent because splice removed it ────────────
    //
    // This is the regression-prone path: disk cache had unread=0 (no divider),
    // server delivers fresh messages with unread > 0.  After the splice the
    // divider sentinel is gone from event_index, so divider_present=false.
    // divider_decision must produce an insertion.

    #[test]
    fn bg_refresh_brings_new_messages_divider_inserted() {
        // Disk loaded 10 messages with unread=0 → no divider.
        // Server delivers 13 messages, unread=3.
        // After splice: 13 events, divider_present=false (splice cleared it).
        // Expected: insert at 10, banner count = 3.
        let events: Vec<String> = (0..13).map(|i| format!("$ev{i}")).collect();
        let ids: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
        // fully_read = $ev9 (last message user read before new ones arrived).
        let result = divider_decision(&ids, Some("$ev9"), 3, false);
        assert_eq!(result, Some((10, 3)));
    }

    #[test]
    fn bg_refresh_fully_read_not_in_window_falls_back_to_count() {
        // Server delivers 50 fresh messages, fully_read is from an older
        // window that has scrolled out.  Count-based fallback: 50 - 5 = 45.
        let events: Vec<String> = (0..50).map(|i| format!("$ev{i}")).collect();
        let ids: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
        let result = divider_decision(&ids, Some("$old_event"), 5, false);
        assert_eq!(result, Some((45, 5)));
    }

    #[test]
    fn bg_refresh_count_larger_than_window_clamps_to_start() {
        // Server reports 20 unread but only 5 messages visible → divider at 0.
        let events: Vec<String> = (0..5).map(|i| format!("$ev{i}")).collect();
        let ids: Vec<&str> = events.iter().map(|s| s.as_str()).collect();
        let result = divider_decision(&ids, None, 20, false);
        assert_eq!(result, Some((0, 5)));
    }

    // ── Banner count accuracy ─────────────────────────────────────────────────

    #[test]
    fn banner_count_matches_messages_after_divider_not_server_count() {
        // Server says unread=5 but the current window only has 3 messages
        // after the fully_read marker.  Banner should show 3, not 5, because
        // that's what's actually visible.
        let events = ["$ev0", "$ev1", "$ev2", "$ev3", "$ev4"];
        // fully_read = $ev1 (index 1), insert at 2, messages after = 3 ($ev2,$ev3,$ev4).
        let result = divider_decision(&events, Some("$ev1"), 5, false);
        assert_eq!(result, Some((2, 3)));
    }

    #[test]
    fn banner_count_is_at_least_one() {
        // Degenerate: 1-item list, fully_read is outside the window, unread=1.
        // insert_pos = 1 - 1 = 0 (count-based), banner_count = 1 - 0 = 1.
        let events = ["$ev0"];
        let result = divider_decision(&events, None, 1, false);
        assert_eq!(result, Some((0, 1)));
    }

    #[test]
    fn empty_list_with_unreads_returns_position_zero() {
        let result = divider_decision(&[], None, 5, false);
        assert_eq!(result, Some((0, 1))); // 0 - 0 = 0 items after divider, but max(0, 1) = 1
    }

    // ── should_skip_empty_splice ─────────────────────────────────────────────
    //
    // Scenario: bg_refresh timed out and sent an empty RoomMessages placeholder.
    // set_messages must not clear the timeline when messages are already loaded.

    #[test]
    fn empty_splice_skipped_after_first_load() {
        // Not the first load and payload is empty → skip the splice.
        assert!(should_skip_empty_splice(true, false),
            "empty placeholder after first_load must be skipped");
    }

    #[test]
    fn empty_splice_processed_on_first_load() {
        // First load: even an empty payload goes through (nothing to lose).
        assert!(!should_skip_empty_splice(true, true),
            "empty payload on first_load must NOT be skipped");
    }

    #[test]
    fn non_empty_splice_never_skipped() {
        // A real payload with messages is always processed regardless of load state.
        assert!(!should_skip_empty_splice(false, false));
        assert!(!should_skip_empty_splice(false, true));
    }

    // ── should_mark_as_new ───────────────────────────────────────────────────
    //
    // Scenario: live message arrives for the current room.  The row should be
    // tinted blue only when the window is not focused (user is away).

    #[test]
    fn mark_as_new_when_window_unfocused() {
        assert!(should_mark_as_new(false),
            "unfocused window → incoming message should be marked new");
    }

    #[test]
    fn no_mark_as_new_when_window_focused() {
        assert!(!should_mark_as_new(true),
            "focused window → user is watching; no new-message tint needed");
    }

    // ── should_show_refresh_banner ───────────────────────────────────────────
    //
    // Scenario: "Updating messages" banner must only appear on genuine first
    // load.  Background re-fetches (SyncGap-triggered) must run silently to
    // avoid the banner flashing every 30 s in active rooms.

    #[test]
    fn refresh_banner_shown_on_first_load() {
        assert!(should_show_refresh_banner(true, false),
            "refreshing with no messages loaded → show the banner");
    }

    #[test]
    fn refresh_banner_hidden_when_messages_already_loaded() {
        assert!(!should_show_refresh_banner(true, true),
            "background re-fetch while messages are displayed → banner stays hidden");
    }

    #[test]
    fn refresh_banner_hidden_when_not_refreshing() {
        assert!(!should_show_refresh_banner(false, false));
        assert!(!should_show_refresh_banner(false, true));
    }

    // ── effective_unread_count ───────────────────────────────────────────────
    //
    // Scenario: user enters GNOME OS with 2 unread messages.  The 15-second
    // read-receipt timer fires while bg_refresh is still in flight.  The SDK
    // resets its counter to 0 before bg_refresh calls unread_notification_counts().
    // The bg_refresh RoomMessages therefore carries unread_count=0.
    //
    // Without the fix, set_room_meta would overwrite room_unread_count with 0
    // and set_messages would not place the divider — the user never sees the
    // "New messages" line or the blue tint on those 2 messages.
    //
    // With the fix, effective_unread_count preserves the original count so the
    // divider stays until dismiss_unread() or clear() explicitly resets it.

    #[test]
    fn initial_load_sets_count_from_server() {
        // First time entering the room: use whatever the server reports.
        assert_eq!(effective_unread_count(false, 0, 2), 2,
            "initial load must adopt server count");
    }

    #[test]
    fn initial_load_zero_stays_zero() {
        // Room was already read — server reports 0, no divider needed.
        assert_eq!(effective_unread_count(false, 0, 0), 0,
            "already-read room must stay at 0");
    }

    #[test]
    fn bg_refresh_preserves_count_when_timer_fired_mid_refresh() {
        // Race condition: read timer fired → SDK reset count to 0 → bg_refresh
        // sends unread_count=0 while the user hasn't scrolled to new messages.
        assert_eq!(effective_unread_count(true, 2, 0), 2,
            "bg_refresh with count=0 must not erase existing unread count");
    }

    #[test]
    fn bg_refresh_updates_count_when_new_messages_arrived() {
        // A new message arrived between initial load and bg_refresh.
        assert_eq!(effective_unread_count(true, 2, 3), 3,
            "bg_refresh may raise the count if more messages arrived");
    }

    #[test]
    fn bg_refresh_keeps_count_when_unchanged() {
        // Normal case: bg_refresh finishes before timer, same count.
        assert_eq!(effective_unread_count(true, 2, 2), 2,
            "identical counts from bg_refresh must not change anything");
    }

    // ── scenario: enter room with N unreads ──────────────────────────────────
    //
    // These tests verify where the divider lands and how many messages are
    // tinted given a concrete timeline.  `divider_decision` is the entry-point
    // used by both the initial set_messages call and every bg_refresh splice.

    #[test]
    fn scenario_2_unreads_in_10_message_room() {
        // GNOME OS: 10 messages in window, 2 unread, no fully_read marker.
        let evs: Vec<String> = (0..10).map(|i| format!("$ev{i}")).collect();
        let ids: Vec<&str> = evs.iter().map(|s| s.as_str()).collect();
        let (pos, tinted) = divider_decision(&ids, None, 2, false).unwrap();
        assert_eq!(pos, 8,   "divider must be before the 9th message (index 8)");
        assert_eq!(tinted, 2, "exactly 2 messages after the divider must be tinted");
    }

    #[test]
    fn scenario_fully_read_marker_places_divider_precisely() {
        // Server knows the last-read event; divider goes right after it.
        let evs = ["$a", "$b", "$c", "$d", "$e"];
        let (pos, tinted) = divider_decision(&evs, Some("$c"), 2, false).unwrap();
        assert_eq!(pos, 3,   "divider after $c → position 3");
        assert_eq!(tinted, 2, "$d and $e are tinted");
    }

    #[test]
    fn scenario_fully_read_is_last_falls_back_to_count() {
        // fully_read points at the last message in the window — unread messages
        // are beyond the current batch.  Fall back to count-based placement.
        let evs = ["$a", "$b", "$c"];
        let (pos, tinted) = divider_decision(&evs, Some("$c"), 2, false).unwrap();
        assert_eq!(pos, 1,   "count-based fallback: 2 from end → position 1");
        assert_eq!(tinted, 2);
    }

    #[test]
    fn scenario_all_messages_new_no_fully_read() {
        // Cold cache: no fully_read marker, all 5 messages are new.
        let evs: Vec<String> = (0..5).map(|i| format!("$ev{i}")).collect();
        let ids: Vec<&str> = evs.iter().map(|s| s.as_str()).collect();
        let (pos, tinted) = divider_decision(&ids, None, 5, false).unwrap();
        assert_eq!(pos, 0, "divider at start — every message is new");
        assert_eq!(tinted, 5);
    }

    #[test]
    fn scenario_zero_unreads_no_divider() {
        // Room is fully read — no divider should be inserted.
        let evs = ["$a", "$b", "$c"];
        assert!(divider_decision(&evs, None, 0, false).is_none(),
            "no divider when unread_count is 0");
    }

    #[test]
    fn scenario_bg_refresh_after_timer_preserves_tinted_range() {
        // Full scenario: initial load set unread_count=2.  Timer fires.
        // bg_refresh arrives with unread_count=0 — effective_unread_count
        // preserves 2 → divider_decision still places at the right position.
        let preserved = effective_unread_count(true, 2, 0); // bg_refresh sees 0
        let evs: Vec<String> = (0..10).map(|i| format!("$ev{i}")).collect();
        let ids: Vec<&str> = evs.iter().map(|s| s.as_str()).collect();
        let result = divider_decision(&ids, None, preserved, false);
        assert_eq!(result, Some((8, 2)),
            "even after timer fires, divider must still tint the original 2 messages");
    }

    // ── Per-room state save/restore (regression for room-switch performance) ──

    #[test]
    fn per_room_state_save_restore_prevents_spurious_splice() {
        // Simulate the logic in clear() + set_messages() for a return visit.
        // When returning to a room, the saved event_index means set_messages()
        // detects "nothing changed" and skips the O(N) splice.

        use std::collections::HashMap;

        let mut saved_indices: HashMap<String, HashMap<String, String>> = HashMap::new();

        // First visit to room_a: load 3 messages.
        let room_a = "!roomA:example.org";
        let idx_a: HashMap<String, String> = [
            ("$ev1".to_string(), "body1".to_string()),
            ("$ev2".to_string(), "body2".to_string()),
            ("$ev3".to_string(), "body3".to_string()),
        ].into_iter().collect();

        // Switch away — save room_a's event_index.
        saved_indices.insert(room_a.to_string(), idx_a.clone());

        // Visit room_b.
        let room_b = "!roomB:example.org";
        let _idx_b: HashMap<String, String> = HashMap::new(); // empty for this test

        // Switch back to room_a — restore its event_index.
        let restored = saved_indices.remove(room_a).unwrap_or_default();
        assert_eq!(restored.len(), 3, "event_index should be restored for room_a");

        // Simulate the has_new check: last incoming message is $ev3, which IS in index.
        let last_incoming_id = "$ev3";
        let has_new = !restored.contains_key(last_incoming_id);
        assert!(!has_new, "bg_refresh should detect no new messages on return visit");

        // Verify room_b (first visit) starts with empty index.
        let _ = room_b; // used above
        let fresh_idx: HashMap<String, String> = saved_indices
            .remove(room_b).unwrap_or_default();
        assert!(fresh_idx.is_empty(), "first visit to room_b has empty event_index");
    }

    #[test]
    fn per_room_messages_loaded_prevents_auto_scroll_on_return() {
        // Simulate clear() save/restore of messages_loaded.
        // Return visits must NOT auto-scroll (was_loaded=true → first_load=false
        // in set_messages, which skips the auto-scroll block).

        let mut saved_loaded: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        let room_a = "!roomA:example.org";

        // First visit: messages_loaded starts false → set to true after set_messages.
        let messages_loaded_first = true; // after set_messages ran
        saved_loaded.insert(room_a.to_string(), messages_loaded_first);

        // Switch to another room, then back to room_a.
        let was_loaded = saved_loaded.remove(room_a).unwrap_or(false);
        assert!(was_loaded, "messages_loaded should be restored for return visit");

        // In set_messages, first_load = !imp.messages_loaded.get().
        // Since messages_loaded was restored to true, first_load = false → no auto-scroll.
        let first_load = !was_loaded;
        assert!(!first_load, "return visit must not trigger first-load auto-scroll");
    }

    // ── divider_should_mark ──────────────────────────────────────────────────

    #[test]
    fn own_message_not_marked_new() {
        // When sender_id matches my_id, we must NOT mark the message as new.
        assert!(!divider_should_mark("@me:example.org", "@me:example.org"));
    }

    #[test]
    fn other_message_marked_new() {
        assert!(divider_should_mark("@alice:example.org", "@me:example.org"));
    }

    #[test]
    fn empty_my_id_marks_all_new() {
        // If user_id is not yet known, mark everything as new (safe fallback).
        assert!(divider_should_mark("@me:example.org", ""));
    }

    #[test]
    fn different_server_not_own() {
        // Same localpart but different homeserver — not the same user.
        assert!(divider_should_mark("@me:other.org", "@me:example.org"));
    }

    // ── is_echo_duplicate ───────────────────────────────────────────────────

    #[test]
    fn echo_duplicate_detected() {
        // If "Hello" is a pending echo, the server confirmation is a duplicate.
        assert!(is_echo_duplicate(&["Hello"], "Hello"));
    }

    #[test]
    fn no_echo_duplicate_different_body() {
        assert!(!is_echo_duplicate(&["Hello"], "World"));
    }

    #[test]
    fn no_echo_duplicate_empty_list() {
        assert!(!is_echo_duplicate(&[], "Hello"));
    }

    #[test]
    fn echo_duplicate_multiple_pending() {
        // Multiple echoes in flight — only the matching one is a dupe.
        assert!(is_echo_duplicate(&["msg1", "msg2", "msg3"], "msg2"));
        assert!(!is_echo_duplicate(&["msg1", "msg2", "msg3"], "msg4"));
    }

    // ── should_skip_append ──────────────────────────────────────────────────
    // Guards the dedup check in append_message: a confirmed event that is
    // already in the timeline must not produce a duplicate row.

    use super::{compute_eviction_boundary, should_skip_append};

    #[test]
    fn skip_append_confirmed_already_in_index() {
        // A confirmed server event (non-empty eid) that is already displayed
        // must be skipped to avoid a double-post.
        assert!(should_skip_append("$ev1", true));
    }

    #[test]
    fn no_skip_confirmed_not_in_index() {
        // A new confirmed event not yet displayed should be added.
        assert!(!should_skip_append("$ev1", false));
    }

    #[test]
    fn no_skip_local_echo() {
        // Local echoes have an empty event_id — must never be skipped so the
        // optimistic placeholder appears immediately after the user sends.
        assert!(!should_skip_append("", true));
        assert!(!should_skip_append("", false));
    }

    // ── compute_eviction_boundary ───────────────────────────────────────────
    // Guards the tail-eviction logic in prepend_messages: in-flight echoes
    // (empty event_id) must survive the cap so they are never silently removed
    // and then re-inserted at a different sorted position.

    #[test]
    fn eviction_boundary_no_overflow() {
        // Store has fewer items than the cap — nothing to evict.
        let ids = ["$a", "$b", "$c"];
        assert_eq!(compute_eviction_boundary(&ids, 10), 10);
    }

    #[test]
    fn eviction_boundary_no_echo_evict_all_overflow() {
        // 5 items, cap=3, none are echoes — evict indices 3 and 4.
        let ids = ["$a", "$b", "$c", "$d", "$e"];
        assert_eq!(compute_eviction_boundary(&ids, 3), 5);
    }

    #[test]
    fn eviction_boundary_echo_at_cap_evict_nothing() {
        // Echo sits exactly at the cap boundary — evict nothing.
        let ids = ["$a", "$b", "$c", "", "$e"];
        assert_eq!(compute_eviction_boundary(&ids, 3), 3);
    }

    #[test]
    fn eviction_boundary_echo_past_cap_partial_evict() {
        // Echo at index 5, cap=3 — evict only indices 3 and 4.
        let ids = ["$a", "$b", "$c", "$d", "$e", "", "$g"];
        assert_eq!(compute_eviction_boundary(&ids, 3), 5);
    }

    #[test]
    fn eviction_boundary_multiple_echoes_stops_at_first() {
        // Two echoes: one at index 4, one at index 6.  Stop at the first.
        let ids = ["$a", "$b", "$c", "$d", "", "$f", ""];
        assert_eq!(compute_eviction_boundary(&ids, 3), 4);
    }

    #[test]
    fn eviction_boundary_exact_cap_size_no_overflow() {
        // Store exactly at the cap — no overflow, nothing to evict.
        let ids = ["$a", "$b", "$c"];
        assert_eq!(compute_eviction_boundary(&ids, 3), 3);
    }

    // ── sorted_insert_pos_in ────────────────────────────────────────────────
    // These tests guard the binary-search used in the incremental set_messages
    // path.  A bug here means messages land at the wrong position in the
    // timeline, which is both a correctness and a scroll-position regression.

    fn make_ts_store(timestamps: &[u64]) -> Vec<u64> {
        timestamps.to_vec()
    }

    fn insert_pos(timestamps: &[u64], ts: u64) -> u32 {
        let store = make_ts_store(timestamps);
        sorted_insert_pos_in(store.len() as u32, ts, |i| store[i as usize])
    }

    #[test]
    fn insert_into_empty_store() {
        assert_eq!(insert_pos(&[], 100), 0);
    }

    #[test]
    fn insert_before_all() {
        // ts=5 is older than all items → goes at position 0.
        assert_eq!(insert_pos(&[10, 20, 30], 5), 0);
    }

    #[test]
    fn insert_after_all() {
        // ts=40 is newer than all items → goes at end.
        assert_eq!(insert_pos(&[10, 20, 30], 40), 3);
    }

    #[test]
    fn insert_in_middle() {
        assert_eq!(insert_pos(&[10, 20, 30], 15), 1);
        assert_eq!(insert_pos(&[10, 20, 30], 25), 2);
    }

    #[test]
    fn insert_duplicate_timestamp_after_existing() {
        // Equal timestamps: new message is inserted AFTER existing one (stable).
        assert_eq!(insert_pos(&[10, 20, 20, 30], 20), 3);
    }

    #[test]
    fn insert_single_item_before() {
        assert_eq!(insert_pos(&[50], 10), 0);
    }

    #[test]
    fn insert_single_item_after() {
        assert_eq!(insert_pos(&[50], 100), 1);
    }

    // ── incremental_prev_batch (pagination token restoration) ────────────────
    //
    // Regression: the incremental set_messages path (taken on all return visits)
    // previously discarded the server's prev_batch_token because clear() resets
    // it to None.  Without restoration the scroll-to-top handler's is_some()
    // check always fails, making history pagination impossible after a room switch.

    #[test]
    fn prev_batch_restored_after_room_switch() {
        // After a room switch, clear() sets our token to None.
        // The next incremental set_messages must restore it from the server.
        let result = incremental_prev_batch(None, Some("server_token_abc"));
        assert_eq!(result.as_deref(), Some("server_token_abc"),
            "token cleared by room switch must be restored from server response");
    }

    #[test]
    fn prev_batch_preserved_after_user_pagination() {
        // User has already scrolled back via prepend_messages; their deeper token
        // must not be overwritten by a bg_refresh carrying the latest-window token.
        let result = incremental_prev_batch(Some("deep_token_xyz"), Some("latest_token_abc"));
        assert_eq!(result.as_deref(), Some("deep_token_xyz"),
            "user's pagination token must be preserved when bg_refresh arrives");
    }

    #[test]
    fn prev_batch_none_when_server_has_no_history() {
        // Room switch clears token; server says no older history → stays None.
        let result = incremental_prev_batch(None, None);
        assert_eq!(result, None,
            "no-history signal from server must be respected");
    }

    #[test]
    fn prev_batch_not_cleared_when_server_sends_none_mid_pagination() {
        // User is mid-pagination (token present); server bg_refresh sends None.
        // Must keep the existing token (server refresh batch ≠ historical fetch).
        let result = incremental_prev_batch(Some("deep_token"), None);
        assert_eq!(result.as_deref(), Some("deep_token"),
            "existing pagination token must survive a None in bg_refresh");
    }

    // ── scroll_save_frac / scroll_restore_value ──────────────────────────────
    // Regression guard: these normalise scroll position across room switches.
    // If the math changes, restored position will be wrong on return visits.

    use super::{scroll_save_frac, scroll_restore_value, should_show_loading_after_switch};
    use std::collections::HashMap;

    #[test]
    fn scroll_save_bottom() {
        // At the very bottom: value == upper - page_size → frac == 1.0.
        let frac = scroll_save_frac(480.0, 500.0, 20.0);
        assert!((frac - 1.0).abs() < 1e-9, "bottom → frac 1.0, got {frac}");
    }

    #[test]
    fn scroll_save_top() {
        let frac = scroll_save_frac(0.0, 500.0, 20.0);
        assert!((frac - 0.0).abs() < 1e-9, "top → frac 0.0, got {frac}");
    }

    #[test]
    fn scroll_save_midpoint() {
        // value = 240, max = 480 → frac = 0.5.
        let frac = scroll_save_frac(240.0, 500.0, 20.0);
        assert!((frac - 0.5).abs() < 1e-9, "midpoint → frac 0.5, got {frac}");
    }

    #[test]
    fn scroll_save_clamps_to_zero_on_tiny_list() {
        // upper == page_size: nothing to scroll, max forced to 1.0 to avoid /0.
        // value == 0 → fraction 0.0.
        let frac = scroll_save_frac(0.0, 20.0, 20.0);
        assert!((frac - 0.0).abs() < 1e-9, "tiny list top → frac 0.0, got {frac}");
    }

    #[test]
    fn scroll_restore_roundtrip() {
        // Save then restore must be lossless.
        let value_in = 240.0_f64;
        let upper = 500.0_f64;
        let page = 20.0_f64;
        let frac = scroll_save_frac(value_in, upper, page);
        let value_out = scroll_restore_value(frac, upper, page);
        assert!((value_out - value_in).abs() < 1e-6,
            "save→restore roundtrip: in={value_in} out={value_out}");
    }

    #[test]
    fn scroll_restore_zero_range_returns_zero() {
        // When content fits entirely on screen, restored value should be 0.
        let value = scroll_restore_value(0.5, 20.0, 20.0);
        assert!((value - 0.0).abs() < 1e-9, "zero-range restore → 0.0, got {value}");
    }

    // ── should_show_loading_after_switch ─────────────────────────────────────
    // Regression guard for the per-room ListView refactor.
    //
    // The invariant: only return visits where messages were fully loaded get
    // to skip the spinner.  If this logic is wrong, users see a spinner on
    // every return visit (was_loaded not restored) or see stale messages where
    // the spinner should be (is_return_visit detection broken).

    #[test]
    fn loading_shown_on_first_visit() {
        assert!(should_show_loading_after_switch(false, false),
            "first visit (is_return_visit=false) must show loading");
    }

    #[test]
    fn loading_shown_on_return_visit_not_yet_loaded() {
        // Room was visited but messages never finished loading (e.g. network error).
        assert!(should_show_loading_after_switch(true, false),
            "return visit with was_loaded=false must show loading");
    }

    #[test]
    fn no_loading_on_return_visit_with_loaded_messages() {
        // Happy path: return to a room whose messages are in the store.
        assert!(!should_show_loading_after_switch(true, true),
            "return visit with was_loaded=true must NOT show loading");
    }

    // ── per-room messages_loaded save/restore ─────────────────────────────────
    // Scenario: switch A→B→A and verify was_loaded is correctly round-tripped.
    // This is the pure-logic part of what clear() does for saved_messages_loaded.

    #[test]
    fn messages_loaded_survives_room_switch() {
        let mut saved: HashMap<String, bool> = HashMap::new();

        // User visits room A; set_messages() fires → messages_loaded = true.
        // On switch away from A, clear() saves it.
        saved.insert("!a:s".into(), true);

        // User visits room B (first visit, not in map yet).
        let was_loaded_b = saved.remove("!b:s").unwrap_or(false);
        assert!(!was_loaded_b, "first visit to B: was_loaded must be false");

        // set_messages() for B fires → messages_loaded = true; save on switch away.
        saved.insert("!b:s".into(), true);

        // Return to room A.
        let was_loaded_a2 = saved.remove("!a:s").unwrap_or(false);
        assert!(was_loaded_a2,
            "return to A: was_loaded must be true (saved before B was visited)");
        // Loading decision: return visit + was_loaded=true → show messages.
        assert!(!should_show_loading_after_switch(true, was_loaded_a2));
    }

    #[test]
    fn messages_loaded_false_when_room_never_loaded() {
        let saved: HashMap<String, bool> = HashMap::new();
        // Room C was added to the view cache but set_messages() never fired.
        let was_loaded = saved.get("!c:s").copied().unwrap_or(false);
        assert!(!was_loaded);
        assert!(should_show_loading_after_switch(true, was_loaded),
            "return visit with no saved state must show loading");
    }

    // ── front_evict_count ────────────────────────────────────────────────────

    #[test]
    fn front_evict_under_cap_no_eviction() {
        let ids = ["a", "b", "c"];
        assert_eq!(front_evict_count(&ids, 400), 0);
        assert_eq!(front_evict_count(&ids, 3), 0);
        assert_eq!(front_evict_count(&ids, 4), 0);
    }

    #[test]
    fn front_evict_one_over_cap() {
        let ids: Vec<&str> = (0..401).map(|_| "x").collect();
        assert_eq!(front_evict_count(&ids, 400), 1);
    }

    #[test]
    fn front_evict_many_over_cap() {
        let ids: Vec<&str> = (0..450).map(|_| "x").collect();
        assert_eq!(front_evict_count(&ids, 400), 50);
    }

    #[test]
    fn front_evict_stops_before_echo_at_front() {
        // Echo (empty event_id) at position 0 — nothing should be evicted.
        let ids = ["", "b", "c", "d", "e"];
        assert_eq!(front_evict_count(&ids, 4), 0, "echo at front stops eviction");
    }

    #[test]
    fn front_evict_stops_before_echo_mid_range() {
        // Echo at position 1 — can only evict position 0.
        let ids = ["a", "", "c", "d", "e"];
        assert_eq!(front_evict_count(&ids, 4), 1, "stops before echo at pos 1");
    }

    #[test]
    fn front_evict_echo_outside_eviction_range_ignored() {
        // Echo is beyond the eviction target — should not affect count.
        // 5 items, cap=3: need to evict 2; echo is at position 3 (outside target).
        let ids = ["a", "b", "c", "", "e"];
        assert_eq!(front_evict_count(&ids, 3), 2);
    }

    #[test]
    fn front_evict_empty_store() {
        assert_eq!(front_evict_count(&[], 400), 0);
    }
}
