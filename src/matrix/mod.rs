mod client;
pub mod verification;

pub use client::{MatrixEvent, MatrixCommand, RoomInfo, RoomKind, spawn_matrix_thread};
