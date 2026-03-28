// Local message bookmark store.
//
// Bookmarks are personal "save for later" pins on individual messages.
// They live in ~/.local/share/hikyaku/bookmarks.json and are never
// sent to the Matrix server.

use std::path::PathBuf;
use std::sync::LazyLock;

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
}

impl BookmarkStore {
    fn new() -> Self {
        let mut path = glib::user_data_dir();
        path.push("hikyaku");
        let _ = std::fs::create_dir_all(&path);
        path.push("bookmarks.json");
        Self { path }
    }

    pub fn load(&self) -> Vec<BookmarkEntry> {
        let Ok(data) = std::fs::read(&self.path) else { return Vec::new() };
        serde_json::from_slice(&data).unwrap_or_default()
    }

    fn persist(&self, entries: &[BookmarkEntry]) {
        if let Ok(data) = serde_json::to_vec_pretty(entries) {
            let _ = std::fs::write(&self.path, data);
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
