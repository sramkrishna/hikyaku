//! Per-room Matrix message timeline model.
//!
//! Two GObjects: [`Timeline`] holds an ordered gio::ListStore of
//! [`MessageObject`]s plus an event_id → MessageObject index. It enforces
//! four invariants on every mutation:
//!
//! 1. The list is sorted ascending by `timestamp`.
//! 2. No two items share a non-empty `event_id`.
//! 3. `event_index` is in sync with the list.
//! 4. `oldest_timestamp` / `newest_timestamp` reflect real list state.
//!
//! This crate is deliberately **matrix-free** and **widget-free** so it
//! can be tested headlessly against random operation sequences. The
//! `hikyaku` bin crate bridges MessageInfo (matrix-side plain data) into
//! MessageObject (this crate) via its `info_to_obj` helper.
//!
//! See [[feedback_timeline_chokepoint]] in the project memory for the
//! chokepoint discipline this module enforces.

pub(crate) mod perf;
pub mod message_object;
pub mod timeline;

pub use message_object::MessageObject;
pub use timeline::{Timeline, set_splice_hook};
