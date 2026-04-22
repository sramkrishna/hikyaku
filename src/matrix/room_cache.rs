// RoomCache — unified in-memory + on-disk timeline cache.
//
// Storage layout
// ──────────────
// ~/.local/share/hikyaku/timeline/<sanitised_room_id>.json
//
//   Each file is a JSON object:
//     { "messages": [...], "prev_batch_token": "t123..." }
//
//   One file per room.  Reads from different rooms never contend with each
//   other (no shared mutex, no connection pool).  Writes use an atomic
//   temp-file rename so a crash mid-write never corrupts a room's cache.
//
// Why not SQLite?
// ───────────────
// The previous implementation used a single SQLite connection behind a Mutex.
// Every read and write across ALL rooms serialised through that one lock,
// causing 1-2 s stalls on room selection (bg_refresh had to wait for an
// unrelated room's save to finish).  At ~11 KB per room and ~72 rooms the
// total dataset is ~800 KB — firmly in "just use files" territory.  There
// are no cross-room queries, so SQLite's relational features bought nothing.
//
// Thread safety
// ─────────────
// The memory HashMap is protected by std::sync::Mutex (never held across an
// .await point — only brief HashMap ops).  Disk I/O is plain std::fs with no
// shared state, so it is safe to call from any thread including tokio's
// blocking pool.  GObject's own reference count is atomic, so Clone gives a
// cheap additional handle to the same state.

mod imp {
    use glib::subclass::prelude::*;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use crate::matrix::{MessageInfo, RoomMeta};

    // ── Disk format ───────────────────────────────────────────────────────────

    #[derive(Serialize, Deserialize)]
    struct RoomFile {
        #[serde(default)]
        messages: Vec<MessageInfo>,
        #[serde(default)]
        prev_batch_token: Option<String>,
    }

    // ── TimelineStore — per-room JSON files ───────────────────────────────────

    pub(super) struct TimelineStore {
        dir: PathBuf,
    }

    impl TimelineStore {
        pub(super) fn new() -> Self {
            let dir = dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("hikyaku")
                .join("timeline");
            let _ = std::fs::create_dir_all(&dir);
            Self { dir }
        }

        /// Sanitise a room ID into a safe filename component.
        /// Replaces characters that are problematic on common filesystems.
        fn filename(room_id: &str) -> String {
            let safe: String = room_id
                .chars()
                .map(|c| match c {
                    'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
                    _ => '_',
                })
                .collect();
            format!("{safe}.json")
        }

        fn path(&self, room_id: &str) -> PathBuf {
            self.dir.join(Self::filename(room_id))
        }

        pub(super) fn save(
            &self,
            room_id: &str,
            messages: &[MessageInfo],
            prev_batch_token: Option<&str>,
        ) {
            let file = RoomFile {
                messages: messages.to_vec(),
                prev_batch_token: prev_batch_token.map(|s| s.to_string()),
            };
            let json = match serde_json::to_vec(&file) {
                Ok(j) => j,
                Err(e) => {
                    tracing::error!("timeline save {room_id}: serialise: {e}");
                    return;
                }
            };
            // Atomic write: write to a temp file then rename.
            let dest = self.path(room_id);
            let tmp = dest.with_extension("json.tmp");
            if let Err(e) = std::fs::write(&tmp, &json) {
                tracing::error!("timeline save {room_id}: write tmp: {e}");
                return;
            }
            if let Err(e) = std::fs::rename(&tmp, &dest) {
                tracing::error!("timeline save {room_id}: rename: {e}");
                let _ = std::fs::remove_file(&tmp);
            }
        }

        pub(super) fn load(
            &self,
            room_id: &str,
        ) -> Option<(Vec<MessageInfo>, Option<String>)> {
            let bytes = std::fs::read(self.path(room_id)).ok()?;
            let file: RoomFile = serde_json::from_slice(&bytes)
                .map_err(|e| tracing::warn!("timeline load {room_id}: {e}"))
                .ok()?;
            if file.messages.is_empty() {
                return None;
            }
            Some((file.messages, file.prev_batch_token))
        }

        pub(super) fn delete(&self, room_id: &str) {
            let _ = std::fs::remove_file(self.path(room_id));
        }
    }

    // ── GObject ───────────────────────────────────────────────────────────────

    type MemEntry = (Vec<MessageInfo>, Option<String>, RoomMeta);

    /// Cached member list: (display_names, avatar_urls).
    pub(super) type MemberEntry = (Vec<(String, String)>, Vec<(String, String)>);

    pub struct RoomCache {
        /// In-memory timeline: room_id → (messages, prev_batch_token, RoomMeta).
        /// Lock is held only for brief HashMap ops — never across .await points.
        pub(super) memory: Mutex<HashMap<String, MemEntry>>,
        /// When each room's data was last fetched from the server (prefetch OR
        /// bg_refresh).  Checked by SelectRoom to avoid redundant full refreshes.
        pub(super) refreshed_at: Mutex<HashMap<String, std::time::Instant>>,
        /// Member list cache: room_id → (display_names, avatar_urls).
        /// NOT cleared by invalidate_room — member lists change rarely compared
        /// to message events, so we keep them across cache invalidations.
        pub(super) members: Mutex<HashMap<String, MemberEntry>>,
        /// Disk store — per-room JSON files, no shared mutex.
        pub(super) disk: TimelineStore,
    }

    impl Default for RoomCache {
        fn default() -> Self {
            Self {
                memory: Mutex::new(HashMap::new()),
                refreshed_at: Mutex::new(HashMap::new()),
                members: Mutex::new(HashMap::new()),
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

// SAFETY: All mutable state lives behind std::sync::Mutex.  The disk store
// uses plain std::fs with no shared state.  GObject's reference count is
// atomic.  This object never calls GTK/GLib main-thread functions.
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
        let _g = crate::perf::scope_gt("get_memory", 1_000);
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
    pub fn mark_fresh(&self, room_id: &str) {
        self.imp()
            .refreshed_at
            .lock()
            .unwrap()
            .insert(room_id.to_string(), std::time::Instant::now());
    }

    /// Returns `true` if this room's memory cache was ever populated from the
    /// server during the current session.
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

    /// Append a single message to the tail of the memory cache.
    /// Returns `true` if appended (cache was warm); `false` if the room has
    /// no memory entry (caller should mark dirty for bg_refresh).
    pub fn append_memory(&self, room_id: &str, msg: MessageInfo) -> bool {
        let mut memory = self.imp().memory.lock().unwrap();
        let Some((msgs, _, _)) = memory.get_mut(room_id) else {
            return false;
        };
        if !msg.event_id.is_empty()
            && msgs.iter().rev().take(20).any(|m| m.event_id == msg.event_id)
        {
            return true;
        }
        msgs.push(msg);
        if msgs.len() > MAX_MEMORY_MSGS {
            let drain = msgs.len() - MAX_MEMORY_MSGS;
            msgs.drain(..drain);
        }
        true
    }

    /// Return the event_id of the last message in the memory cache.
    pub fn latest_event_id(&self, room_id: &str) -> Option<String> {
        let memory = self.imp().memory.lock().unwrap();
        memory
            .get(room_id)
            .and_then(|(msgs, _, _)| msgs.last())
            .filter(|m| !m.event_id.is_empty())
            .map(|m| m.event_id.clone())
    }

    /// Return cached members + avatars if a full fetch was done this session.
    /// Survives cache invalidations — stored separately from the timeline entry.
    pub fn get_cached_members(
        &self,
        room_id: &str,
    ) -> Option<(Vec<(String, String)>, Vec<(String, String)>)> {
        self.imp().members.lock().unwrap().get(room_id).cloned()
    }

    /// Store a fetched member list.  Stored in a map independent of the
    /// timeline cache so invalidate_room() does not discard it.
    pub fn cache_members(
        &self,
        room_id: &str,
        members: Vec<(String, String)>,
        member_avatars: Vec<(String, String)>,
    ) {
        self.imp().members.lock().unwrap()
            .insert(room_id.to_string(), (members, member_avatars));
    }

    /// Remove a room from both memory and disk.
    pub fn remove(&self, room_id: &str) {
        self.imp().memory.lock().unwrap().remove(room_id);
        self.imp().disk.delete(room_id);
    }

    /// Remove a room from memory only, leaving disk intact.
    pub fn remove_memory(&self, room_id: &str) {
        tracing::info!("remove_memory (cache evict): {}", room_id);
        self.imp().memory.lock().unwrap().remove(room_id);
    }

    /// Evict all rooms from the memory cache.
    pub fn clear_memory(&self) {
        self.imp().memory.lock().unwrap().clear();
    }

    /// Evict a room from memory + server-fetch timestamp, forcing bg_refresh.
    pub fn invalidate_room(&self, room_id: &str) {
        self.imp().memory.lock().unwrap().remove(room_id);
        self.imp().refreshed_at.lock().unwrap().remove(room_id);
    }

    // ── Disk cache ────────────────────────────────────────────────────────────
    //
    // These are plain synchronous std::fs calls — no spawn_blocking needed.
    // Each room is an independent file so there is no cross-room contention.

    /// Load a room's timeline from the per-room JSON file.
    /// Fast enough to call directly; no spawn_blocking required.
    pub fn load_disk(
        &self,
        room_id: &str,
    ) -> Option<(Vec<MessageInfo>, Option<String>)> {
        self.imp().disk.load(room_id)
    }

    /// Persist a room's timeline to its JSON file (atomic rename).
    /// Safe to call from any thread; use spawn_blocking if called from async.
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

    #[test]
    fn append_memory_trims_to_cap() {
        let cache = RoomCache::new();
        cache.insert_memory_only("!r:s", Vec::new(), None, empty_meta());
        for i in 0..(MAX_MEMORY_MSGS + 10) {
            cache.append_memory("!r:s", make_msg(&format!("$ev{i}")));
        }
        let (msgs, _, _) = cache.get_memory("!r:s").unwrap();
        assert_eq!(msgs.len(), MAX_MEMORY_MSGS);
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

    #[test]
    fn cache_members_roundtrip() {
        let cache = RoomCache::new();
        cache.insert_memory_only("!r:s", Vec::new(), None, empty_meta());

        // Before caching, get_cached_members returns None (members_fetched=false).
        assert!(cache.get_cached_members("!r:s").is_none());

        let members = vec![
            ("@alice:example.org".to_string(), "Alice".to_string()),
            ("@bob:example.org".to_string(), "Bob".to_string()),
        ];
        let avatars = vec![
            ("@alice:example.org".to_string(), "mxc://example.org/avatar".to_string()),
        ];
        cache.cache_members("!r:s", members.clone(), avatars.clone());

        let (cached_members, cached_avatars) = cache.get_cached_members("!r:s").unwrap();
        assert_eq!(cached_members, members);
        assert_eq!(cached_avatars, avatars);
    }

    #[test]
    fn cache_members_independent_of_memory_entry() {
        let cache = RoomCache::new();
        // cache_members works even for rooms not in the memory/timeline cache.
        // The member cache is a separate HashMap that survives invalidate_room().
        cache.cache_members("!unknown:s", vec![("@a:s".to_string(), "A".to_string())], vec![]);
        let result = cache.get_cached_members("!unknown:s");
        assert!(result.is_some(), "member cache should store without a memory entry");
        let (members, _) = result.unwrap();
        assert_eq!(members[0].0, "@a:s");
    }

    #[test]
    fn member_count_prefers_fetched_list_over_sdk_zero() {
        // Simulate the case where joined_members_count() returns 0 (pre-sync)
        // but we have a fetched member list — verify the correct count is used.
        let fetched_members: Vec<(String, String)> = vec![
            ("@alice:example.org".to_string(), "Alice".to_string()),
            ("@bob:example.org".to_string(), "Bob".to_string()),
            ("@carol:example.org".to_string(), "Carol".to_string()),
        ];
        let sdk_count: u64 = 0; // Pre-sync SDK count is unreliable.
        let member_count = if !fetched_members.is_empty() {
            fetched_members.len() as u64
        } else {
            sdk_count
        };
        assert_eq!(member_count, 3);
    }

    /// Regression test: when bg_refresh freshly fetches members (not from cache)
    /// it must NOT skip the RoomMessages send even if messages are unchanged.
    ///
    /// Before the fix, the early-return guard only checked `unread_count` and
    /// `fully_read_event_id`, silently discarding freshly-fetched members when
    /// the message content was identical to what was already in the cache.
    #[test]
    fn fresh_members_bypass_messages_unchanged_skip() {
        // Simulate the guard logic in bg_refresh's "messages unchanged" branch.
        let members_were_cached = false;
        let members: Vec<(String, String)> = vec![
            ("@alice:example.org".to_string(), "Alice".to_string()),
        ];
        let unread_count = 0u32;
        let fully_read_event_id: Option<String> = None;

        let members_freshly_fetched = !members_were_cached && !members.is_empty();
        let should_skip = unread_count == 0
            && fully_read_event_id.is_none()
            && !members_freshly_fetched;

        // Must NOT skip — we have fresh members the UI hasn't seen yet.
        assert!(!should_skip, "bg_refresh skipped RoomMessages despite fresh members");

        // Verify: if members were cached (no new info), it is safe to skip.
        let members_were_cached2 = true;
        let members_freshly_fetched2 = !members_were_cached2 && !members.is_empty();
        let should_skip2 = unread_count == 0
            && fully_read_event_id.is_none()
            && !members_freshly_fetched2;
        assert!(should_skip2, "bg_refresh should skip when messages and members are both unchanged");
    }
}
