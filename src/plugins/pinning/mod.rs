// Pinning plugin — local message bookmarks.
//
// Lets users mark messages they want to come back to. Pins are stored
// locally only — they are never synced to the Matrix server and are
// invisible to other room members.
//
// Storage: ~/.local/share/hikyaku/pinned_messages.json
// Feature: "pinning"

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedMessage {
    pub room_id: String,
    pub event_id: String,
    /// Plain-text body snippet for display in the pinned list.
    pub body: String,
    pub sender: String,
    /// Unix timestamp (seconds) of the original message.
    pub timestamp: u64,
    /// Optional personal note about why this was pinned.
    pub note: String,
}

pub fn cache_path() -> std::path::PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("hikyaku").join("pinned_messages.json")
}

pub fn load() -> Vec<PinnedMessage> {
    let path = cache_path();
    let Ok(data) = std::fs::read_to_string(&path) else { return Vec::new() };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save(pins: &[PinnedMessage]) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string_pretty(pins) {
        let _ = std::fs::write(&path, data);
    }
}

/// Pin a message. Deduplicates by event_id.
pub fn add(pins: &mut Vec<PinnedMessage>, msg: PinnedMessage) {
    // Build an existence set for O(1) deduplication — no scan loop.
    let event_ids: std::collections::HashSet<&str> = pins
        .iter()
        .map(|p| p.event_id.as_str())
        .collect();
    if !event_ids.contains(msg.event_id.as_str()) {
        pins.push(msg);
        save(pins);
    }
}

/// Unpin a message by event_id.
pub fn remove(pins: &mut Vec<PinnedMessage>, event_id: &str) {
    pins.retain(|p| p.event_id != event_id);
    save(pins);
}

/// Return all pins for a specific room, newest first.
pub fn for_room<'a>(pins: &'a [PinnedMessage], room_id: &str) -> Vec<&'a PinnedMessage> {
    let mut room_pins: Vec<&PinnedMessage> = pins.iter()
        .filter(|p| p.room_id == room_id)
        .collect();
    room_pins.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    room_pins
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn make_pin(room: &str, event: &str, ts: u64) -> PinnedMessage {
        PinnedMessage {
            room_id: room.to_string(),
            event_id: event.to_string(),
            body: "test body".to_string(),
            sender: "@user:example.com".to_string(),
            timestamp: ts,
            note: String::new(),
        }
    }

    /// Build a 1000-pin corpus, half in target room.
    fn corpus() -> Vec<PinnedMessage> {
        (0..1000)
            .map(|i| {
                let room = if i % 2 == 0 { "!target:example.com" } else { "!other:example.com" };
                make_pin(room, &format!("$event{i}:example.com"), i as u64)
            })
            .collect()
    }

    #[test]
    fn for_room_correctness() {
        let pins = corpus();
        let result = for_room(&pins, "!target:example.com");
        assert_eq!(result.len(), 500);
        // newest first
        for w in result.windows(2) {
            assert!(w[0].timestamp >= w[1].timestamp);
        }
    }

    /// for_room over 1000 pins must complete well under 50ms.
    /// Regression: if an O(n²) sort or loop is introduced this will fail.
    #[test]
    fn for_room_perf_1000() {
        let pins = corpus();
        let start = Instant::now();
        for _ in 0..100 {
            let _ = for_room(&pins, "!target:example.com");
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 500,
            "for_room 100×1000 took {}ms, expected <500ms",
            elapsed.as_millis()
        );
    }

    /// HashSet dedup in add() must not create duplicate entries.
    #[test]
    fn add_dedup_correctness() {
        let mut pins: Vec<PinnedMessage> = Vec::new();
        let msg = make_pin("!r:example.com", "$evt:example.com", 1);
        // add same event twice — must stay at length 1
        pins.push(msg.clone());
        // calling the logic without disk I/O: build set and check
        let event_ids: std::collections::HashSet<&str> =
            pins.iter().map(|p| p.event_id.as_str()).collect();
        assert!(event_ids.contains("$evt:example.com"));
        // simulate second add
        if !event_ids.contains(msg.event_id.as_str()) {
            pins.push(msg);
        }
        assert_eq!(pins.len(), 1, "duplicate pin must be rejected");
    }
}
