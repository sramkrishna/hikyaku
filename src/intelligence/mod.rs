// Intelligence — local LLM integration via Ollama.
//
// Sends structured data to a local Ollama instance and returns a plain-text
// summary. No data leaves the machine.
//
// All functions run on the GLib main thread and use libsoup for HTTP.

pub mod gpu_detect;
pub mod ollama_manager;
#[cfg(feature = "ai")]
pub mod watcher;

use soup::prelude::*;

/// Pull a model via Ollama's /api/pull, reporting progress via callback.
///
/// `on_progress` is called with values in [0.0, 1.0] as layer data arrives.
/// Uses Ollama's streaming NDJSON response so each layer reports its own
/// `completed`/`total` bytes; we average across layers for an overall %.
pub async fn pull_model(
    endpoint: &str,
    model: &str,
    on_progress: impl Fn(f64),
) -> Result<(), String> {
    use soup::prelude::SessionExt;

    let url = format!("{}/api/pull", endpoint.trim_end_matches('/'));
    // Use stream:true so we get per-layer progress lines instead of one
    // big response at the end.
    let body = serde_json::to_vec(&serde_json::json!({ "name": model, "stream": true }))
        .map_err(|e| e.to_string())?;

    let session = soup::Session::new();
    session.set_timeout(600);
    let msg = soup::Message::new("POST", &url).map_err(|e| e.to_string())?;
    msg.set_request_body_from_bytes(
        Some("application/json"),
        Some(&glib::Bytes::from(&body)),
    );

    let stream = session
        .send_future(&msg, glib::Priority::DEFAULT)
        .await
        .map_err(|e| format!("Pull failed: {e}"))?;

    // Read NDJSON lines. Each line is a JSON object; layers that are
    // downloading have "completed" and "total" fields.
    let mut buf = Vec::<u8>::new();
    let chunk_size = 4096usize;
    loop {
        let chunk = stream
            .read_bytes_future(chunk_size, glib::Priority::DEFAULT)
            .await
            .map_err(|e| format!("Read failed: {e}"))?;
        if chunk.is_empty() { break; }
        buf.extend_from_slice(&chunk);

        // Process all complete lines in the buffer.
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line = buf.drain(..=pos).collect::<Vec<u8>>();
            if let Ok(s) = std::str::from_utf8(&line) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(s.trim()) {
                    if let (Some(completed), Some(total)) = (
                        v["completed"].as_f64(),
                        v["total"].as_f64(),
                    ) {
                        if total > 0.0 {
                            on_progress((completed / total).clamp(0.0, 1.0));
                        }
                    }
                }
            }
        }
    }

    if msg.status() == soup::Status::Ok {
        Ok(())
    } else {
        Err(format!("Pull failed: HTTP {:?}", msg.status()))
    }
}

/// Return the names of all models installed in Ollama (via GET /api/tags).
pub async fn list_models(endpoint: &str) -> Vec<String> {
    use soup::prelude::SessionExt;
    let url = format!("{}/api/tags", endpoint.trim_end_matches('/'));
    let session = soup::Session::new();
    session.set_timeout(10);
    let Ok(msg) = soup::Message::new("GET", &url) else { return vec![] };
    let Ok(bytes) = session.send_and_read_future(&msg, glib::Priority::DEFAULT).await else { return vec![] };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return vec![] };
    json["models"]
        .as_array()
        .map(|arr| arr.iter()
            .filter_map(|m| m["name"].as_str().map(str::to_string))
            .collect())
        .unwrap_or_default()
}

/// Delete a model from Ollama (via DELETE /api/delete).
pub async fn delete_model(endpoint: &str, model: &str) -> Result<(), String> {
    use soup::prelude::SessionExt;
    let url = format!("{}/api/delete", endpoint.trim_end_matches('/'));
    let body = serde_json::to_vec(&serde_json::json!({ "name": model }))
        .map_err(|e| e.to_string())?;
    let session = soup::Session::new();
    session.set_timeout(30);
    let msg = soup::Message::new("DELETE", &url).map_err(|e| e.to_string())?;
    msg.set_request_body_from_bytes(Some("application/json"), Some(&glib::Bytes::from(&body)));
    session.send_and_read_future(&msg, glib::Priority::DEFAULT)
        .await
        .map(|_| ())
        .map_err(|e| format!("Delete failed: {e}"))
}

