// Directory — process-wide lookup for users, rooms, and (eventually) spaces.
//
// Motivation: every widget that wants to turn a Matrix identifier into a
// human-readable string was reaching for its own cache. Matrix.to pills in
// message bodies needed a room resolver; the rolodex wanted a user resolver;
// the nick popover and user-info panel each held their own slice of member
// state. When the rooms-in-markdown lookup ran from the markup-worker thread
// (off the GTK main thread) a thread-local closure wasn't visible — fix #1
// moved rooms to a global cache. This module generalises that pattern so
// the same global serves users and spaces too.
//
// Threading: reads and writes go through RwLock so the worker thread can
// resolve names safely while the GTK thread is updating them. Writes are
// infrequent (once per sync); reads are per-bind-callback, so a read-heavy
// lock is the right shape.
//
// Not a GObject. GObjects live on the GTK thread only; we need worker-side
// lookups, which a GObject singleton could not serve. Widgets that want
// reactive behaviour can wrap the calls in their own notify handlers.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

/// Information we keep about a Matrix user. Extend as more call sites
/// outgrow their local caches.
#[derive(Clone, Debug, Default)]
pub struct UserInfo {
    /// Display name as last seen in any room we share with the user.
    /// Empty string means "unknown — fall back to the mxid".
    pub display_name: String,
    /// mxc:// URL for the user's avatar, if any. Download + cache lives
    /// elsewhere (window::avatar_cache); the mxc is here so any widget
    /// can request a fetch without hunting down the room member list.
    pub avatar_mxc: String,
}

/// Information we keep about a Matrix room.
#[derive(Clone, Debug, Default)]
pub struct RoomInfo {
    /// Display name shown in the room list.
    pub name: String,
    /// Canonical alias when set (`#name:server`), empty otherwise.
    pub canonical_alias: String,
    /// True when this room is actually a space (matrix.to pills can
    /// choose a different glyph for spaces in a later iteration).
    pub is_space: bool,
}

/// Back each map behind its own RwLock so a writer on one side (say, a
/// big member sync landing a wave of user updates) doesn't block a
/// pill-rendering read on the other side.
struct DirectoryInner {
    users: RwLock<HashMap<String, UserInfo>>,
    rooms: RwLock<HashMap<String, RoomInfo>>,
    /// Servers we've observed, keyed by hostname. The stored value is
    /// a "last seen" monotonic counter so autocompleters can rank
    /// recently-used servers first without needing a separate score.
    servers: RwLock<HashMap<String, u64>>,
    /// Monotonic "clock" for server observations. Incremented on every
    /// `observe_server` call; the value is stored on the server record
    /// so callers can sort by recency.
    server_tick: std::sync::atomic::AtomicU64,
}

static DIRECTORY: LazyLock<DirectoryInner> = LazyLock::new(|| DirectoryInner {
    users: RwLock::new(HashMap::new()),
    rooms: RwLock::new(HashMap::new()),
    servers: RwLock::new(HashMap::new()),
    server_tick: std::sync::atomic::AtomicU64::new(0),
});

// ── Users ────────────────────────────────────────────────────────────────

/// Look up a user by their Matrix ID (`@user:server`). Returns None when
/// we have no record; callers fall back to the mxid localpart.
pub fn user(user_id: &str) -> Option<UserInfo> {
    DIRECTORY.users.read().ok()?.get(user_id).cloned()
}

/// Convenience: just the display name, with mxid-localpart fallback.
/// Empty `user_id` yields empty string so caller doesn't have to guard.
pub fn user_display_name(user_id: &str) -> String {
    if user_id.is_empty() { return String::new(); }
    if let Some(info) = user(user_id) {
        if !info.display_name.is_empty() { return info.display_name; }
    }
    user_id.trim_start_matches('@').split(':').next().unwrap_or(user_id).to_string()
}

/// Record a user's display name. Empty names are dropped so an
/// unresolved member doesn't shadow a prior good name.
pub fn set_user_display_name(user_id: &str, name: &str) {
    if user_id.is_empty() || name.is_empty() { return; }
    if let Ok(mut map) = DIRECTORY.users.write() {
        let entry = map.entry(user_id.to_string()).or_default();
        entry.display_name = name.to_string();
    }
}

/// Record a user's avatar mxc URL. Empty mxc is dropped for the same
/// reason as empty display names.
pub fn set_user_avatar_mxc(user_id: &str, mxc: &str) {
    if user_id.is_empty() || mxc.is_empty() { return; }
    if let Ok(mut map) = DIRECTORY.users.write() {
        let entry = map.entry(user_id.to_string()).or_default();
        entry.avatar_mxc = mxc.to_string();
    }
}

// ── Rooms ────────────────────────────────────────────────────────────────

pub fn room(room_id: &str) -> Option<RoomInfo> {
    DIRECTORY.rooms.read().ok()?.get(room_id).cloned()
}

/// Display name for a room. Unlike users, rooms have no natural fallback
/// from the id (`!abc:server` carries no semantic meaning), so the
/// caller is expected to handle None explicitly.
pub fn room_name(room_id: &str) -> Option<String> {
    let info = room(room_id)?;
    if info.name.is_empty() { None } else { Some(info.name) }
}

pub fn set_room_name(room_id: &str, name: &str) {
    if room_id.is_empty() || name.is_empty() { return; }
    if let Ok(mut map) = DIRECTORY.rooms.write() {
        let entry = map.entry(room_id.to_string()).or_default();
        entry.name = name.to_string();
    }
}

pub fn set_room_alias(room_id: &str, alias: &str) {
    if room_id.is_empty() { return; }
    if let Ok(mut map) = DIRECTORY.rooms.write() {
        let entry = map.entry(room_id.to_string()).or_default();
        entry.canonical_alias = alias.to_string();
    }
}

/// Reverse lookup: alias → room id. Linear scan, small N (hundreds of
/// rooms). Only used by the matrix.to pill renderer when the link
/// points at an alias and we want to resolve to the display name via
/// the room id the alias points to.
pub fn room_id_for_alias(alias: &str) -> Option<String> {
    if alias.is_empty() { return None; }
    let map = DIRECTORY.rooms.read().ok()?;
    for (rid, info) in map.iter() {
        if info.canonical_alias == alias { return Some(rid.clone()); }
    }
    None
}

// ── Servers ──────────────────────────────────────────────────────────────

/// Extract the server (homeserver hostname) portion of a Matrix ID. Works
/// for user IDs (`@alice:matrix.org`), room IDs (`!abc:matrix.org`), and
/// aliases (`#room:matrix.org`). Returns None when the input lacks the
/// colon-server separator.
pub fn server_from_matrix_id(mxid: &str) -> Option<&str> {
    mxid.splitn(2, ':').nth(1).filter(|s| !s.is_empty())
}

/// Record that we've seen a given server. Bumps the server's "last seen"
/// tick so autocompletion / via-server ranking can prefer recently used
/// entries. Callers should feed any server name that appears in a user
/// id, room id, alias, or join attempt through here.
pub fn observe_server(name: &str) {
    if name.is_empty() { return; }
    let tick = DIRECTORY.server_tick
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if let Ok(mut map) = DIRECTORY.servers.write() {
        map.insert(name.to_string(), tick);
    }
}

/// Shorthand: observe the server portion of a Matrix ID if present.
pub fn observe_server_from_mxid(mxid: &str) {
    if let Some(server) = server_from_matrix_id(mxid) {
        observe_server(server);
    }
}

/// Return all known servers, sorted by recency (most recently observed
/// first). Used by the DM bar / join bar autocompleters.
pub fn servers_by_recency() -> Vec<String> {
    let Ok(map) = DIRECTORY.servers.read() else { return Vec::new(); };
    let mut pairs: Vec<(String, u64)> = map.iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1));
    pairs.into_iter().map(|(name, _)| name).collect()
}

// ── Testing support ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_display_name_falls_back_to_localpart() {
        assert_eq!(user_display_name("@alice:example.com"), "alice");
        assert_eq!(user_display_name(""), "");
    }

    #[test]
    fn user_display_name_uses_recorded_name() {
        set_user_display_name("@bob:example.com", "Bob Smith");
        assert_eq!(user_display_name("@bob:example.com"), "Bob Smith");
    }

    #[test]
    fn room_name_roundtrip() {
        set_room_name("!room1:example.com", "Standup");
        assert_eq!(room_name("!room1:example.com"), Some("Standup".into()));
        assert_eq!(room_name("!room2:example.com"), None);
    }

    #[test]
    fn alias_reverse_lookup() {
        set_room_name("!aliased:example.com", "Project Kai");
        set_room_alias("!aliased:example.com", "#kai:example.com");
        assert_eq!(
            room_id_for_alias("#kai:example.com").as_deref(),
            Some("!aliased:example.com"),
        );
    }

    #[test]
    fn server_extraction_from_matrix_ids() {
        assert_eq!(server_from_matrix_id("@alice:matrix.org"), Some("matrix.org"));
        assert_eq!(server_from_matrix_id("!abc:gnome.org"),     Some("gnome.org"));
        assert_eq!(server_from_matrix_id("#room:fedora.im"),    Some("fedora.im"));
        assert_eq!(server_from_matrix_id("nocolon"),            None);
        assert_eq!(server_from_matrix_id("alice:"),             None);
    }

    #[test]
    fn servers_ranked_by_recency() {
        observe_server("earliest.test");
        observe_server("middle.test");
        observe_server("latest.test");
        let ranked = servers_by_recency();
        // "latest" must come before "earliest" — most recent first.
        let latest = ranked.iter().position(|s| s == "latest.test").unwrap();
        let earliest = ranked.iter().position(|s| s == "earliest.test").unwrap();
        assert!(latest < earliest, "ranking wrong: {ranked:?}");
    }
}
