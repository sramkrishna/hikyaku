// RoomCache — unified in-memory + on-disk timeline cache.
//
// A GObject that centralises both the in-memory HashMap (fast room switches,
// zero I/O) and the on-disk TimelineStore (instant restarts).  Keeping them
// together means every write hits both stores atomically — no chance of the
// two caches drifting apart.
//
// Storage layout
// ──────────────
// ~/.local/share/hikyaku/timeline.db  — single SQLite database.
//
//   messages(room_id, position, event_id, sender, sender_id, body,
//            formatted_body, timestamp, reply_to, reply_to_sender,
//            thread_root, reactions, media, is_highlight)
//
//   room_state(room_id, prev_batch_token)
//
// One SQLite connection replaces the previous per-room JSON files, reducing
// open file-descriptors from O(rooms) to 1 and enabling indexed queries.
//
// Thread safety
// ─────────────
// The memory HashMap is protected by std::sync::Mutex (never held across an
// .await point — only brief HashMap ops).  The SQLite connection is also
// behind std::sync::Mutex; all disk methods are blocking and must be called
// from spawn_blocking.  GObject's own reference count is already atomic, so
// Clone gives a cheap additional reference to the same state — exactly the
// Arc<> semantics the tokio code needs.
//
// unsafe impl Send/Sync is safe here because:
//   1. No GTK or GLib main-thread calls are made on this object.
//   2. All mutable state is behind std::sync::Mutex.

mod imp {
    use glib::subclass::prelude::*;
    use rusqlite::{Connection, params};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use crate::matrix::{MessageInfo, MediaInfo, MediaKind, RoomMeta};

    // ── TimelineStore ─────────────────────────────────────────────────────────
    // Single SQLite connection shared across all rooms.

    pub(super) struct TimelineStore {
        conn: Mutex<Connection>,
    }

    impl TimelineStore {
        pub(super) fn new() -> Self {
            let db_path = Self::db_path();
            if let Some(parent) = db_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let conn = Connection::open(&db_path)
                .expect("failed to open timeline.db");
            Self::init_schema(&conn);
            Self { conn: Mutex::new(conn) }
        }

        fn db_path() -> PathBuf {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("hikyaku")
                .join("timeline.db")
        }

        fn init_schema(conn: &Connection) {
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS messages (
                     room_id          TEXT    NOT NULL,
                     position         INTEGER NOT NULL,
                     event_id         TEXT    NOT NULL DEFAULT '',
                     sender           TEXT    NOT NULL DEFAULT '',
                     sender_id        TEXT    NOT NULL DEFAULT '',
                     body             TEXT    NOT NULL DEFAULT '',
                     formatted_body   TEXT,
                     timestamp        INTEGER NOT NULL DEFAULT 0,
                     reply_to         TEXT,
                     reply_to_sender  TEXT,
                     thread_root      TEXT,
                     reactions        TEXT    NOT NULL DEFAULT '[]',
                     media            TEXT,
                     is_highlight     INTEGER NOT NULL DEFAULT 0,
                     is_system_event  INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (room_id, position)
                 );
                 CREATE TABLE IF NOT EXISTS room_state (
                     room_id           TEXT PRIMARY KEY,
                     prev_batch_token  TEXT
                 );",
            )
            .expect("failed to initialise timeline schema");
        // Migration: add is_system_event column to existing databases.
        // Errors are ignored — if the column already exists this is a no-op.
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN is_system_event INTEGER NOT NULL DEFAULT 0",
            [],
        );
        }

        pub(super) fn save(
            &self,
            room_id: &str,
            messages: &[MessageInfo],
            prev_batch: Option<&str>,
        ) {
            let mut conn = self.conn.lock().unwrap();
            // Wrap in a transaction: DELETE old rows + INSERT new ones atomically.
            let tx = match conn.transaction() {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("timeline save: begin tx: {e}");
                    return;
                }
            };
            if let Err(e) = tx.execute(
                "DELETE FROM messages WHERE room_id = ?1",
                params![room_id],
            ) {
                tracing::error!("timeline save: delete: {e}");
                return;
            }
            for (pos, msg) in messages.iter().enumerate() {
                let reactions_json =
                    serde_json::to_string(&msg.reactions).unwrap_or_else(|_| "[]".into());
                let media_json = msg
                    .media
                    .as_ref()
                    .and_then(|m| serde_json::to_string(m).ok());
                if let Err(e) = tx.execute(
                    "INSERT INTO messages
                         (room_id, position, event_id, sender, sender_id, body,
                          formatted_body, timestamp, reply_to, reply_to_sender,
                          thread_root, reactions, media, is_highlight, is_system_event)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                    params![
                        room_id,
                        pos as i64,
                        msg.event_id,
                        msg.sender,
                        msg.sender_id,
                        msg.body,
                        msg.formatted_body,
                        msg.timestamp as i64,
                        msg.reply_to,
                        msg.reply_to_sender,
                        msg.thread_root,
                        reactions_json,
                        media_json,
                        msg.is_highlight as i32,
                        msg.is_system_event as i32,
                    ],
                ) {
                    tracing::error!("timeline save: insert row {pos}: {e}");
                    return;
                }
            }
            if let Err(e) = tx.execute(
                "INSERT OR REPLACE INTO room_state (room_id, prev_batch_token)
                 VALUES (?1, ?2)",
                params![room_id, prev_batch],
            ) {
                tracing::error!("timeline save: upsert room_state: {e}");
                return;
            }
            if let Err(e) = tx.commit() {
                tracing::error!("timeline save: commit: {e}");
            }
        }

        pub(super) fn load(
            &self,
            room_id: &str,
        ) -> Option<(Vec<MessageInfo>, Option<String>)> {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn
                .prepare_cached(
                    "SELECT event_id, sender, sender_id, body, formatted_body,
                            timestamp, reply_to, reply_to_sender, thread_root,
                            reactions, media, is_highlight, is_system_event
                     FROM messages
                     WHERE room_id = ?1
                     ORDER BY position",
                )
                .ok()?;

            let messages: Vec<MessageInfo> = stmt
                .query_map(params![room_id], |row| {
                    let reactions_json: String = row.get(9)?;
                    let media_json: Option<String> = row.get(10)?;
                    let is_highlight: i32 = row.get(11)?;
                    let is_system_event: i32 = row.get(12).unwrap_or(0);
                    let timestamp: i64 = row.get(5)?;
                    Ok((
                        row.get::<_, String>(0)?,  // event_id
                        row.get::<_, String>(1)?,  // sender
                        row.get::<_, String>(2)?,  // sender_id
                        row.get::<_, String>(3)?,  // body
                        row.get::<_, Option<String>>(4)?,  // formatted_body
                        timestamp,
                        row.get::<_, Option<String>>(6)?,  // reply_to
                        row.get::<_, Option<String>>(7)?,  // reply_to_sender
                        row.get::<_, Option<String>>(8)?,  // thread_root
                        reactions_json,
                        media_json,
                        is_highlight,
                        is_system_event,
                    ))
                })
                .ok()?
                .filter_map(|r| r.ok())
                .map(|(event_id, sender, sender_id, body, formatted_body,
                        timestamp, reply_to, reply_to_sender, thread_root,
                        reactions_json, media_json, is_highlight, is_system_event)| {
                    let reactions = serde_json::from_str(&reactions_json)
                        .unwrap_or_default();
                    let media: Option<MediaInfo> = media_json
                        .as_deref()
                        .and_then(|j| serde_json::from_str(j).ok());
                    MessageInfo {
                        event_id,
                        sender,
                        sender_id,
                        body,
                        formatted_body,
                        timestamp: timestamp as u64,
                        reply_to,
                        reply_to_sender,
                        thread_root,
                        reactions,
                        media,
                        is_highlight: is_highlight != 0,
                        is_system_event: is_system_event != 0,
                    }
                })
                .collect();

            if messages.is_empty() {
                return None;
            }

            let token: Option<String> = conn
                .query_row(
                    "SELECT prev_batch_token FROM room_state WHERE room_id = ?1",
                    params![room_id],
                    |row| row.get(0),
                )
                .ok()
                .flatten();

            Some((messages, token))
        }

        pub(super) fn delete(&self, room_id: &str) {
            let conn = self.conn.lock().unwrap();
            let _ = conn.execute(
                "DELETE FROM messages WHERE room_id = ?1",
                params![room_id],
            );
            let _ = conn.execute(
                "DELETE FROM room_state WHERE room_id = ?1",
                params![room_id],
            );
        }
    }

    // ── GObject ───────────────────────────────────────────────────────────────

    type MemEntry = (Vec<MessageInfo>, Option<String>, RoomMeta);

    pub struct RoomCache {
        /// In-memory timeline: room_id → (messages, prev_batch_token, RoomMeta).
        /// Lock is held only for brief HashMap ops — never across .await points.
        pub(super) memory: Mutex<HashMap<String, MemEntry>>,
        /// When each room's data was last fetched from the server (prefetch OR
        /// bg_refresh).  Checked by SelectRoom to avoid redundant full refreshes.
        pub(super) refreshed_at: Mutex<HashMap<String, std::time::Instant>>,
        /// Disk store — single SQLite connection behind its own Mutex.
        pub(super) disk: TimelineStore,
    }

    impl Default for RoomCache {
        fn default() -> Self {
            Self {
                memory: Mutex::new(HashMap::new()),
                refreshed_at: Mutex::new(HashMap::new()),
                disk: TimelineStore::new(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RoomCache {
        const NAME: &'static str = "MxRoomCache";
        type Type = super::RoomCache;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for RoomCache {}
}

glib::wrapper! {
    pub struct RoomCache(ObjectSubclass<imp::RoomCache>);
}

// SAFETY: All mutable state lives behind std::sync::Mutex.  GObject's own
// reference count is atomic.  This object never calls GTK/GLib main-thread
// functions — it is a pure data cache used only on tokio worker threads.
unsafe impl Send for RoomCache {}
unsafe impl Sync for RoomCache {}

use glib::subclass::prelude::ObjectSubclassIsExt;
use crate::matrix::{MessageInfo, RoomMeta};

/// Maximum messages kept per room in the memory cache.
/// Sync appends grow the Vec; this trim keeps it from growing without bound.
const MAX_MEMORY_MSGS: usize = 100;

impl RoomCache {
    pub fn new() -> Self {
        glib::Object::new()
    }

    // ── Memory cache ──────────────────────────────────────────────────────────

    /// Clone a room's full timeline entry from the memory cache.
    pub fn get_memory(
        &self,
        room_id: &str,
    ) -> Option<(Vec<MessageInfo>, Option<String>, RoomMeta)> {
        self.imp().memory.lock().unwrap().get(room_id).cloned()
    }

    /// Returns `true` if the room has a memory cache entry.
    pub fn has_memory(&self, room_id: &str) -> bool {
        self.imp().memory.lock().unwrap().contains_key(room_id)
    }

    /// Insert (or overwrite) a room's timeline in the memory cache only.
    /// Does NOT touch the disk — callers use [`save_disk`] for persistence.
    pub fn insert_memory_only(
        &self,
        room_id: &str,
        msgs: Vec<MessageInfo>,
        token: Option<String>,
        meta: RoomMeta,
    ) {
        self.imp()
            .memory
            .lock()
            .unwrap()
            .insert(room_id.to_string(), (msgs, token, meta));
    }

    /// Insert into memory only if the room has no entry yet.
    /// Used to seed from disk without overwriting a fresher in-memory entry.
    pub fn insert_memory_if_absent(
        &self,
        room_id: &str,
        msgs: Vec<MessageInfo>,
        token: Option<String>,
        meta: RoomMeta,
    ) {
        self.imp()
            .memory
            .lock()
            .unwrap()
            .entry(room_id.to_string())
            .or_insert_with(|| (msgs, token, meta));
    }

    /// Record that `room_id` was just populated from the server.
    /// Called by both `prefetch_room_timeline` and `handle_select_room_bg`.
    pub fn mark_fresh(&self, room_id: &str) {
        tracing::debug!("mark_fresh: {}", room_id);
        self.imp()
            .refreshed_at
            .lock()
            .unwrap()
            .insert(room_id.to_string(), std::time::Instant::now());
    }

    /// Returns `true` if this room's memory cache was ever populated from the
    /// server (via `mark_fresh`) during the current session.
    ///
    /// Rooms loaded only from the SQLite disk cache have `has_memory = true`
    /// but return `false` here — they need a bg_refresh to pick up messages
    /// that arrived since the disk snapshot was written.
    pub fn was_server_fetched(&self, room_id: &str) -> bool {
        self.imp()
            .refreshed_at
            .lock()
            .unwrap()
            .contains_key(room_id)
    }

    /// Returns `true` if the room was refreshed from the server less than
    /// `max_age` ago AND the memory cache is populated.
    pub fn is_fresh(&self, room_id: &str, max_age: std::time::Duration) -> bool {
        if !self.has_memory(room_id) {
            return false;
        }
        self.imp()
            .refreshed_at
            .lock()
            .unwrap()
            .get(room_id)
            .map(|t| t.elapsed() < max_age)
            .unwrap_or(false)
    }

    /// Update a single message's body in the memory cache (for edits).
    /// Returns true if the message was found and updated.
    pub fn update_message_body_in_cache(
        &self,
        room_id: &str,
        event_id: &str,
        new_body: &str,
        new_formatted: Option<&str>,
    ) -> bool {
        let mut memory = self.imp().memory.lock().unwrap();
        let Some((msgs, _, _)) = memory.get_mut(room_id) else {
            return false;
        };
        for msg in msgs.iter_mut() {
            if msg.event_id == event_id {
                msg.body = new_body.to_string();
                msg.formatted_body = new_formatted.map(|s| s.to_string());
                return true;
            }
        }
        false
    }

    /// Append a single message to the tail of the memory cache if the room has
    /// a cache entry and the message is not already present (checked by event_id).
    ///
    /// Returns `true` if the message was appended (cache was warm and updated).
    /// Returns `false` if the room has no memory cache entry (caller should keep
    /// the dirty flag set so bg_refresh runs on next SelectRoom).
    pub fn append_memory(&self, room_id: &str, msg: MessageInfo) -> bool {
        let mut memory = self.imp().memory.lock().unwrap();
        let Some((msgs, _, _)) = memory.get_mut(room_id) else {
            return false;
        };
        // Deduplicate: skip if this event_id is already in the last few entries.
        if !msg.event_id.is_empty()
            && msgs.iter().rev().take(20).any(|m| m.event_id == msg.event_id)
        {
            return true; // Already present — cache is still valid.
        }
        msgs.push(msg);
        // Trim to the cap so sync appends can't grow the Vec without bound.
        if msgs.len() > MAX_MEMORY_MSGS {
            let drain = msgs.len() - MAX_MEMORY_MSGS;
            msgs.drain(..drain);
        }
        true
    }

    /// Return the event_id of the last message in the memory cache.
    ///
    /// Used by the MarkRead path — `room.latest_event()` is only populated by
    /// the SDK's sync timeline, which is not used here (we use pagination).
    pub fn latest_event_id(&self, room_id: &str) -> Option<String> {
        let memory = self.imp().memory.lock().unwrap();
        memory
            .get(room_id)
            .and_then(|(msgs, _, _)| msgs.last())
            .filter(|m| !m.event_id.is_empty())
            .map(|m| m.event_id.clone())
    }

    /// Return cached members + avatars if a full server fetch was done this session.
    pub fn get_cached_members(
        &self,
        room_id: &str,
    ) -> Option<(Vec<(String, String)>, Vec<(String, String)>)> {
        let memory = self.imp().memory.lock().unwrap();
        memory
            .get(room_id)
            .filter(|(_, _, m)| m.members_fetched)
            .map(|(_, _, m)| (m.members.clone(), m.member_avatars.clone()))
    }

    /// Remove a room from both memory and disk.
    /// Used for redactions and edits (stale cached timeline).
    pub fn remove(&self, room_id: &str) {
        self.imp().memory.lock().unwrap().remove(room_id);
        self.imp().disk.delete(room_id);
    }

    /// Remove a room from memory only, leaving disk intact.
    /// Used for sync gaps: disk cache still serves an instant first display
    /// while bg_refresh fetches fresh data (has_memory=false → needs_refresh=true).
    pub fn remove_memory(&self, room_id: &str) {
        tracing::info!("remove_memory (cache evict): {}", room_id);
        self.imp().memory.lock().unwrap().remove(room_id);
    }

    /// Evict all rooms from the memory cache.
    /// Called after key recovery / import so UTD messages are re-fetched.
    pub fn clear_memory(&self) {
        self.imp().memory.lock().unwrap().clear();
    }

    /// Evict a single room from both memory cache and the server-fetched
    /// timestamp, so the next SelectRoom forces a full bg_refresh.
    /// Called when new session keys arrive for that room.
    pub fn invalidate_room(&self, room_id: &str) {
        self.imp().memory.lock().unwrap().remove(room_id);
        self.imp().refreshed_at.lock().unwrap().remove(room_id);
    }

    // ── Disk cache (blocking — must be called from spawn_blocking) ────────────

    /// Load a room's timeline from the SQLite store.
    ///
    /// **Blocking** — wrap the call in `tokio::task::spawn_blocking`.
    pub fn load_disk(
        &self,
        room_id: &str,
    ) -> Option<(Vec<MessageInfo>, Option<String>)> {
        self.imp().disk.load(room_id)
    }

    /// Persist a room's timeline to the SQLite store.
    ///
    /// **Blocking** — wrap the call in `tokio::task::spawn_blocking`.
    pub fn save_disk(&self, room_id: &str, msgs: &[MessageInfo], token: Option<&str>) {
        self.imp().disk.save(room_id, msgs, token);
    }
}

#[cfg(test)]
mod tests {
    use super::{RoomCache, MAX_MEMORY_MSGS};
    use crate::matrix::{MessageInfo, RoomMeta};

    fn make_msg(event_id: &str) -> MessageInfo {
        MessageInfo {
            event_id: event_id.to_string(),
            sender: "Alice".to_string(),
            sender_id: "@alice:example.org".to_string(),
            body: "hello".to_string(),
            formatted_body: None,
            timestamp: 0,
            reply_to: None,
            reply_to_sender: None,
            thread_root: None,
            reactions: Vec::new(),
            media: None,
            is_highlight: false,
            is_system_event: false,
        }
    }

    fn empty_meta() -> RoomMeta {
        RoomMeta {
            topic: String::new(),
            is_tombstoned: false,
            replacement_room: None,
            replacement_room_name: None,
            pinned_messages: Vec::new(),
            is_encrypted: false,
            member_count: 0,
            is_favourite: false,
            members: Vec::new(),
            member_avatars: Vec::new(),
            members_fetched: false,
            unread_count: 0,
            fully_read_event_id: None,
        }
    }

    // ── append_memory cap ─────────────────────────────────────────────────────

    #[test]
    fn append_memory_trims_to_cap() {
        let cache = RoomCache::new();
        cache.insert_memory_only("!r:s", Vec::new(), None, empty_meta());

        // Append MAX_MEMORY_MSGS + 10 messages simulating a busy sync feed.
        for i in 0..(MAX_MEMORY_MSGS + 10) {
            cache.append_memory("!r:s", make_msg(&format!("$ev{i}")));
        }

        let (msgs, _, _) = cache.get_memory("!r:s").unwrap();
        assert_eq!(msgs.len(), MAX_MEMORY_MSGS,
            "Vec must be capped at MAX_MEMORY_MSGS; got {}", msgs.len());
    }

    #[test]
    fn append_memory_keeps_newest_messages_after_trim() {
        let cache = RoomCache::new();
        cache.insert_memory_only("!r:s", Vec::new(), None, empty_meta());

        let total = MAX_MEMORY_MSGS + 5;
        for i in 0..total {
            cache.append_memory("!r:s", make_msg(&format!("$ev{i}")));
        }

        let (msgs, _, _) = cache.get_memory("!r:s").unwrap();
        // The oldest 5 should have been drained; the last should be $ev{total-1}.
        assert_eq!(msgs.last().unwrap().event_id, format!("$ev{}", total - 1));
        assert_eq!(msgs.first().unwrap().event_id, "$ev5");
    }

    #[test]
    fn append_memory_under_cap_does_not_trim() {
        let cache = RoomCache::new();
        cache.insert_memory_only("!r:s", Vec::new(), None, empty_meta());

        for i in 0..10 {
            cache.append_memory("!r:s", make_msg(&format!("$ev{i}")));
        }

        let (msgs, _, _) = cache.get_memory("!r:s").unwrap();
        assert_eq!(msgs.len(), 10);
    }

}
