use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

pub const APP_ID: &str = "com.github.matx";
pub const APP_NAME: &str = "Matx";

/// User-facing settings loaded from ~/.config/matx/config.toml.
///
/// All fields have sensible defaults so the file is optional — Matx works
/// out of the box. Users can create/edit the file to tune behaviour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub rooms: RoomSettings,
    pub sync: SyncSettings,
    pub appearance: AppearanceSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomSettings {
    /// Maximum number of DMs to show in the sidebar.
    pub max_dms: usize,
    /// Maximum number of rooms (non-DM) to show in the sidebar.
    pub max_rooms: usize,
    /// Room IDs that are pinned (e.g. friend DMs you always want visible).
    pub pinned_rooms: std::collections::HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// Font family for message text (e.g. "Cantarell", "Monospace").
    pub font_family: String,
    /// Font size in points for message text.
    pub font_size: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncSettings {
    /// Number of timeline events per room during sync.
    pub timeline_limit: u32,
    /// Sync timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            font_family: String::new(), // empty = system default
            font_size: 11,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rooms: RoomSettings::default(),
            sync: SyncSettings::default(),
            appearance: AppearanceSettings::default(),
        }
    }
}

impl Default for RoomSettings {
    fn default() -> Self {
        Self {
            max_dms: 50,
            max_rooms: 100,
            pinned_rooms: std::collections::HashSet::new(),
        }
    }
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            timeline_limit: 10,
            timeout_secs: 60,
        }
    }
}

fn config_file_path() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("matx");
    path.push("config.toml");
    path
}

/// Load settings from disk, falling back to defaults.
fn load_settings() -> Settings {
    let path = config_file_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(settings) => {
                tracing::info!("Loaded settings from {}", path.display());
                settings
            }
            Err(e) => {
                tracing::warn!("Failed to parse {}: {e}, using defaults", path.display());
                Settings::default()
            }
        },
        Err(_) => {
            tracing::info!("No config file at {}, using defaults", path.display());
            Settings::default()
        }
    }
}

/// Global settings instance. Loaded once at startup.
static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Get the global settings (loads from disk on first call).
pub fn settings() -> &'static Settings {
    SETTINGS.get_or_init(load_settings)
}

/// Save settings to the config file. Changes take effect on next launch.
pub fn save_settings(settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let toml_str = toml::to_string_pretty(settings)?;
    std::fs::write(&path, toml_str)?;
    tracing::info!("Settings saved to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let s = Settings::default();
        assert_eq!(s.rooms.max_dms, 50);
        assert_eq!(s.rooms.max_rooms, 100);
        assert!(s.rooms.pinned_rooms.is_empty());
        assert_eq!(s.sync.timeline_limit, 10);
        assert_eq!(s.sync.timeout_secs, 60);
    }

    #[test]
    fn test_toml_round_trip() {
        let original = Settings {
            rooms: RoomSettings {
                max_dms: 25,
                max_rooms: 200,
                pinned_rooms: ["!room1:matrix.org".to_string(), "!room2:matrix.org".to_string()].into(),
            },
            sync: SyncSettings {
                timeline_limit: 10,
                timeout_secs: 120,
            },
            appearance: AppearanceSettings {
                font_family: "Monospace".into(),
                font_size: 14,
            },
        };
        let toml_str = toml::to_string_pretty(&original).unwrap();
        let parsed: Settings = toml::from_str(&toml_str).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_partial_toml_uses_defaults() {
        let toml_str = "[rooms]\nmax_dms = 10\n";
        let s: Settings = toml::from_str(toml_str).unwrap();
        assert_eq!(s.rooms.max_dms, 10);
        // Unspecified fields should use defaults.
        assert_eq!(s.rooms.max_rooms, 100);
        assert_eq!(s.sync.timeline_limit, 10);
        assert_eq!(s.sync.timeout_secs, 60);
    }

    #[test]
    fn test_empty_toml_uses_all_defaults() {
        let s: Settings = toml::from_str("").unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn test_malformed_toml_errors() {
        let result = toml::from_str::<Settings>("not valid toml {{{{");
        assert!(result.is_err());
    }

    #[test]
    fn test_extra_keys_ignored() {
        let toml_str = "[rooms]\nmax_dms = 30\nfuture_setting = true\n";
        let s: Settings = toml::from_str(toml_str).unwrap();
        assert_eq!(s.rooms.max_dms, 30);
    }
}
