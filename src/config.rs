use gio::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const APP_ID: &str = "me.ramkrishna.hikyaku";
pub const APP_NAME: &str = "Hikyaku";

const SCHEMA_ID: &str = "me.ramkrishna.hikyaku";

/// User-facing settings. A plain struct for passing around; backed by GSettings/dconf.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub rooms: RoomSettings,
    pub sync: SyncSettings,
    pub appearance: AppearanceSettings,
    pub ollama: OllamaSettings,
    pub plugins: PluginsSettings,
    pub watch: WatchSettings,
    /// Pinned contacts for @ completion: "Display Name|@user:server" entries.
    pub rolodex: Vec<String>,
}

/// Room interest watcher settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchSettings {
    pub enabled: bool,
    pub terms: Vec<String>,
    /// Cosine similarity threshold (0.0–1.0). Default 0.65.
    pub threshold: f64,
}

/// Runtime enable/disable for each plugin.
/// The AI plugin reuses ollama.enabled as its toggle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginsSettings {
    /// Whether the Rolodex contact book plugin is active.
    pub rolodex: bool,
    /// Whether the local message pinning plugin is active.
    pub pinning: bool,
    /// Whether the room topic change (MOTD) tracker is active.
    pub motd: bool,
    /// Whether the community health monitor is active (community-health feature).
    pub community_health: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OllamaSettings {
    pub endpoint: String,
    pub model: String,
    /// Whether AI summaries are enabled (Ctrl+click popover).
    pub enabled: bool,
    /// Whether the first-run AI setup dialog has been shown.
    pub setup_done: bool,
    /// Extra instructions appended to the room preview prompt.
    /// E.g. "Include the names of active participants. Describe the mood."
    pub room_preview_extra: String,
    /// Hidden: include conflict/spam detection in summaries.
    /// Enable via: gsettings set me.ramkrishna.hikyaku ai-detect-conflict true
    pub detect_conflict: bool,
    /// Hidden: include code-of-conduct signals in summaries.
    /// Enable via: gsettings set me.ramkrishna.hikyaku ai-detect-coc true
    pub detect_coc: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomSettings {
    pub pinned_rooms: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppearanceSettings {
    pub font_family: String,
    pub font_size: u32,
    pub tint_color: String,
    pub tint_opacity: f64,
    /// Gradient end color for the message area (empty = solid tint).
    pub tint_color2: String,
    pub sidebar_tint_color: String,
    pub sidebar_tint_opacity: f64,
    /// Gradient end color for the sidebar (empty = solid tint).
    pub sidebar_tint_color2: String,
    /// Color sender names by their Matrix user ID.
    pub colorize_nicks: bool,
    /// Highlight color for bookmarked messages (CSS hex, default amber).
    pub bookmark_highlight_color: String,
    /// Background tint color for new (unread) messages (CSS hex, default light blue).
    pub new_message_highlight_color: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncSettings {
    pub timeline_limit: u32,
    pub timeout_secs: u64,
}

/// Get a handle to the GSettings backend.
pub fn gsettings() -> gio::Settings {
    gio::Settings::new(SCHEMA_ID)
}

/// Load all settings from GSettings/dconf.
pub fn settings() -> Settings {
    let gs = gsettings();
    Settings {
        rooms: RoomSettings {
            pinned_rooms: gs.strv("pinned-rooms").iter().map(|s| s.to_string()).collect(),
        },
        sync: SyncSettings {
            timeline_limit: gs.int("timeline-limit").max(1) as u32,
            timeout_secs: gs.int("timeout-secs").max(1) as u64,
        },
        appearance: AppearanceSettings {
            font_family: gs.string("font-family").to_string(),
            font_size: gs.int("font-size").max(6) as u32,
            tint_color: gs.string("tint-color").to_string(),
            tint_opacity: gs.double("tint-opacity").clamp(0.0, 0.5),
            tint_color2: gs.string("tint-color2").to_string(),
            sidebar_tint_color: gs.string("sidebar-tint-color").to_string(),
            sidebar_tint_opacity: gs.double("sidebar-tint-opacity").clamp(0.0, 0.5),
            sidebar_tint_color2: gs.string("sidebar-tint-color2").to_string(),
            colorize_nicks: gs.boolean("colorize-nicks"),
            bookmark_highlight_color: gs.string("bookmark-highlight-color").to_string(),
            new_message_highlight_color: gs.string("new-message-highlight-color").to_string(),
        },
        ollama: OllamaSettings {
            endpoint: gs.string("ollama-endpoint").to_string(),
            model: gs.string("ollama-model").to_string(),
            enabled: gs.boolean("ollama-enabled"),
            setup_done: gs.boolean("ai-setup-done"),
            room_preview_extra: gs.string("room-preview-extra").to_string(),
            detect_conflict: gs.boolean("ai-detect-conflict"),
            detect_coc: gs.boolean("ai-detect-coc"),
        },
        plugins: PluginsSettings {
            rolodex: gs.boolean("plugin-rolodex-enabled"),
            pinning: gs.boolean("plugin-pinning-enabled"),
            motd: gs.boolean("plugin-motd-enabled"),
            community_health: gs.boolean("plugin-community-health-enabled"),
        },
        watch: WatchSettings {
            enabled: gs.boolean("watch-enabled"),
            terms: gs.strv("watch-terms").iter().map(|s| s.to_string()).collect(),
            threshold: gs.double("watch-threshold").clamp(0.0, 1.0),
        },
        rolodex: gs.strv("rolodex").iter().map(|s| s.to_string()).collect(),
    }
}

/// Save all settings to GSettings/dconf.
pub fn save_settings(settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    let gs = gsettings();
    let pinned: Vec<&str> = settings.rooms.pinned_rooms.iter().map(|s| s.as_str()).collect();
    gs.set_strv("pinned-rooms", pinned.as_slice())?;
    gs.set_int("timeline-limit", settings.sync.timeline_limit as i32)?;
    gs.set_int("timeout-secs", settings.sync.timeout_secs as i32)?;
    gs.set_string("font-family", &settings.appearance.font_family)?;
    gs.set_int("font-size", settings.appearance.font_size as i32)?;
    gs.set_string("tint-color", &settings.appearance.tint_color)?;
    gs.set_double("tint-opacity", settings.appearance.tint_opacity)?;
    gs.set_string("tint-color2", &settings.appearance.tint_color2)?;
    gs.set_string("sidebar-tint-color", &settings.appearance.sidebar_tint_color)?;
    gs.set_double("sidebar-tint-opacity", settings.appearance.sidebar_tint_opacity)?;
    gs.set_string("sidebar-tint-color2", &settings.appearance.sidebar_tint_color2)?;
    gs.set_boolean("colorize-nicks", settings.appearance.colorize_nicks)?;
    gs.set_string("bookmark-highlight-color", &settings.appearance.bookmark_highlight_color)?;
    gs.set_string("new-message-highlight-color", &settings.appearance.new_message_highlight_color)?;
    gs.set_string("ollama-endpoint", &settings.ollama.endpoint)?;
    gs.set_string("ollama-model", &settings.ollama.model)?;
    gs.set_boolean("ollama-enabled", settings.ollama.enabled)?;
    gs.set_boolean("ai-setup-done", settings.ollama.setup_done)?;
    gs.set_string("room-preview-extra", &settings.ollama.room_preview_extra)?;
    gs.set_boolean("ai-detect-conflict", settings.ollama.detect_conflict)?;
    gs.set_boolean("ai-detect-coc", settings.ollama.detect_coc)?;
    gs.set_boolean("plugin-rolodex-enabled", settings.plugins.rolodex)?;
    gs.set_boolean("plugin-pinning-enabled", settings.plugins.pinning)?;
    gs.set_boolean("plugin-motd-enabled", settings.plugins.motd)?;
    gs.set_boolean("plugin-community-health-enabled", settings.plugins.community_health)?;
    gs.set_boolean("watch-enabled", settings.watch.enabled)?;
    let watch_terms: Vec<&str> = settings.watch.terms.iter().map(|s| s.as_str()).collect();
    gs.set_strv("watch-terms", watch_terms.as_slice())?;
    gs.set_double("watch-threshold", settings.watch.threshold)?;
    let rolodex: Vec<&str> = settings.rolodex.iter().map(|s| s.as_str()).collect();
    gs.set_strv("rolodex", rolodex.as_slice())?;
    tracing::info!("Settings saved to dconf");
    Ok(())
}

/// Parse rolodex entries into (display_name, user_id) pairs.
pub fn parse_rolodex(entries: &[String]) -> Vec<(String, String)> {
    entries.iter()
        .filter_map(|e| {
            let (name, uid) = e.split_once('|')?;
            let name = name.trim().to_string();
            let uid = uid.trim().to_string();
            if name.is_empty() || uid.is_empty() { return None; }
            Some((name, uid))
        })
        .collect()
}

/// Directly write rolodex to gsettings (avoids a full settings round-trip).
pub fn set_rolodex(entries: &[String]) {
    let gs = gsettings();
    let v: Vec<&str> = entries.iter().map(|s| s.as_str()).collect();
    let _ = gs.set_strv("rolodex", v.as_slice());
}

/// Set pinned rooms directly (avoids a full settings round-trip).
pub fn set_pinned_rooms(rooms: &HashSet<String>) {
    let gs = gsettings();
    let pinned: Vec<&str> = rooms.iter().map(|s| s.as_str()).collect();
    let _ = gs.set_strv("pinned-rooms", pinned.as_slice());
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        // Just verify the struct shape is sensible.
        let s = Settings {
            rooms: RoomSettings { pinned_rooms: HashSet::new() },
            sync: SyncSettings { timeline_limit: 10, timeout_secs: 60 },
            appearance: AppearanceSettings {
                font_family: String::new(),
                font_size: 11,
                tint_color: String::new(),
                tint_opacity: 0.05,
                tint_color2: String::new(),
                sidebar_tint_color: String::new(),
                sidebar_tint_opacity: 0.05,
                sidebar_tint_color2: String::new(),
                colorize_nicks: false,
                bookmark_highlight_color: "#f5c542".to_string(),
                new_message_highlight_color: "#5B9BD5".to_string(),
            },
            ollama: OllamaSettings {
                endpoint: "http://localhost:11434".to_string(),
                model: "qwen2.5:3b".to_string(),
                enabled: true,
                setup_done: false,
                room_preview_extra: String::new(),
                detect_conflict: false,
                detect_coc: false,
            },
            plugins: PluginsSettings { rolodex: true, pinning: true, motd: true, community_health: false },
            rolodex: Vec::new(),
            watch: WatchSettings { enabled: false, terms: Vec::new(), threshold: 0.65 },
        };
        assert!(s.rooms.pinned_rooms.is_empty());
        assert_eq!(s.sync.timeline_limit, 10);
        assert_eq!(s.sync.timeout_secs, 60);
    }
}
