// MOTD plugin — tracks room topic changes.
//
// Persists the last-seen topic per room. When the topic changes:
//   - If the user is currently in that room → toast notification.
//   - Otherwise → a changed-topic icon appears on the room row
//     in the sidebar until they visit the room.
//
// Storage: $XDG_DATA_HOME/hikyaku/motd_seen.json
// (~/.local/share/hikyaku/motd_seen.json natively, or
// ~/.var/app/me.ramkrishna.hikyaku/data/hikyaku/motd_seen.json in Flatpak)
// Feature: "motd"

use std::collections::HashMap;

pub fn cache_path() -> std::path::PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("hikyaku").join("motd_seen.json")
}

/// Load the persisted map of room_id → last-seen topic.
pub fn load() -> HashMap<String, String> {
    let path = cache_path();
    let Ok(data) = std::fs::read_to_string(&path) else { return HashMap::new() };
    serde_json::from_str(&data).unwrap_or_default()
}

/// Persist the map to disk.
pub fn save(map: &HashMap<String, String>) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string(map) {
        let _ = std::fs::write(&path, data);
    }
}

/// Compare `new_topic` against the stored last-seen value for `room_id`.
///
/// - If this is the first time we see a topic for this room, we store it
///   silently (no notification — the user has never seen a different one).
/// - If the topic differs from what was stored, we update the store and
///   return `true` so the caller can notify the user.
/// - If the topic is unchanged, returns `false`.
pub fn check_and_update(
    room_id: &str,
    new_topic: &str,
    cache: &mut HashMap<String, String>,
) -> bool {
    if new_topic.is_empty() {
        return false;
    }
    match cache.get(room_id) {
        None => {
            // First time seeing this room's topic — bootstrap, no alert.
            cache.insert(room_id.to_string(), new_topic.to_string());
            save(cache);
            false
        }
        Some(stored) if stored == new_topic => false,
        Some(_) => {
            cache.insert(room_id.to_string(), new_topic.to_string());
            save(cache);
            true
        }
    }
}

/// Mark the current topic as seen (called when the user opens the room).
/// Clears the "changed" state for the next visit.
pub fn mark_seen(room_id: &str, current_topic: &str, cache: &mut HashMap<String, String>) {
    if !current_topic.is_empty() {
        cache.insert(room_id.to_string(), current_topic.to_string());
        save(cache);
    }
}
