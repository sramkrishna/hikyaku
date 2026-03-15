mod client;
pub mod verification;

pub use client::{MatrixEvent, MatrixCommand, MessageInfo, RoomInfo, RoomKind, RoomMeta, SpaceDirectoryRoom, spawn_matrix_thread};
