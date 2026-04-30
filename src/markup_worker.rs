// Background HTML→Pango conversion for message bodies.
//
// `prerender_body` normally runs synchronously inside `info_to_obj`, which
// is called once per incoming message on the GTK thread. With the byte-based
// `html_to_segments` rewrite in v0.2.0 the common case is under 200µs, but
// pathological HTML (long quote chains, code blocks with many attributes,
// pasted content) can still cross 10ms per message. First-load of a 100-msg
// room could therefore block the compositor for hundreds of ms even though
// each individual parse is small.
//
// This module moves the HTML parse to a single dedicated worker thread that
// owns a bounded channel. `info_to_obj` enqueues `(job_id, html)` via
// `try_send`, registering the target MessageObject's WeakRef in a
// thread-local keyed by job_id. The worker runs `html_to_pango` and sends
// the result back through the matrix event channel as a plain `(id, markup)`
// tuple — no GObject crosses the thread boundary because GObjects are not
// Send. The GTK event loop picks up the result, looks up the WeakRef in the
// thread-local, upgrades if still alive, and calls `set_rendered_markup`.
//
// `try_send` drops jobs when the queue is saturated — we prefer briefly
// empty `rendered_markup` (plain-text fallback in render_body) over
// blocking any thread under load. Pattern from CLAUDE.md §5 (dedicated
// thread + bounded std::sync::mpsc::sync_channel).

use glib::prelude::*;
use glib::WeakRef;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::OnceLock;

use crate::models::MessageObject;

/// One unit of work sent to the parser thread. Contains only Send data —
/// no GObject reference, because GObjects aren't Send.
pub struct MarkupJob {
    pub id: u64,
    pub formatted_body: String,
}

/// Monotonic job-id counter. Never reused; a u64 at 1M parses/sec takes
/// ~584,000 years to wrap. Not a concern.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Global handle to the worker's sender. `None` until `start()` runs at
/// application startup; callers must degrade gracefully when unset.
static MARKUP_TX: OnceLock<SyncSender<MarkupJob>> = OnceLock::new();

thread_local! {
    /// GTK-thread-only map of job_id → WeakRef target. Populated in
    /// `try_enqueue`, drained when `apply_result` fires from the event
    /// handler. Keeping it thread-local lets us hold a WeakRef<MessageObject>
    /// (which is !Send because MessageObject is !Send) without a mutex —
    /// the worker thread never touches this map; it only sees the numeric
    /// `id`.
    static PENDING: RefCell<HashMap<u64, WeakRef<MessageObject>>> =
        RefCell::new(HashMap::new());
}

/// Spawn the markup worker. Idempotent — a second call is a no-op.
///
/// `on_result` is called on the worker thread with `(job_id, markup)`. The
/// caller is responsible for routing that into the GTK event loop (typically
/// by sending on an `async_channel::Sender<MatrixEvent>` so the main-thread
/// event handler picks it up and calls `apply_result`).
pub fn start<F>(on_result: F)
where
    F: Fn(u64, String) + Send + 'static,
{
    // Bounded queue: 512 pending parses covers first_load of a 400-message
    // room plus live-sync burst; `try_send` drops past that.
    let (tx, rx) = sync_channel::<MarkupJob>(512);
    if MARKUP_TX.set(tx).is_err() {
        return; // already started
    }
    std::thread::Builder::new()
        .name("markup-parser".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                let markup = crate::markdown::html_to_pango(&job.formatted_body);
                on_result(job.id, markup);
            }
        })
        .ok();
}

/// Queue a MessageObject's HTML body for background Pango conversion.
///
/// Must be called on the GTK thread (owns the PENDING thread-local).
/// Drops silently when the worker hasn't started or the queue is full;
/// the MessageObject keeps its empty `rendered_markup` and `render_body`
/// falls back to the plain-text escape path.
pub fn try_enqueue(obj: &MessageObject, formatted_body: String) {
    let Some(tx) = MARKUP_TX.get() else { return };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    // Register the target BEFORE sending; otherwise the worker could race
    // us and the result would arrive at apply_result with no registered
    // weak ref.
    PENDING.with(|p| {
        p.borrow_mut().insert(id, obj.downgrade());
    });
    if tx.try_send(MarkupJob { id, formatted_body }).is_err() {
        // Queue saturated or channel closed — undo the registration so we
        // don't leak the WeakRef.
        PENDING.with(|p| {
            p.borrow_mut().remove(&id);
        });
    }
}

/// Apply a worker result on the GTK thread. Call from the matrix event
/// handler when `MatrixEvent::MarkupRendered { id, markup }` arrives.
///
/// Looks up the WeakRef for `id`, upgrades it if the object still exists,
/// and calls `set_rendered_markup` which fires notify::rendered-markup for
/// any bound MessageRow to pick up via its notify handler.
pub fn apply_result(id: u64, markup: String) {
    let weak = PENDING.with(|p| p.borrow_mut().remove(&id));
    let Some(weak) = weak else { return };
    if let Some(obj) = weak.upgrade() {
        // Don't overwrite the plain-text fallback (set in info_to_obj)
        // with an empty result. Pathological HTML that html_to_pango
        // can't parse produces a "" string here; clobbering would
        // leave the row visually blank ("see the nick but no
        // content"). Keep the fallback so the user at least reads
        // the raw text.
        if markup.is_empty() {
            tracing::warn!(
                "markup_worker: html_to_pango returned empty for id={id}; \
                 keeping plain-text fallback to avoid blank row"
            );
            return;
        }
        obj.set_rendered_markup(markup);
    }
}
