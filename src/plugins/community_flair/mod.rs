// community-flair plugin — reusable labels you attach to users.
//
// Use case: you want to categorise contacts ("downstream distro",
// "upstream maintainer", "GNOME Foundation", "work", "family") and
// have a small pill appear next to their name in every room. The flair
// text is the category, the notes field (community-safety) carries the
// specifics of who the person is; the two plugins are complementary.
//
// Data model:
//   - Flair = { id, name, color }.  Stored once in the flairs list.
//   - Per-user assignment = user_id → flair_id (at most one flair
//     per user). Stored separately so renaming / recolouring a flair
//     updates every user who carries it in one write.
//
// Storage:
//   $XDG_DATA_HOME/hikyaku/flairs.json          ← the flairs library
//   $XDG_DATA_HOME/hikyaku/user_flairs.json     ← user_id → flair_id
//
// Storage rationale: these are session-local personal labels, not
// synced to Matrix and not exposed to other users — same shape as
// community-safety flags. They live in data_dir (not config_dir) so
// they sit alongside the other local user state.
//
// Feature: "community-flair"

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Flair {
    /// Stable 32-bit id. Monotonic counter stored in the flairs file.
    pub id: u32,
    pub name: String,
    /// Hex string like `#66a0ea`. Validated at set-time — malformed
    /// strings are coerced to the default pill colour on render.
    pub color: String,
}

#[derive(Serialize, Deserialize, Default)]
struct FlairsFile {
    flairs: Vec<Flair>,
    /// Next id to hand out. Not `flairs.len()` because deletions
    /// leave gaps we must not reuse — per-user assignments would
    /// silently retarget to a different flair if the id got recycled.
    next_id: u32,
}

pub struct FlairStore {
    path_flairs: PathBuf,
    path_user_flairs: PathBuf,
    inner: RwLock<Inner>,
}

#[derive(Default)]
struct Inner {
    flairs: HashMap<u32, Flair>,
    /// Monotonic id counter; never reused even across deletions.
    next_id: u32,
    /// user_id → flair_id. At most one flair per user — notes carry
    /// the "who they are" detail so we don't need multi-flair.
    user_flairs: HashMap<String, u32>,
}

fn flairs_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("hikyaku").join("flairs.json")
}

fn user_flairs_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("hikyaku").join("user_flairs.json")
}

impl FlairStore {
    pub fn new() -> Self {
        Self::new_at(flairs_path(), user_flairs_path())
    }

    /// Construct a store at explicit paths. Production code uses
    /// `new()` (which routes to data_dir); tests use this to keep
    /// disk state isolated to a tempdir.
    pub fn new_at(path_flairs: PathBuf, path_user_flairs: PathBuf) -> Self {

        let mut inner = Inner::default();

        // sync-io-ok: session-local store loaded once at startup;
        // flairs.json is typically a few entries and user_flairs.json
        // scales with how many people the user has tagged.
        if let Ok(data) = std::fs::read_to_string(&path_flairs) {
            if let Ok(file) = serde_json::from_str::<FlairsFile>(&data) {
                inner.next_id = file.next_id;
                for f in file.flairs {
                    // Keep next_id monotonic across corruption too —
                    // if any stored id sits above next_id, advance.
                    if f.id >= inner.next_id {
                        inner.next_id = f.id.saturating_add(1);
                    }
                    inner.flairs.insert(f.id, f);
                }
            }
        }
        // sync-io-ok: see above.
        if let Ok(data) = std::fs::read_to_string(&path_user_flairs) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, u32>>(&data) {
                // Drop assignments pointing at flairs that no longer
                // exist — happens if the user edits the files by hand
                // or after a clear_flair call that didn't complete.
                inner.user_flairs = map.into_iter()
                    .filter(|(_, id)| inner.flairs.contains_key(id))
                    .collect();
            }
        }

        Self {
            path_flairs,
            path_user_flairs,
            inner: RwLock::new(inner),
        }
    }

    fn persist_flairs(&self, inner: &Inner) {
        if let Some(parent) = self.path_flairs.parent() {
            // sync-io-ok: user-triggered flair CRUD, not a hot path.
            let _ = std::fs::create_dir_all(parent);
        }
        let file = FlairsFile {
            flairs: inner.flairs.values().cloned().collect(),
            next_id: inner.next_id,
        };
        if let Ok(data) = serde_json::to_string_pretty(&file) {
            // sync-io-ok: same as above.
            let _ = std::fs::write(&self.path_flairs, data);
        }
    }

    fn persist_user_flairs(&self, inner: &Inner) {
        if let Some(parent) = self.path_user_flairs.parent() {
            // sync-io-ok: user-triggered assignment change.
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string_pretty(&inner.user_flairs) {
            // sync-io-ok: same as above.
            let _ = std::fs::write(&self.path_user_flairs, data);
        }
    }

    /// Every flair in the library, in stable id order so UIs don't
    /// shuffle entries between renders.
    pub fn list_flairs(&self) -> Vec<Flair> {
        let inner = self.inner.read().unwrap();
        let mut out: Vec<Flair> = inner.flairs.values().cloned().collect();
        out.sort_by_key(|f| f.id);
        out
    }

    pub fn get_flair(&self, id: u32) -> Option<Flair> {
        self.inner.read().unwrap().flairs.get(&id).cloned()
    }

    /// Look up the flair attached to a user — `None` when unassigned
    /// OR when the assignment points at a flair that's since been
    /// deleted (load-time filter drops orphans, but this guard also
    /// covers the in-memory race between delete_flair and the UI).
    pub fn get_user_flair(&self, user_id: &str) -> Option<Flair> {
        let inner = self.inner.read().unwrap();
        let id = inner.user_flairs.get(user_id).copied()?;
        inner.flairs.get(&id).cloned()
    }

    /// Create a new flair and return its id. Names and colors are
    /// normalised (trimmed; color defaults to #66a0ea if not a valid
    /// `#rrggbb` or `#rgb` hex string).
    pub fn create_flair(&self, name: &str, color: &str) -> u32 {
        let mut inner = self.inner.write().unwrap();
        let id = inner.next_id;
        inner.next_id = id.saturating_add(1);
        inner.flairs.insert(id, Flair {
            id,
            name: name.trim().to_string(),
            color: normalise_color(color),
        });
        self.persist_flairs(&inner);
        id
    }

    /// Update an existing flair in place. No-op if the id isn't in the
    /// library. Users carrying this flair automatically see the new
    /// rendering on the next bind — no per-user write needed.
    pub fn update_flair(&self, id: u32, name: &str, color: &str) {
        let mut inner = self.inner.write().unwrap();
        if let Some(f) = inner.flairs.get_mut(&id) {
            f.name = name.trim().to_string();
            f.color = normalise_color(color);
            self.persist_flairs(&inner);
        }
    }

    /// Delete a flair and clear it from every user it was assigned to.
    pub fn delete_flair(&self, id: u32) {
        let mut inner = self.inner.write().unwrap();
        let removed = inner.flairs.remove(&id).is_some();
        let before = inner.user_flairs.len();
        inner.user_flairs.retain(|_, v| *v != id);
        let after = inner.user_flairs.len();
        if removed { self.persist_flairs(&inner); }
        if before != after { self.persist_user_flairs(&inner); }
    }

    /// Assign a flair to a user, or clear the assignment with `None`.
    /// Silently ignored when `flair_id` refers to a missing flair so
    /// the UI doesn't have to double-check against `list_flairs`.
    pub fn set_user_flair(&self, user_id: &str, flair_id: Option<u32>) {
        let mut inner = self.inner.write().unwrap();
        match flair_id {
            Some(id) if inner.flairs.contains_key(&id) => {
                inner.user_flairs.insert(user_id.to_string(), id);
            }
            None => {
                inner.user_flairs.remove(user_id);
            }
            _ => return, // missing id — no-op
        }
        self.persist_user_flairs(&inner);
    }
}

/// Process-wide singleton. Lazily initialised on first access.
pub static FLAIR_STORE: LazyLock<FlairStore> = LazyLock::new(FlairStore::new);

/// Curated swatch palette for dark mode — saturated mid-tones that
/// stand out against the dark sidebar / message body without being
/// eye-searing, while staying dark enough for white pill text to
/// pass WCAG AA. Order is roughly hue-cycled (blue → green → orange
/// → red → purple → pink → teal → gray).
pub const DARK_PALETTE: &[&str] = &[
    "#62a0ea", // blue
    "#57e389", // green
    "#ffa348", // orange
    "#f66151", // red
    "#c061cb", // purple
    "#dc8add", // pink
    "#62afd5", // teal
    "#9a9996", // gray
];

/// Curated swatch palette for light mode — deeper / more saturated
/// versions of the same hues so pills still pop against a white
/// surface and white pill text reads cleanly.
pub const LIGHT_PALETTE: &[&str] = &[
    "#1c71d8", // blue
    "#26a269", // green
    "#c64600", // orange
    "#a51d2d", // red
    "#813d9c", // purple
    "#cb2c8a", // pink
    "#1f7c8c", // teal
    "#5e5c64", // gray
];

/// Pick the palette appropriate to the current adw style mode.
/// Caller is responsible for rebuilding the swatch row when the
/// user toggles dark/light — we don't auto-listen here because the
/// flair editor only renders when the user-info panel opens, and
/// we just re-check the mode on each open.
pub fn current_palette() -> &'static [&'static str] {
    let manager = adw::StyleManager::default();
    if manager.is_dark() { DARK_PALETTE } else { LIGHT_PALETTE }
}

/// Coerce a user-supplied color to a valid `#rrggbb` or `#rgb` hex
/// string. Falls back to a readable blue (#66a0ea) when the input
/// doesn't match — the pill still renders; the user sees the default
/// colour and can correct in the editor.
pub fn normalise_color(input: &str) -> String {
    let s = input.trim();
    if is_valid_hex_color(s) {
        s.to_string()
    } else {
        "#66a0ea".to_string()
    }
}

fn is_valid_hex_color(s: &str) -> bool {
    // Accept `#rgb`, `#rrggbb`, `#rrggbbaa`.
    let bytes = s.as_bytes();
    if !matches!(bytes.first(), Some(b'#')) { return false; }
    let rest = &bytes[1..];
    if !matches!(rest.len(), 3 | 6 | 8) { return false; }
    rest.iter().all(|c| c.is_ascii_hexdigit())
}

/// Build the Pango markup pill for a flair. Uses `size="smaller"`
/// so the pill tracks the user's base font preference one step down —
/// identical mechanism to how GTK scales dim-label captions. NBSP
/// padding gives the pill visual breathing room against adjacent text.
///
/// Pure — unit-testable without a live GObject tree.
pub fn flair_markup(flair: &Flair) -> String {
    let escaped = escape_pango_attr(&flair.name);
    let color = if is_valid_hex_color(&flair.color) {
        flair.color.as_str()
    } else {
        "#66a0ea"
    };
    format!(
        "<span size=\"smaller\" foreground=\"#ffffff\" background=\"{color}\">\u{a0}{escaped}\u{a0}</span>"
    )
}

fn escape_pango_attr(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_hex_colors_accepted() {
        assert!(is_valid_hex_color("#fff"));
        assert!(is_valid_hex_color("#ffaa00"));
        assert!(is_valid_hex_color("#ffaa00ff"));
    }

    #[test]
    fn invalid_hex_colors_rejected() {
        assert!(!is_valid_hex_color(""));
        assert!(!is_valid_hex_color("fff"));          // missing #
        assert!(!is_valid_hex_color("#ff"));          // wrong length
        assert!(!is_valid_hex_color("#gghhii"));      // non-hex digit
        assert!(!is_valid_hex_color("blue"));
    }

    #[test]
    fn normalise_color_passes_valid_through() {
        assert_eq!(normalise_color("#ff0000"), "#ff0000");
        assert_eq!(normalise_color("  #abc  "), "#abc");
    }

    #[test]
    fn normalise_color_falls_back_on_garbage() {
        assert_eq!(normalise_color("not a color"), "#66a0ea");
        assert_eq!(normalise_color(""), "#66a0ea");
    }

    #[test]
    fn markup_escapes_html_in_name() {
        let f = Flair {
            id: 1,
            name: "a & <b>".to_string(),
            color: "#112233".to_string(),
        };
        let m = flair_markup(&f);
        assert!(m.contains("a &amp; &lt;b&gt;"));
        assert!(!m.contains("a & <b>"), "raw markup should not leak");
    }

    #[test]
    fn markup_uses_size_smaller_for_font_scaling() {
        let f = Flair {
            id: 1,
            name: "downstream distro".to_string(),
            color: "#66a0ea".to_string(),
        };
        let m = flair_markup(&f);
        assert!(m.contains("size=\"smaller\""),
            "must use Pango 'smaller' so pill scales with user font");
    }

    #[test]
    fn markup_falls_back_to_default_color_on_garbage() {
        let f = Flair {
            id: 1,
            name: "x".to_string(),
            color: "garbage".to_string(),
        };
        let m = flair_markup(&f);
        assert!(m.contains("#66a0ea"),
            "bad color stored should still render; default covers it");
    }

    // ── store CRUD (edit + delete features) ──────────────────────────

    /// Build a FlairStore against unique tempdir paths so tests don't
    /// touch the user's real flair library.
    fn temp_store() -> FlairStore {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("hikyaku-flair-test-{pid}-{n}"));
        let _ = std::fs::create_dir_all(&dir);
        FlairStore::new_at(
            dir.join("flairs.json"),
            dir.join("user_flairs.json"),
        )
    }

    #[test]
    fn create_then_update_renames_and_recolours() {
        let s = temp_store();
        let id = s.create_flair("coworker", "#3584e4");
        s.update_flair(id, "ex-coworker", "#a51d2d");
        let f = s.get_flair(id).expect("flair must still exist");
        assert_eq!(f.name, "ex-coworker");
        assert_eq!(f.color, "#a51d2d");
    }

    #[test]
    fn update_flair_normalises_garbage_color() {
        let s = temp_store();
        let id = s.create_flair("x", "#3584e4");
        s.update_flair(id, "x", "definitely not a hex");
        let f = s.get_flair(id).expect("flair must exist");
        assert_eq!(f.color, "#66a0ea", "garbage colour should fall back to default");
    }

    #[test]
    fn update_missing_flair_is_noop() {
        let s = temp_store();
        // Library empty, id=42 doesn't exist — must not panic.
        s.update_flair(42, "ghost", "#3584e4");
        assert!(s.get_flair(42).is_none());
    }

    #[test]
    fn delete_flair_removes_from_library() {
        let s = temp_store();
        let id = s.create_flair("temp", "#3584e4");
        assert!(s.get_flair(id).is_some());
        s.delete_flair(id);
        assert!(s.get_flair(id).is_none());
    }

    #[test]
    fn delete_flair_unassigns_every_user() {
        let s = temp_store();
        let id = s.create_flair("workmate", "#26a269");
        s.set_user_flair("@alice:example.org", Some(id));
        s.set_user_flair("@bob:example.org", Some(id));
        assert!(s.get_user_flair("@alice:example.org").is_some());
        assert!(s.get_user_flair("@bob:example.org").is_some());
        s.delete_flair(id);
        assert!(s.get_user_flair("@alice:example.org").is_none(),
            "delete must unassign all carriers");
        assert!(s.get_user_flair("@bob:example.org").is_none(),
            "delete must unassign all carriers");
    }

    #[test]
    fn delete_missing_flair_is_noop() {
        let s = temp_store();
        s.delete_flair(99);
        assert!(s.list_flairs().is_empty());
    }

    #[test]
    fn ids_are_not_reused_across_deletes() {
        let s = temp_store();
        let a = s.create_flair("a", "#3584e4");
        s.delete_flair(a);
        let b = s.create_flair("b", "#26a269");
        assert_ne!(a, b, "next_id must keep advancing — re-use would silently \
                          retarget stale per-user assignments to a new flair");
    }

    // ── theme-aware palette ──────────────────────────────────────────

    #[test]
    fn dark_palette_entries_are_all_valid_hex() {
        for hex in DARK_PALETTE {
            assert!(is_valid_hex_color(hex),
                "DARK_PALETTE entry {hex} is not a valid hex");
        }
    }

    #[test]
    fn light_palette_entries_are_all_valid_hex() {
        for hex in LIGHT_PALETTE {
            assert!(is_valid_hex_color(hex),
                "LIGHT_PALETTE entry {hex} is not a valid hex");
        }
    }

    #[test]
    fn palettes_are_not_trivial() {
        assert!(DARK_PALETTE.len() >= 4, "need a meaningful range of dark choices");
        assert!(LIGHT_PALETTE.len() >= 4, "need a meaningful range of light choices");
    }

    #[test]
    fn palettes_have_distinct_colors() {
        let dark: std::collections::HashSet<_> = DARK_PALETTE.iter().collect();
        assert_eq!(dark.len(), DARK_PALETTE.len(),
            "duplicate colour in dark palette would waste a swatch slot");
        let light: std::collections::HashSet<_> = LIGHT_PALETTE.iter().collect();
        assert_eq!(light.len(), LIGHT_PALETTE.len(),
            "duplicate colour in light palette would waste a swatch slot");
    }
}
