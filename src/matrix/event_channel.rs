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
            // Fast path: priority-order try_recv. Anything already in a
            // channel returns in priority order.
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
            // Slow path: await ANY channel and RETURN the winner's message.
            //
            // Priority is enforced by the try_recv chain above, not by the
            // slow path: if a critical event arrives while we're awaiting a
            // bulk, the bulk still wins THIS call (it arrived first — fair
            // scheduling), but the next call's try_recv drains any queued
            // critical before touching bulk/plugin.
            //
            // Bug that this replaces: the previous version threw away the
            // winner's value (`let _ = select(...).await`) and looped back
            // to try_recv, which returned Empty because the value had been
            // consumed from the channel. Every event that arrived via the
            // slow path was silently dropped — including OlderMessages,
            // which is why backpagination stopped working.
            let c = self.critical.recv();
            let b = self.bulk.recv();
            let p = self.plugin.recv();
            futures_util::pin_mut!(c, b, p);
            use futures_util::future::{select, Either};
            let bp = select(b, p);
            futures_util::pin_mut!(bp);
            let result = match select(c, bp).await {
                Either::Left((res, _)) => res,
                Either::Right((Either::Left((res, _)), _)) => res,
                Either::Right((Either::Right((res, _)), _)) => res,
            };
            // Ok → return. Err → the losing channel closed empty; loop back
            // so the try_recv chain checks the still-open channels for a
            // queued message before we conclude shutdown. Without this, the
            // race where one channel closes (Err) at the same moment another
            // delivers a message (Ok) can return Err even though a message
            // was waiting — `select` is not biased on ties.
            if let Ok(e) = result {
                return Ok(e);
            }
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

#[cfg(test)]
mod tests {
    //! Coverage for the routing + priority semantics. These are pure
    //! async_channel + futures_util — no GTK init needed, so they run
    //! in tokio's single-thread runtime cleanly and in parallel with
    //! other tests.
    //!
    //! Rationale for this test module: `[[feedback_test_before_ship]]`.
    //! The previous version of `EventReceiver::recv` silently dropped
    //! every event delivered via the slow path. Compile-clean, one
    //! manual repro, shipped — and pagination broke completely. A test
    //! this small (build a channel, send, recv, assert) would have
    //! caught it. This module now covers each documented semantic so
    //! the next edit either preserves the behaviour or fails a test.
    use super::*;
    use crate::matrix::MatrixEvent;

    fn crit() -> MatrixEvent {
        MatrixEvent::SyncStarted
    }
    fn bulk_evt() -> MatrixEvent {
        MatrixEvent::MarkupRendered { id: 1, markup: String::new() }
    }
    fn plugin_evt() -> MatrixEvent {
        MatrixEvent::TypingUsers { room_id: String::new(), names: Vec::new() }
    }

    fn variant_name(e: &MatrixEvent) -> &'static str {
        match e {
            MatrixEvent::SyncStarted => "SyncStarted",
            MatrixEvent::MarkupRendered { .. } => "MarkupRendered",
            MatrixEvent::TypingUsers { .. } => "TypingUsers",
            _ => "other",
        }
    }

    #[test]
    fn classification_covers_each_class() {
        assert_eq!(crit().class(), EventClass::Critical);
        assert_eq!(bulk_evt().class(), EventClass::Bulk);
        assert_eq!(plugin_evt().class(), EventClass::Plugin);
    }

    #[tokio::test]
    async fn fast_path_returns_critical_message() {
        // Fast path: message already in channel when recv is called.
        // This exercised OK even in the buggy version.
        let (tx, rx) = channels();
        tx.send(crit()).await.unwrap();
        let got = rx.recv().await.unwrap();
        assert_eq!(variant_name(&got), "SyncStarted");
    }

    #[tokio::test]
    async fn slow_path_returns_message_not_lost() {
        // Regression test for the discard bug in EventReceiver::recv.
        // The GTK drainer is usually idle awaiting recv — nearly every
        // event arrives via the slow path. The previous code consumed
        // the message from the winning future and threw it away, then
        // looped back to try_recv which returned Empty. This test
        // fails without the fix.
        let (tx, rx) = channels();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            tx.send(crit()).await.unwrap();
        });
        let got = rx.recv().await.unwrap();
        assert_eq!(variant_name(&got), "SyncStarted");
    }

    #[tokio::test]
    async fn slow_path_returns_bulk_and_plugin_too() {
        // Same regression class as above but for bulk and plugin —
        // the discard bug affected all three channels.
        let (tx, rx) = channels();
        let tx2 = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tx2.send(bulk_evt()).await.unwrap();
        });
        assert_eq!(variant_name(&rx.recv().await.unwrap()), "MarkupRendered");

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tx.send(plugin_evt()).await.unwrap();
        });
        assert_eq!(variant_name(&rx.recv().await.unwrap()), "TypingUsers");
    }

    #[tokio::test]
    async fn priority_order_when_all_queued() {
        // With messages waiting on all three channels, recv returns
        // critical first, then bulk, then plugin. This is the
        // "critical jumps to the head" guarantee.
        let (tx, rx) = channels();
        tx.send(plugin_evt()).await.unwrap();
        tx.send(bulk_evt()).await.unwrap();
        tx.send(crit()).await.unwrap();
        // Give a moment for all sends to complete on the channel side.
        tokio::task::yield_now().await;
        assert_eq!(variant_name(&rx.recv().await.unwrap()), "SyncStarted");
        assert_eq!(variant_name(&rx.recv().await.unwrap()), "MarkupRendered");
        assert_eq!(variant_name(&rx.recv().await.unwrap()), "TypingUsers");
    }

    #[tokio::test]
    async fn plugin_overflow_drops_silently() {
        // Plugin channel is bounded (64) with try_send semantics —
        // filling past capacity drops rather than blocking. Sending
        // PLUGIN_CAPACITY + N events should never hang the sender.
        let (tx, _rx) = channels();
        for _ in 0..(PLUGIN_CAPACITY + 32) {
            tx.send(plugin_evt()).await.unwrap();
        }
        // If try_send weren't in effect the .await above would block
        // forever once capacity fills; reaching this line proves the
        // drop policy.
    }

    #[tokio::test]
    async fn bulk_backpressures_at_capacity() {
        // Bulk uses send.await — sender blocks when full. Verify by
        // filling BULK_CAPACITY, then spawning one more send that
        // MUST NOT complete until we drain something. Then drain and
        // confirm the spawned send completes.
        let (tx, rx) = channels();
        for _ in 0..BULK_CAPACITY {
            tx.send(bulk_evt()).await.unwrap();
        }
        let tx_bg = tx.clone();
        let sender_handle = tokio::spawn(async move {
            tx_bg.send(bulk_evt()).await.unwrap();
        });
        // Give the spawned send a moment to prove it can't complete.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!sender_handle.is_finished(),
            "bulk send should backpressure when channel is full");
        // Drain one — the backpressured sender should now unblock.
        let _ = rx.recv().await.unwrap();
        sender_handle.await.unwrap();
    }

    #[tokio::test]
    async fn all_closed_returns_err() {
        // When every sender is dropped, all three underlying channels
        // close, and recv returns Err to signal shutdown.
        let (tx, rx) = channels();
        drop(tx);
        assert!(rx.recv().await.is_err());
    }

    #[tokio::test]
    async fn critical_arriving_during_bulk_await_wins_next_call() {
        // The priority contract: try_recv chain enforces ordering, so
        // if a critical arrives DURING a bulk-await, the bulk wins
        // this call (fair to the message that arrived first), and the
        // next call's try_recv returns the queued critical before
        // touching bulk/plugin.
        let (tx, rx) = channels();
        // Send bulk first, then a critical shortly after — the bulk
        // will be delivered as the first recv result, and the critical
        // will be waiting on the second call.
        let tx2 = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tx2.send(bulk_evt()).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tx2.send(crit()).await.unwrap();
        });
        assert_eq!(variant_name(&rx.recv().await.unwrap()), "MarkupRendered");
        assert_eq!(variant_name(&rx.recv().await.unwrap()), "SyncStarted");
        drop(tx);
    }
}
