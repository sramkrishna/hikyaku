// Local message bookmark store.
//
// Bookmarks are personal "save for later" pins on individual messages.
// They live under $XDG_DATA_HOME/hikyaku/bookmarks.json (that is,
// ~/.local/share/hikyaku/bookmarks.json natively or
// ~/.var/app/me.ramkrishna.hikyaku/data/hikyaku/bookmarks.json inside
// the Flatpak sandbox) and are never sent to the Matrix server.

use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct BookmarkEntry {
    pub room_id: String,
    pub room_name: String,
    pub event_id: String,
    pub sender: String,
    pub body_preview: String,
    /// Unix seconds of the original message.
    pub timestamp: u64,
}

pub struct BookmarkStore {
    path: PathBuf,
    /// In-memory cache — avoids synchronous file reads on the GTK thread.
    /// None until first load; afterwards always reflects disk state.
    cache: RwLock<Option<Vec<BookmarkEntry>>>,
}

impl BookmarkStore {
    fn new() -> Self {
        let mut path = glib::user_data_dir();
        path.push("hikyaku");
        let _ = std::fs::create_dir_all(&path);
        path.push("bookmarks.json");
        Self { path, cache: RwLock::new(None) }
    }

    /// Load all bookmarks.  First call reads from disk; subsequent calls return
    /// the in-memory cache (O(1) — no file I/O on the GTK thread).
    pub fn load(&self) -> Vec<BookmarkEntry> {
        // Fast path: cache populated.
        if let Ok(guard) = self.cache.read() {
            if let Some(ref entries) = *guard {
                return entries.clone();
            }
        }
        // Slow path: read from disk and populate the cache.
        let entries: Vec<BookmarkEntry> = std::fs::read(&self.path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        if let Ok(mut guard) = self.cache.write() {
            *guard = Some(entries.clone());
        }
        entries
    }

    fn persist(&self, entries: &[BookmarkEntry]) {
        if let Ok(data) = serde_json::to_vec_pretty(entries) {
            let _ = std::fs::write(&self.path, data);
        }
        // Update in-memory cache to match.
        if let Ok(mut guard) = self.cache.write() {
            *guard = Some(entries.to_vec());
        }
    }

    /// Add a bookmark, ignoring duplicates by event_id. Returns updated list.
    pub fn add(&self, entry: BookmarkEntry) -> Vec<BookmarkEntry> {
        let mut entries = self.load();
        if !entries.iter().any(|e| e.event_id == entry.event_id) {
            entries.push(entry);
            self.persist(&entries);
        }
        entries
    }

    /// Remove the bookmark with the given event_id. Returns updated list.
    pub fn remove(&self, event_id: &str) -> Vec<BookmarkEntry> {
        let mut entries = self.load();
        entries.retain(|e| e.event_id != event_id);
        self.persist(&entries);
        entries
    }
}

pub static BOOKMARK_STORE: LazyLock<BookmarkStore> = LazyLock::new(BookmarkStore::new);
