mod client;
pub mod encryption;
pub mod room_cache;
pub mod verification;

/// Priority flag: true while a room is being opened so background tasks yield.
/// Shared between client (sets it) and encryption (reads it to throttle key downloads).
pub(crate) static ROOM_LOAD_IN_PROGRESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub use client::{MatrixEvent, MatrixCommand, MediaInfo, MediaKind, MessageInfo, RoomInfo, RoomKind, RoomMeta, SpaceDirectoryRoom, spawn_matrix_thread};
