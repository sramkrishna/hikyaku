// Ollama lifecycle management.
//
// Detection priority:
//   1. Configured endpoint already responding        → Running
//   2. Flatpak extension at /app/extensions/Ollama   → start it
//   3. Managed binary in $XDG_DATA_HOME/hikyaku      → start it
//   4. System `ollama` on $PATH (non-Flatpak)        → start it
//   5. In Flatpak, no binary                         → NeedDownload
//   6. Otherwise                                     → NotAvailable
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

/// Path to the Ollama binary provided by the flatpak extension.
pub fn extension_binary_path() -> PathBuf {
    PathBuf::from("/app/extensions/Ollama/bin/ollama")
}

/// Detect the current availability of Ollama given the configured endpoint.
pub async fn detect(endpoint: &str) -> OllamaStatus {
    if is_endpoint_reachable(endpoint).await {
        return OllamaStatus::Running { endpoint: endpoint.to_string() };
    }

    if in_flatpak() {
        let ext = extension_binary_path();
        if ext.exists() {
            return OllamaStatus::Found { path: ext };
        }
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
    // HOME must be a writable directory; always use the managed data dir
    // regardless of whether the binary came from the extension or a download.
    let home_dir = managed_models_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));

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

/// Download the Ollama Linux binary to the managed path.
/// `progress` is called with values in [0.0, 1.0].
/// Used in Flatpak where the system Ollama is not available.
pub async fn download_ollama_binary<F>(mut progress: F) -> Result<PathBuf, String>
where
    F: FnMut(f64) + 'static,
{
    let url = "https://github.com/ollama/ollama/releases/latest/download/ollama-linux-amd64";
    let dest = managed_binary_path();

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directories: {e}"))?;
    }

    let session = soup::Session::new();
    session.set_timeout(0); // no timeout for large download
    let msg = soup::Message::new("GET", url)
        .map_err(|e| format!("Bad URL: {e}"))?;

    // Get the response input stream.
    let stream = session
        .send_future(&msg, glib::Priority::DEFAULT)
        .await
        .map_err(|e| format!("Download failed: {e}"))?;

    // Read content-length for progress (may be absent or -1).
    let content_len = msg
        .response_headers()
        .map(|h| h.content_length())
        .filter(|&n| n > 0)
        .map(|n| n as u64)
        .unwrap_or(0);

    let mut bytes_read: u64 = 0;
    let chunk_size: usize = 65536;
    let mut data: Vec<u8> = Vec::new();

    loop {
        let buf = stream
            .read_bytes_future(chunk_size, glib::Priority::DEFAULT)
            .await
            .map_err(|e| format!("Read error: {e}"))?;
        if buf.is_empty() { break; }
        bytes_read += buf.len() as u64;
        data.extend_from_slice(&buf);
        if content_len > 0 {
            progress((bytes_read as f64) / (content_len as f64));
        } else {
            progress(-1.0); // indeterminate
        }
    }

    std::fs::write(&dest, &data)
        .map_err(|e| format!("Write failed: {e}"))?;

    // Make the binary executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)
            .map_err(|e| format!("stat failed: {e}"))?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)
            .map_err(|e| format!("chmod failed: {e}"))?;
    }

    tracing::info!("Ollama binary downloaded to {}", dest.display());
    Ok(dest)
}
