// Community-safety plugin — local, private "caution" flags on user ids.
//
// Tracks users the local user has marked as problematic (anti-trans,
// racist, harassing, etc.) so their messages get a visible amber pill
// next to the sender name. Stored as JSON at
//   $XDG_DATA_HOME/hikyaku/flagged_users.json
// which resolves to ~/.local/share/hikyaku/flagged_users.json on a
// native install and ~/.var/app/me.ramkrishna.hikyaku/data/hikyaku/
// flagged_users.json inside the Flatpak sandbox. Never sent to Matrix.
//
// v1 is deliberately local-first:
//   * No outgoing network traffic. Nothing is shared unless the user
//     explicitly exports the file.
//   * One severity level for MVP (caution). Severity ladder and
//     import/export land in follow-up commits.
//   * Category is stored as a free-form short tag; built-in labels
//     live in the UI layer so translations can evolve without schema
//     changes.
//
// See the tracked-issue for the full roadmap (Mjolnir integration,
// preferences pane with search/edit, collapsed-body severity-3
// warnings, JSON import/export).

use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct FlagEntry {
    pub user_id: String,
    /// Short category tag: "transphobic", "racist", "harassment",
    /// "scam", "custom", or anything the user types in the dialog
    /// (free-form so translations and new categories don't require
    /// a schema change).
    pub category: String,
    /// Free-form reason / note. Shown in tooltip and preferences list.
    #[serde(default)]
    pub reason: String,
    /// Unix seconds of the original flag.
    pub created_at: u64,
}

pub struct FlaggedStore {
    path: PathBuf,
    /// In-memory cache keyed by user_id. Populated on first access;
    /// afterwards always reflects disk state. Writes go to both.
    cache: RwLock<Option<std::collections::HashMap<String, FlagEntry>>>,
}

impl FlaggedStore {
    fn new() -> Self {
        let mut path = glib::user_data_dir();
        path.push("hikyaku");
        let _ = std::fs::create_dir_all(&path);
        path.push("flagged_users.json");
        Self { path, cache: RwLock::new(None) }
    }

    /// Load from disk on first access; cached afterwards.
    fn load(&self) -> std::collections::HashMap<String, FlagEntry> {
        if let Ok(guard) = self.cache.read() {
            if let Some(ref map) = *guard {
                return map.clone();
            }
        }
        let map: std::collections::HashMap<String, FlagEntry> = std::fs::read(&self.path)
            .ok()
            .and_then(|data| {
                // Stored as a Vec<FlagEntry> for easier human editing /
                // diffing, hydrated into a HashMap in memory.
                serde_json::from_slice::<Vec<FlagEntry>>(&data).ok()
            })
            .map(|v| v.into_iter().map(|e| (e.user_id.clone(), e)).collect())
            .unwrap_or_default();
        if let Ok(mut guard) = self.cache.write() {
            *guard = Some(map.clone());
        }
        map
    }

    fn persist(&self, map: &std::collections::HashMap<String, FlagEntry>) {
        let mut entries: Vec<&FlagEntry> = map.values().collect();
        entries.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        if let Ok(data) = serde_json::to_vec_pretty(&entries) {
            let _ = std::fs::write(&self.path, data);
        }
        if let Ok(mut guard) = self.cache.write() {
            *guard = Some(map.clone());
        }
    }

    /// Insert or replace a flag. `category` and `reason` overwrite any
    /// existing entry so the latest flag wins. Returns the stored entry.
    pub fn flag(&self, user_id: &str, category: &str, reason: &str) -> FlagEntry {
        let mut map = self.load();
        let entry = FlagEntry {
            user_id: user_id.to_string(),
            category: category.to_string(),
            reason: reason.to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs()).unwrap_or(0),
        };
        map.insert(user_id.to_string(), entry.clone());
        self.persist(&map);
        entry
    }

    /// Remove the flag for `user_id`. No-op if not flagged.
    pub fn unflag(&self, user_id: &str) {
        let mut map = self.load();
        if map.remove(user_id).is_some() {
            self.persist(&map);
        }
    }

    /// O(1) lookup. Returns None if not flagged.
    pub fn get(&self, user_id: &str) -> Option<FlagEntry> {
        self.load().get(user_id).cloned()
    }

    /// Return every flag record sorted by user_id (for the future
    /// Preferences pane / import/export tooling).
    pub fn list_all(&self) -> Vec<FlagEntry> {
        let map = self.load();
        let mut v: Vec<FlagEntry> = map.into_values().collect();
        v.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        v
    }
}

pub static FLAGGED_STORE: LazyLock<FlaggedStore> = LazyLock::new(FlaggedStore::new);
