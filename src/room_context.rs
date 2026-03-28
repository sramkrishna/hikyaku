// room_context.rs — Persistent context overrides for rooms and spaces.
//
// Each room or space carries a tri-state media-preview setting:
//   Inherit   — no preference; defer to parent space, then global default.
//   NoMedia   — suppress inline media and URL image previews.
//   ShowMedia — explicitly show media (overrides a space-level NoMedia).
//
// Context chain: room.ctx → parent_space.ctx → global default (ShowMedia).
//
// Overrides are stored in a JSON file (sparse — only explicit overrides present).
// The in-memory cache is loaded once at startup and updated on every write,
// so sync cycles never hit disk.

use gtk::glib;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

// ── Tri-state enum ────────────────────────────────────────────────────────────

/// Tri-state context value stored on every RoomObject (room or space).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "MxCtxValue")]
pub enum CtxValue {
    /// Inherit from parent space; if no parent, use the global default.
    #[default]
    Inherit,
    /// Suppress inline media and URL image previews.
    NoMedia,
    /// Explicitly show media (can override a space-level NoMedia).
    ShowMedia,
}

impl CtxValue {
    fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::NoMedia,
            2 => Self::ShowMedia,
            _ => Self::Inherit,
        }
    }

    fn to_i32(self) -> i32 {
        match self {
            Self::Inherit => 0,
            Self::NoMedia => 1,
            Self::ShowMedia => 2,
        }
    }
}

// ── In-memory cache (loaded once; updated on write) ───────────────────────────

static CACHE: Mutex<Option<HashMap<String, CtxValue>>> = Mutex::new(None);

fn context_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hikyaku")
        .join("room_context.json")
}

/// Ensure the cache is populated, loading from disk only if needed.
fn ensure_loaded(cache: &mut HashMap<String, CtxValue>) {
    let Ok(data) = std::fs::read_to_string(context_path()) else { return };
    // Stored as HashMap<String, i32> for forward-compatibility.
    if let Ok(raw) = serde_json::from_str::<HashMap<String, i32>>(&data) {
        for (id, v) in raw {
            let c = CtxValue::from_i32(v);
            if c != CtxValue::Inherit {
                cache.insert(id, c);
            }
        }
    }
}

/// Read the in-memory cache, loading from disk on first call.
pub fn load() -> HashMap<String, CtxValue> {
    let mut guard = CACHE.lock().unwrap();
    if guard.is_none() {
        let mut map = HashMap::new();
        ensure_loaded(&mut map);
        *guard = Some(map);
    }
    guard.as_ref().unwrap().clone()
}

/// Set an override for one room or space and flush to disk.
/// `Inherit` removes the entry (room reverts to space/global default).
pub fn save_override(id: &str, value: CtxValue) {
    let path = context_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut guard = CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);

    if value == CtxValue::Inherit {
        cache.remove(id);
    } else {
        cache.insert(id.to_string(), value);
    }

    // Persist as HashMap<String, i32>.
    let raw: HashMap<&str, i32> = cache.iter().map(|(k, &v)| (k.as_str(), v.to_i32())).collect();
    if let Ok(json) = serde_json::to_string(&raw) {
        let _ = std::fs::write(&path, json);
    }
}

// ── Context resolution ────────────────────────────────────────────────────────

/// Resolve the effective no_media flag for `room_id`.
/// Chain: room override → parent space override → false (global default: show media).
pub fn resolve_no_media(
    room_id: &str,
    registry: &HashMap<String, crate::models::RoomObject>,
) -> bool {
    let Some(room) = registry.get(room_id) else { return false };
    match room.ctx_no_media() {
        CtxValue::NoMedia => return true,
        CtxValue::ShowMedia => return false,
        CtxValue::Inherit => {}
    }
    let sid = room.parent_space_id();
    if !sid.is_empty() {
        if let Some(space) = registry.get(&sid) {
            return space.ctx_no_media() == CtxValue::NoMedia;
        }
    }
    false
}

/// Apply all persisted overrides to RoomObjects after a registry rebuild.
/// Called once per RoomListUpdated — never hits disk (uses in-memory cache).
pub fn apply_to_registry(
    registry: &HashMap<String, crate::models::RoomObject>,
) {
    let overrides = load();
    for (id, &value) in &overrides {
        if let Some(obj) = registry.get(id) {
            obj.set_ctx_no_media(value);
        }
    }
}
