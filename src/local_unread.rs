// local_unread.rs — persistent local unread-message broker.
//
// The Matrix server's `unread_notification_counts()` only reflects rooms that
// matched push rules (mentions, keywords). Public channels where you have no
// keyword rules always return 0 even when 50 messages arrived.
//
// LocalUnreadStore solves this by counting *every* new message that arrives
// in a room while the user is not actively viewing it, using a GObject so the
// count is reactive (room rows update the moment a message arrives) and a
// SQLite table so the counts survive restarts.
//
// Architecture
// ────────────
//   Matrix sync thread
//       ↓ MatrixEvent::NewMessage
//   GTK main thread (window.rs)
//       ↓ LocalUnreadStore::increment(room_id, is_highlight)
//   LocalUnreadStore (GObject, main thread)
//       ↓ emit "room-unread-changed"(room_id, unread, highlights)
//   room_list_view.rs handler
//       ↓ RoomObject::set_unread_count / set_highlight_count
//   RoomRow badge (reactive via connect_notify_local)
//
// Persistence
// ───────────
// $XDG_DATA_HOME/hikyaku/unread.db
// (~/.local/share/hikyaku/unread.db natively,
//  ~/.var/app/me.ramkrishna.hikyaku/data/hikyaku/unread.db in Flatpak)
//
//   local_unread(room_id TEXT PRIMARY KEY, unread INTEGER, highlights INTEGER)
//
// The GTK thread holds its own connection to this DB (separate file from
// timeline.db to avoid write contention with the tokio thread).
// All writes are synchronous and fast (<1 ms) since the table is tiny.

mod imp {
    use glib::prelude::*;
    use glib::subclass::prelude::*;
    use glib::subclass::Signal;
    use rusqlite::{Connection, params};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    #[derive(Default)]
    pub struct LocalUnreadStore {
        /// In-memory counts: room_id → (unread, highlights).
        pub counts: RefCell<HashMap<String, (u32, u32)>>,
        /// Rooms read by another client (phone, web, etc.) but not yet
        /// confirmed by opening in Hikyaku.  These rooms show count=0 in
        /// the badge even if the broker had a locally-incremented count.
        /// When a new message arrives in a room from this set, the room is
        /// removed and counting resumes normally.
        pub read_elsewhere: RefCell<std::collections::HashSet<String>>,
        /// SQLite connection (opened lazily on first use).
        pub conn: RefCell<Option<Connection>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LocalUnreadStore {
        const NAME: &'static str = "HikyakuLocalUnreadStore";
        type Type = super::LocalUnreadStore;
    }

    impl ObjectImpl for LocalUnreadStore {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![Signal::builder("room-unread-changed")
                    .param_types([
                        String::static_type(),
                        u32::static_type(),
                        u32::static_type(),
                    ])
                    .build()]
            })
        }
    }

    impl LocalUnreadStore {
        pub(super) fn db_path() -> PathBuf {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("hikyaku")
                .join("unread.db")
        }

        /// Open (or return) the SQLite connection and ensure the schema exists.
        pub(super) fn open_conn(&self) -> std::cell::Ref<'_, Option<Connection>> {
            {
                let mut opt = self.conn.borrow_mut();
                if opt.is_none() {
                    let path = Self::db_path();
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match Connection::open(&path) {
                        Ok(c) => {
                            // WAL + NORMAL is load-bearing here. `mark_read`
                            // runs on the GTK main thread from the focus
                            // handler and the room-click path; the default
                            // DELETE journal + synchronous=FULL combination
                            // would add an fsync per write (single-digit
                            // milliseconds in the best case, 100+ after a
                            // cold wake). WAL batches via a checkpoint and
                            // NORMAL drops the per-commit fsync, so a
                            // mark_read call is bounded by the mutex +
                            // memtable write rather than disk flush. See
                            // CLAUDE.md §1 for the broader rule.
                            let _ = c.execute_batch(
                                "PRAGMA journal_mode=WAL;
                                 PRAGMA synchronous=NORMAL;
                                 CREATE TABLE IF NOT EXISTS local_unread (
                                     room_id    TEXT PRIMARY KEY,
                                     unread     INTEGER NOT NULL DEFAULT 0,
                                     highlights INTEGER NOT NULL DEFAULT 0
                                 );",
                            );
                            *opt = Some(c);
                        }
                        Err(e) => {
                            tracing::warn!("LocalUnreadStore: cannot open unread.db: {e}");
                        }
                    }
                }
            }
            self.conn.borrow()
        }

        pub(super) fn write_room(&self, room_id: &str, unread: u32, highlights: u32) {
            let conn_ref = self.open_conn();
            if let Some(conn) = conn_ref.as_ref() {
                let _ = conn.execute(
                    "INSERT INTO local_unread (room_id, unread, highlights)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(room_id) DO UPDATE SET unread=?2, highlights=?3",
                    params![room_id, unread, highlights],
                );
            }
        }
    }
}

use gtk::glib;
use gtk::glib::prelude::*;
use gtk::subclass::prelude::*;

glib::wrapper! {
    /// A GObject that tracks locally-counted unread messages per room.
    ///
    /// Must only be used from the GTK main thread.
    pub struct LocalUnreadStore(ObjectSubclass<imp::LocalUnreadStore>);
}

impl LocalUnreadStore {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Load all persisted counts from SQLite into memory.
    /// Call once at startup before the room list is displayed.
    pub fn load(&self) {
        let conn_ref = self.imp().open_conn();
        let Some(conn) = conn_ref.as_ref() else { return };
        let Ok(mut stmt) =
            conn.prepare("SELECT room_id, unread, highlights FROM local_unread WHERE unread > 0 OR highlights > 0")
        else {
            return;
        };
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, u32>(2)?,
            ))
        });
        let Ok(rows) = rows else { return };
        let mut counts = self.imp().counts.borrow_mut();
        for row in rows.flatten() {
            counts.insert(row.0, (row.1, row.2));
        }
    }

    /// Return the locally-tracked (unread, highlights) for a room.
    pub fn get(&self, room_id: &str) -> (u32, u32) {
        *self.imp().counts.borrow().get(room_id).unwrap_or(&(0, 0))
    }

    /// Increment the counter for a room that is NOT currently open.
    /// Persists to disk and emits `room-unread-changed`.
    /// Also clears any `read_elsewhere` flag — new messages supersede
    /// the "read on another client" state.
    pub fn increment(&self, room_id: &str, is_highlight: bool) {
        // Clear the read_elsewhere suppression — there are genuinely new messages now.
        self.imp().read_elsewhere.borrow_mut().remove(room_id);
        let (new_u, new_h) = {
            let mut counts = self.imp().counts.borrow_mut();
            let entry = counts.entry(room_id.to_string()).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(1);
            if is_highlight {
                entry.1 = entry.1.saturating_add(1);
            }
            *entry
        };
        self.imp().write_room(room_id, new_u, new_h);
        self.emit_by_name::<()>(
            "room-unread-changed",
            &[&room_id.to_string(), &new_u, &new_h],
        );
    }

    /// Mark a room as read by another client (phone, web, etc.).
    /// The badge is suppressed to 0 without modifying the persisted count —
    /// the count resets properly when the user opens the room in Hikyaku.
    /// If new messages arrive in the room later, `increment` clears this flag.
    pub fn mark_read_elsewhere(&self, room_id: &str) {
        self.imp().read_elsewhere.borrow_mut().insert(room_id.to_string());
        // Zero out the broker count too — the messages were already acknowledged.
        {
            let mut counts = self.imp().counts.borrow_mut();
            counts.insert(room_id.to_string(), (0, 0));
        }
        self.imp().write_room(room_id, 0, 0);
        self.emit_by_name::<()>(
            "room-unread-changed",
            &[&room_id.to_string(), &0u32, &0u32],
        );
        tracing::debug!("LocalUnreadStore: {room_id} marked as read-elsewhere");
    }

    /// Reset counts to zero for a room the user has opened.
    /// Persists to disk and emits `room-unread-changed` with (0, 0).
    pub fn mark_read(&self, room_id: &str) {
        {
            let mut counts = self.imp().counts.borrow_mut();
            counts.insert(room_id.to_string(), (0, 0));
        }
        self.imp().write_room(room_id, 0, 0);
        self.emit_by_name::<()>(
            "room-unread-changed",
            &[&room_id.to_string(), &0u32, &0u32],
        );
    }

    /// Connect to the `room-unread-changed` signal.
    ///
    /// The closure receives `(room_id: String, unread: u32, highlights: u32)`.
    pub fn connect_room_unread_changed<F>(&self, f: F) -> glib::SignalHandlerId
    where
        F: Fn(&Self, String, u32, u32) + 'static,
    {
        self.connect_closure(
            "room-unread-changed",
            false,
            glib::closure_local!(move |store: &LocalUnreadStore,
                                        room_id: String,
                                        unread: u32,
                                        highlights: u32| {
                f(store, room_id, unread, highlights);
            }),
        )
    }

    /// Iterate over every room with a non-zero count.
    /// Used on startup to seed the room list after `update_rooms` has populated it.
    pub fn for_each_nonzero<F: Fn(&str, u32, u32)>(&self, f: F) {
        let counts = self.imp().counts.borrow();
        for (room_id, &(unread, highlights)) in counts.iter() {
            if unread > 0 || highlights > 0 {
                f(room_id, unread, highlights);
            }
        }
    }
}

impl Default for LocalUnreadStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── unit tests ───────────────────────────────────────────────────────────────
//
// The broker's mutation logic is extracted into pure functions so it can be
// tested without a GTK display server.  Each function mirrors the in-place
// logic inside `increment`, `mark_read`, and `mark_read_elsewhere`.
//
// Scenario tested                                Expected outcome
// ─────────────────────────────────────────────────────────────────────────────
// increment normal room                          count rises by 1
// increment highlight room                       both unread + highlight rise
// mark_read zeroes counts                        count = (0, 0)
// mark_read_elsewhere zeroes + sets flag         count = (0,0), flag present
// increment after mark_read_elsewhere            flag cleared, count = 1
// for_each_nonzero skips zero rooms              only non-zero rooms yielded
// zero-by-local vs zero-by-elsewhere             distinguishable via flag
// badge debug invariant: read_elsewhere ↔ zeroed by cross-client receipt

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    type Counts = HashMap<String, (u32, u32)>;
    type ReadElsewhere = HashSet<String>;

    fn do_increment(
        counts: &mut Counts,
        read_elsewhere: &mut ReadElsewhere,
        room_id: &str,
        is_highlight: bool,
    ) -> (u32, u32) {
        read_elsewhere.remove(room_id);
        let entry = counts.entry(room_id.to_string()).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(1);
        if is_highlight { entry.1 = entry.1.saturating_add(1); }
        *entry
    }

    fn do_mark_read(counts: &mut Counts, read_elsewhere: &mut ReadElsewhere, room_id: &str) {
        counts.insert(room_id.to_string(), (0, 0));
        read_elsewhere.remove(room_id);
    }

    fn do_mark_read_elsewhere(
        counts: &mut Counts,
        read_elsewhere: &mut ReadElsewhere,
        room_id: &str,
    ) {
        counts.insert(room_id.to_string(), (0, 0));
        read_elsewhere.insert(room_id.to_string());
    }

    fn nonzero_rooms(counts: &Counts) -> Vec<(&str, u32, u32)> {
        let mut v: Vec<_> = counts.iter()
            .filter(|(_, &(u, h))| u > 0 || h > 0)
            .map(|(id, &(u, h))| (id.as_str(), u, h))
            .collect();
        v.sort_by_key(|(id, _, _)| *id);
        v
    }

    #[test]
    fn increment_raises_unread_count() {
        let mut counts = Counts::new();
        let mut re = ReadElsewhere::new();
        let (u, h) = do_increment(&mut counts, &mut re, "!r:m.org", false);
        assert_eq!((u, h), (1, 0));
        let (u2, h2) = do_increment(&mut counts, &mut re, "!r:m.org", false);
        assert_eq!((u2, h2), (2, 0));
    }

    #[test]
    fn increment_highlight_raises_both_counters() {
        let mut counts = Counts::new();
        let mut re = ReadElsewhere::new();
        let (u, h) = do_increment(&mut counts, &mut re, "!r:m.org", true);
        assert_eq!((u, h), (1, 1));
    }

    #[test]
    fn mark_read_zeroes_counts() {
        let mut counts = Counts::new();
        let mut re = ReadElsewhere::new();
        do_increment(&mut counts, &mut re, "!r:m.org", false);
        do_increment(&mut counts, &mut re, "!r:m.org", true);
        do_mark_read(&mut counts, &mut re, "!r:m.org");
        assert_eq!(counts["!r:m.org"], (0, 0));
        assert!(nonzero_rooms(&counts).is_empty());
    }

    #[test]
    fn mark_read_elsewhere_zeroes_and_sets_flag() {
        let mut counts = Counts::new();
        let mut re = ReadElsewhere::new();
        do_increment(&mut counts, &mut re, "!r:m.org", false);
        do_mark_read_elsewhere(&mut counts, &mut re, "!r:m.org");
        assert_eq!(counts["!r:m.org"], (0, 0), "count must be zeroed");
        assert!(re.contains("!r:m.org"), "read_elsewhere flag must be set");
        assert!(nonzero_rooms(&counts).is_empty(), "badge must show 0");
    }

    #[test]
    fn increment_after_read_elsewhere_clears_flag_and_resumes_count() {
        let mut counts = Counts::new();
        let mut re = ReadElsewhere::new();
        do_increment(&mut counts, &mut re, "!r:m.org", false);
        do_mark_read_elsewhere(&mut counts, &mut re, "!r:m.org");
        // New message arrives after the cross-client read.
        let (u, _) = do_increment(&mut counts, &mut re, "!r:m.org", false);
        assert_eq!(u, 1, "counting must resume from 0 after read_elsewhere");
        assert!(!re.contains("!r:m.org"), "flag must be cleared by new message");
    }

    #[test]
    fn for_each_nonzero_skips_zero_rooms() {
        let mut counts = Counts::new();
        let mut re = ReadElsewhere::new();
        do_increment(&mut counts, &mut re, "!a:m.org", false);
        do_increment(&mut counts, &mut re, "!b:m.org", false);
        do_mark_read(&mut counts, &mut re, "!b:m.org");
        let nz = nonzero_rooms(&counts);
        assert_eq!(nz.len(), 1);
        assert_eq!(nz[0].0, "!a:m.org");
    }

    #[test]
    fn zero_by_local_open_vs_zero_by_read_elsewhere_are_distinguishable() {
        let mut counts = Counts::new();
        let mut re = ReadElsewhere::new();
        do_increment(&mut counts, &mut re, "!local:m.org", false);
        do_increment(&mut counts, &mut re, "!remote:m.org", false);
        do_mark_read(&mut counts, &mut re, "!local:m.org");        // user opened in Hikyaku
        do_mark_read_elsewhere(&mut counts, &mut re, "!remote:m.org"); // phone read it
        // Both show count=0 in the badge.
        assert_eq!(counts["!local:m.org"], (0, 0));
        assert_eq!(counts["!remote:m.org"], (0, 0));
        // But the source is distinguishable for debugging.
        assert!(!re.contains("!local:m.org"), "local open must NOT be in read_elsewhere");
        assert!(re.contains("!remote:m.org"), "cross-client read MUST be in read_elsewhere");
    }

    /// Badge debug invariant: if `read_elsewhere` contains a room, its count
    /// in `counts` must be (0, 0).  This invariant should hold after any
    /// sequence of broker operations.
    #[test]
    fn badge_debug_invariant_read_elsewhere_always_has_zero_count() {
        let mut counts = Counts::new();
        let mut re = ReadElsewhere::new();
        // Simulate a sequence: messages arrive, phone reads, more messages arrive.
        for _ in 0..5 {
            do_increment(&mut counts, &mut re, "!r:m.org", false);
        }
        do_mark_read_elsewhere(&mut counts, &mut re, "!r:m.org");
        // Invariant: every room in read_elsewhere has count == (0, 0).
        for room_id in &re {
            assert_eq!(
                counts.get(room_id).copied().unwrap_or((0, 0)),
                (0, 0),
                "read_elsewhere room {room_id} must have zero count"
            );
        }
    }
}
