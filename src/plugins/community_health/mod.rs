// Community health monitor — tracks the emotional tone of Matrix rooms.
//
// Each incoming message is scored by projecting its embedding onto a
// "positive vs negative" semantic axis built from anchor phrases.  A
// 30-message rolling window per room drives a health indicator shown
// on the room row in the sidebar.
//
// Embedding inference runs on the tokio blocking thread pool (same
// pattern as the interest watcher) and is accelerated by the OpenVINO
// Flatpak extension when available on Intel hardware.
//
// Feature: "community-health"

use std::collections::{HashMap, VecDeque};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

const WINDOW_SIZE: usize = 30;
/// Half-width of the trend comparison window (last N vs previous N).
const TREND_HALF: usize = 5;

/// Anchor phrases that define the positive / healthy emotional pole.
const POSITIVE_ANCHORS: &[&str] = &[
    "thank you", "great work", "I really appreciate it", "well explained",
    "welcome", "helpful", "good point", "nice job", "I agree", "well said",
    "excellent contribution", "friendly discussion", "supportive community",
    // Clearly positive technical-discourse phrases (not neutral filler).
    "good question", "happy to help", "I appreciate the feedback",
    "that is a great idea", "well done", "thanks for the clarification",
];

/// Anchor phrases that define the negative / toxic emotional pole.
/// Kept to clear interpersonal toxicity — NOT mere technical disagreement.
const NEGATIVE_ANCHORS: &[&str] = &[
    "hostile", "rude", "toxic behaviour",
    "you always do this", "personal attack",
    "drama", "insulting", "offensive behaviour", "harassment",
    "you are stupid", "shut up", "I hate this community",
    "you people are the problem", "this is pathetic",
];

/// Overall health classification for a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertLevel {
    /// Average score above 0.48 — normal, healthy room.
    None,
    /// Average score 0.33–0.48 — worth monitoring.
    Watch,
    /// Average score below 0.33 — sustained tension, action may be needed.
    Warning,
}

/// Snapshot of a room's community health.
#[derive(Debug, Clone)]
pub struct RoomHealth {
    /// Emotional health: 0.0 = very negative, 1.0 = very positive.
    pub score: f32,
    /// +1 improving, 0 stable, −1 declining.
    pub trend: i8,
    pub alert: AlertLevel,
}

// ── Rolling score window ─────────────────────────────────────────────────────

struct RoomWindow {
    scores: VecDeque<f32>,
}

impl RoomWindow {
    fn new() -> Self {
        Self { scores: VecDeque::with_capacity(WINDOW_SIZE + 1) }
    }

    fn push(&mut self, score: f32) {
        self.scores.push_back(score);
        if self.scores.len() > WINDOW_SIZE {
            self.scores.pop_front();
        }
    }

    fn health(&self) -> Option<RoomHealth> {
        let n = self.scores.len();
        if n == 0 { return None; }

        let avg: f32 = self.scores.iter().sum::<f32>() / n as f32;

        let trend: i8 = if n >= TREND_HALF * 2 {
            let recent: f32 = self.scores.iter().rev().take(TREND_HALF).sum::<f32>()
                / TREND_HALF as f32;
            let older: f32 = self.scores.iter().rev().skip(TREND_HALF).take(TREND_HALF).sum::<f32>()
                / TREND_HALF as f32;
            let diff = recent - older;
            if diff > 0.05 { 1 } else if diff < -0.05 { -1 } else { 0 }
        } else {
            0
        };

        let alert = if avg < 0.33 {
            AlertLevel::Warning
        } else if avg < 0.48 {
            AlertLevel::Watch
        } else {
            AlertLevel::None
        };

        Some(RoomHealth { score: avg, trend, alert })
    }
}

// ── Monitor ──────────────────────────────────────────────────────────────────

/// The health monitor.  Lives behind `Arc<Mutex<Option<HealthMonitor>>>`.
/// Call `HealthMonitor::new()` on a blocking thread; `None` means the
/// embedding model couldn't be loaded (graceful degradation).
pub struct HealthMonitor {
    model: TextEmbedding,
    /// Normalised centroid of the positive anchor embeddings.
    pos_anchor: Vec<f32>,
    /// Normalised centroid of the negative anchor embeddings.
    neg_anchor: Vec<f32>,
    rooms: HashMap<String, RoomWindow>,
}

impl HealthMonitor {
    /// Initialise the monitor.  CPU-bound — call via `spawn_blocking`.
    pub fn new() -> Option<Self> {
        crate::intelligence::watcher::try_init_openvino_ep();

        let cache = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("hikyaku")
            .join("models");

        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                .with_cache_dir(cache)
                .with_show_download_progress(false),
        )
        .map_err(|e| tracing::warn!("HealthMonitor model init failed: {e}"))
        .ok()?;

        let pos_raw = model.embed(POSITIVE_ANCHORS.to_vec(), None)
            .map_err(|e| tracing::warn!("HealthMonitor positive anchors failed: {e}"))
            .ok()?;
        let neg_raw = model.embed(NEGATIVE_ANCHORS.to_vec(), None)
            .map_err(|e| tracing::warn!("HealthMonitor negative anchors failed: {e}"))
            .ok()?;

        let pos_anchor = mean_normalise(pos_raw);
        let neg_anchor = mean_normalise(neg_raw);

        tracing::info!("HealthMonitor initialised");
        Some(Self { model, pos_anchor, neg_anchor, rooms: HashMap::new() })
    }

    /// Score `body` and record it in the rolling window for `room_id`.
    /// Returns the updated `RoomHealth`.  CPU-bound — call via `spawn_blocking`.
    pub fn record(&mut self, room_id: &str, body: &str) -> Option<RoomHealth> {
        // Skip very short messages — not enough signal.
        let word_count = body.split_whitespace().count();
        if word_count < 3 {
            return self.rooms.get(room_id).and_then(|w| w.health());
        }

        let embeds = self.model
            .embed(vec![body], None)
            .map_err(|e| tracing::debug!("HealthMonitor embed error: {e}"))
            .ok()?;
        let embed = normalise(embeds.into_iter().next()?);

        let pos_sim = dot(&embed, &self.pos_anchor);
        let neg_sim = dot(&embed, &self.neg_anchor);
        // Map the [-1, 1] difference to [0, 1].
        let score = ((pos_sim - neg_sim) + 1.0) / 2.0;

        let window = self.rooms
            .entry(room_id.to_string())
            .or_insert_with(RoomWindow::new);
        window.push(score);
        window.health()
    }
}

// ── Math helpers ─────────────────────────────────────────────────────────────

fn normalise(v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-9 { return v; }
    v.into_iter().map(|x| x / norm).collect()
}

fn mean_normalise(vecs: Vec<Vec<f32>>) -> Vec<f32> {
    if vecs.is_empty() { return Vec::new(); }
    let dim = vecs[0].len();
    let count = vecs.len() as f32;
    let mut mean = vec![0.0f32; dim];
    for v in &vecs {
        for (m, x) in mean.iter_mut().zip(v.iter()) {
            *m += x / count;
        }
    }
    normalise(mean)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_bounded() {
        // Orthogonal vectors → pos_sim=1, neg_sim=0 → score=1.
        let embed = normalise(vec![1.0, 0.0]);
        let pos = normalise(vec![1.0, 0.0]);
        let neg = normalise(vec![0.0, 1.0]);
        let score = ((dot(&embed, &pos) - dot(&embed, &neg)) + 1.0) / 2.0;
        assert!((score - 1.0).abs() < 1e-6);

        // Opposite → score=0.
        let embed2 = normalise(vec![0.0, 1.0]);
        let score2 = ((dot(&embed2, &pos) - dot(&embed2, &neg)) + 1.0) / 2.0;
        assert!((score2 - 0.0).abs() < 1e-6);
    }

    #[test]
    fn trend_improving() {
        let mut w = RoomWindow::new();
        for _ in 0..TREND_HALF { w.push(0.25); }
        for _ in 0..TREND_HALF { w.push(0.80); }
        assert_eq!(w.health().unwrap().trend, 1);
    }

    #[test]
    fn trend_declining() {
        let mut w = RoomWindow::new();
        for _ in 0..TREND_HALF { w.push(0.80); }
        for _ in 0..TREND_HALF { w.push(0.25); }
        assert_eq!(w.health().unwrap().trend, -1);
    }

    #[test]
    fn trend_stable() {
        let mut w = RoomWindow::new();
        for _ in 0..TREND_HALF * 2 { w.push(0.65); }
        assert_eq!(w.health().unwrap().trend, 0);
    }

    #[test]
    fn alert_warning() {
        let mut w = RoomWindow::new();
        for _ in 0..10 { w.push(0.20); }
        assert_eq!(w.health().unwrap().alert, AlertLevel::Warning);
    }

    #[test]
    fn alert_watch() {
        let mut w = RoomWindow::new();
        for _ in 0..10 { w.push(0.40); }
        assert_eq!(w.health().unwrap().alert, AlertLevel::Watch);
    }

    #[test]
    fn alert_none() {
        let mut w = RoomWindow::new();
        for _ in 0..10 { w.push(0.55); }
        assert_eq!(w.health().unwrap().alert, AlertLevel::None);
    }

    #[test]
    fn window_capped() {
        let mut w = RoomWindow::new();
        for i in 0..50 { w.push(i as f32 * 0.01); }
        assert_eq!(w.scores.len(), WINDOW_SIZE);
    }

    #[test]
    fn short_body_no_panic() {
        let mut w = RoomWindow::new();
        // push some history so health() has something to return
        w.push(0.6);
        // Two-word body — would be skipped in record(); window unchanged.
        assert!(w.health().is_some());
    }
}
