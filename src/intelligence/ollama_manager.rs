// Ollama lifecycle management.
//
// Detection priority:
//   1. Configured endpoint already responding   → Running
//   2. System `ollama` on $PATH (non-Flatpak)   → start it
//   3. Managed binary in $XDG_DATA_HOME/hikyaku  → start it
//   4. In Flatpak, no binary                     → NeedDownload
//   5. Otherwise                                  → NotAvailable
//
// All async functions run on the GLib main thread and use libsoup for HTTP.

use soup::prelude::*;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static MANAGED_PROCESS: OnceLock<Mutex<Option<std::process::Child>>> = OnceLock::new();

fn process_lock() -> &'static Mutex<Option<std::process::Child>> {
    MANAGED_PROCESS.get_or_init(|| Mutex::new(None))
}

/// Current availability state of Ollama.
#[derive(Debug, Clone, PartialEq)]
pub enum OllamaStatus {
    /// Endpoint is responding — external or our own managed process.
    Running { endpoint: String },
    /// Found a binary but it is not yet started.
    Found { path: PathBuf },
    /// In Flatpak, managed binary not yet downloaded.
    NeedDownload,
    /// No binary found; not in a context where we can download.
    NotAvailable,
}

impl OllamaStatus {
    /// User-facing description for the Preferences status row.
    pub fn label(&self) -> String {
        match self {
            OllamaStatus::Running { endpoint } => format!("Connected — {endpoint}"),
            OllamaStatus::Found { path } => format!("Found — {}", path.display()),
            OllamaStatus::NeedDownload => "Not installed — download available".to_string(),
            OllamaStatus::NotAvailable => "Not found — install Ollama to enable AI summaries".to_string(),
        }
    }
}

/// True when running inside a Flatpak sandbox.
pub fn in_flatpak() -> bool {
    std::env::var("FLATPAK_ID").is_ok()
}

/// Path where the app manages its own Ollama binary (used in Flatpak).
pub fn managed_binary_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("hikyaku").join("ollama").join("bin").join("ollama")
}

/// Models directory for the managed Ollama instance.
pub fn managed_models_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("hikyaku").join("ollama").join("models")
}

/// Probe the given endpoint with a short timeout.
pub async fn is_endpoint_reachable(endpoint: &str) -> bool {
    let url = format!("{}/api/tags", endpoint.trim_end_matches('/'));
    let session = soup::Session::new();
    session.set_timeout(2);
    let Ok(msg) = soup::Message::new("GET", &url) else { return false };
    session.send_and_read_future(&msg, glib::Priority::DEFAULT).await.is_ok()
}

/// Find `ollama` on $PATH (non-Flatpak installs).
fn which_ollama() -> Option<PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    std::env::split_paths(&path_var)
        .map(|dir| dir.join("ollama"))
        .find(|p| p.is_file())
}

/// Detect the current availability of Ollama given the configured endpoint.
pub async fn detect(endpoint: &str) -> OllamaStatus {
    if is_endpoint_reachable(endpoint).await {
        return OllamaStatus::Running { endpoint: endpoint.to_string() };
    }

    let managed = managed_binary_path();
    if managed.exists() {
        return OllamaStatus::Found { path: managed };
    }

    if !in_flatpak() {
        if let Some(path) = which_ollama() {
            return OllamaStatus::Found { path };
        }
    }

    if in_flatpak() {
        return OllamaStatus::NeedDownload;
    }

    OllamaStatus::NotAvailable
}

/// Ensure Ollama is running. Starts the local binary if needed.
/// Returns the effective endpoint to use, or None if unavailable.
pub async fn ensure_running(configured_endpoint: &str) -> Option<String> {
    match detect(configured_endpoint).await {
        OllamaStatus::Running { endpoint } => Some(endpoint),
        OllamaStatus::Found { path } => start_binary(&path, configured_endpoint).await,
        OllamaStatus::NeedDownload | OllamaStatus::NotAvailable => None,
    }
}

/// Spawn the binary and wait up to 5 s for it to become reachable.
async fn start_binary(binary: &std::path::Path, endpoint: &str) -> Option<String> {
    let host = endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://");

    let models_dir = managed_models_path();
    let home_dir = managed_binary_path()
        .parent()?.parent()?.to_path_buf();

    tracing::info!("Starting managed Ollama: {}", binary.display());

    let child = std::process::Command::new(binary)
        .arg("serve")
        .env("OLLAMA_HOST", host)
        .env("OLLAMA_MODELS", &models_dir)
        .env("HOME", &home_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    if let Ok(mut guard) = process_lock().lock() {
        *guard = Some(child);
    }

    // Poll until reachable (max 5 s) using GLib timers.
    for _ in 0..10 {
        glib::timeout_future(std::time::Duration::from_millis(500)).await;
        if is_endpoint_reachable(endpoint).await {
            tracing::info!("Ollama is now reachable at {endpoint}");
            return Some(endpoint.to_string());
        }
    }

    tracing::warn!("Ollama did not become reachable within 5 s");
    None
}

/// Kill the managed Ollama process if we started one. Safe to call on exit.
pub fn stop() {
    let Ok(mut guard) = process_lock().lock() else { return };
    if let Some(mut child) = guard.take() {
        let _ = child.kill();
        let _ = child.wait();
        tracing::info!("Managed Ollama stopped");
    }
}
