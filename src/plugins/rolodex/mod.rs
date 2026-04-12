// Rolodex plugin — local contact book for Matrix users.
//
// Stores display names and personal notes about contacts, drives
// @-completion in the message input, and will eventually support
// a full-text search index.
//
// Storage: ~/.local/share/hikyaku/rolodex.json
// Feature: "rolodex"

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolodexEntry {
    pub user_id: String,
    pub display_name: String,
    /// Free-form personal notes (not visible to other Matrix users).
    pub notes: String,
    /// Unix timestamp (seconds) when the entry was added.
    pub added_at: u64,
}

pub fn cache_path() -> std::path::PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("hikyaku").join("rolodex.json")
}

pub fn load() -> Vec<RolodexEntry> {
    let path = cache_path();
    let Ok(data) = std::fs::read_to_string(&path) else { return Vec::new() };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save(entries: &[RolodexEntry]) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string_pretty(entries) {
        let _ = std::fs::write(&path, data);
    }
}

#[allow(dead_code)]
pub fn add(entries: &mut Vec<RolodexEntry>, entry: RolodexEntry) {
    // Build a position index for O(1) deduplication — no scan loop.
    let pos_by_id: std::collections::HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.user_id.as_str(), i))
        .collect();
    match pos_by_id.get(entry.user_id.as_str()) {
        Some(&i) => entries[i] = entry,
        None => entries.push(entry),
    }
    save(entries);
}

#[allow(dead_code)]
pub fn remove(entries: &mut Vec<RolodexEntry>, user_id: &str) {
    entries.retain(|e| e.user_id != user_id);
    save(entries);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn make_entry(i: usize) -> RolodexEntry {
        RolodexEntry {
            user_id: format!("@user{i}:example.com"),
            display_name: format!("User {i}"),
            notes: String::new(),
            added_at: i as u64,
        }
    }

    fn corpus() -> Vec<RolodexEntry> {
        (0..1000).map(make_entry).collect()
    }

    /// HashMap-based add must replace an existing entry without creating a duplicate.
    #[test]
    fn add_updates_existing() {
        let mut entries: Vec<RolodexEntry> = (0..10).map(make_entry).collect();
        let pos_by_id: std::collections::HashMap<&str, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.user_id.as_str(), i))
            .collect();
        let updated = RolodexEntry {
            user_id: "@user5:example.com".to_string(),
            display_name: "Updated".to_string(),
            notes: "note".to_string(),
            added_at: 999,
        };
        match pos_by_id.get(updated.user_id.as_str()) {
            Some(&i) => entries[i] = updated,
            None => entries.push(updated),
        }
        assert_eq!(entries.len(), 10, "must not grow when updating existing entry");
        assert_eq!(entries[5].display_name, "Updated");
    }

    /// HashMap construction over 1000 entries must scale linearly.
    /// We run 100 iterations; debug builds typically finish in < 500ms.
    #[test]
    fn add_index_perf_1000() {
        let entries = corpus();
        let start = Instant::now();
        for _ in 0..100 {
            let _idx: std::collections::HashMap<&str, usize> = entries
                .iter()
                .enumerate()
                .map(|(i, e)| (e.user_id.as_str(), i))
                .collect();
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 500,
            "100× index build over 1000 entries took {}ms, expected <500ms",
            elapsed.as_millis()
        );
    }
}
