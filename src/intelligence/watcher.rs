// Room interest watcher — uses a local sentence embedding model to flag
// messages that semantically match user-defined watch terms.
//
// Runs entirely on the tokio blocking thread pool; never touches GTK.
// The model is downloaded once (~23 MB) to the app cache dir and reused.
// On machines without the model cached, the watcher silently does nothing
// until the download completes.

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::path::PathBuf;

pub struct Watcher {
    model: TextEmbedding,
    /// (term_text, normalised_embedding)
    interests: Vec<(String, Vec<f32>)>,
    threshold: f32,
}

impl Watcher {
    /// Initialise the watcher. Returns `None` if:
    ///  - `terms` is empty
    ///  - the model cannot be loaded (missing cache + no network)
    ///  - embedding the terms fails for any reason
    ///
    /// This function is CPU-bound and should be called via `spawn_blocking`.
    pub fn new(terms: &[String], threshold: f64) -> Option<Self> {
        if terms.is_empty() {
            return None;
        }

        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                .with_cache_dir(model_cache_dir())
                .with_show_download_progress(false),
        )
        .map_err(|e| tracing::warn!("Watcher model init failed: {e}"))
        .ok()?;

        let term_strs: Vec<&str> = terms.iter().map(|s| s.as_str()).collect();
        let raw = model
            .embed(term_strs, None)
            .map_err(|e| tracing::warn!("Watcher term embedding failed: {e}"))
            .ok()?;

        let interests = terms
            .iter()
            .cloned()
            .zip(raw.into_iter().map(normalise))
            .collect();

        tracing::info!("Watcher ready with {} terms (threshold {:.2})", terms.len(), threshold);
        Some(Self { model, interests, threshold: threshold as f32 })
    }

    /// Check a message body against watch terms. Returns the matched term name
    /// if any term matches, otherwise `None`.
    ///
    /// Strategy:
    ///  1. Keyword pre-pass: whole-word case-insensitive match — catches exact
    ///     occurrences regardless of sentence context (negations, questions, etc.).
    ///  2. Semantic fallback: cosine similarity on embeddings — catches paraphrases
    ///     and topical references that don't use the exact term word.
    ///
    /// CPU-bound — call via `spawn_blocking`.
    pub fn check(&self, body: &str) -> Option<String> {
        if body.is_empty() { return None; }

        let body_lower = body.to_lowercase();

        // Keyword pre-pass: whole-word match (unicode word boundaries via char class).
        for (term, _) in &self.interests {
            let term_lower = term.to_lowercase();
            if whole_word_match(&body_lower, &term_lower) {
                tracing::debug!("Watcher keyword hit: {:?}", term);
                return Some(term.clone());
            }
        }

        // Semantic fallback for paraphrases / topic references.
        let vecs = self.model
            .embed(vec![body], None)
            .map_err(|e| tracing::debug!("Watcher embed failed: {e}"))
            .ok()?;
        let msg_vec = normalise(vecs.into_iter().next()?);

        self.interests
            .iter()
            .map(|(term, interest)| (term, cosine(&msg_vec, interest)))
            .filter(|(_, sim)| *sim >= self.threshold)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(term, _)| term.clone())
    }
}

/// True if `term` appears in `text` as a whole word (not part of a larger word).
/// Both arguments must already be lowercased.
fn whole_word_match(text: &str, term: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = text[start..].find(term) {
        let abs = start + pos;
        let before_ok = abs == 0
            || !text[..abs].chars().next_back().map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);
        let after = abs + term.len();
        let after_ok = after >= text.len()
            || !text[after..].chars().next().map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// L2-normalise a vector in-place so dot product == cosine similarity.
fn normalise(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v { *x /= norm; }
    }
    v
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn model_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("hikyaku/models")
}
