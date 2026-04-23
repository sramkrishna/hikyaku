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

/// Canonical category tags. Stored values are strings so users can
/// keep hand-edited entries beyond this list, but the UI surfaces
/// these as the curated set.
pub const CATEGORY_INTERACTIVE: &str = "interactive";
pub const CATEGORY_RACE: &str       = "race";
pub const CATEGORY_LGBTQ: &str      = "lgbtq";
pub const CATEGORY_A11Y: &str       = "a11y";
pub const CATEGORY_FAR_RIGHT: &str  = "far-right";

/// Human-readable label for a known category tag. Returns the tag
/// itself for unknown / custom tags.
pub fn category_label(tag: &str) -> &str {
    match tag {
        CATEGORY_INTERACTIVE => "Interactive issues",
        CATEGORY_RACE        => "Race",
        CATEGORY_LGBTQ       => "LGBTQ+",
        CATEGORY_A11Y        => "Accessibility",
        CATEGORY_FAR_RIGHT   => "Far-right / Nazi",
        _                    => tag,
    }
}

/// All built-in tags, in the order the Flag dialog dropdown should
/// present them. `custom` isn't included here — the UI uses an
/// empty-text-entry fallback for user-specified categories.
pub const BUILTIN_CATEGORIES: &[&str] = &[
    CATEGORY_INTERACTIVE,
    CATEGORY_RACE,
    CATEGORY_LGBTQ,
    CATEGORY_A11Y,
    CATEGORY_FAR_RIGHT,
];

/// Severity ladder. Drives colour/icon in the message row and
/// eventually the collapsed-body warning at severity 3.
///   1 — Note   : subtle yellow dot; doesn't interrupt reading.
///   2 — Caution: amber pill with category label (current default).
///   3 — Warning: red pill with category; future follow-up will
///                collapse the message body behind a content warning.
pub const SEVERITY_NOTE: u8 = 1;
pub const SEVERITY_CAUTION: u8 = 2;
pub const SEVERITY_WARNING: u8 = 3;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct FlagEntry {
    pub user_id: String,
    /// Short category tag for an active flag. Curated values are the
    /// `CATEGORY_*` constants above; free-form strings also work for
    /// custom entries. Empty string means "not flagged, but may
    /// still have notes" — the record lives in the store so notes
    /// survive unflag/reflag cycles without being dropped.
    #[serde(default)]
    pub category: String,
    /// Severity level (1-3). 0 is the serde default for old records
    /// written before this field existed — `effective_severity()`
    /// promotes those to SEVERITY_CAUTION so legacy flags render
    /// the same as before.
    #[serde(default)]
    pub severity: u8,
    /// Free-form reason / note for the flag specifically.
    /// Distinct from `notes` below which is user-agnostic of flag state.
    #[serde(default)]
    pub reason: String,
    /// Optional evidence link — a matrix.to URL to a specific message
    /// or DM that supports the flag. Surfaced in the user-info dialog
    /// as a clickable link so you can jump back to the behaviour that
    /// triggered the flag.
    #[serde(default)]
    pub evidence: String,
    /// Free-form notes about the user, independent of flag state.
    /// Typed in the user-info dialog; shown there and surfaced as a
    /// small "has notes" indicator next to the sender label.
    #[serde(default)]
    pub notes: String,
    /// Unix seconds of the original record creation.
    #[serde(default)]
    pub created_at: u64,
    /// Unix seconds of the last mutation (flag / notes edit).
    #[serde(default)]
    pub updated_at: u64,
}

impl FlagEntry {
    pub fn is_flagged(&self) -> bool { !self.category.is_empty() }
    pub fn has_notes(&self) -> bool { !self.notes.is_empty() }
    /// True when the record is effectively empty and can be dropped
    /// from the store entirely (neither flagged nor has notes).
    pub fn is_empty(&self) -> bool {
        self.category.is_empty() && self.notes.is_empty()
    }
    /// Severity with back-compat: a stored zero (legacy records
    /// written before the field existed, or deserialisation default)
    /// is treated as SEVERITY_CAUTION so existing flags render
    /// unchanged.
    pub fn effective_severity(&self) -> u8 {
        match self.severity {
            0 => SEVERITY_CAUTION,
            s @ 1..=3 => s,
            _ => SEVERITY_CAUTION,
        }
    }
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

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Set the flag on `user_id` with full category/severity/reason/
    /// evidence. Preserves any existing notes. Returns the stored entry.
    pub fn set_flag_full(
        &self,
        user_id: &str,
        category: &str,
        severity: u8,
        reason: &str,
        evidence: &str,
    ) -> FlagEntry {
        let mut map = self.load();
        let now = Self::now();
        let entry = map.entry(user_id.to_string()).or_insert_with(|| FlagEntry {
            user_id: user_id.to_string(),
            created_at: now,
            ..Default::default()
        });
        entry.category = category.to_string();
        entry.severity = severity;
        entry.reason = reason.to_string();
        entry.evidence = evidence.to_string();
        entry.updated_at = now;
        let result = entry.clone();
        self.persist(&map);
        result
    }

    /// Compatibility wrapper: older callers that just pass
    /// category + reason get SEVERITY_CAUTION and no evidence link.
    pub fn set_flag(&self, user_id: &str, category: &str, reason: &str) -> FlagEntry {
        self.set_flag_full(user_id, category, SEVERITY_CAUTION, reason, "")
    }

    /// Clear the flag on `user_id` (not the notes). If the record has
    /// neither a flag nor notes afterwards, it's dropped entirely.
    pub fn clear_flag(&self, user_id: &str) {
        let mut map = self.load();
        let Some(entry) = map.get_mut(user_id) else { return };
        entry.category.clear();
        entry.reason.clear();
        entry.updated_at = Self::now();
        let is_empty = entry.is_empty();
        if is_empty {
            map.remove(user_id);
        }
        self.persist(&map);
    }

    /// Set or clear free-form notes on `user_id`. Preserves any
    /// existing flag. An empty `notes` string removes the notes and
    /// drops the record entirely if the user was not flagged.
    pub fn set_notes(&self, user_id: &str, notes: &str) {
        let mut map = self.load();
        let now = Self::now();
        let entry = map.entry(user_id.to_string()).or_insert_with(|| FlagEntry {
            user_id: user_id.to_string(),
            created_at: now,
            ..Default::default()
        });
        entry.notes = notes.to_string();
        entry.updated_at = now;
        let is_empty = entry.is_empty();
        if is_empty {
            map.remove(user_id);
        }
        self.persist(&map);
    }

    /// Back-compat wrapper: used by the message-row one-click flag
    /// toggle. Sets a flag with the given category/reason.
    pub fn flag(&self, user_id: &str, category: &str, reason: &str) -> FlagEntry {
        self.set_flag(user_id, category, reason)
    }

    /// Back-compat wrapper: fully removes any record for `user_id`.
    /// Clears both flag and notes.
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
