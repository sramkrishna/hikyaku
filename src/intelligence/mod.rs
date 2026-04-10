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
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
struct OllamaRequest<'a> {
    model: &'a str,
    messages: Vec<OllamaMessage<'a>>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct OllamaMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: String,
}

/// Ask the local Ollama instance to summarize metrics data.
/// Auto-starts a local Ollama binary if the endpoint is not yet reachable.
/// `detect_conflict` and `detect_coc` add hidden moderation analysis layers
/// (enabled via gsettings, not shown in UI).
/// Returns None if Ollama is unavailable or returns an error.
pub async fn summarize_metrics(
    endpoint: &str,
    model: &str,
    metrics_text: &str,
    detect_conflict: bool,
    detect_coc: bool,
) -> Option<String> {
    let endpoint = ollama_manager::ensure_running(endpoint).await?;
    let url = format!("{}/api/chat", endpoint.trim_end_matches('/'));

    let mut sections = vec![
        "1. Interesting conversations: identify threads or exchanges that show \
         genuine knowledge-sharing, creative problem-solving, or topics of broad \
         community interest. Note who contributed and what made it notable.".to_string(),
    ];
    if detect_conflict {
        sections.push(
            "2. Conflict and spam signals: flag any patterns of escalating \
             disagreement between users, unusually high message frequency from \
             single users, or repetitive content that may indicate spam. \
             Be specific about user counts and message volumes.".to_string(),
        );
    }
    if detect_coc {
        sections.push(
            "3. Code-of-conduct signals: note ban/kick events, users who have \
             been actioned more than once, or time windows with unusual \
             moderation activity. This is for community-health review only.".to_string(),
        );
    }

    let task_list = sections.join("\n");
    let prompt = format!(
        "You are a community manager assistant analyzing Matrix room activity. \
         Review the metrics below and provide a concise report (3-5 bullet points \
         per section) covering:\n{task_list}\n\nBe specific about numbers. \
         If data is insufficient to draw a conclusion for a section, say so briefly.\n\
         \nMetrics:\n{metrics_text}"
    );

    let request = OllamaRequest {
        model,
        messages: vec![OllamaMessage { role: "user", content: &prompt }],
        stream: false,
    };

    let body = serde_json::to_vec(&request).ok()?;
    let bytes = soup_post_json(&url, &body, 120).await?;
    let resp: OllamaResponse = serde_json::from_slice(&bytes).ok()?;
    Some(resp.message.content)
}

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

/// POST JSON to url, return response body. timeout_secs: 0 = no timeout.
async fn soup_post_json(url: &str, body: &[u8], timeout_secs: u32) -> Option<glib::Bytes> {
    let session = soup::Session::new();
    if timeout_secs > 0 { session.set_timeout(timeout_secs); }
    let msg = soup::Message::new("POST", url).ok()?;
    let bytes = glib::Bytes::from(body);
    msg.set_request_body_from_bytes(Some("application/json"), Some(&bytes));
    session.send_and_read_future(&msg, glib::Priority::DEFAULT).await.ok()
}
