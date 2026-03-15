use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

pub const APP_ID: &str = "com.github.matx";
pub const APP_NAME: &str = "Matx";

/// User-facing settings loaded from ~/.config/matx/config.toml.
///
/// All fields have sensible defaults so the file is optional — Matx works
/// out of the box. Users can create/edit the file to tune behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub rooms: RoomSettings,
    pub sync: SyncSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomSettings {
    /// Maximum number of DMs to show in the sidebar.
    pub max_dms: usize,
    /// Maximum number of rooms (non-DM) to show in the sidebar.
    pub max_rooms: usize,
    /// Room IDs that are pinned (e.g. friend DMs you always want visible).
    pub pinned_rooms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncSettings {
    /// Number of timeline events per room during sync.
    pub timeline_limit: u32,
    /// Sync timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rooms: RoomSettings::default(),
            sync: SyncSettings::default(),
        }
    }
}

impl Default for RoomSettings {
    fn default() -> Self {
        Self {
            max_dms: 50,
            max_rooms: 100,
            pinned_rooms: Vec::new(),
        }
    }
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            timeline_limit: 1,
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
