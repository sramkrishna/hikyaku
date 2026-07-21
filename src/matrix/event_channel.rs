//! Prioritized event channels between the matrix thread and the GTK loop.
//!
//! Replaces the single unbounded `async_channel<MatrixEvent>` with three
//! prioritized channels. The GTK-side drain reads them in priority order,
//! so under load (matrix sync burst, backpagination flood, worker delivering
//! markup) the important events don't queue behind ephemeral plugin traffic.
//!
//! Classes:
//! - **Critical** — login flow, sync state, message/room mutations,
//!   verification, decryption, room-membership changes. Unbounded, lossless.
//!   The GTK loop MUST see all of these.
//! - **Bulk** — pagination results, media/avatar downloads, thread replies,
//!   search results, background markup renders. Bounded (1024); senders
//!   backpressure via `send().await` / `send_blocking()`.
//! - **Plugin** — typing indicators, reaction notifications, plugin-gated
//!   events (MOTD topic, AI watcher, community-health), Ollama streaming.
//!   Bounded (64); `try_send` drops on overflow. Ephemeral by design.
//!
//! Fix for the audit finding on `event_tx` being `async_channel::unbounded`:
//! see [[feedback_synchronous_state_access]] rule and CLAUDE.md §5.

use async_channel::{Receiver, RecvError, SendError, Sender};
use crate::matrix::MatrixEvent;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventClass {
    Critical,
    Bulk,
    Plugin,
}

impl MatrixEvent {
    /// Priority class for cross-thread delivery.
    ///
    /// New variants MUST be classified here. The compiler enforces this via
    /// the exhaustive match — adding a MatrixEvent variant without touching
    /// this method fails to build.
    pub fn class(&self) -> EventClass {
        match self {
            // Critical — never drop.
            MatrixEvent::LoginRequired
            | MatrixEvent::LoginSuccess { .. }
            | MatrixEvent::LoginFailed { .. }
            | MatrixEvent::LoggedOut
            | MatrixEvent::SyncStarted
            | MatrixEvent::SyncError { .. }
            | MatrixEvent::SyncGap { .. }
            | MatrixEvent::RoomListUpdated { .. }
            | MatrixEvent::BgRefreshStarted { .. }
            | MatrixEvent::RoomMessages { .. }
            | MatrixEvent::OlderMessages { .. }
            | MatrixEvent::SeekResult { .. }
            | MatrixEvent::NewMessage { .. }
            | MatrixEvent::MessageSent { .. }
            | MatrixEvent::MessageEdited { .. }
            | MatrixEvent::MessageRedacted { .. }
            | MatrixEvent::ReactionUpdate { .. }
            | MatrixEvent::VerificationRequest { .. }
            | MatrixEvent::VerificationEmojis { .. }
            | MatrixEvent::VerificationDone { .. }
            | MatrixEvent::VerificationCancelled { .. }
            | MatrixEvent::DeviceUnverified
            | MatrixEvent::RecoveryStarted
            | MatrixEvent::RecoveryComplete { .. }
            | MatrixEvent::RecoveryFailed { .. }
            | MatrixEvent::BackupVersionMismatch
            | MatrixEvent::StaleBackupDeleted
            | MatrixEvent::CrossSigningBootstrapped
            | MatrixEvent::CrossSigningNeedsPassword
            | MatrixEvent::RecoveryKeyGenerated { .. }
            | MatrixEvent::KeysImported { .. }
            | MatrixEvent::KeyImportFailed { .. }
            | MatrixEvent::RoomKeysReceived { .. }
            | MatrixEvent::RegistrationSuccess { .. }
            | MatrixEvent::RegistrationFailed { .. }
            | MatrixEvent::RoomJoined { .. }
            | MatrixEvent::JoinFailed { .. }
            | MatrixEvent::RoomLeft { .. }
            | MatrixEvent::LeaveFailed { .. }
            | MatrixEvent::InviteSuccess { .. }
            | MatrixEvent::InviteFailed { .. }
            | MatrixEvent::RoomInvited { .. }
            | MatrixEvent::KnockSent { .. }
            | MatrixEvent::KnockFailed { .. }
            | MatrixEvent::KnockReceived { .. }
            | MatrixEvent::DmReady { .. }
            | MatrixEvent::DmFailed { .. }
            | MatrixEvent::RoomPrefetched { .. }
            | MatrixEvent::AliasInfoResolved { .. } => EventClass::Critical,

            // Bulk — bounded, backpressure.
            MatrixEvent::MediaReady { .. }
            | MatrixEvent::AvatarReady { .. }
            | MatrixEvent::RoomAvatarReady { .. }
            | MatrixEvent::MarkupRendered { .. }
            | MatrixEvent::ThreadReplies { .. }
            | MatrixEvent::SearchResults { .. }
            | MatrixEvent::SearchFailed { .. }
            | MatrixEvent::UserSearchResults { .. }
            | MatrixEvent::PublicRoomDirectory { .. }
            | MatrixEvent::SpaceDirectory { .. }
            | MatrixEvent::PublicSpacesForServer { .. }
            | MatrixEvent::OwnAvatarUpdated { .. }
            | MatrixEvent::MessagesExported { .. }
            | MatrixEvent::MessagesExportFailed { .. }
            | MatrixEvent::MetricsReady { .. }
            | MatrixEvent::MetricsFailed { .. }
            | MatrixEvent::RoomPreview { .. } => EventClass::Bulk,

            // Plugin / ephemeral — try_send drop.
            MatrixEvent::TypingUsers { .. }
            | MatrixEvent::ReactionNotification { .. }
            | MatrixEvent::OllamaChunk { .. } => EventClass::Plugin,

            #[cfg(feature = "motd")]
            MatrixEvent::TopicChanged { .. } => EventClass::Plugin,
            #[cfg(feature = "ai")]
            MatrixEvent::RoomAlert { .. } => EventClass::Plugin,
            #[cfg(feature = "community-health")]
            MatrixEvent::HealthUpdate { .. } => EventClass::Plugin,
        }
    }
}

/// Bulk channel capacity. Senders block when full — that backpressures
/// the matrix sync loop / markup worker instead of dropping downloads or
/// rendered markup that the UI needs.
const BULK_CAPACITY: usize = 1024;

/// Plugin channel capacity. `try_send` drops on overflow — plugin events
/// are ephemeral (typing, toasts, health scores), a dropped one is not a
/// correctness failure. Small so overflow is quick to detect in profiling.
const PLUGIN_CAPACITY: usize = 64;

/// The sender the matrix thread holds. All routing lives here — call sites
/// see the same `.send(event).await` / `.send_blocking(event)` / `.clone()`
/// surface as `async_channel::Sender<MatrixEvent>`.
#[derive(Clone)]
pub struct EventSender {
    critical: Sender<MatrixEvent>,
    bulk: Sender<MatrixEvent>,
    plugin: Sender<MatrixEvent>,
}

impl EventSender {
    /// Async send. Bulk waits when full (backpressure). Plugin overflow
    /// silently drops — see class-level docs.
    pub async fn send(&self, event: MatrixEvent) -> Result<(), SendError<MatrixEvent>> {
        match event.class() {
            EventClass::Critical => self.critical.send(event).await,
            EventClass::Bulk => self.bulk.send(event).await,
            EventClass::Plugin => {
                let _ = self.plugin.try_send(event);
                Ok(())
            }
        }
    }

    /// Blocking send from worker threads (markup_worker, health scorer,
    /// watcher). Same routing semantics as async `send`.
    pub fn send_blocking(&self, event: MatrixEvent) -> Result<(), SendError<MatrixEvent>> {
        match event.class() {
            EventClass::Critical => self.critical.send_blocking(event),
            EventClass::Bulk => self.bulk.send_blocking(event),
            EventClass::Plugin => {
                let _ = self.plugin.try_send(event);
                Ok(())
            }
        }
    }
}

/// The receiver the GTK loop holds. `recv()` awaits the next event with
/// priority Critical → Bulk → Plugin — a critical event that arrives while
/// bulk/plugin are queued jumps to the head.
pub struct EventReceiver {
    critical: Receiver<MatrixEvent>,
    bulk: Receiver<MatrixEvent>,
    plugin: Receiver<MatrixEvent>,
}

impl EventReceiver {
    /// Await the next event, priority-ordered.
    ///
    /// Fast path: try_recv each channel in priority order — if any has a
    /// pending message, return it immediately.
    ///
    /// Slow path: await ANY of the three. When one wakes, loop back to
    /// the fast path so priority ordering is preserved (an arriving
    /// bulk event won't jump ahead of a critical event that arrived
    /// during the same poll cycle).
    ///
    /// Returns Err only when all three channels are closed — i.e. the
    /// matrix thread and all worker senders have dropped their handles,
    /// which means shutdown.
    pub async fn recv(&self) -> Result<MatrixEvent, RecvError> {
        loop {
            if let Ok(e) = self.critical.try_recv() {
                return Ok(e);
            }
            if let Ok(e) = self.bulk.try_recv() {
                return Ok(e);
            }
            if let Ok(e) = self.plugin.try_recv() {
                return Ok(e);
            }
            if self.critical.is_closed() && self.bulk.is_closed() && self.plugin.is_closed() {
                return Err(RecvError);
            }
            let c = self.critical.recv();
            let b = self.bulk.recv();
            let p = self.plugin.recv();
            futures_util::pin_mut!(c, b, p);
            use futures_util::future::select;
            let bp = select(b, p);
            futures_util::pin_mut!(bp);
            let _ = select(c, bp).await;
        }
    }
}

/// Create the paired sender + receiver. Call once at app start.
pub fn channels() -> (EventSender, EventReceiver) {
    let (c_tx, c_rx) = async_channel::unbounded::<MatrixEvent>();
    let (b_tx, b_rx) = async_channel::bounded::<MatrixEvent>(BULK_CAPACITY);
    let (p_tx, p_rx) = async_channel::bounded::<MatrixEvent>(PLUGIN_CAPACITY);
    (
        EventSender { critical: c_tx, bulk: b_tx, plugin: p_tx },
        EventReceiver { critical: c_rx, bulk: b_rx, plugin: p_rx },
    )
}
