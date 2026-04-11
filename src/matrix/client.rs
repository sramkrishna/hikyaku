// Matrix client — runs on a background tokio thread.
//
// This module handles login and sync. It communicates with the GTK main
// thread through two async channels:
//   - event_tx: sends MatrixEvents TO the UI (login result, new rooms, etc.)
//   - command_rx: receives MatrixCommands FROM the UI (login request, send message, etc.)
//
// Why a separate thread? matrix-sdk uses tokio for async I/O, but GTK
// requires all UI work on the main thread with its own glib event loop.
// We bridge them with async-channel, which works across both runtimes.

use async_channel::{Receiver, Sender};
use matrix_sdk::{
    config::SyncSettings,
    ruma::{RoomId, UserId},
    Client, ServerName,
    authentication::matrix::MatrixSession,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What kind of room this is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq,
         serde::Serialize, serde::Deserialize,
         glib::Enum)]
#[enum_type(name = "MxRoomKind")]
pub enum RoomKind {
    /// A regular room (channel).
    #[default]
    Room,
    /// A direct message (1:1 or small group DM).
    DirectMessage,
    /// A space (container for other rooms).
    Space,
}

/// A snapshot of one room's state, sent from the Matrix thread to the UI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoomInfo {
    pub room_id: String,
    pub name: String,
    pub last_activity_ts: u64,
    pub kind: RoomKind,
    pub is_encrypted: bool,
    /// If this room belongs to a space, the space's display name.
    pub parent_space: Option<String>,
    /// The Matrix room_id of the parent space (empty string = no parent).
    #[serde(default)]
    pub parent_space_id: String,
    /// Whether the user has pinned this room (e.g. friend DMs).
    pub is_pinned: bool,
    /// Number of unread notifications (messages since last read receipt).
    pub unread_count: u64,
    /// Number of highlights (mentions of you, keyword matches).
    pub highlight_count: u64,
    /// Whether the current user has admin power level in this room.
    pub is_admin: bool,
    /// Whether this room has been tombstoned (upgraded to a new room).
    pub is_tombstoned: bool,
    /// Whether this room is marked as favourite (m.favourite tag).
    pub is_favourite: bool,
    /// Room avatar mxc:// URL, empty string if none.
    pub avatar_url: String,
    /// Current room topic (m.room.topic), empty string if none.
    #[serde(default)]
    pub topic: String,
}

/// A single message sent to the UI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MessageInfo {
    pub sender: String,
    pub sender_id: String,
    pub body: String,
    /// HTML formatted body (Matrix `formatted_body`), if present.
    pub formatted_body: Option<String>,
    pub timestamp: u64,
    pub event_id: String,
    /// If this message is a reply, the event ID it replies to.
    pub reply_to: Option<String>,
    /// Display name of who this message replies to (if known).
    pub reply_to_sender: Option<String>,
    /// If this message is part of a thread, the thread root event ID.
    pub thread_root: Option<String>,
    /// Aggregated emoji reactions: (emoji, count, reactor_names).
    pub reactions: Vec<(String, u64, Vec<String>)>,
    /// Media attachment info (if this is an image/file/video/audio message).
    pub media: Option<MediaInfo>,
    /// Whether this message should be highlighted (reply to us, thread reply, etc.)
    pub is_highlight: bool,
    /// True for system events (member join/leave/invite/kick/ban) that should be
    /// rendered as compact inline rows rather than regular message bubbles.
    #[serde(default)]
    pub is_system_event: bool,
}

/// Media attachment on a message.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MediaInfo {
    pub kind: MediaKind,
    pub filename: String,
    pub size: Option<u64>,
    /// Matrix content URI (mxc://) or file:// for local files.
    pub url: String,
    /// Raw JSON of the MediaSource for encrypted media downloads.
    /// Empty for unencrypted or non-Matrix URLs.
    #[serde(default)]
    pub source_json: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    File,
}

/// Room metadata sent alongside messages for display in the content header.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct RoomMeta {
    /// Room topic (m.room.topic).
    pub topic: String,
    /// Whether the room has been tombstoned.
    pub is_tombstoned: bool,
    /// Replacement room ID if tombstoned.
    pub replacement_room: Option<String>,
    /// Human-readable name of the replacement room (if we can resolve it).
    pub replacement_room_name: Option<String>,
    /// Pinned messages: (sender, plain_body, formatted_body) triples.
    pub pinned_messages: Vec<(String, String, Option<String>)>,
    /// Whether the room is encrypted.
    pub is_encrypted: bool,
    /// Number of joined members.
    pub member_count: u64,
    /// Whether this room is bookmarked (m.favourite).
    pub is_favourite: bool,
    /// Room member display names (for nick completion).
    pub members: Vec<(String, String)>, // (user_id, display_name)
    /// Room member avatar mxc:// URLs — (user_id, mxc_url). Empty string = no avatar.
    pub member_avatars: Vec<(String, String)>,
    /// True once a full room.members() fetch has been done this session.
    /// Prevents re-fetching the member list on every room switch.
    pub members_fetched: bool,
    /// Server-side unread notification count (0 = all caught up).
    pub unread_count: u32,
    /// The event_id stored in m.fully_read account data for this room.
    /// None if the marker is absent or couldn't be fetched.
    pub fully_read_event_id: Option<String>,
}

/// A room entry from a space directory listing.
#[derive(Debug, Clone)]
pub struct SpaceDirectoryRoom {
    pub room_id: String,
    /// Canonical alias, if the room has one (e.g. `#gnome-shell:gnome.org`).
    /// Prefer this over room_id for joining — alias resolution returns live
    /// federation servers, avoiding "M_UNKNOWN: no known servers".
    pub canonical_alias: Option<String>,
    pub name: String,
    pub topic: String,
    pub member_count: u64,
    pub already_joined: bool,
    /// The homeserver that returned this room in its public directory.
    /// Passed as a `via` hint so federation works even when the room_id's
    /// original server is no longer in the room.
    pub via_server: Option<String>,
}

/// A single emoji from SAS verification.
#[derive(Debug, Clone)]
pub struct VerificationEmoji {
    pub symbol: String,
    pub description: String,
}

/// Events sent FROM the Matrix thread TO the GTK UI.
#[derive(Debug, Clone)]
pub enum MatrixEvent {
    /// No saved session found — show the login page.
    LoginRequired,
    LoginSuccess { display_name: String, user_id: String, from_registration: bool, is_fresh_login: bool },
    LoginFailed { error: String },
    SyncStarted,
    SyncError { error: String },
    RoomListUpdated { rooms: Vec<RoomInfo> },
    /// Sent when a background refresh starts for a room that already has stale
    /// cached messages displayed.  The UI should show a loading indicator until
    /// the matching `RoomMessages` arrives.
    BgRefreshStarted { room_id: String },
    RoomMessages {
        room_id: String,
        messages: Vec<MessageInfo>,
        /// Pagination token — pass to FetchOlderMessages to get the next batch.
        prev_batch_token: Option<String>,
        /// Room metadata for the content header.
        room_meta: RoomMeta,
        /// True when sent from bg_refresh (server fetch).  Window defers the
        /// set_messages splice to an idle so it doesn't block the GTK frame.
        is_background: bool,
    },
    /// Older messages prepended at the top (pagination result).
    OlderMessages {
        room_id: String,
        messages: Vec<MessageInfo>,
        prev_batch_token: Option<String>,
    },
    /// Result of a SeekToEvent — events around the target, for replacing the timeline.
    SeekResult {
        room_id: String,
        /// The event the user wanted to jump to.
        target_event_id: String,
        /// Events in chronological order (before + target + after), ready to display.
        messages: Vec<MessageInfo>,
        /// Pagination token for loading events older than this window.
        before_token: Option<String>,
    },
    NewMessage {
        room_id: String,
        room_name: String,
        sender_id: String,
        message: MessageInfo,
        is_mention: bool,
        is_dm: bool,
    },
    /// An incoming verification request — show UI to accept.
    VerificationRequest {
        flow_id: String,
        other_user: String,
        other_device: String,
    },
    /// SAS emojis to display for user confirmation.
    VerificationEmojis {
        flow_id: String,
        emojis: Vec<VerificationEmoji>,
    },
    /// Verification completed successfully.
    VerificationDone { flow_id: String },
    /// Our device is not verified — user should verify to decrypt messages.
    DeviceUnverified,
    /// Verification was cancelled.
    VerificationCancelled { flow_id: String, reason: String },
    /// Recovery has started — show a spinner/toast immediately.
    RecoveryStarted,
    /// Recovery key import succeeded.
    /// `backup_connected` is true if the backup is now active and keys can be downloaded.
    RecoveryComplete { backup_connected: bool },
    /// Recovery key import failed.
    RecoveryFailed { error: String },
    /// Local backup version is stale (doesn't match the server).
    /// User must delete local data and re-link via Recover Keys.
    BackupVersionMismatch,
    /// The stale empty backup we created has been deleted from the server.
    /// The original backup is now current — user should click Recover Keys again.
    StaleBackupDeleted,
    /// Cross-signing keys were successfully bootstrapped for a brand-new account.
    CrossSigningBootstrapped,
    /// Cross-signing requires UIAA but no password was available (restored session,
    /// account never completed initial setup).  User should re-login to fix this.
    CrossSigningNeedsPassword,
    /// Public room directory from the homeserver.
    PublicRoomDirectory {
        title: String,
        rooms: Vec<SpaceDirectoryRoom>,
    },
    /// Space directory rooms for the "Join Room" browser.
    SpaceDirectory {
        space_id: String,
        rooms: Vec<SpaceDirectoryRoom>,
    },
    /// Space listings from a server's public directory (spaces_only browse).
    PublicSpacesForServer {
        server: String,
        rooms: Vec<SpaceDirectoryRoom>,
    },
    /// Media downloaded to a temp file — show preview.
    MediaReady { url: String, path: String },
    /// A member avatar has been downloaded and is available at `path`.
    AvatarReady { user_id: String, path: String },
    /// A room avatar has been downloaded and is available at `path`.
    RoomAvatarReady { room_id: String, path: String },
    /// A message we sent has been confirmed by the server with a real event_id.
    MessageSent { room_id: String, echo_body: String, event_id: String },
    /// Reactions on a message were updated.
    ReactionUpdate { room_id: String, event_id: String, reactions: Vec<(String, u64, Vec<String>)> },
    /// Someone reacted to one of our messages — fire a toast / desktop notif.
    ReactionNotification { room_id: String, room_name: String, reactor: String, emoji: String },
    /// A message was edited.
    MessageEdited { room_id: String, event_id: String, new_body: String, formatted_body: Option<String> },
    /// A message was redacted (deleted).
    MessageRedacted { room_id: String, event_id: String },
    /// Successfully joined a room.
    RoomJoined { room_id: String, room_name: String },
    /// Failed to join a room.
    JoinFailed { error: String },
    /// Successfully left a room.
    RoomLeft { room_id: String },
    /// Failed to leave a room.
    LeaveFailed { error: String },
    /// Successfully invited a user to a room.
    InviteSuccess { user_id: String },
    /// Failed to invite a user to a room.
    InviteFailed { error: String },
    /// The local user was invited to a room by someone else.
    RoomInvited { room_id: String, room_name: String, inviter_name: String },
    /// DM room is ready — navigate to it.
    DmReady { user_id: String, room_id: String, room_name: String },
    /// Results from a user directory search (display_name, user_id).
    UserSearchResults { results: Vec<(String, String)> },
    /// Users currently typing in a room.
    TypingUsers { room_id: String, names: Vec<String> },
    /// A sync gap was detected — the room's timeline was limited.
    /// The UI should re-fetch messages for this room if it's currently selected.
    SyncGap { room_id: String },
    /// Thread replies fetched — display in sidebar.
    ThreadReplies {
        room_id: String,
        thread_root_id: String,
        root_message: Option<MessageInfo>,
        replies: Vec<MessageInfo>,
    },
    /// Failed to create/find DM.
    DmFailed { error: String },
    /// Logged out — session wiped, show login page.
    LoggedOut,
    /// Room topic changed (MOTD plugin).
    #[cfg(feature = "motd")]
    TopicChanged { room_id: String, new_topic: String },
    /// New room keys arrived (from backup retry or key forwarding).
    /// Re-fetch any of these rooms if currently visible.
    RoomKeysReceived { room_ids: Vec<String> },
    /// Account registration succeeded.
    RegistrationSuccess { display_name: String, user_id: String },
    /// Account registration failed.
    RegistrationFailed { error: String },
    /// Recovery key was generated for a new account.
    RecoveryKeyGenerated { key: String },
    /// Key file import result.
    KeysImported { imported: u64, total: u64 },
    KeyImportFailed { error: String },
    /// Metrics export complete — path to the CSV file.
    MetricsReady { path: String, event_count: usize, metrics_text: String },
    /// Metrics export failed.
    MetricsFailed { error: String },
    /// Message export (JSONL) complete.
    MessagesExported { path: String, count: usize },
    /// Message export failed.
    MessagesExportFailed { error: String },
    /// Recent messages for the hover-preview AI summary.
    /// `is_unread` is true when the text contains only the unread window.
    RoomPreview { room_id: String, messages_text: String, is_unread: bool },
    /// Streaming chunk from Ollama inference (room preview or metrics summary).
    /// `done=true` signals the final chunk. On error, `text` is empty and `done=true`.
    OllamaChunk { context: String, chunk: String, done: bool },
    /// A watch term semantically matched a new message in this room.
    #[cfg(feature = "ai")]
    RoomAlert { room_id: String, room_name: String, matched_term: String },
    /// Community health score updated for a room (community-health plugin).
    #[cfg(feature = "community-health")]
    HealthUpdate {
        room_id: String,
        score: f32,
        /// +1 improving, 0 stable, −1 declining.
        trend: i8,
        alert: crate::plugins::community_health::AlertLevel,
    },
}

/// Commands sent FROM the GTK UI TO the Matrix thread.
#[derive(Debug, Clone)]
pub enum MatrixCommand {
    Login {
        homeserver: String,
        username: String,
        password: String,
    },
    SelectRoom {
        room_id: String,
        /// Unread count read from the UI badge before clear_unread() zeroed it.
        /// Used as a floor for quick_meta when the SDK store returns 0 pre-sync.
        known_unread: u32,
    },
    /// Re-fetch a room's messages without changing any UI state.
    /// Used after a sync gap to fill in events missed by the limited timeline.
    RefreshRoom { room_id: String },
    SendMessage {
        room_id: String,
        body: String,
        /// HTML formatted body (if markdown mode is on).
        formatted_body: Option<String>,
        /// Event ID to reply to (if replying).
        reply_to: Option<String>,
        /// Original sender + body for quote fallback (if replying).
        quote_text: Option<(String, String)>,
        /// True when the body was entered with `/me` or `:` prefix — sends m.emote.
        is_emote: bool,
        /// Matrix user IDs (@user:server) mentioned via nick completion.
        mentioned_user_ids: Vec<String>,
    },
    /// Send an emoji reaction to a message.
    SendReaction {
        room_id: String,
        event_id: String,
        emoji: String,
    },
    /// Accept an incoming verification request and start SAS.
    AcceptVerification { flow_id: String },
    /// Confirm that the displayed emojis match.
    ConfirmVerification { flow_id: String },
    /// Cancel a verification.
    CancelVerification { flow_id: String },
    /// Request verification of our own device from another session.
    RequestSelfVerification,
    /// Fetch older messages for a room (pagination).
    FetchOlderMessages {
        room_id: String,
        from_token: String,
    },
    /// Import secrets using a recovery key or passphrase.
    RecoverKeys { recovery_key: String },
    /// Fetch public room directory. `server` overrides the homeserver to query.
    BrowsePublicRooms { search_term: Option<String>, spaces_only: bool, server: Option<String> },
    /// Fetch the room directory for a space.
    BrowseSpaceRooms { space_id: String },
    /// Join a room by ID or alias.
    JoinRoom { room_id_or_alias: String, via_servers: Vec<String> },
    /// Delete (redact) a message.
    RedactMessage { room_id: String, event_id: String },
    /// Edit a message (send replacement).
    EditMessage { room_id: String, event_id: String, new_body: String, new_formatted_body: Option<String> },
    /// Upload and send a media file.
    SendMedia { room_id: String, file_path: String },
    /// Download media and open with system viewer.
    DownloadMedia { url: String, filename: String, source_json: String },
    /// Fetch and cache a member's avatar. No-op if already cached on disk.
    FetchAvatar { user_id: String, mxc_url: String },
    /// Fetch and cache a room's avatar. No-op if already cached on disk.
    FetchRoomAvatar { room_id: String, mxc_url: String },
    /// Toggle bookmark (m.favourite tag) on a room.
    SetFavourite { room_id: String, is_favourite: bool },
    /// Accept a room invitation.
    AcceptInvite { room_id: String },
    /// Decline a room invitation.
    DeclineInvite { room_id: String },
    /// Leave a room.
    LeaveRoom { room_id: String },
    /// Invite a Matrix user to a room.
    InviteUser { room_id: String, user_id: String },
    /// Search the homeserver user directory by display name or Matrix ID prefix.
    SearchUsers { query: String },
    /// Send read receipt for the latest message in a room.
    MarkRead { room_id: String },
    /// Open or create a DM with a user. Finds existing DM room or creates one.
    CreateDm { user_id: String },
    /// Send typing indicator to the current room.
    TypingNotice { room_id: String, typing: bool },
    /// Fetch thread replies for a message.
    FetchThreadReplies { room_id: String, thread_root_id: String },
    /// Log out: deactivate the access token, wipe local data, and restart at login.
    Logout,
    /// Download room keys directly from whichever backup version the SSSS
    /// decryption key (already in SQLite after a successful recover() call)
    /// actually belongs to. This tries backup versions descending from the
    /// current one until it finds one whose keys decrypt correctly.
    DownloadFromSsssBackup,
    /// Import session keys from an Element-exported key file.
    ImportRoomKeys { path: std::path::PathBuf, passphrase: String },
    /// Export room metrics (moderation events + activity) to CSV.
    ExportRoomMetrics { room_id: String, days: u32 },
    /// Fetch up to `limit` recent messages for a room and write them to `path` as JSONL.
    ExportMessages { room_id: String, path: std::path::PathBuf, limit: u32 },
    /// Fetch recent messages for the hover-preview AI summary and run Ollama inference.
    /// `unread_count` > 0 means only the last N unread messages should be summarised.
    FetchRoomPreview {
        room_id: String,
        unread_count: u32,
        ollama_endpoint: String,
        ollama_model: String,
        extra_instructions: String,
    },
    /// Abort any in-flight AI room preview request (sent when the user cancels).
    CancelRoomPreview,
    /// Run Ollama inference for metrics summary on the tokio thread.
    RunOllamaMetrics {
        prompt: String,
        endpoint: String,
        model: String,
    },
    /// Preload the Ollama model in the background so the first inference is fast.
    WarmupOllama { endpoint: String, model: String },
    /// Fetch event context so the UI can jump to a specific event in the timeline.
    SeekToEvent {
        room_id: String,
        event_id: String,
    },
    /// Drain the command queue and then signal the sync loop to stop.
    /// Must be sent LAST by the application — all preceding commands run first.
    Shutdown,
    /// Register a new account on a homeserver.
    Register {
        homeserver: String,
        username: String,
        password: String,
        display_name: String,
        email: String,
    },
}

/// Session data serialized into the keyring. The access token and auth
/// details are stored in GNOME Keyring via the Secret Service D-Bus API
/// (oo7 crate), not in a plaintext file on disk.
#[derive(Serialize, Deserialize)]
struct PersistedSession {
    homeserver: String,
    /// The resolved homeserver URL (e.g. "https://matrix.gnome.org/").
    /// Populated after the first successful login/restore so that subsequent
    /// restores can use `.homeserver_url()` and skip the `.well-known` network
    /// lookup — allowing offline startup without losing the session.
    #[serde(default)]
    homeserver_url: Option<String>,
    session: MatrixSession,
}

/// The attributes used to look up our secret in the keyring.
const KEYRING_LABEL: &str = "Hikyaku Matrix session";

/// Keyring application attribute — namespaced per-profile so the wizard
/// sandbox never collides with the real session.
fn keyring_app_attr() -> &'static str {
    if std::env::var_os("HIKYAKU_WIZARD").is_some() {
        "me.ramkrishna.hikyaku.wizard"
    } else {
        "me.ramkrishna.hikyaku"
    }
}

/// Compute the number of unread messages when entering a room.
///
/// Priority:
///   1. Count messages that come AFTER the `fully_read` marker in the fetched
///      window.  This is precise and works even when the SDK notification count
///      is 0 (e.g. before the first sync has refreshed counts from the server,
///      or when the Messages API was used instead of the sync timeline).
///   2. Fall back to `max(sdk_count, known_unread)` when the marker is absent
///      or outside the current window.
///
/// `messages` must be **oldest-first** (the order returned after reversal in
/// `extract_messages`).
pub(crate) fn compute_enter_unread(
    messages: &[crate::matrix::MessageInfo],
    fully_read: Option<&str>,
    sdk_count: u32,
    known_unread: u32,
) -> u32 {
    if let Some(eid) = fully_read {
        if let Some(pos) = messages.iter().position(|m| m.event_id == eid) {
            // Messages at pos+1..len are after the fully_read marker → new.
            return (messages.len().saturating_sub(pos + 1)) as u32;
        }
    }
    sdk_count.max(known_unread)
}

/// Merge disk-cached unread/highlight counts into a freshly-queried room list.
///
/// The SDK's `unread_notification_counts()` may return 0 for all rooms
/// before the first sync has refreshed notification counts from the server.
/// This function preserves the higher count so badges from the previous
/// session are not lost during the pre-sync window on startup.
///
/// After the first sync fires the SDK has authoritative counts and will
/// overwrite the disk cache again — so this only guards the narrow window
/// between startup and first-sync-complete.
pub(crate) fn merge_disk_unread_counts(
    rooms: &mut [RoomInfo],
    disk: &std::collections::HashMap<String, (u64, u64)>,
) {
    for room in rooms {
        if let Some(&(disk_un, disk_hl)) = disk.get(&room.room_id) {
            room.unread_count    = room.unread_count.max(disk_un);
            room.highlight_count = room.highlight_count.max(disk_hl);
        }
    }
}

fn room_list_cache_path() -> PathBuf {
    let mut p = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("hikyaku");
    p.push("room_list_cache.json");
    p
}

fn save_room_list_cache(rooms: &[RoomInfo]) {
    let path = room_list_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(rooms) {
        let _ = std::fs::write(&path, json);
    }
}

/// Zero a single room's unread/highlight counts in the disk cache.
/// Called when the user opens a room so that future merge-saves don't
/// resurrect a stale badge for a room they have already read.
fn zero_room_unread_in_disk_cache(room_id: &str) {
    let mut rooms = load_room_list_cache();
    let mut changed = false;
    for room in &mut rooms {
        if room.room_id == room_id && (room.unread_count > 0 || room.highlight_count > 0) {
            room.unread_count = 0;
            room.highlight_count = 0;
            changed = true;
            break;
        }
    }
    if changed {
        save_room_list_cache(&rooms);
    }
}

fn load_room_list_cache() -> Vec<RoomInfo> {
    let path = room_list_cache_path();
    let Ok(data) = std::fs::read_to_string(&path) else { return Vec::new() };
    serde_json::from_str(&data).unwrap_or_default()
}


fn db_dir_path(homeserver: &str) -> PathBuf {
    // Sanitise the homeserver string into a safe directory name.
    // Strip scheme, replace characters invalid in path components.
    let clean = homeserver
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .replace(['/', '\\', ':', '?', '#'], "_");
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("hikyaku");
    path.push("db");
    path.push(clean);
    path
}

/// One-time migration: move ~/.local/share/matx/ → ~/.local/share/hikyaku/
/// if the old directory exists and the new one does not yet.
/// Safe to call on every startup — no-op once migration is done.
fn migrate_legacy_storage() {
    let Some(data_dir) = dirs::data_dir() else { return };
    let old_base = data_dir.join("matx");
    let new_base = data_dir.join("hikyaku");
    if !old_base.exists() || new_base.exists() {
        return;
    }
    match std::fs::rename(&old_base, &new_base) {
        Ok(()) => tracing::info!(
            "Migrated storage: {} → {}", old_base.display(), new_base.display()
        ),
        Err(e) => tracing::warn!(
            "Could not migrate storage from {} to {}: {e}",
            old_base.display(), new_base.display()
        ),
    }
}

/// Save session to GNOME Keyring via Secret Service.
async fn save_session_to_keyring(
    persisted: &PersistedSession,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let keyring = oo7::Keyring::new().await?;
    let json = serde_json::to_string(persisted)?;
    let attributes = vec![("application", keyring_app_attr())];
    keyring
        .create_item(KEYRING_LABEL, &attributes, json, true)
        .await?;
    tracing::info!("Session saved to GNOME Keyring");
    Ok(())
}

/// Load session from GNOME Keyring.
async fn load_session_from_keyring() -> Option<PersistedSession> {
    let keyring = oo7::Keyring::new().await.ok()?;
    let attributes = vec![("application", keyring_app_attr())];
    let items = keyring.search_items(&attributes).await.ok()?;
    let item = items.first()?;
    let secret = item.secret().await.ok()?;
    let json = std::str::from_utf8(&secret).ok()?;
    serde_json::from_str(json).ok()
}

/// Delete session from GNOME Keyring.
async fn delete_session_from_keyring() {
    if let Ok(keyring) = oo7::Keyring::new().await {
        let attributes = vec![("application", keyring_app_attr())];
        if let Ok(items) = keyring.search_items(&attributes).await {
            for item in items {
                let _ = item.delete().await;
            }
        }
    }
}

/// Remove all stored session data for a homeserver so the next login starts clean.
async fn cleanup_session(homeserver: &str) {
    let db_path = db_dir_path(homeserver);
    tracing::info!("Cleaning up session data for {homeserver}");
    delete_session_from_keyring().await;
    let _ = std::fs::remove_dir_all(&db_path);
}

/// Spawn a background thread running a tokio runtime for the Matrix SDK.
/// Spawn the Matrix thread.  Shutdown is initiated by sending
/// `MatrixCommand::Shutdown` through the command channel — all commands
/// queued before it are guaranteed to run first.
pub fn spawn_matrix_thread(
    event_tx: Sender<MatrixEvent>,
    command_rx: Receiver<MatrixCommand>,
) -> super::room_cache::RoomCache {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Create the cache before spawning so the GTK side gets a clone it can
    // query synchronously on room selection — eliminating the async round-trip
    // for warm-cache rooms.
    let timeline_cache = super::room_cache::RoomCache::new();
    let thread_cache = timeline_cache.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");

        rt.block_on(async move {
            matrix_task(event_tx, command_rx, shutdown_rx, shutdown_tx, thread_cache).await;
        });

        tracing::info!("Matrix thread shut down cleanly");
    });

    timeline_cache
}

async fn matrix_task(
    event_tx: Sender<MatrixEvent>,
    command_rx: Receiver<MatrixCommand>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    timeline_cache: super::room_cache::RoomCache,
) {
    // Move legacy "matx" storage to "hikyaku" before touching the DB.
    migrate_legacy_storage();

    // Credentials from a fresh interactive login, passed to setup_encryption
    // so it can satisfy the UIAA challenge for new accounts.  None for restored
    // sessions (password is never persisted to disk).
    let mut login_creds: Option<(String, String)> = None;

    // In wizard mode there is no session to restore — always show the setup flow.
    let client = if std::env::var_os("HIKYAKU_WIZARD").is_some() {
        None
    // Try to restore a previous session first.
    } else if let Some(client) = try_restore_session(&event_tx).await {
        Some(client)
    } else {
        None
    };
    let client = if let Some(client) = client {
        client
    } else {
        // No saved session — tell the UI to show the login page.
        let _ = event_tx.send(MatrixEvent::LoginRequired).await;
        // Wait for login command from the UI.
        loop {
            match command_rx.recv().await {
                Ok(MatrixCommand::Login {
                    homeserver,
                    username,
                    password,
                }) => {
                    match do_login(&homeserver, &username, &password).await {
                        Ok(client) => {
                            login_creds = Some((username.clone(), password.clone()));
                            let display_name = username.clone();
                            let user_id = client.user_id().map(|u| u.to_string()).unwrap_or_default();
                            let _ = event_tx.send(MatrixEvent::LoginSuccess { display_name, user_id, from_registration: false, is_fresh_login: true }).await;
                            break client;
                        }
                        Err(e) => {
                            cleanup_session(&homeserver).await;
                            let _ = event_tx
                                .send(MatrixEvent::LoginFailed {
                                    error: e.to_string(),
                                })
                                .await;
                        }
                    }
                }
                Ok(MatrixCommand::Register { homeserver, username, password, display_name, email }) => {
                    match do_register(&homeserver, &username, &password, &display_name, &email).await {
                        Ok(client) => {
                            login_creds = Some((username.clone(), password.clone()));
                            let display = if display_name.is_empty() { username.clone() } else { display_name.clone() };
                            let user_id = client.user_id().map(|u| u.to_string()).unwrap_or_default();
                            let _ = event_tx.send(MatrixEvent::LoginSuccess { display_name: display, user_id, from_registration: true, is_fresh_login: true }).await;
                            break client;
                        }
                        Err(e) => {
                            tracing::error!("Registration/login failed: {e}");
                            let _ = event_tx.send(MatrixEvent::RegistrationFailed { error: e.to_string() }).await;
                        }
                    }
                }
                Ok(_) => {
                    tracing::warn!("Ignoring command before login");
                }
                Err(_) => return, // Channel closed, UI is gone.
            }
        }
    };

    // Set up verification state and handlers.
    let vs: super::verification::SharedVerificationState =
        std::sync::Arc::new(tokio::sync::Mutex::new(
            super::verification::VerificationState::new(),
        ));
    super::verification::register_verification_handlers(
        &client, event_tx.clone(), vs.clone(),
    );

    // NOTE: event cache (enable_storage + subscribe) intentionally disabled.
    // We use room.messages() + our own in-memory cache instead. The SDK's
    // event cache processes every event for all rooms, causing unnecessary
    // CPU and memory pressure for our use case.

    // timeline_cache is passed in from spawn_matrix_thread (created before the
    // thread so the GTK side holds a clone for synchronous cache reads).

    // Run E2E encryption setup in the background — it makes several network
    // calls (cross-signing probe, key backup check) that previously blocked
    // the room list from appearing. Sync starts in parallel so DMs show up
    // from the disk cache immediately.
    let enc_client = client.clone();
    let enc_tx = event_tx.clone();
    let enc_creds = login_creds.take();
    tokio::spawn(async move {
        super::encryption::setup_encryption(&enc_client, &enc_tx, enc_creds).await;
    });

    // Rooms that received new messages while their memory cache was cold.
    // Stores the actual pending MessageInfo objects so SelectRoom can apply
    // them directly without a network round-trip.
    // HashMap<room_id, Vec<pending_messages>>
    let dirty_rooms: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<crate::matrix::MessageInfo>>>> =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

    // Spawn sync in a separate task so we can keep processing commands.
    let sync_event_tx = event_tx.clone();
    let sync_client = client.clone();
    let sync_shutdown = shutdown_rx.clone();
    let sync_cache = timeline_cache.clone();
    let sync_dirty = dirty_rooms.clone();
    tokio::spawn(async move {
        start_sync(sync_client, &sync_event_tx, sync_shutdown, sync_cache, sync_dirty).await;
    });

    // Watch for newly imported room keys (from backup retry or key forwarding)
    // and tell the UI to re-fetch affected rooms so UTD messages re-render.
    super::encryption::spawn_keys_watcher(&client, event_tx.clone(), timeline_cache.clone());

    // Active typing subscription task — cancelled when switching rooms.
    let mut typing_task: Option<tokio::task::JoinHandle<()>> = None;
    // Background room-data refresh task.  We detach (drop) rather than abort
    // stale tasks to avoid propagating JoinError::Cancelled into deadpool-
    // runtime's internal spawn_blocking, which panics on non-panic JoinErrors.
    // A generation counter ensures only the latest task clears the priority flag.
    let mut bg_refresh_task: Option<tokio::task::JoinHandle<()>> = None;
    // Cooperative cancel flag for the current bg_refresh_task.  Setting it to
    // true asks the running task to exit at its next await checkpoint.  We use
    // this instead of task.abort() to avoid propagating JoinError::Cancelled
    // into deadpool-runtime's spawn_blocking wrapper, which panics on that.
    let mut bg_cancel: std::sync::Arc<std::sync::atomic::AtomicBool> =
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));


    // Track the most recent MarkRead task so Shutdown can await it.
    // MarkRead is fire-and-forget during normal operation (doesn't block
    // SelectRoom) but must complete before Shutdown drops the runtime.
    let mut pending_mark_read: Option<tokio::task::JoinHandle<()>> = None;

    // Track the in-flight AI room-preview task so we can abort it when
    // the user cancels or a new request arrives before the old one finishes.
    let mut pending_preview: Option<tokio::task::JoinHandle<()>> = None;

    // Process commands while sync runs in the background.
    let mut shutdown_rx = shutdown_rx;
    loop {
        tokio::select! {
            cmd = command_rx.recv() => {
                match cmd {
                    Ok(MatrixCommand::Login { .. }) => {
                        tracing::warn!("Already logged in, ignoring duplicate login command");
                    }
                    Ok(MatrixCommand::Register { .. }) => {
                        tracing::warn!("Already logged in, ignoring register command");
                    }
                    Ok(MatrixCommand::SelectRoom { room_id, known_unread }) => {
                        // Zero this room in the disk cache so that merge-saves during
                        // subsequent syncs don't resurrect a stale unread badge.
                        let zrid = room_id.clone();
                        tokio::task::spawn_blocking(move || zero_room_unread_in_disk_cache(&zrid));
                        // Signal the previous bg task to exit cooperatively at its
                        // next await checkpoint, then detach it.  We do NOT abort()
                        // because that propagates JoinError::Cancelled into deadpool-
                        // runtime's spawn_blocking wrapper, which panics.  Setting the
                        // cancel flag is safe: the task checks it and returns early,
                        // so SQLite connections from zombie tasks are released quickly
                        // instead of piling up (which caused "gets slower over time").
                        bg_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        drop(bg_refresh_task.take());
                        // Fresh cancel flag for the new task.
                        bg_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                        let task_cancel = bg_cancel.clone();
                        let bg_gen = BG_REFRESH_GENERATION
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

                        // Signal low-priority background tasks to yield.
                        ROOM_LOAD_IN_PROGRESS.store(true, std::sync::atomic::Ordering::Relaxed);

                        // Check in-memory cache first — show instantly if available.
                        let cached_data = timeline_cache.get_memory(&room_id);
                        if let Some((msgs, prev_batch, mut meta)) = cached_data {
                            // Refresh unread_count from the SDK's local store — the cached
                            // meta value is frozen at the last bg_refresh but new messages
                            // may have arrived via append_memory since then.
                            // Use known_unread as a floor in case the SDK returns 0 pre-sync.
                            if let Ok(rid) = RoomId::parse(&room_id) {
                                if let Some(r) = client.get_room(&rid) {
                                    let sdk_unread = r.unread_notification_counts().notification_count as u32;
                                    meta.unread_count = sdk_unread.max(known_unread);
                                } else {
                                    meta.unread_count = meta.unread_count.max(known_unread);
                                }
                            } else {
                                meta.unread_count = meta.unread_count.max(known_unread);
                            }
                            let _ = event_tx.send(MatrixEvent::RoomMessages {
                                room_id: room_id.clone(),
                                messages: msgs,
                                prev_batch_token: prev_batch,
                                room_meta: meta,
                                is_background: false,
                            }).await;
                        } else {
                            // Disk cache read: offload to blocking thread so the async
                            // command loop isn't stalled by filesystem I/O.
                            let disk_cache = timeline_cache.clone();
                            let disk_room_id = room_id.clone();
                            let disk_data = tokio::task::spawn_blocking(move || {
                                disk_cache.load_disk(&disk_room_id)
                            }).await.ok().flatten();

                            if let Some((disk_msgs, disk_token)) = disk_data {
                                // Fetch unread count for the "New messages" divider.
                                // The SDK store may return 0 before the first sync completes
                                // (notification counts aren't persisted reliably pre-sync).
                                // Use known_unread (from the UI badge before clear_unread) as a
                                // floor so the divider and tinting are shown immediately.
                                let quick_meta = if let Ok(rid) = RoomId::parse(&room_id) {
                                    if let Some(r) = client.get_room(&rid) {
                                        let sdk_unread = r.unread_notification_counts().notification_count as u32;
                                        let unread = sdk_unread.max(known_unread);
                                        RoomMeta { unread_count: unread, ..Default::default() }
                                    } else { RoomMeta { unread_count: known_unread, ..Default::default() } }
                                } else { RoomMeta { unread_count: known_unread, ..Default::default() } };
                                // Disk cache hit — show instantly while bg_refresh fetches fresh data.
                                // Seed in-memory cache so a rapid re-visit doesn't re-read disk.
                                timeline_cache.insert_memory_if_absent(
                                    &room_id,
                                    disk_msgs.clone(),
                                    disk_token.clone(),
                                    quick_meta.clone(),
                                );
                                let _ = event_tx.send(MatrixEvent::RoomMessages {
                                    room_id: room_id.clone(),
                                    messages: disk_msgs,
                                    prev_batch_token: disk_token,
                                    room_meta: quick_meta,
                                    is_background: false,
                                }).await;
                            } else {
                                tracing::info!("Cache miss (memory + disk) for {}", room_id);
                            }
                        }

                        // Drain any pending messages that arrived while memory was cold.
                        // These are the actual MessageInfo objects stored by the sync
                        // handler when append_memory() failed (cache was cold at that time).
                        let pending_msgs: Vec<crate::matrix::MessageInfo> =
                            dirty_rooms.lock().unwrap().remove(&room_id).unwrap_or_default();
                        let has_mem = timeline_cache.has_memory(&room_id);
                        tracing::info!(
                            "SelectRoom {}: has_memory={} pending_msgs={}",
                            room_id, has_mem, pending_msgs.len()
                        );

                        // Apply any pending messages (arrived while cache was cold).
                        if !pending_msgs.is_empty() {
                            for msg in &pending_msgs {
                                timeline_cache.append_memory(&room_id, msg.clone());
                            }
                            if let Some((msgs, token, mut meta)) = timeline_cache.get_memory(&room_id) {
                                meta.unread_count = meta.unread_count.max(known_unread);
                                let disk_cache = timeline_cache.clone();
                                let disk_rid = room_id.clone();
                                let disk_msgs = msgs.clone();
                                let disk_tok = token.clone();
                                tokio::task::spawn_blocking(move || {
                                    disk_cache.save_disk(&disk_rid, &disk_msgs, disk_tok.as_deref());
                                });
                                let _ = event_tx.send(MatrixEvent::RoomMessages {
                                    room_id: room_id.clone(),
                                    messages: msgs,
                                    prev_batch_token: token,
                                    room_meta: meta,
                                    is_background: false,
                                }).await;
                            }
                        }

                        // Skip the server fetch if the cache was populated within the
                        // last 60 seconds — the data is fresh enough.  SyncGap events
                        // call invalidate_room() which clears the freshness timestamp,
                        // so gap rooms always get a full refetch regardless of age.
                        let fresh = timeline_cache.is_fresh(
                            &room_id, std::time::Duration::from_secs(60),
                        );
                        if fresh {
                            tracing::debug!(
                                "SelectRoom {room_id}: cache fresh, skipping server fetch"
                            );
                            ROOM_LOAD_IN_PROGRESS.store(false, std::sync::atomic::Ordering::Relaxed);
                            // Cancel token still needs to be released so a future SelectRoom
                            // can spawn a new task cleanly.
                            bg_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        } else {
                        let _ = event_tx.send(MatrixEvent::BgRefreshStarted {
                            room_id: room_id.clone(),
                        }).await;
                        let bg_client = client.clone();
                        let bg_tx = event_tx.clone();
                        let bg_cache = timeline_cache.clone();
                        let bg_room_id = room_id.clone();
                        let bg_known_unread = known_unread;
                        bg_refresh_task = Some(tokio::spawn(async move {
                            if task_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                            handle_select_room_bg(
                                &bg_client, &bg_tx, &bg_room_id,
                                bg_cache, task_cancel,
                                bg_known_unread,
                            ).await;
                            if BG_REFRESH_GENERATION.load(
                                std::sync::atomic::Ordering::Relaxed,
                            ) == bg_gen {
                                ROOM_LOAD_IN_PROGRESS.store(
                                    false, std::sync::atomic::Ordering::Relaxed,
                                );
                            }
                        }));
                        } // end !fresh

                        // Cancel previous typing subscription and start new one.
                        if let Some(task) = typing_task.take() {
                            task.abort();
                        }
                        if let Ok(rid) = RoomId::parse(&room_id) {
                            if let Some(room) = client.get_room(&rid) {
                                let (guard, mut typing_rx) = room.subscribe_to_typing_notifications();
                                let typing_tx = event_tx.clone();
                                let typing_room = room.clone();
                                let typing_rid = room_id.clone();
                                typing_task = Some(tokio::spawn(async move {
                                    let _guard = guard;
                                    while let Ok(user_ids) = typing_rx.recv().await {
                                        let mut names = Vec::new();
                                        for uid in &user_ids {
                                            let name = resolve_display_name(&typing_room, uid).await;
                                            names.push(name);
                                        }
                                        let _ = typing_tx.send(MatrixEvent::TypingUsers {
                                            room_id: typing_rid.clone(),
                                            names,
                                        }).await;
                                    }
                                }));
                            }
                        }
                    }
                    Ok(MatrixCommand::RefreshRoom { room_id }) => {
                        // Force bg_refresh for the room regardless of cache state.
                        // Used after a sync gap to fill in events missed by the
                        // limited timeline.  Does NOT change any UI selection state.
                        // Wipe both memory AND disk so handle_select_room_bg fetches
                        // fresh from the server instead of replaying the stale JSON.
                        timeline_cache.remove(&room_id);
                        let bg_client = client.clone();
                        let bg_tx = event_tx.clone();
                        let bg_cache = timeline_cache.clone();
                        tokio::spawn(async move {
                            handle_select_room_bg(
                                &bg_client, &bg_tx, &room_id,
                                bg_cache,
                                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                                0,
                            ).await;
                        });
                    }
                    Ok(MatrixCommand::SendMessage { room_id, body, formatted_body, reply_to, quote_text, is_emote, mentioned_user_ids }) => {
                        handle_send_message(&client, &event_tx, &room_id, &body, formatted_body.as_deref(), reply_to.as_deref(), quote_text.as_ref(), is_emote, &mentioned_user_ids).await;
                    }
                    Ok(MatrixCommand::SendReaction { room_id, event_id, emoji }) => {
                        handle_send_reaction(&client, &room_id, &event_id, &emoji).await;
                    }
                    Ok(MatrixCommand::AcceptVerification { flow_id }) => {
                        super::verification::accept_verification(
                            &vs, &event_tx, &flow_id,
                        ).await;
                    }
                    Ok(MatrixCommand::ConfirmVerification { flow_id }) => {
                        super::verification::confirm_verification(&vs, &flow_id).await;
                    }
                    Ok(MatrixCommand::CancelVerification { flow_id }) => {
                        super::verification::cancel_verification(&vs, &flow_id).await;
                    }
                    Ok(MatrixCommand::RequestSelfVerification) => {
                        super::verification::request_self_verification(
                            &client, &event_tx, &vs,
                        ).await;
                    }
                    Ok(MatrixCommand::FetchOlderMessages { room_id, from_token }) => {
                        handle_fetch_older(&client, &event_tx, &room_id, &from_token).await;
                    }
                    Ok(MatrixCommand::RecoverKeys { recovery_key }) => {
                        super::encryption::handle_recover_keys(&client, &event_tx, &recovery_key, &timeline_cache).await;
                    }
                    Ok(MatrixCommand::BrowsePublicRooms { search_term, spaces_only, server }) => {
                        handle_browse_public_rooms(&client, &event_tx, search_term.as_deref(), spaces_only, server.as_deref()).await;
                    }
                    Ok(MatrixCommand::BrowseSpaceRooms { space_id }) => {
                        handle_browse_space(&client, &event_tx, &space_id).await;
                    }
                    Ok(MatrixCommand::JoinRoom { room_id_or_alias, via_servers }) => {
                        handle_join_room(&client, &event_tx, &room_id_or_alias, &via_servers).await;
                    }
                    Ok(MatrixCommand::RedactMessage { room_id, event_id }) => {
                        if let (Ok(rid), Ok(eid)) = (RoomId::parse(&room_id), matrix_sdk::ruma::EventId::parse(&event_id)) {
                            if let Some(room) = client.get_room(&rid) {
                                if let Err(e) = room.redact(&eid, None, None).await {
                                    tracing::error!("Failed to redact: {e}");
                                }
                                // Only wipe memory — disk stays so a sync gap doesn't
                                // cause a blank screen. bg_refresh will refresh the room.
                                timeline_cache.remove_memory(&room_id);
                            }
                        }
                    }
                    Ok(MatrixCommand::EditMessage { room_id, event_id, new_body, new_formatted_body }) => {
                        if let (Ok(rid), Ok(eid)) = (RoomId::parse(&room_id), matrix_sdk::ruma::EventId::parse(&event_id)) {
                            if let Some(room) = client.get_room(&rid) {
                                use matrix_sdk::ruma::events::room::message::{
                                    RoomMessageEventContent, ReplacementMetadata,
                                };
                                let new_formatted = new_formatted_body
                                    .as_deref()
                                    .unwrap_or_else(|| "")
                                    .to_string();
                                let use_html = !new_formatted.is_empty();
                                timeline_cache.update_message_body_in_cache(
                                    &room_id, &event_id, &new_body,
                                    if use_html { Some(&new_formatted) } else { None },
                                );
                                let metadata = ReplacementMetadata::new(eid.to_owned(), None);
                                let content = if use_html {
                                    RoomMessageEventContent::text_html(&new_body, &new_formatted)
                                        .make_replacement(metadata, None)
                                } else {
                                    RoomMessageEventContent::text_plain(&new_body)
                                        .make_replacement(metadata, None)
                                };
                                if let Err(e) = room.send(content).await {
                                    tracing::error!("Failed to edit message: {e}");
                                }
                            }
                        }
                    }
                    Ok(MatrixCommand::SendMedia { room_id, file_path }) => {
                        handle_send_media(&client, &event_tx, &room_id, &file_path).await;
                    }
                    Ok(MatrixCommand::FetchRoomAvatar { room_id, mxc_url }) => {
                        let bg_client = client.clone();
                        let bg_tx = event_tx.clone();
                        tokio::spawn(async move {
                            // Limit to 4 concurrent downloads — prevents 295 simultaneous
                            // HTTP requests from stalling the tokio thread pool.
                            let _permit = AVATAR_PERMITS.acquire().await;
                            handle_fetch_room_avatar(&bg_client, &bg_tx, &room_id, &mxc_url).await;
                        });
                    }
                    Ok(MatrixCommand::FetchAvatar { user_id, mxc_url }) => {
                        let bg_client = client.clone();
                        let bg_tx = event_tx.clone();
                        tokio::spawn(async move {
                            let _permit = AVATAR_PERMITS.acquire().await;
                            handle_fetch_avatar(&bg_client, &bg_tx, &user_id, &mxc_url).await;
                        });
                    }
                    Ok(MatrixCommand::DownloadMedia { url, filename, source_json }) => {
                        handle_download_media(&client, &event_tx, &url, &filename, &source_json).await;
                    }
                    Ok(MatrixCommand::SetFavourite { room_id, is_favourite }) => {
                        if let Ok(rid) = RoomId::parse(&room_id) {
                            if let Some(room) = client.get_room(&rid) {
                                if let Err(e) = room.set_is_favourite(is_favourite, None).await {
                                    tracing::error!("Failed to set favourite: {e}");
                                }
                            }
                        }
                    }
                    Ok(MatrixCommand::AcceptInvite { room_id }) => {
                        handle_accept_invite(&client, &event_tx, &room_id).await;
                    }
                    Ok(MatrixCommand::DeclineInvite { room_id }) => {
                        handle_decline_invite(&client, &event_tx, &room_id).await;
                    }
                    Ok(MatrixCommand::LeaveRoom { room_id }) => {
                        handle_leave_room(&client, &event_tx, &room_id).await;
                    }
                    Ok(MatrixCommand::InviteUser { room_id, user_id }) => {
                        handle_invite_user(&client, &event_tx, &room_id, &user_id).await;
                    }
                    Ok(MatrixCommand::SearchUsers { query }) => {
                        handle_search_users(&client, &event_tx, &query).await;
                    }
                    Ok(MatrixCommand::MarkRead { room_id }) => {
                        tracing::info!("MarkRead command received for {room_id}");
                        // Get the latest event_id from our own timeline cache —
                        // room.latest_event() only reflects the SDK's sync-timeline
                        // and is None when we used room.messages() pagination instead.
                        let cached_event_id = timeline_cache.latest_event_id(&room_id);
                        let mr_client = client.clone();
                        pending_mark_read = Some(tokio::spawn(async move {
                            handle_mark_read(&mr_client, &room_id, cached_event_id).await;
                        }));
                    }
                    Ok(MatrixCommand::CreateDm { user_id }) => {
                        handle_create_dm(&client, &event_tx, &user_id).await;
                    }
                    Ok(MatrixCommand::TypingNotice { room_id, typing }) => {
                        // Fire-and-forget: typing notices must not block SelectRoom.
                        let tn_client = client.clone();
                        tokio::spawn(async move {
                            if let Ok(rid) = RoomId::parse(&room_id) {
                                if let Some(room) = tn_client.get_room(&rid) {
                                    let _ = room.typing_notice(typing).await;
                                }
                            }
                        });
                    }
                    Ok(MatrixCommand::FetchThreadReplies { room_id, thread_root_id }) => {
                        handle_fetch_thread(&client, &event_tx, &room_id, &thread_root_id).await;
                    }
                    Ok(MatrixCommand::SeekToEvent { room_id, event_id }) => {
                        handle_seek_to_event(&client, &event_tx, &room_id, &event_id).await;
                    }
                    Ok(MatrixCommand::Logout) => {
                        tracing::info!("Logging out…");
                        // Invalidate the access token on the server.
                        if let Err(e) = client.matrix_auth().logout().await {
                            tracing::warn!("Server logout failed (continuing anyway): {e}");
                        }
                        // Wipe local session data (keyring + SQLite store).
                        let hs = client.homeserver().host_str()
                            .unwrap_or("unknown").to_string();
                        cleanup_session(&hs).await;
                        // Tell UI to show login page.
                        let _ = event_tx.send(MatrixEvent::LoggedOut).await;
                        break;
                    }
                    Ok(MatrixCommand::DownloadFromSsssBackup) => {
                        super::encryption::handle_download_from_ssss_backup(&client, &event_tx).await;
                    }
                    Ok(MatrixCommand::ImportRoomKeys { path, passphrase }) => {
                        super::encryption::handle_import_room_keys(&client, &event_tx, path, &passphrase, &timeline_cache).await;
                    }
                    Ok(MatrixCommand::ExportRoomMetrics { room_id, days }) => {
                        handle_export_room_metrics(&client, &event_tx, &room_id, days).await;
                    }
                    Ok(MatrixCommand::ExportMessages { room_id, path, limit: _ }) => {
                        handle_export_messages(&client, &event_tx, &room_id, &path, &timeline_cache).await;
                    }
                    Ok(MatrixCommand::FetchRoomPreview { room_id, unread_count, ollama_endpoint, ollama_model, extra_instructions }) => {
                        // Abort any previous preview task before starting a new one.
                        // This prevents multiple concurrent inference requests from
                        // queueing up and delivering stale results after a cancel.
                        if let Some(h) = pending_preview.take() {
                            h.abort();
                        }
                        let bg_client = client.clone();
                        let bg_tx = event_tx.clone();
                        pending_preview = Some(tokio::spawn(async move {
                            // Yield until room load finishes — LLM inference does
                            // SQLite reads for the room's message history.
                            yield_if_room_loading().await;
                            handle_fetch_room_preview(
                                &bg_client, &bg_tx, &room_id, unread_count,
                                &ollama_endpoint, &ollama_model, &extra_instructions,
                            ).await;
                        }));
                    }
                    Ok(MatrixCommand::CancelRoomPreview) => {
                        if let Some(h) = pending_preview.take() {
                            tracing::info!("CancelRoomPreview: aborting in-flight preview task");
                            h.abort();
                        }
                    }
                    Ok(MatrixCommand::RunOllamaMetrics { prompt, endpoint, model }) => {
                        let bg_tx = event_tx.clone();
                        tokio::spawn(async move {
                            yield_if_room_loading().await;
                            ollama_stream_to_event(
                                &endpoint, &model, &prompt, "metrics", &bg_tx,
                            ).await;
                        });
                    }
                    Ok(MatrixCommand::WarmupOllama { endpoint, model }) => {
                        tokio::spawn(async move {
                            yield_if_room_loading().await;
                            warmup_ollama_model(&endpoint, &model).await;
                        });
                    }
                    Ok(MatrixCommand::Shutdown) => {
                        // All preceding commands have been processed.  Await any
                        // in-flight MarkRead so the receipt reaches the server before
                        // the runtime is dropped.
                        tracing::info!("Shutdown: awaiting pending MarkRead (if any)");
                        if let Some(h) = pending_mark_read.take() {
                            let _ = h.await;
                            tracing::info!("Shutdown: MarkRead completed");
                        } else {
                            tracing::info!("Shutdown: no pending MarkRead");
                        }
                        tracing::info!("Shutdown command processed, stopping");
                        let _ = shutdown_tx.send(true);
                        break;
                    }
                    Err(_) => break,
                }
            }
            // Keep as a fallback for OS-level SIGTERM (process kill without Shutdown command).
            _ = shutdown_rx.changed() => {
                tracing::info!("Shutdown watch fired, stopping command loop");
                break;
            }
        }
    }
}

pub(crate) async fn do_login(
    homeserver: &str,
    username: &str,
    password: &str,
) -> Result<Client, Box<dyn std::error::Error + Send + Sync>> {
    let db_path = db_dir_path(homeserver);
    tracing::info!("do_login: sqlite store path = {db_path:?}");
    std::fs::create_dir_all(&db_path)?;

    let enc = matrix_sdk::encryption::EncryptionSettings {
        backup_download_strategy:
            matrix_sdk::encryption::BackupDownloadStrategy::AfterDecryptionFailure,
        ..Default::default()
    };

    // If the user supplied a full URL (http:// or https://) use it directly —
    // this is needed for local dev servers (e.g. http://127.0.0.1:6167) where
    // there is no .well-known and plain HTTP must be used.
    // Otherwise treat the input as a bare server name and let the SDK resolve it.
    let client = if homeserver.starts_with("http://") || homeserver.starts_with("https://") {
        Client::builder()
            .homeserver_url(homeserver)
            .sqlite_store(&db_path, None)
            .with_encryption_settings(enc)
    } else {
        let server_name = ServerName::parse(homeserver)?;
        Client::builder()
            .server_name(&server_name)
            .sqlite_store(&db_path, None)
            .with_encryption_settings(enc)
    }
        .build()
        .await?;

    client
        .matrix_auth()
        .login_username(username, password)
        .initial_device_display_name("Hikyaku")
        .await?;

    // Persist session info so we can restore it next launch.
    use matrix_sdk::AuthSession;
    let matrix_session = match client.session().expect("just logged in, session must exist") {
        AuthSession::Matrix(s) => s,
        #[allow(unreachable_patterns)]
        _ => panic!("we used password login, not OIDC"),
    };

    // Cache the resolved homeserver URL so restore can skip .well-known lookups.
    let homeserver_url = Some(client.homeserver().to_string());
    let persisted = PersistedSession {
        homeserver: homeserver.to_string(),
        homeserver_url,
        session: matrix_session,
    };
    save_session_to_keyring(&persisted).await?;
    Ok(client)
}

pub(crate) async fn do_register(
    homeserver: &str,
    username: &str,
    password: &str,
    display_name: &str,
    _email: &str,
) -> Result<Client, Box<dyn std::error::Error + Send + Sync>> {
    use matrix_sdk::ruma::api::client::account::register::v3::Request as RegisterRequest;
    use matrix_sdk::ruma::api::client::account::register::RegistrationKind;

    // No sqlite store for the registration client — it only needs to complete the
    // UIAA exchange, not persist any state.  do_login() below creates the real
    // persistent client; giving this client a store would initialize the crypto
    // layer with a different device ID than the one login produces, causing a
    // "account in store doesn't match" error.
    let client = if homeserver.starts_with("http://") || homeserver.starts_with("https://") {
        Client::builder()
            .homeserver_url(homeserver)
    } else {
        let server_name = ServerName::parse(homeserver)?;
        Client::builder()
            .server_name(&server_name)
    }
    .build()
    .await?;

    use matrix_sdk::ruma::api::client::uiaa::{AuthData, Dummy};

    let mut req = RegisterRequest::new();
    req.username = Some(username.to_owned());
    req.password = Some(password.to_owned());
    req.initial_device_display_name = Some(
        if display_name.is_empty() { "Hikyaku".to_owned() }
        else { display_name.to_owned() }
    );
    req.kind = RegistrationKind::User;

    // First attempt — open-registration servers (e.g. Conduit) return a UIAA 401
    // with an m.login.dummy stage even when no token is required. Retry with the
    // dummy auth stage to complete the flow.
    match client.matrix_auth().register(req.clone()).await {
        Ok(_) => {}
        Err(e) => {
            // Try the proper UIAA path first (well-formed servers that include `params`).
            let session = if let Some(uiaa_info) = e.as_uiaa_response() {
                uiaa_info.session.clone()
            } else {
                // Conduit omits the required `params` field from its UIAA 401, so
                // ruma falls back to ClientApi instead of Uiaa.  Extract the session
                // directly from the raw JSON body.
                use matrix_sdk::ruma::api::client::error::ErrorBody;
                e.as_client_api_error()
                    .filter(|ce| ce.status_code.as_u16() == 401)
                    .and_then(|ce| match &ce.body {
                        ErrorBody::Json(v) => v.get("session")?.as_str().map(str::to_owned),
                        _ => None,
                    })
                    .map(Some)
                    .ok_or_else(|| {
                        tracing::error!("Registration failed: {e}");
                        e.to_string()
                    })?
            };

            tracing::info!("UIAA required for registration, session={session:?}");
            let mut dummy = Dummy::new();
            dummy.session = session;
            req.auth = Some(AuthData::Dummy(dummy));
            client.matrix_auth().register(req).await
                .map_err(|e| { tracing::error!("Registration retry failed: {e}"); e.to_string() })?;
            tracing::info!("Registration succeeded");
        }
    }

    // Wipe any stale crypto store before logging in with the new account.
    // A previous failed or different-account run may have left state that would
    // cause a "account in store doesn't match" error in do_login.
    let db_path = db_dir_path(homeserver);
    if db_path.exists() {
        tracing::info!("Wiping stale store at {db_path:?} before first login");
        let _ = std::fs::remove_dir_all(&db_path);
    }

    // Register doesn't give us a full session — log in with the new credentials.
    do_login(homeserver, username, password).await
}

async fn try_restore_session(
    event_tx: &Sender<MatrixEvent>,
) -> Option<Client> {
    let mut persisted = load_session_from_keyring().await?;

    tracing::info!("Restoring session from GNOME Keyring");

    let db_path = db_dir_path(&persisted.homeserver);

    // Build the client.  If we have a cached homeserver URL use it directly —
    // this skips the `.well-known` network lookup so offline startup works.
    // Only fall back to server_name (which needs the network) when no URL is cached.
    let builder = Client::builder()
        .sqlite_store(&db_path, None)
        .with_encryption_settings(matrix_sdk::encryption::EncryptionSettings {
            backup_download_strategy:
                matrix_sdk::encryption::BackupDownloadStrategy::AfterDecryptionFailure,
            ..Default::default()
        });

    let builder = if let Some(ref url) = persisted.homeserver_url {
        match matrix_sdk::reqwest::Url::parse(url) {
            Ok(u) => {
                tracing::info!("Restoring via cached homeserver URL: {url}");
                builder.homeserver_url(u)
            }
            Err(_) => {
                tracing::warn!("Cached homeserver_url is malformed ({url}), falling back to server_name");
                let server_name = ServerName::parse(&persisted.homeserver).ok()?;
                builder.server_name(&server_name)
            }
        }
    } else {
        let server_name = ServerName::parse(&persisted.homeserver).ok()?;
        builder.server_name(&server_name)
    };

    let client = match builder.build().await {
        Ok(c) => c,
        Err(e) => {
            // A build failure means we can't reach the homeserver right now
            // (network down, DNS failure, etc.).  The credentials in the keyring
            // are still valid — do NOT call cleanup_session here or the user
            // loses their session when starting offline.
            tracing::warn!("Failed to restore client: {e}");
            return None;
        }
    };

    // Restore the auth session (access token, user ID, device ID).
    // The SQLite store only persists crypto state, not the auth session itself.
    if let Err(e) = client.restore_session(persisted.session.clone()).await {
        tracing::warn!("Failed to restore session: {e}");
        // Only wipe the session when the server explicitly rejects the token
        // (HTTP 401/403).  A generic network failure should not destroy the session.
        let is_auth_error = e.to_string().contains("401")
            || e.to_string().contains("403")
            || e.to_string().contains("Unknown access token");
        if is_auth_error {
            tracing::info!("Access token rejected by server, cleaning up session");
            cleanup_session(&persisted.homeserver).await;
        } else {
            tracing::warn!("Session restore failed (likely offline), keeping credentials");
        }
        return None;
    }

    // Cache the resolved homeserver URL so the next startup can skip .well-known.
    let resolved_url = client.homeserver().to_string();
    if persisted.homeserver_url.as_deref() != Some(&resolved_url) {
        persisted.homeserver_url = Some(resolved_url);
        if let Err(e) = save_session_to_keyring(&persisted).await {
            tracing::warn!("Could not update cached homeserver_url: {e}");
        }
    }

    if client.logged_in() {
        let user_id = client.user_id().map(|u| u.to_string()).unwrap_or_default();
        // Use the localpart as an instant fallback — avoids a network round-trip
        // on the critical path. The display name will update once the main view
        // fires its own profile fetch, or on the next sync.
        let display_name = user_id
            .trim_start_matches('@')
            .split(':')
            .next()
            .unwrap_or("User")
            .to_string();
        let _ = event_tx
            .send(MatrixEvent::LoginSuccess { display_name, user_id, from_registration: false, is_fresh_login: false })
            .await;
        Some(client)
    } else {
        tracing::info!("Stored session is invalid, cleaning up");
        cleanup_session(&persisted.homeserver).await;
        None
    }
}


// E2EE setup, recovery, and key import are handled in super::encryption.

/// Cache format version. Bump this whenever the definition of "activity"
/// changes (e.g. adding/removing event types from is_room_activity) so
/// stale caches are automatically invalidated.
const TIMESTAMP_CACHE_VERSION: u32 = 2;

/// On-disk timestamp cache with versioning.
#[derive(serde::Serialize, serde::Deserialize)]
struct TimestampCache {
    version: u32,
    timestamps: std::collections::HashMap<String, u64>,
}

/// Path to the on-disk timestamp cache.
fn timestamp_cache_path() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("hikyaku");
    path.push("room_timestamps.json");
    path
}

/// Load cached room timestamps from disk. Returns empty if the cache
/// is missing, corrupt, or from an older version.
fn load_timestamp_cache() -> std::collections::HashMap<String, u64> {
    let path = timestamp_cache_path();
    let cache: Option<TimestampCache> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    match cache {
        Some(c) if c.version == TIMESTAMP_CACHE_VERSION => c.timestamps,
        Some(c) => {
            tracing::info!(
                "Timestamp cache version {} != {}, rebuilding",
                c.version, TIMESTAMP_CACHE_VERSION
            );
            // Delete stale cache file.
            let _ = std::fs::remove_file(&path);
            std::collections::HashMap::new()
        }
        None => std::collections::HashMap::new(),
    }
}

/// Save room timestamps to disk with version tag. Prunes entries for
/// rooms not in `joined_room_ids` (rooms we've left).
fn save_timestamp_cache(
    map: &std::collections::HashMap<String, u64>,
    joined_room_ids: Option<&[String]>,
) {
    let timestamps = if let Some(ids) = joined_room_ids {
        let joined: std::collections::HashSet<&str> =
            ids.iter().map(|s| s.as_str()).collect();
        map.iter()
            .filter(|(k, _)| joined.contains(k.as_str()))
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    } else {
        map.clone()
    };
    let cache = TimestampCache {
        version: TIMESTAMP_CACHE_VERSION,
        timestamps,
    };
    let path = timestamp_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = std::fs::write(&path, json);
    }
}

/// Check whether a raw event JSON object represents meaningful room
/// activity — something a human initiated that's worth sorting by.
///
/// Relevant: messages (all types incl. notices), encrypted events,
///           topic/name changes, pin updates.
/// Not relevant: member joins/leaves, server ACLs, power level changes,
///               alias changes, avatar changes — these are automated
///               churn that doesn't reflect real interaction.
fn is_room_activity(v: &serde_json::Value) -> bool {
    use std::sync::LazyLock;
    static ACTIVITY_TYPES: LazyLock<std::collections::HashSet<&'static str>> = LazyLock::new(|| {
        [
            "m.room.message",
            "m.room.encrypted",
            "m.room.topic",
            "m.room.name",
            "m.room.pinned_events",
        ]
        .into_iter()
        .collect()
    });
    v.get("type")
        .and_then(|t| t.as_str())
        .map_or(false, |t| ACTIVITY_TYPES.contains(t))
}

/// Serialize a MediaSource to JSON for later reconstruction during download.
fn media_source_json(source: &matrix_sdk::ruma::events::room::MediaSource) -> String {
    serde_json::to_string(source).unwrap_or_default()
}

/// Extract the mxc:// URL from a MediaSource.
fn media_source_url(source: &matrix_sdk::ruma::events::room::MediaSource) -> String {
    use matrix_sdk::ruma::events::room::MediaSource;
    match source {
        MediaSource::Plain(uri) => uri.to_string(),
        MediaSource::Encrypted(file) => file.url.to_string(),
    }
}

/// Extract body text, optional formatted body, and optional media from a MessageType.
/// Returns `(body, formatted_body, media)`.
fn extract_message_content(
    msgtype: &matrix_sdk::ruma::events::room::message::MessageType,
) -> Option<(String, Option<String>, Option<MediaInfo>)> {
    use matrix_sdk::ruma::events::room::message::MessageType;
    match msgtype {
        MessageType::Text(text) => {
            let formatted = text.formatted.as_ref()
                .filter(|f| f.format == matrix_sdk::ruma::events::room::message::MessageFormat::Html)
                .map(|f| f.body.clone());
            Some((text.body.clone(), formatted, None))
        }
        MessageType::Notice(notice) => {
            let formatted = notice.formatted.as_ref()
                .filter(|f| f.format == matrix_sdk::ruma::events::room::message::MessageFormat::Html)
                .map(|f| f.body.clone());
            Some((notice.body.clone(), formatted, None))
        }
        MessageType::Image(image) => {
            let url = media_source_url(&image.source);
            let source_json = media_source_json(&image.source);
            let size = image.info.as_ref().and_then(|i| i.size).map(|s| s.into());
            Some((image.body.clone(), None, Some(MediaInfo {
                kind: MediaKind::Image,
                filename: image.filename.clone().unwrap_or_else(|| image.body.clone()),
                size, url, source_json,
            })))
        }
        MessageType::Video(video) => {
            let url = media_source_url(&video.source);
            let source_json = media_source_json(&video.source);
            let size = video.info.as_ref().and_then(|i| i.size).map(|s| s.into());
            Some((video.body.clone(), None, Some(MediaInfo {
                kind: MediaKind::Video,
                filename: video.filename.clone().unwrap_or_else(|| video.body.clone()),
                size, url, source_json,
            })))
        }
        MessageType::Audio(audio) => {
            let url = media_source_url(&audio.source);
            let source_json = media_source_json(&audio.source);
            let size = audio.info.as_ref().and_then(|i| i.size).map(|s| s.into());
            Some((audio.body.clone(), None, Some(MediaInfo {
                kind: MediaKind::Audio,
                filename: audio.body.clone(),
                size, url, source_json,
            })))
        }
        MessageType::File(file) => {
            let url = media_source_url(&file.source);
            let source_json = media_source_json(&file.source);
            let size = file.info.as_ref().and_then(|i| i.size).map(|s| s.into());
            Some((file.body.clone(), None, Some(MediaInfo {
                kind: MediaKind::File,
                filename: file.filename.clone().unwrap_or_else(|| file.body.clone()),
                size, url, source_json,
            })))
        }
        MessageType::Emote(emote) => {
            // /me actions — render as "* body" so sender row + italic body make sense.
            let formatted = emote.formatted.as_ref()
                .filter(|f| f.format == matrix_sdk::ruma::events::room::message::MessageFormat::Html)
                .map(|f| format!("<i>{}</i>", f.body));
            Some((format!("* {}", emote.body), formatted, None))
        }
        MessageType::Location(loc) => {
            Some((format!("📍 {}", loc.body), None, None))
        }
        MessageType::ServerNotice(notice) => {
            Some((notice.body.clone(), None, None))
        }
        // Unknown / future message types: show body text so reply chains stay
        // intact.  The `msgtype` discriminant is not available from the enum,
        // so we fall back to a generic placeholder.
        _ => Some(("[unsupported message type]".to_string(), None, None)),
    }
}

/// Aggregate a list of emoji strings into (emoji, count) pairs using a HashMap.
/// Aggregate reactions: (emoji, count, list of reactor display names).
fn aggregate_reactions(
    entries: Option<&Vec<(String, String)>>,
) -> Vec<(String, u64, Vec<String>)> {
    let Some(entries) = entries else {
        return Vec::new();
    };
    let mut map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (emoji, sender) in entries {
        map.entry(emoji.clone()).or_default().push(sender.clone());
    }
    map.into_iter()
        .map(|(emoji, senders)| {
            let count = senders.len() as u64;
            (emoji, count, senders)
        })
        .collect()
}

/// Resolve a user's display name from room membership, falling back to user ID.
/// Global display name cache — avoids re-resolving the same user across rooms.
/// Uses a tokio Mutex since it's accessed from async contexts.
static DISPLAY_NAME_CACHE: std::sync::LazyLock<tokio::sync::Mutex<std::collections::HashMap<String, String>>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

// Defined in super (matrix/mod.rs) so both client and encryption can share it.
use super::ROOM_LOAD_IN_PROGRESS;

/// Global semaphore limiting concurrent avatar HTTP downloads.
/// Without this, selecting a room list with 295 rooms fires ~295 simultaneous
/// HTTP requests, stalling the tokio thread pool.
static AVATAR_PERMITS: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(4));


/// Monotonically increasing generation for bg_refresh tasks.
/// Incremented on every SelectRoom; the spawned task captures its generation
/// and only clears ROOM_LOAD_IN_PROGRESS if no newer task has been superseded.
static BG_REFRESH_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Yield to the runtime while `ROOM_LOAD_IN_PROGRESS` is set.
/// Used by prefetch and key-download tasks that predate the semaphore.
async fn yield_if_room_loading() {
    while ROOM_LOAD_IN_PROGRESS.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn resolve_display_name(
    room: &matrix_sdk::room::Room,
    user_id: &matrix_sdk::ruma::UserId,
) -> String {
    let uid_str = user_id.to_string();

    // Check cache first — only real display names are cached, not fallbacks,
    // so a miss here always triggers a fresh member lookup.
    {
        let cache = DISPLAY_NAME_CACHE.lock().await;
        if let Some(name) = cache.get(&uid_str) {
            return name.clone();
        }
    }

    let resolved = room.get_member_no_sync(user_id)
        .await
        .ok()
        .flatten()
        .and_then(|m| m.display_name().map(|s| s.to_string()));

    if let Some(name) = resolved {
        // Cache real display names so we don't hit the state store on every message.
        let mut cache = DISPLAY_NAME_CACHE.lock().await;
        cache.insert(uid_str, name.clone());
        name
    } else {
        // Member not yet in local state — return localpart as a readable fallback
        // but do NOT cache it so the next message retries the lookup.
        user_id.localpart().to_string()
    }
}


/// Fetch the timestamp of the most recent message for rooms where we
/// don't have one yet. Runs up to 20 requests in parallel.
async fn backfill_timestamps(client: &Client, rooms: &mut [RoomInfo]) {
    let missing: Vec<usize> = rooms
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.last_activity_ts == 0
                && r.kind != RoomKind::Space
                && !r.name.to_lowercase().starts_with("empty room")
        })
        .map(|(i, _)| i)
        .collect();

    if missing.is_empty() {
        return;
    }

    tracing::info!("Backfilling timestamps for {} rooms (sequential, yields to user)", missing.len());
    let start = std::time::Instant::now();

    // Sequential with yield points — previously ran 20 concurrent room.messages()
    // calls which saturated all CPU cores and caused GTK rendering stalls.
    // Sequential is slower but never blocks user interaction.
    let mut results: Vec<(usize, u64)> = Vec::with_capacity(missing.len());
    for &idx in &missing {
        // Yield before each network call so user interactions aren't starved.
        yield_if_room_loading().await;
        let room_id_str = &rooms[idx].room_id;
            let room_id = match RoomId::parse(room_id_str) {
                Ok(id) => id,
                Err(_) => { results.push((idx, 0u64)); continue; }
            };
            let Some(room) = client.get_room(&room_id) else {
                results.push((idx, 0u64)); continue;
            };
            // Filter server-side to only return message/encrypted events,
            // avoiding pagination through membership churn.
            let mut filter = matrix_sdk::ruma::api::client::filter::RoomEventFilter::default();
            filter.types = Some(vec![
                "m.room.message".to_string(),
                "m.room.encrypted".to_string(),
            ]);
            let mut opts = matrix_sdk::room::MessagesOptions::backward();
            opts.limit = matrix_sdk::ruma::UInt::from(1u32);
            opts.filter = filter;
            let ts = match room.messages(opts).await {
                Ok(resp) => {
                    resp.chunk.first()
                        .and_then(|ev| {
                            let raw = ev.raw().json().get();
                            let v = serde_json::from_str::<serde_json::Value>(raw).ok()?;
                            v.get("origin_server_ts")?.as_u64()
                        })
                        .map(|ms| ms / 1000)
                        .unwrap_or(0)
                }
                Err(_) => 0,
            };
        results.push((idx, ts));
    }

    let mut filled = 0u32;
    for (idx, ts) in results {
        if ts > 0 {
            rooms[idx].last_activity_ts = ts;
            filled += 1;
        }
    }

    tracing::info!(
        "Backfilled {}/{} room timestamps in {:?}",
        filled,
        missing.len(),
        start.elapsed()
    );
}

/// Collect current room info from the client's joined rooms.
///
/// First builds a map of space → child room IDs by iterating spaces
/// (typically ~20) and reading their m.space.child state events. Then
/// classifies each non-space room as DM or Room, attaching the parent
/// space name so the UI can group them.
async fn collect_room_info(
    client: &Client,
    ts_cache: Option<&std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, u64>>>>,
) -> Vec<RoomInfo> {
    use matrix_sdk::ruma::events::space::child::SpaceChildEventContent;
    use std::collections::HashMap;

    let start = std::time::Instant::now();
    let joined = client.joined_rooms();
    let total = joined.len();
    let cfg = crate::config::settings();

    // Step 1: Build two complementary maps for resolving a room's parent space.
    //
    // Approach A (parent-side): read m.space.child events from each space.
    //   Works when the space's state is locally cached — fails for federated
    //   spaces whose state hasn't been downloaded yet.
    //
    // Approach B (child-side): read m.space.parent events from each room.
    //   Always works because the room belongs to the user's account and its
    //   own state is always local.
    //
    // We build both and use A first, falling back to B for rooms A misses.

    // Build space_id → space_name for the child-side lookup.
    // Use cached_display_name() / name() — no SQLite pool needed.
    let space_id_to_name: HashMap<String, String> = joined.iter()
        .filter(|r| r.is_space())
        .map(|room| {
            let name = room.cached_display_name()
                .map(|n| n.to_string())
                .unwrap_or_else(|| room.name().unwrap_or_else(|| room.room_id().to_string()));
            (room.room_id().to_string(), name)
        })
        .collect();

    // Space child events — sequential to avoid exhausting matrix-sdk's internal
    // SQLite connection pool.  Concurrent join_all across all spaces caused the
    // pool to stall bg_refresh's room.messages() call for 3-4 s after ~60 s of
    // inactivity when collect_room_info fired.
    let child_to_space: HashMap<String, String> = {
        let mut map: HashMap<String, String> = HashMap::new();
        for room in joined.iter().filter(|r| r.is_space()) {
            let space_name = room.cached_display_name()
                .map(|n| n.to_string())
                .unwrap_or_else(|| room.name().unwrap_or_else(|| room.room_id().to_string()));
            let children = room
                .get_state_events_static::<SpaceChildEventContent>().await
                .unwrap_or_default();
            for raw_event in children {
                if let Ok(event) = raw_event.deserialize() {
                    map.insert(event.state_key().to_string(), space_name.clone());
                }
            }
        }
        tracing::info!("Space child mappings (parent-side): {}", map.len());
        map
    };

    // Step 2: Build per-room data synchronously — all fields use cached/sync
    // accessors, so no SQLite pool is needed here.
    struct RoomData {
        room_id: String,
        is_space: bool,
        name: String,
        topic: String,
        is_encrypted: bool,
        is_tombstoned: bool,
        is_dm: bool,
        is_admin: bool,
        last_activity_ts: u64,
        unread_count: u64,
        highlight_count: u64,
        is_pinned: bool,
        is_favourite: bool,
        avatar_url: String,
        /// Space room IDs declared in this room's own m.space.parent state events.
        /// Used as a fallback when the parent-side child_to_space map misses this room.
        parent_space_ids: Vec<String>,
    }

    let mut all_data = Vec::with_capacity(joined.len());
    for room in &joined {
        let room_id = room.room_id().to_string();
        let is_pinned = cfg.rooms.pinned_rooms.contains(&room_id);
        let is_favourite = room.is_favourite();
        let avatar_url = room.avatar_url().map(|u| u.to_string()).unwrap_or_default();
        let topic = room.topic().unwrap_or_default();
        let unread = room.unread_notification_counts();
        let last_activity_ts = room.recency_stamp()
            .or_else(|| room.latest_event().and_then(|e| {
                let raw = e.event().raw().json().get();
                let v = serde_json::from_str::<serde_json::Value>(raw).ok()?;
                if is_room_activity(&v) {
                    v.get("origin_server_ts")?.as_u64().map(|ms| ms / 1000)
                } else {
                    None
                }
            }))
            .unwrap_or(0u64);

        // Use synchronous methods for the fields that have them — this
        // eliminates all SQLite pool contention between collect_room_info
        // and a concurrent handle_select_room_bg.
        // Access is_encrypted, is_tombstoned, and is_dm via the BaseRoom
        // deref target — all three read from in-memory state, no I/O.
        let is_encrypted = matrix_sdk::BaseRoom::is_encrypted(&room);
        let is_tombstoned = matrix_sdk::BaseRoom::is_tombstoned(&room);
        let is_dm = matrix_sdk::BaseRoom::direct_targets_length(&room) > 0;
        // For DMs, use the hero's display name: heroes are stored in the
        // SQLite-backed RoomInfo and loaded into memory on startup, so this
        // is synchronous but accurate.  cached_display_name() is only
        // populated after display_name().await has been called, so it
        // returns None for DMs on fresh startup → shows raw user IDs.
        let name = if is_dm {
            let heroes = room.heroes();
            heroes.iter()
                .find_map(|h| h.display_name.as_deref().map(str::to_owned))
                .or_else(|| heroes.first().map(|h| h.user_id.localpart().to_owned()))
                .or_else(|| room.cached_display_name().map(|n| n.to_string()))
                .unwrap_or_else(|| room_id.clone())
        } else {
            room.cached_display_name()
                .map(|n| n.to_string())
                .unwrap_or_else(|| room.name().unwrap_or_else(|| room_id.clone()))
        };
        // is_admin: the power-levels state event requires an async store read
        // per room (296 rooms × ~3 ms pool overhead ≈ 2 s total).  We skip it
        // here and let handle_select_room_bg fill it in when the user opens the
        // room — that path already computes membership/power for the header bar.
        let is_admin = false;
        // parent_space_ids: the child_to_space map built above (parent-side)
        // covers the common case.  The per-room fallback would need an async
        // store read; skip it so the per-room loop is fully synchronous.
        let parent_space_ids: Vec<String> = vec![];

        all_data.push(RoomData {
            room_id,
            is_space: room.is_space(),
            name,
            topic,
            is_encrypted,
            is_tombstoned,
            is_dm,
            is_admin,
            last_activity_ts,
            unread_count: unread.notification_count.into(),
            highlight_count: unread.highlight_count.into(),
            is_pinned,
            is_favourite,
            avatar_url,
            parent_space_ids,
        });
    }

    // Step 3: Bucket results — no more awaits needed.
    let mut with_unread = Vec::new();
    let mut direct = Vec::new();
    let mut rest = Vec::new();
    let mut spaces = Vec::new();

    for d in all_data {
        // Try parent-side map first; fall back to the room's own m.space.parent
        // events for federated spaces whose child-list may not be locally cached.
        let parent_space = child_to_space.get(&d.room_id).cloned()
            .or_else(|| {
                d.parent_space_ids.iter()
                    .find_map(|sid| space_id_to_name.get(sid).cloned())
            });
        // First parent_space_id wins — used for context inheritance chain.
        let parent_space_id = d.parent_space_ids.into_iter().next().unwrap_or_default();
        let info = RoomInfo {
            room_id: d.room_id,
            name: d.name,
            last_activity_ts: d.last_activity_ts,
            kind: if d.is_space {
                RoomKind::Space
            } else if d.is_dm {
                RoomKind::DirectMessage
            } else {
                RoomKind::Room
            },
            is_encrypted: d.is_encrypted,
            parent_space,
            parent_space_id,
            is_pinned: d.is_pinned,
            unread_count: d.unread_count,
            highlight_count: d.highlight_count,
            is_admin: d.is_admin,
            is_tombstoned: d.is_tombstoned,
            is_favourite: d.is_favourite,
            avatar_url: d.avatar_url,
            topic: d.topic,
        };

        if d.is_space {
            spaces.push(info);
        } else if info.unread_count > 0 || info.highlight_count > 0 {
            with_unread.push(info);
        } else if d.is_dm {
            direct.push(info);
        } else {
            rest.push(info);
        }
    }

    // Space-child rooms must not be truncated — they're needed for space
    // drill-down. Split them out, truncate only ungrouped rooms, then
    // recombine.
    let mut space_children = Vec::new();
    let mut ungrouped = Vec::new();
    for r in rest {
        if r.parent_space.is_some() {
            space_children.push(r);
        } else {
            ungrouped.push(r);
        }
    }

    tracing::info!(
        "Room buckets: {} with unread, {} DMs, {} ungrouped, {} space-children, {} spaces (of {} total joined)",
        with_unread.len(), direct.len(), ungrouped.len(), space_children.len(), spaces.len(), total
    );

    // Apply cached timestamps before sorting so rooms with ts=0 that have
    // a cached value don't get wrongly truncated.
    if let Some(ts_arc) = ts_cache {
        let ts_map = ts_arc.lock().unwrap();
        for room in direct.iter_mut()
            .chain(ungrouped.iter_mut())
            .chain(space_children.iter_mut())
            .chain(with_unread.iter_mut())
        {
            if room.last_activity_ts == 0 {
                if let Some(&cached) = ts_map.get(&room.room_id) {
                    room.last_activity_ts = cached;
                }
            }
        }
    }

    // Sort each bucket by activity (most recent first) before truncating,
    // so dead rooms get cut rather than active ones.
    let sort_by_activity = |a: &RoomInfo, b: &RoomInfo| {
        b.last_activity_ts.cmp(&a.last_activity_ts)
    };
    direct.sort_by(sort_by_activity);
    ungrouped.sort_by(sort_by_activity);
    with_unread.sort_by(sort_by_activity);

    // Do NOT truncate here — all rooms must be registered in room_registry
    // so that increment_unread works for rooms not currently visible in
    // the sidebar.  The display cap is applied in rebuild_stores().

    // Combine all rooms — the UI separates them into tabs.
    let mut rooms = Vec::with_capacity(
        with_unread.len() + direct.len() + ungrouped.len()
            + space_children.len() + spaces.len(),
    );
    rooms.extend(with_unread);
    rooms.extend(direct);
    rooms.extend(ungrouped);
    rooms.extend(space_children);
    rooms.extend(spaces);

    let hidden = total.saturating_sub(rooms.len());
    tracing::info!(
        "Showing {} rooms ({} hidden) of {} joined in {:?}",
        rooms.len(),
        hidden,
        total,
        start.elapsed()
    );
    rooms
}

async fn start_sync(
    client: Client,
    event_tx: &Sender<MatrixEvent>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    timeline_cache: super::room_cache::RoomCache,
    dirty_rooms: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<crate::matrix::MessageInfo>>>>,
) {
    use matrix_sdk::ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent,
    };

    let _ = event_tx.send(MatrixEvent::SyncStarted).await;

    // Persistent map of room_id → most recent message timestamp (seconds).
    // Loaded from disk so rooms appear in the right order immediately on
    // restart, without waiting for backfill.
    // Send the persisted room list immediately (instant, no DB queries) so
    // the sidebar is populated before the matrix-sdk store is queried.
    let disk_cached = load_room_list_cache();

    // Seed timestamp map from both the dedicated timestamp cache AND the room
    // list cache.  The room list cache has last_activity_ts for every room
    // saved at the end of the previous session — seeding from it means
    // backfill_timestamps finds zero missing rooms on all but the very first
    // ever startup, eliminating the startup CPU spike entirely.
    let room_timestamps: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, u64>>> = {
        let mut ts = load_timestamp_cache();
        for room in &disk_cached {
            if room.last_activity_ts > 0 {
                ts.entry(room.room_id.clone()).or_insert(room.last_activity_ts);
            }
        }
        std::sync::Arc::new(std::sync::Mutex::new(ts))
    };
    // Build a lookup of last-known unread/highlight counts from the disk cache.
    // Used below to guard against the SDK returning 0 pre-sync (before the
    // first sync has refreshed notification counts from the server).
    let disk_unread: std::collections::HashMap<String, (u64, u64)> = disk_cached
        .iter()
        .map(|r| (r.room_id.clone(), (r.unread_count, r.highlight_count)))
        .collect();
    if !disk_cached.is_empty() {
        tracing::info!("Loaded {} rooms from disk cache (instant)", disk_cached.len());
        let _ = event_tx.send(MatrixEvent::RoomListUpdated { rooms: disk_cached }).await;
    }

    // Now query the matrix-sdk local store (slower — multiple async DB reads).
    let mut cached_rooms = collect_room_info(&client, Some(&room_timestamps)).await;
    if !cached_rooms.is_empty() {
        // Apply cached timestamps so rooms sort correctly from the start.
        // Merge timestamps: take the MAX of what collect_room_info found
        // (from latest_event()) and what the disk cache recorded (from the
        // previous session's backfill/sync).  This prevents a dormant room
        // whose latest_event() returns a months-old event from overwriting a
        // newer value that was captured by the sync-response extractor.
        {
            let mut ts_map = room_timestamps.lock().unwrap();
            for room in &mut cached_rooms {
                let cached = ts_map.get(&room.room_id).copied().unwrap_or(0);
                if room.last_activity_ts > cached {
                    ts_map.insert(room.room_id.clone(), room.last_activity_ts);
                } else if cached > 0 {
                    room.last_activity_ts = cached;
                }
            }
        }

        // Guard: the SDK's unread_notification_counts() may return 0 before
        // the first sync completes.  Merge with the disk-cached counts so we
        // never overwrite a valid badge with a stale 0.  After the first sync
        // fires (is_first path below), the SDK has fresh counts and the disk
        // cache is updated again with the authoritative values.
        merge_disk_unread_counts(&mut cached_rooms, &disk_unread);
        tracing::info!("Loaded {} rooms from local store", cached_rooms.len());
        save_room_list_cache(&cached_rooms);

        // MOTD startup check: emit TopicChanged for any room whose topic
        // differs from the last time the user ran the app.
        #[cfg(feature = "motd")]
        {
            let mut motd_cache = crate::plugins::motd::load();
            for room in &cached_rooms {
                if crate::plugins::motd::check_and_update(
                    &room.room_id, &room.topic, &mut motd_cache,
                ) {
                    let _ = event_tx.send(MatrixEvent::TopicChanged {
                        room_id: room.room_id.clone(),
                        new_topic: room.topic.clone(),
                    }).await;
                }
            }
        }

        let _ = event_tx
            .send(MatrixEvent::RoomListUpdated {
                rooms: cached_rooms,
            })
            .await;

        // Startup prefetch intentionally removed.
        // Disk cache (per-room JSON files) persists across sessions, so rooms
        // visited before are served instantly from disk on next launch.
        // Cold rooms (never visited) get cached on first open and are instant
        // thereafter.  Prefetching all unread rooms at startup was causing CPU
        // spikes (room.messages() + AES-GCM decryption per encrypted room).
        // Keys are fetched on-demand in handle_select_room_bg when UTDs are
        // detected in the visible window.

        // Timestamp backfill is deferred to the first sync (is_first=true path
        // in start_sync), where the SDK has had a chance to process the first
        // sync response and populate recency_stamp / latest_event for more rooms.
        // Running it here would just duplicate the work done 1-2 seconds later.
    }

    // Interest watcher: one dedicated OS thread owns the model and processes
    // messages sequentially — same pattern as the community-health scorer.
    // try_send drops under load; never blocks the sync loop; never spawns extra threads.
    // The thread lazily inits/reinits the model when config changes.
    #[cfg(feature = "ai")]
    let (watch_tx, watch_rx) =
        std::sync::mpsc::sync_channel::<(String, String, String)>(32); // (body, room_id, room_name)
    #[cfg(feature = "ai")]
    {
        let watch_event_tx = event_tx.clone();
        std::thread::Builder::new()
            .name("watcher".into())
            .spawn(move || {
                let mut current_terms: Vec<String> = Vec::new();
                let mut current_threshold: f64 = 0.0;
                let mut watcher: Option<crate::intelligence::watcher::Watcher> = None;
                while let Ok((body, room_id, room_name)) = watch_rx.recv() {
                    let cfg = crate::config::settings();
                    if !cfg.watch.enabled || cfg.watch.terms.is_empty() { continue; }
                    // Reinit model only when terms or threshold actually changed.
                    if watcher.is_none()
                        || current_terms != cfg.watch.terms
                        || (current_threshold - cfg.watch.threshold).abs() > 1e-9
                    {
                        current_terms = cfg.watch.terms.clone();
                        current_threshold = cfg.watch.threshold;
                        watcher = crate::intelligence::watcher::Watcher::new(
                            &current_terms, current_threshold,
                        );
                    }
                    if let Some(w) = &watcher {
                        if let Some(term) = w.check(&body) {
                            let _ = watch_event_tx.send_blocking(
                                crate::matrix::MatrixEvent::RoomAlert {
                                    room_id,
                                    room_name,
                                    matched_term: term,
                                },
                            );
                        }
                    }
                }
            })
            .ok();
    }

    // Community health scoring: one dedicated OS thread reads from a bounded
    // channel and runs fastembed inference sequentially — no spawn_blocking
    // pileup, no CPU starvation of the GTK thread.
    // Bounded to 32 slots: try_send drops messages when the scorer is behind.
    // Scoring is non-critical so dropping is preferable to blocking the sync loop.
    #[cfg(feature = "community-health")]
    let (health_score_tx, health_score_rx) =
        std::sync::mpsc::sync_channel::<(String, String)>(32);
    // Spawn the dedicated health scorer thread (one thread for all scoring).
    #[cfg(feature = "community-health")]
    {
        let settings = crate::config::settings();
        if settings.plugins.community_health {
            use crate::plugins::community_health::HealthMonitor;
            let health_event_tx = event_tx.clone();
            let rx = health_score_rx;
            std::thread::Builder::new()
                .name("health-scorer".to_string())
                .spawn(move || {
                    let Some(mut monitor) = HealthMonitor::new() else {
                        tracing::warn!("HealthMonitor: model init failed, scoring disabled");
                        return;
                    };
                    tracing::info!("HealthMonitor ready");
                    while let Ok((rid, body)) = rx.recv() {
                        if let Some(h) = monitor.record(&rid, &body) {
                            let _ = health_event_tx.send_blocking(
                                crate::matrix::MatrixEvent::HealthUpdate {
                                    room_id: rid,
                                    score: h.score,
                                    trend: h.trend,
                                    alert: h.alert,
                                },
                            );
                        }
                    }
                })
                .ok();
        }
    }

    // Detect incoming invites — fire RoomInvited when the local user is invited.
    {
        use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;
        use matrix_sdk::ruma::events::room::member::MembershipState;
        let invite_tx = event_tx.clone();
        client.add_event_handler(
            move |event: StrippedRoomMemberEvent,
                  room: matrix_sdk::room::Room,
                  client: matrix_sdk::Client| {
                let tx = invite_tx.clone();
                async move {
                    if event.content.membership != MembershipState::Invite {
                        return;
                    }
                    let Some(my_id) = client.user_id() else { return };
                    if event.state_key.as_str() != my_id.as_str() {
                        return;
                    }
                    let room_name = room.display_name().await
                        .map(|n| n.to_string())
                        .unwrap_or_else(|_| room.room_id().to_string());
                    let inviter_name = event.sender.localpart().to_string();
                    let _ = tx.send(MatrixEvent::RoomInvited {
                        room_id: room.room_id().to_string(),
                        room_name,
                        inviter_name,
                    }).await;
                }
            },
        );
    }

    // Register handlers for new messages (both decrypted and encrypted).
    // The SDK auto-decrypts when keys are available and fires the
    // RoomMessage handler. When decryption fails, the RoomEncrypted
    // handler fires instead.
    let msg_tx = event_tx.clone();
    let msg_client = client.clone();
    let msg_dirty = dirty_rooms.clone();
    let msg_cache = timeline_cache.clone();
    #[cfg(feature = "ai")]
    let msg_watcher = watch_tx.clone();
    #[cfg(feature = "community-health")]
    let msg_health_score_tx = health_score_tx.clone();
    client.add_event_handler(
        move |event: OriginalSyncRoomMessageEvent,
              room: matrix_sdk::room::Room| {
            let tx = msg_tx.clone();
            let dirty = msg_dirty.clone();
            let cache = msg_cache.clone();
            let client = msg_client.clone();
            #[cfg(feature = "ai")]
            let watcher = msg_watcher.clone(); // SyncSender<(body, room_id, room_name)>
            #[cfg(feature = "community-health")]
            let health_score_tx = msg_health_score_tx.clone();
            async move {
                // If this is an edit (m.replace), fire MessageEdited and bail out.
                // Do NOT treat it as a new message — the fallback body ("* ...") must
                // never be appended to the timeline.
                use matrix_sdk::ruma::events::room::message::Relation;
                if let Some(Relation::Replacement(replacement)) = &event.content.relates_to {
                    let (new_body, new_formatted, _) = extract_message_content(&replacement.new_content.msgtype)
                        .unwrap_or_default();
                    if !new_body.is_empty() {
                        let room_id_str = room.room_id().to_string();
                        let event_id_str = replacement.event_id.to_string();
                        // Update the memory cache incrementally — no need to wipe it.
                        cache.update_message_body_in_cache(
                            &room_id_str,
                            &event_id_str,
                            &new_body,
                            new_formatted.as_deref(),
                        );
                        // Cache already patched; no dirty/refresh needed.
                        let _ = tx.send(MatrixEvent::MessageEdited {
                            room_id: room_id_str,
                            event_id: event_id_str,
                            new_body,
                            formatted_body: new_formatted,
                        }).await;
                    }
                    return;
                }

                let Some((body, formatted_body, media)) = extract_message_content(&event.content.msgtype) else {
                    return;
                };
                let timestamp = event
                    .origin_server_ts
                    .as_secs()
                    .into();

                let display_name = resolve_display_name(&room, &event.sender).await;

                // Check if this message mentions the current user:
                // 1. Direct @mention in body text
                // 2. Reply to one of our messages (in_reply_to or thread)
                let is_mention = if let Some(user_id) = client.user_id() {
                    let uid = user_id.as_str();
                    let local = user_id.localpart();
                    let text_mention = body.contains(uid)
                        || body.to_lowercase().contains(&local.to_lowercase());
                    if text_mention {
                        tracing::warn!("Text mention detected for {local}");
                    }

                    // Check if this is a reply to our message.
                    let is_reply_to_us = if let Some(ref relates_to) = event.content.relates_to {
                        use matrix_sdk::ruma::events::room::message::Relation;
                        match relates_to {
                            Relation::Reply { in_reply_to } => {
                                // Check if the replied-to event is ours.
                                if let Some(original_room) = client.get_room(room.room_id()) {
                                    original_room.event(&in_reply_to.event_id, None).await
                                        .ok()
                                        .and_then(|ev| ev.raw().deserialize().ok())
                                        .map(|ev: matrix_sdk::ruma::events::AnySyncTimelineEvent| {
                                            ev.sender() == user_id
                                        })
                                        .unwrap_or(false)
                                } else {
                                    false
                                }
                            }
                            Relation::Thread(thread) => {
                                // Thread reply — check if the thread root is ours.
                                if let Some(original_room) = client.get_room(room.room_id()) {
                                    original_room.event(&thread.event_id, None).await
                                        .ok()
                                        .and_then(|ev| ev.raw().deserialize().ok())
                                        .map(|ev: matrix_sdk::ruma::events::AnySyncTimelineEvent| {
                                            ev.sender() == user_id
                                        })
                                        .unwrap_or(false)
                                } else {
                                    false
                                }
                            }
                            _ => false,
                        }
                    } else {
                        false
                    };

                    if is_reply_to_us {
                        tracing::warn!("Reply-to-us detected!");
                    }
                    text_mention || is_reply_to_us
                } else {
                    false
                };

                // Use synchronous room.name() to avoid blocking the sync callback
                // on an async member-list computation.
                let room_name = room.name().unwrap_or_else(|| room.room_id().to_string());
                let is_dm = matrix_sdk::BaseRoom::direct_targets_length(&room) > 0;
                tracing::debug!(
                    "NewMessage room={} is_dm={} sender={}",
                    room.room_id(), is_dm, event.sender
                );

                let sender_id = event.sender.to_string();

                // Extract reply/thread relations.
                let (reply_to, thread_root) = match &event.content.relates_to {
                    Some(Relation::Reply { in_reply_to }) => {
                        (Some(in_reply_to.event_id.to_string()), None)
                    }
                    Some(Relation::Thread(thread)) => {
                        let reply = thread.in_reply_to.as_ref()
                            .map(|r| r.event_id.to_string());
                        (reply, Some(thread.event_id.to_string()))
                    }
                    _ => (None, None),
                };

                // Resolve reply-to sender name.
                let reply_to_sender = if let Some(ref rt_eid) = reply_to {
                    use matrix_sdk::ruma::OwnedEventId;
                    if let Ok(eid) = rt_eid.parse::<OwnedEventId>() {
                        if let Ok(ev) = room.event(&eid, None).await {
                            if let Ok(parsed) = ev.raw().deserialize() {
                                let sender = parsed.sender().to_owned();
                                Some(resolve_display_name(&room, &sender).await)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let room_id_str = room.room_id().to_string();
                let room_name_str = room_name.clone();
                let msg = MessageInfo {
                    sender: display_name,
                    sender_id: sender_id.clone(),
                    body: body.clone(),
                    formatted_body: formatted_body.clone(),
                    timestamp,
                    event_id: event.event_id.to_string(),
                    reply_to,
                    reply_to_sender,
                    thread_root,
                    reactions: Vec::new(),
                    media,
                    is_highlight: false,
                    is_system_event: false,
                };
                // Try to append directly to the memory cache.
                // If warm: keeps cache fresh — SelectRoom skips bg_refresh.
                // If cold: store the pending MessageInfo so SelectRoom can
                // apply it after loading disk cache, avoiding a network fetch.
                if !cache.append_memory(&room_id_str, msg.clone()) {
                    dirty.lock().unwrap().entry(room_id_str.clone()).or_default().push(msg.clone());
                }
                let _ = tx
                    .send(MatrixEvent::NewMessage {
                        room_id: room_id_str.clone(),
                        room_name: room_name_str.clone(),
                        sender_id: sender_id.clone(),
                        message: msg,
                        is_mention,
                        is_dm,
                    })
                    .await;

                // Forward to the dedicated watcher thread via try_send.
                // Drops the message if the thread is busy — never blocks sync.
                #[cfg(feature = "ai")]
                {
                    let cfg = crate::config::settings();
                    if cfg.watch.enabled && !cfg.watch.terms.is_empty() {
                        let _ = watcher.try_send((
                            body.clone(),
                            room_id_str.clone(),
                            room_name_str.clone(),
                        ));
                    }
                }

                // Community health monitor — forward to the dedicated scorer thread.
                // try_send drops the message if the scorer is busy (bounded channel).
                // This never blocks the sync loop and never spawns extra OS threads.
                #[cfg(feature = "community-health")]
                {
                    let cfg = crate::config::settings();
                    if cfg.plugins.community_health {
                        let _ = health_score_tx.try_send((room_id_str.to_string(), body.clone()));
                    }
                }
            }
        },
    );

    // Handler for encrypted messages that couldn't be decrypted.
    use matrix_sdk::ruma::events::room::encrypted::OriginalSyncRoomEncryptedEvent;
    let enc_tx = event_tx.clone();
    let enc_dirty = dirty_rooms.clone();
    let enc_cache = timeline_cache.clone();
    client.add_event_handler(
        move |event: OriginalSyncRoomEncryptedEvent,
              room: matrix_sdk::room::Room| {
            let tx = enc_tx.clone();
            let dirty = enc_dirty.clone();
            let cache = enc_cache.clone();
            async move {
                let display_name = resolve_display_name(&room, &event.sender).await;
                let is_dm = matrix_sdk::BaseRoom::direct_targets_length(&room) > 0;
                let room_name = room.name().unwrap_or_else(|| room.room_id().to_string());
                tracing::debug!(
                    "NewMessage (encrypted/UTD) room={} is_dm={} sender={}",
                    room.room_id(), is_dm, event.sender
                );

                let sender_id = event.sender.to_string();
                let rid_str = room.room_id().to_string();
                let msg = MessageInfo {
                    sender: display_name,
                    sender_id: sender_id.clone(),
                    body: "\u{1f512} Unable to decrypt message".to_string(),
                    formatted_body: None,
                    timestamp: event.origin_server_ts.as_secs().into(),
                    event_id: event.event_id.to_string(),
                    reply_to: None,
                    reply_to_sender: None,
                    thread_root: None,
                    reactions: Vec::new(),
                    media: None,
                    is_highlight: false,
                    is_system_event: false,
                };
                if !cache.append_memory(&rid_str, msg.clone()) {
                    dirty.lock().unwrap().entry(rid_str.clone()).or_default().push(msg.clone());
                }
                let _ = tx
                    .send(MatrixEvent::NewMessage {
                        room_id: rid_str,
                        room_name,
                        sender_id: sender_id.clone(),
                        message: msg,
                        is_mention: false,
                        is_dm,
                    })
                    .await;
            }
        },
    );

    // Handler for reaction events — update reaction aggregations live.
    {
        use matrix_sdk::ruma::events::reaction::OriginalSyncReactionEvent;
        let react_tx = event_tx.clone();
        let react_client = client.clone();
        client.add_event_handler(
            move |event: OriginalSyncReactionEvent,
                  room: matrix_sdk::room::Room| {
                let tx = react_tx.clone();
                let client = react_client.clone();
                async move {
                    let target_event_id = event.content.relates_to.event_id.to_string();
                    let emoji = event.content.relates_to.key.clone();
                    let my_id = client.user_id().map(|u| u.to_string());
                    let reactor_is_me = my_id.as_deref()
                        .map_or(false, |uid| uid == event.sender.as_str());
                    let sender_name = resolve_display_name(&room, &event.sender).await;
                    let display = if reactor_is_me { "You".to_string() } else { sender_name.clone() };
                    let _ = tx.send(MatrixEvent::ReactionUpdate {
                        room_id: room.room_id().to_string(),
                        event_id: target_event_id.clone(),
                        reactions: vec![(emoji.clone(), 1, vec![display])],
                    }).await;

                    // Notify if someone else reacted to our message.
                    if !reactor_is_me {
                        if let Some(my_uid) = &my_id {
                            use matrix_sdk::ruma::OwnedEventId;
                            if let Ok(eid) = target_event_id.parse::<OwnedEventId>() {
                                if let Ok(orig) = room.event(&eid, None).await {
                                    let is_ours = orig.raw().deserialize()
                                        .map(|e| e.sender().as_str() == my_uid.as_str())
                                        .unwrap_or(false);
                                    if is_ours {
                                        let room_name = room.display_name().await
                                            .map(|n| n.to_string())
                                            .unwrap_or_else(|_| room.room_id().to_string());
                                        let _ = tx.send(MatrixEvent::ReactionNotification {
                                            room_id: room.room_id().to_string(),
                                            room_name,
                                            reactor: sender_name,
                                            emoji,
                                        }).await;
                                    }
                                }
                            }
                        }
                    }
                }
            },
        );
    }

    // Handler for redaction events.
    {
        use matrix_sdk::ruma::events::room::redaction::OriginalSyncRoomRedactionEvent;
        let redact_tx = event_tx.clone();
        client.add_event_handler(
            move |event: OriginalSyncRoomRedactionEvent,
                  room: matrix_sdk::room::Room| {
                let tx = redact_tx.clone();
                async move {
                    // content.redacts is the standard field for the redacted event ID.
                    if let Some(ref redacts) = event.content.redacts {
                        let _ = tx.send(MatrixEvent::MessageRedacted {
                            room_id: room.room_id().to_string(),
                            event_id: redacts.to_string(),
                        }).await;
                    }
                }
            },
        );
    }

    // Handler for member events — show join/leave/invite/kick/ban in timeline.
    {
        use matrix_sdk::ruma::events::room::member::{
            MembershipState, OriginalSyncRoomMemberEvent,
        };
        let member_tx = event_tx.clone();
        let member_client = client.clone();
        let member_cache = timeline_cache.clone();
        client.add_event_handler(
            move |event: OriginalSyncRoomMemberEvent, room: matrix_sdk::room::Room| {
                let tx = member_tx.clone();
                let client = member_client.clone();
                let cache = member_cache.clone();
                async move {
                    let sender_id = event.sender.to_string();
                    let target_id = event.state_key.to_string();
                    let my_id = client.user_id().map(|u| u.to_string());
                    // Suppress own join events (initial sync noise).
                    if matches!(event.content.membership, MembershipState::Join)
                        && my_id.as_deref() == Some(sender_id.as_str())
                    {
                        return;
                    }
                    let (sender_name, target_name) = tokio::join!(
                        resolve_display_name(&room, &event.sender),
                        async {
                            if let Ok(uid) = <&matrix_sdk::ruma::UserId>::try_from(target_id.as_str()) {
                                resolve_display_name(&room, uid).await
                            } else {
                                target_id.clone()
                            }
                        },
                    );
                    let body = match &event.content.membership {
                        MembershipState::Join => format!("{target_name} joined"),
                        MembershipState::Leave if sender_id != target_id => {
                            let reason = event.content.reason.as_deref()
                                .map(|r| format!(": {r}"))
                                .unwrap_or_default();
                            format!("{sender_name} kicked {target_name}{reason}")
                        }
                        MembershipState::Leave => format!("{target_name} left"),
                        MembershipState::Invite => format!("{sender_name} invited {target_name}"),
                        MembershipState::Ban => {
                            let reason = event.content.reason.as_deref()
                                .map(|r| format!(": {r}"))
                                .unwrap_or_default();
                            format!("{sender_name} banned {target_name}{reason}")
                        }
                        _ => return,
                    };
                    let rid_str = room.room_id().to_string();
                    let msg = MessageInfo {
                        sender: String::new(),
                        sender_id: String::new(),
                        body,
                        formatted_body: None,
                        timestamp: event.origin_server_ts.as_secs().into(),
                        event_id: event.event_id.to_string(),
                        reply_to: None,
                        reply_to_sender: None,
                        thread_root: None,
                        reactions: Vec::new(),
                        media: None,
                        is_highlight: false,
                        is_system_event: true,
                    };
                    if !cache.append_memory(&rid_str, msg.clone()) {
                        // room not in cache — no-op, it will appear on next fetch
                    }
                    let room_name = room.name().unwrap_or_else(|| room.room_id().to_string());
                    let _ = tx.send(MatrixEvent::NewMessage {
                        room_id: rid_str,
                        room_name,
                        sender_id: String::new(),
                        message: msg,
                        is_mention: false,
                        is_dm: false,
                    }).await;
                }
            },
        );
    }

    // Handler for typing events — show indicator in the room row for DMs.
    {
        use matrix_sdk::ruma::events::typing::SyncTypingEvent;
        let typing_tx = event_tx.clone();
        let typing_client = client.clone();
        client.add_event_handler(
            move |event: SyncTypingEvent, room: matrix_sdk::room::Room| {
                let tx = typing_tx.clone();
                let client = typing_client.clone();
                async move {
                    let room_id = room.room_id().to_string();
                    let my_id = client.user_id().map(|u| u.to_string());
                    let typing_ids = &event.content.user_ids;
                    // Exclude ourselves from the typing list.
                    let others: Vec<_> = typing_ids.iter()
                        .filter(|uid| my_id.as_deref() != Some(uid.as_str()))
                        .collect();
                    let names: Vec<String> = futures_util::future::join_all(
                        others.iter().map(|uid| resolve_display_name(&room, uid))
                    ).await;
                    let _ = tx.send(MatrixEvent::TypingUsers { room_id, names }).await;
                }
            },
        );
    }

    // MOTD plugin: watch for room topic changes and notify the UI.
    #[cfg(feature = "motd")]
    {
        use matrix_sdk::ruma::events::room::topic::OriginalSyncRoomTopicEvent;
        let motd_tx = event_tx.clone();
        let motd_cache = std::sync::Arc::new(std::sync::Mutex::new(
            crate::plugins::motd::load(),
        ));
        client.add_event_handler(
            move |event: OriginalSyncRoomTopicEvent, room: matrix_sdk::room::Room| {
                let tx = motd_tx.clone();
                let cache = motd_cache.clone();
                async move {
                    let new_topic = event.content.topic.clone();
                    let room_id = room.room_id().to_string();
                    let changed = {
                        let mut map = cache.lock().unwrap();
                        crate::plugins::motd::check_and_update(&room_id, &new_topic, &mut map)
                    };
                    if changed {
                        let _ = tx.send(MatrixEvent::TopicChanged { room_id, new_topic }).await;
                    }
                }
            },
        );
    }

    // Sync loop with retry.
    // Send full room list only on the first sync response (initial sync),
    // not on every incremental sync — that was causing major slowness.
    loop {
        let tx = event_tx.clone();
        let sync_client = client.clone();
        let sync_shutdown = shutdown_rx.clone();
        let timeline_cache_for_sync = timeline_cache.clone();
        let initial_sync_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let initial_flag = initial_sync_done.clone();
        let last_room_update = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let last_update_flag = last_room_update.clone();

        // Build a sync filter to minimize data transferred:
        // - Lazy-load room members (biggest win — avoids downloading every
        //   member's state for every room)
        // - Limit timeline to 1 event per room (we fetch full history
        //   on-demand when the user opens a room)
        // - Skip presence updates (we don't display online status yet)
        use matrix_sdk::ruma::api::client::filter::{
            FilterDefinition, RoomEventFilter, RoomFilter,
        };
        use matrix_sdk::ruma::api::client::sync::sync_events::v3::Filter;
        use matrix_sdk::ruma::UInt;

        let cfg = crate::config::settings();

        let mut room_filter = RoomFilter::with_lazy_loading();
        let mut timeline_filter = RoomEventFilter::with_lazy_loading();
        timeline_filter.limit = Some(UInt::from(cfg.sync.timeline_limit));
        room_filter.timeline = timeline_filter;

        let mut filter_def = FilterDefinition::with_lazy_loading();
        filter_def.room = room_filter;
        filter_def.presence = matrix_sdk::ruma::api::client::filter::Filter::ignore_all();

        let settings = SyncSettings::default()
            .timeout(std::time::Duration::from_secs(cfg.sync.timeout_secs))
            .filter(Filter::FilterDefinition(filter_def));
        let ts_ref = room_timestamps.clone();

        let sync_future = client
            .sync_with_callback(settings, move |response| {
                // Extract timestamps from the sync response for every room
                // that had timeline events. Only count real user messages,
                // not state events or bot notices.
                // Detect sync gaps — rooms where the timeline was limited
                // (server dropped events between last sync and now).
                let mut gap_rooms: Vec<String> = Vec::new();
                for (room_id, joined_room) in &response.rooms.join {
                    if joined_room.timeline.limited {
                        gap_rooms.push(room_id.to_string());
                    }
                    // Only extract timestamps from real activity events.
                    let events = &joined_room.timeline.events;
                    let activity_event = events.iter().rev().find(|ev| {
                        let raw = ev.raw().json().get();
                        serde_json::from_str::<serde_json::Value>(raw)
                            .ok()
                            .map(|v| is_room_activity(&v))
                            .unwrap_or(false)
                    });
                    if let Some(ev) = activity_event {
                        let raw_json = ev.raw().json().get();
                        if let Some(ts_ms) = serde_json::from_str::<serde_json::Value>(raw_json)
                            .ok()
                            .and_then(|v| v.get("origin_server_ts")?.as_u64())
                        {
                            let ts_sec = ts_ms / 1000;
                            let mut map = ts_ref.lock().unwrap();
                            let entry = map.entry(room_id.to_string()).or_insert(0);
                            if ts_sec > *entry {
                                *entry = ts_sec;
                            }
                        }
                    }
                }

                let tx = tx.clone();
                let client = sync_client.clone();
                let mut shutdown = sync_shutdown.clone();
                let is_first = !initial_flag.swap(true, std::sync::atomic::Ordering::Relaxed);
                let last_update = last_update_flag.clone();
                let timestamps = ts_ref.clone();
                let gap_cache = timeline_cache_for_sync.clone();
                async move {
                    // Notify UI about sync gaps so it can re-fetch affected rooms.
                    if !gap_rooms.is_empty() {
                        tracing::info!("Sync gap detected for {} rooms", gap_rooms.len());
                        // Flush each room's in-memory cache to disk BEFORE wiping it,
                        // so that the disk copy stays current (or gets promoted if it
                        // was stale).  Without this, a room that had good decrypted
                        // messages in memory from append_memory/bg_refresh would
                        // regress to whatever old snapshot was on disk.
                        for rid in &gap_rooms {
                            if let Some((msgs, token, _meta)) = gap_cache.get_memory(rid) {
                                if !msgs.is_empty() {
                                    let flush_cache = gap_cache.clone();
                                    let flush_rid = rid.clone();
                                    let flush_token = token.clone();
                                    tokio::task::spawn_blocking(move || {
                                        flush_cache.save_disk(&flush_rid, &msgs, flush_token.as_deref());
                                    });
                                }
                            }
                            gap_cache.remove_memory(rid);
                        }
                        for rid in gap_rooms {
                            let _ = tx.send(MatrixEvent::SyncGap { room_id: rid }).await;
                        }
                    }
                    // Refresh room list on initial sync and periodically after.
                    // Throttled to once every 3 minutes — unread badges update
                    // live via NewMessage events, so we only need periodic full
                    // refreshes for room ordering and space assignments.
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let prev = last_update.load(std::sync::atomic::Ordering::Relaxed);
                    let should_update = is_first || (now_secs - prev >= 180);

                    if should_update {
                        // Stamp update time NOW so the next sync (30 s away) doesn't
                        // spawn a second concurrent collect_room_info task.
                        last_update.store(now_secs, std::sync::atomic::Ordering::Relaxed);

                        // Offload the heavy work to a background task so the sync
                        // callback returns immediately — the task first yields until
                        // any user-triggered room load finishes.
                        //
                        // Why: collect_room_info iterates spaces sequentially to
                        // avoid exhausting matrix-sdk's internal SQLite pool.  This
                        // is the "click after 60 s idle = slow" pattern.
                        let bg_tx = tx.clone();
                        let bg_client = client.clone();
                        let bg_ts = timestamps.clone();
                        tokio::spawn(async move {
                            tracing::info!("collect_room_info: starting (is_first={})", is_first);
                            let t0 = std::time::Instant::now();
                            let mut rooms = collect_room_info(&bg_client, Some(&bg_ts)).await;
                            tracing::info!("collect_room_info: done in {:?} ({} rooms)", t0.elapsed(), rooms.len());

                            // Merge disk unread counts and patch timestamps from the
                            // in-memory cache *before* the first RoomListUpdated so the
                            // sidebar gets correct data immediately.
                            {
                                let prev_disk: std::collections::HashMap<String, (u64, u64)> =
                                    load_room_list_cache()
                                        .into_iter()
                                        .map(|r| (r.room_id, (r.unread_count, r.highlight_count)))
                                        .collect();
                                merge_disk_unread_counts(&mut rooms, &prev_disk);
                            }
                            {
                                let mut ts_map = bg_ts.lock().unwrap();
                                let mut patched = 0u32;
                                for room in &mut rooms {
                                    let cached = ts_map.get(&room.room_id).copied().unwrap_or(0);
                                    if room.last_activity_ts > cached {
                                        ts_map.insert(room.room_id.clone(), room.last_activity_ts);
                                    } else if cached > 0 {
                                        room.last_activity_ts = cached;
                                        patched += 1;
                                    }
                                }
                                tracing::debug!(
                                    "Timestamp patching: {} from cache, {} in map",
                                    patched, ts_map.len()
                                );
                                let room_ids: Vec<String> =
                                    rooms.iter().map(|r| r.room_id.clone()).collect();
                                save_timestamp_cache(&ts_map, Some(&room_ids));
                            }

                            // Send the sidebar update immediately — don't block on backfill.
                            save_room_list_cache(&rooms);
                            let _ = bg_tx.send(MatrixEvent::RoomListUpdated { rooms: rooms.clone() }).await;

                            // Backfill timestamps for rooms that still have ts=0 (first
                            // ever launch or rooms with no cached ts). This is slow (network
                            // requests) and runs AFTER the sidebar is already showing data.
                            if is_first {
                                backfill_timestamps(&bg_client, &mut rooms).await;
                                // Only send a second update if backfill actually filled something.
                                let filled = rooms.iter().filter(|r| r.last_activity_ts > 0).count();
                                if filled > 0 {
                                    {
                                        let mut ts_map = bg_ts.lock().unwrap();
                                        for room in &rooms {
                                            if room.last_activity_ts > 0 {
                                                ts_map.insert(room.room_id.clone(), room.last_activity_ts);
                                            }
                                        }
                                        let room_ids: Vec<String> =
                                            rooms.iter().map(|r| r.room_id.clone()).collect();
                                        save_timestamp_cache(&ts_map, Some(&room_ids));
                                    }
                                    save_room_list_cache(&rooms);
                                    let _ = bg_tx.send(MatrixEvent::RoomListUpdated { rooms }).await;
                                }
                            }
                        });
                    }

                    if *shutdown.borrow_and_update() {
                        tracing::info!("Shutdown requested, stopping sync");
                        matrix_sdk::LoopCtrl::Break
                    } else {
                        matrix_sdk::LoopCtrl::Continue
                    }
                }
            });

        // Race the sync future against the shutdown signal.
        // When shutdown fires, dropping `sync_future` cancels the in-flight
        // HTTP long-poll — making quit nearly instant instead of waiting up
        // to `timeout_secs` for the current poll to complete.
        let result = tokio::select! {
            r = sync_future => r,
            _ = shutdown_rx.changed() => {
                tracing::info!("Shutdown signal received, aborting sync long-poll");
                break;
            }
        };

        match result {
            Ok(()) => break,
            Err(e) => {
                // Check shutdown before retrying.
                if *shutdown_rx.borrow() {
                    tracing::info!("Shutdown during sync error, exiting");
                    break;
                }
                tracing::warn!("Sync error, retrying in 5s: {e}");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                    _ = shutdown_rx.changed() => {
                        tracing::info!("Shutdown during retry wait, exiting");
                        break;
                    }
                }
            }
        }
    }

    tracing::info!("Sync loop exited cleanly");
}

/// Extract messages from a chunk of timeline events.
async fn extract_messages(
    room: &matrix_sdk::room::Room,
    chunk: &[matrix_sdk::deserialized_responses::TimelineEvent],
    reverse: bool,
    my_user_id: Option<&matrix_sdk::ruma::UserId>,
) -> Vec<MessageInfo> {
    let mut messages = Vec::new();
    let iter: Box<dyn Iterator<Item = &matrix_sdk::deserialized_responses::TimelineEvent> + Send> =
        if reverse {
            Box::new(chunk.iter().rev())
        } else {
            Box::new(chunk.iter())
        };

    // First pass: collect reactions, replacements, and unique sender IDs.
    let mut reaction_map: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();
    // original_event_id → (new_body, new_formatted_body, replacement_event_id)
    let mut replacement_map: std::collections::HashMap<String, (String, Option<String>, String)> =
        std::collections::HashMap::new();
    let mut replacement_event_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut sender_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for timeline_event in chunk {
        if let Ok(ev) = timeline_event.raw().deserialize() {
            match ev {
                matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                    matrix_sdk::ruma::events::AnySyncMessageLikeEvent::Reaction(
                        matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(reaction),
                    ),
                ) => {
                    let target = reaction.content.relates_to.event_id.to_string();
                    let emoji = reaction.content.relates_to.key.clone();
                    let sender = reaction.sender.to_string();
                    sender_ids.insert(sender.clone());
                    reaction_map.entry(target).or_default().push((emoji, sender));
                }
                matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                    matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomMessage(
                        matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(msg),
                    ),
                ) => {
                    sender_ids.insert(msg.sender.to_string());
                    use matrix_sdk::ruma::events::room::message::Relation;
                    if let Some(Relation::Replacement(replacement)) = &msg.content.relates_to {
                        let original_id = replacement.event_id.to_string();
                        // Use m.new_content (the canonical new body) not the outer
                        // fallback body ("* ...") that is only for clients unaware of edits.
                        let (new_body, new_formatted, _) = extract_message_content(&replacement.new_content.msgtype)
                            .unwrap_or_default();
                        replacement_map.insert(original_id, (new_body, new_formatted, msg.event_id.to_string()));
                        replacement_event_ids.insert(msg.event_id.to_string());
                    }
                }
                matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                    matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomEncrypted(
                        matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(enc),
                    ),
                ) => {
                    sender_ids.insert(enc.sender.to_string());
                }
                matrix_sdk::ruma::events::AnySyncTimelineEvent::State(
                    matrix_sdk::ruma::events::AnySyncStateEvent::RoomMember(
                        matrix_sdk::ruma::events::SyncStateEvent::Original(member),
                    ),
                ) => {
                    sender_ids.insert(member.sender.to_string());
                    sender_ids.insert(member.state_key.to_string());
                }
                _ => {}
            }
        }
    }

    // Batch-resolve display names in parallel — one lookup per unique sender.
    // join_all fires all lookups concurrently; cache hits return immediately.
    let display_names: std::collections::HashMap<String, String> = {
        let futs: Vec<_> = sender_ids.iter()
            .filter_map(|uid_str| {
                <&matrix_sdk::ruma::UserId>::try_from(uid_str.as_str())
                    .ok()
                    .map(|uid| {
                        let uid_str = uid_str.clone();
                        async move {
                            let name = resolve_display_name(room, uid).await;
                            (uid_str, name)
                        }
                    })
            })
            .collect();
        futures_util::future::join_all(futs).await.into_iter().collect()
    };

    // Count event kinds and log a sample UTD reason for diagnostics.
    let (mut n_plain, mut n_decrypted, mut n_utd) = (0usize, 0usize, 0usize);
    let mut first_utd_reason: Option<String> = None;
    for te in chunk {
        use matrix_sdk::deserialized_responses::TimelineEventKind;
        match &te.kind {
            TimelineEventKind::PlainText { .. } => n_plain += 1,
            TimelineEventKind::Decrypted(_) => n_decrypted += 1,
            TimelineEventKind::UnableToDecrypt { utd_info, .. } => {
                n_utd += 1;
                if first_utd_reason.is_none() {
                    first_utd_reason = Some(format!("{:?} session={:?}", utd_info.reason, utd_info.session_id));
                }
            }
        }
    }
    if n_utd > 0 || n_decrypted > 0 {
        tracing::info!(
            "Event kinds: plain={n_plain} decrypted={n_decrypted} utd={n_utd}{}",
            first_utd_reason.map(|r| format!(" (sample UTD: {r})")).unwrap_or_default()
        );
    }

    for timeline_event in iter {
        let event = match timeline_event.raw().deserialize() {
            Ok(ev) => ev,
            Err(_) => continue,
        };
        match event {
            matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomMessage(msg_event),
            ) => {
                let msg_event = match msg_event {
                    matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(orig) => orig,
                    matrix_sdk::ruma::events::SyncMessageLikeEvent::Redacted(_) => {
                        tracing::debug!("Skipping redacted message event");
                        continue;
                    }
                    _ => continue,
                };
                let event_id = msg_event.event_id.to_string();

                if replacement_event_ids.contains(&event_id) {
                    continue;
                }

                use matrix_sdk::ruma::events::room::message::Relation;
                if matches!(&msg_event.content.relates_to, Some(Relation::Replacement(_))) {
                    continue;
                }

                let Some((mut body, mut formatted_body, media)) = extract_message_content(&msg_event.content.msgtype) else {
                    continue;
                };

                if let Some((new_body, new_formatted, _)) = replacement_map.get(&event_id) {
                    tracing::info!("Applying edit to {event_id}: '{}'", &new_body[..new_body.len().min(40)]);
                    body = new_body.clone();
                    formatted_body = new_formatted.clone();
                }

                let sender_id = msg_event.sender.to_string();
                let display_name = display_names.get(&sender_id)
                    .cloned()
                    .unwrap_or_else(|| sender_id.clone());

                let (reply_to, thread_root) = match &msg_event.content.relates_to {
                    Some(Relation::Reply { in_reply_to }) => {
                        (Some(in_reply_to.event_id.to_string()), None)
                    }
                    Some(Relation::Thread(thread)) => {
                        let reply = thread.in_reply_to.as_ref()
                            .map(|r| r.event_id.to_string());
                        (reply, Some(thread.event_id.to_string()))
                    }
                    _ => (None, None),
                };

                let reactions = aggregate_reactions(reaction_map.get(&event_id));

                messages.push(MessageInfo {
                    sender: display_name,
                    sender_id,
                    body,
                    formatted_body: formatted_body.clone(),
                    timestamp: msg_event.origin_server_ts.as_secs().into(),
                    event_id,
                    reply_to,
                    reply_to_sender: None,
                    thread_root,
                    reactions,
                    media,
                    is_highlight: false,
                    is_system_event: false,
                });
            }
            matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomEncrypted(enc),
            ) => {
                let (sender, event_id) = match &enc {
                    matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(o) => {
                        (o.sender.to_string(), o.event_id.to_string())
                    }
                    _ => continue,
                };
                if replacement_event_ids.contains(&event_id) || event_id.is_empty() {
                    continue;
                }
                let raw = timeline_event.raw().json().get();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                    if let Some(rel_type) = v.get("content")
                        .and_then(|c| c.get("m.relates_to"))
                        .and_then(|r| r.get("rel_type"))
                        .and_then(|t| t.as_str())
                    {
                        if rel_type == "m.replace" {
                            continue;
                        }
                    }
                }
                let display_name = display_names.get(&sender)
                    .cloned()
                    .unwrap_or_else(|| sender.clone());
                messages.push(MessageInfo {
                    sender: display_name,
                    sender_id: sender,
                    body: "\u{1f512} Unable to decrypt message".to_string(),
                    formatted_body: None,
                    timestamp: 0,
                    event_id,
                    reply_to: None,
                    reply_to_sender: None,
                    thread_root: None,
                    reactions: Vec::new(),
                    media: None,
                    is_highlight: false,
                    is_system_event: false,
                });
            }
            matrix_sdk::ruma::events::AnySyncTimelineEvent::State(
                matrix_sdk::ruma::events::AnySyncStateEvent::RoomMember(
                    matrix_sdk::ruma::events::SyncStateEvent::Original(member),
                ),
            ) => {
                let sender_id = member.sender.to_string();
                let target_id = member.state_key.to_string();
                let membership = &member.content.membership;
                use matrix_sdk::ruma::events::room::member::MembershipState;
                let sender_name = display_names.get(&sender_id)
                    .cloned()
                    .unwrap_or_else(|| sender_id.clone());
                let target_name = display_names.get(&target_id)
                    .cloned()
                    .unwrap_or_else(|| target_id.clone());
                let body = match membership {
                    MembershipState::Join => format!("{target_name} joined"),
                    MembershipState::Leave if sender_id != target_id => {
                        let reason = member.content.reason.as_deref()
                            .map(|r| format!(": {r}"))
                            .unwrap_or_default();
                        format!("{sender_name} kicked {target_name}{reason}")
                    }
                    MembershipState::Leave => format!("{target_name} left"),
                    MembershipState::Invite => format!("{sender_name} invited {target_name}"),
                    MembershipState::Ban => {
                        let reason = member.content.reason.as_deref()
                            .map(|r| format!(": {r}"))
                            .unwrap_or_default();
                        format!("{sender_name} banned {target_name}{reason}")
                    }
                    _ => continue,
                };
                messages.push(MessageInfo {
                    sender: String::new(),
                    sender_id: String::new(),
                    body,
                    formatted_body: None,
                    timestamp: member.origin_server_ts.as_secs().into(),
                    event_id: member.event_id.to_string(),
                    reply_to: None,
                    reply_to_sender: None,
                    thread_root: None,
                    reactions: Vec::new(),
                    media: None,
                    is_highlight: false,
                    is_system_event: true,
                });
            }
            _ => continue,
        }
    }
    // Post-process: populate reply_to_sender and mark highlights.
    {
        let sender_map: std::collections::HashMap<String, String> = messages
            .iter()
            .filter(|m| !m.event_id.is_empty())
            .map(|m| (m.event_id.clone(), m.sender.clone()))
            .collect();

        let my_event_ids: std::collections::HashSet<String> = my_user_id
            .map(|uid| {
                messages.iter()
                    .filter(|m| m.sender_id == uid.as_str())
                    .map(|m| m.event_id.clone())
                    .collect()
            })
            .unwrap_or_default();

        for msg in &mut messages {
            if let Some(ref reply_to) = msg.reply_to {
                if let Some(name) = sender_map.get(reply_to) {
                    msg.reply_to_sender = Some(name.clone());
                }
                if my_event_ids.contains(reply_to) {
                    msg.is_highlight = true;
                }
            }
            if let Some(ref thread_root) = msg.thread_root {
                if my_event_ids.contains(thread_root) {
                    msg.is_highlight = true;
                }
            }
        }
    }

    messages
}

/// Fetch recent messages for a room and send them to the UI.
/// `key_fetched_rooms` tracks room IDs for which we've already triggered a
/// key download from backup this session (to avoid redundant network calls).
/// This set contains only room ID strings, NOT encryption keys.
/// Pre-fetch a room's timeline into the cache without sending to the UI.
/// Used during startup to warm the cache for unread rooms.

/// Background room select — fetches messages and metadata, updates cache + UI.
/// Runs in a spawned task so it doesn't block the command loop.
async fn handle_select_room_bg(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    timeline_cache: super::room_cache::RoomCache,
    // Cooperative cancel flag.
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    // Unread count from the UI badge before clear_unread() zeroed it.
    // Passed as floor to compute_enter_unread() for the pre-sync window
    // where sdk_unread is still 0 but the user clearly has unread messages.
    known_unread: u32,
) {
    use matrix_sdk::ruma::UInt;

    let Ok(room_id) = RoomId::parse(room_id) else {
        tracing::error!("Invalid room ID: {room_id}");
        return;
    };

    let Some(room) = client.get_room(&room_id) else {
        tracing::error!("Room not found: {room_id}");
        return;
    };

    let t_enter = std::time::Instant::now();
    tracing::info!("bg_refresh {room_id}: entered");

    // Exit early if a newer SelectRoom has already been issued.
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::info!("bg_refresh {room_id}: cancelled before spawn ({:?})", t_enter.elapsed());
        return;
    }

    // Spawn the fetch + post-join work as a detachable task.
    //
    // If room.messages() takes > 20 s (slow/overloaded homeserver) we send an
    // empty RoomMessages to stop the spinner, then DROP this JoinHandle.
    // Dropping a JoinHandle in Tokio detaches the task — it keeps running.
    // When it eventually finishes it updates the cache (silent) and, if the
    // user is still on the room, sends the real messages to the UI.
    let inner_client     = client.clone();
    let inner_tx         = event_tx.clone();
    let inner_cache      = timeline_cache.clone();
    let inner_cancel     = cancel.clone();
    let inner_room       = room.clone();
    let inner_room_id    = room_id.clone();

    let inner = tokio::spawn(async move {
        let room       = inner_room;
        let client     = inner_client;
        let event_tx   = inner_tx;
        let timeline_cache = inner_cache;
        let cancel     = inner_cancel;
        let room_id    = inner_room_id;
        let start      = std::time::Instant::now();
        let t_inner    = start;

        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!("bg_refresh {room_id}: cancelled before fetch");
            return;
        }

        tracing::info!("bg_refresh {room_id}: starting room.messages()");

        // Run message fetch, metadata, and member list in parallel.
        use matrix_sdk::ruma::api::client::filter::RoomEventFilter;

        let make_msg_options = || {
            let mut f = RoomEventFilter::default();
            // Include m.reaction so the reaction_map is populated and reactions
            // are stored in the disk cache.  Without this, historical reactions
            // (added before the current session) are never shown.  In practice
            // reactions are sparse — even in active rooms they rarely exceed
            // 10-15% of events, so the effective message count stays ~85+/100.
            f.types = Some(vec![
                "m.room.message".to_string(),
                "m.room.encrypted".to_string(),
                "m.reaction".to_string(),
            ]);
            let mut o = matrix_sdk::room::MessagesOptions::backward();
            o.limit = UInt::from(100u32);
            o.filter = f;
            o
        };

        // Fork: messages, tombstone, pinned, members — all independent.
        let msg_room = room.clone();
        let msg_client = client.clone();
        let cancel_msg = cancel.clone();
        let msg_future = async move {
            use matrix_sdk::deserialized_responses::TimelineEventKind;

            let response = match msg_room.messages(make_msg_options()).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Failed to fetch messages for {}: {e}", msg_room.room_id());
                    return (Vec::new(), None);
                }
            };
        // Collect the unique Megolm session IDs that caused Unable-to-Decrypt.
        // For a DM this is typically 1-5 sessions (one per key rotation).
        let utd_sessions: std::collections::HashSet<String> = response.chunk.iter()
            .filter_map(|te| {
                if let TimelineEventKind::UnableToDecrypt { utd_info, .. } = &te.kind {
                    utd_info.session_id.clone()
                } else {
                    None
                }
            })
            .collect();

        // Note: we deliberately do NOT check cancel here.  room.messages() has
        // already paid its full cost; returning empty would discard the results
        // and prevent the disc cache from being populated.  The cancel check
        // after tokio::join! (in handle_select_room_bg) saves to disc cache
        // and exits before sending to the UI.  Skip Phase 2 only (cheaper).
        let skip_phase2 = cancel_msg.load(std::sync::atomic::Ordering::Relaxed);

        // Two-phase decrypt: if there were UTDs, download only the specific
        // sessions needed for these 50 messages (not all sessions for the room),
        // then re-fetch so room.messages() decrypts from the in-memory cache.
        //
        // This avoids the SQLite write-lock contention that happened when we ran
        // a concurrent download_room_keys_for_room alongside room.messages() —
        // the write lock blocked per-event session reads, causing 7 s spikes.
        let (chunk, token) = if !skip_phase2 && !utd_sessions.is_empty() {
            let backups = msg_client.encryption().backups();
            let enabled = backups.are_enabled().await;
            tracing::info!(
                "bg_refresh {}: {} UTD session(s), backup_enabled={}",
                msg_room.room_id(), utd_sessions.len(), enabled
            );
            if enabled {
                let dl_results = futures_util::future::join_all(
                    utd_sessions.iter()
                        .map(|sid| backups.download_room_key(msg_room.room_id(), sid))
                ).await;
                let downloaded = dl_results.iter().filter(|r| r.is_ok()).count();
                tracing::info!(
                    "bg_refresh {}: downloaded {}/{} sessions from backup",
                    msg_room.room_id(), downloaded, dl_results.len()
                );
                // Phase-2 fetch: sessions are now in the in-memory OlmMachine
                // cache so decryption is pure RAM with no SQLite round-trips.
                match msg_room.messages(make_msg_options()).await {
                    Ok(r2) => {
                        let tok2 = r2.end.map(|t| t.to_string());
                        (r2.chunk, tok2)
                    }
                    Err(_) => {
                        let tok = response.end.map(|t| t.to_string());
                        (response.chunk, tok)
                    }
                }
            } else {
                tracing::warn!(
                    "bg_refresh {}: backup not enabled, skipping phase-2 decrypt",
                    msg_room.room_id()
                );
                let tok = response.end.map(|t| t.to_string());
                (response.chunk, tok)
            }
        } else {
            let tok = response.end.map(|t| t.to_string());
            (response.chunk, tok)
        };

        let msgs = extract_messages(&msg_room, &chunk, true, msg_client.user_id()).await;

        // Read receipts are sent via the MarkRead command after the user has
        // stayed in the room for 15 seconds (see window.rs read_timer).
        // Do NOT send one here — it would mark unread messages as read
        // immediately, before the user has actually seen them.
        (msgs, token)
    };

        let tombstone_room = room.clone();
        let tombstone_client = client.clone();
        let tombstone_future = async move {
        use matrix_sdk::ruma::events::room::tombstone::RoomTombstoneEventContent;
        let tombstone = tombstone_room
            .get_state_event_static::<RoomTombstoneEventContent>()
            .await
            .ok()
            .flatten()
            .and_then(|raw| raw.deserialize().ok())
            .and_then(|ev| {
                if let matrix_sdk::deserialized_responses::SyncOrStrippedState::Sync(
                    matrix_sdk::ruma::events::SyncStateEvent::Original(orig),
                ) = ev
                {
                    Some(orig.content)
                } else {
                    None
                }
            });
        match tombstone {
            Some(content) => {
                let rid = content.replacement_room.to_string();
                let name = tombstone_client
                    .get_room(&content.replacement_room)
                    .and_then(|r| r.cached_display_name().map(|n| n.to_string()));
                (true, Some(rid), name)
            }
            None => (false, None, None),
        }
    };

        let pinned_room = room.clone();
        let pinned_future = async move {
        use matrix_sdk::ruma::events::room::pinned_events::RoomPinnedEventsEventContent;
        match pinned_room
            .get_state_event_static::<RoomPinnedEventsEventContent>()
            .await
        {
            Ok(Some(raw)) => {
                if let Ok(ev) = raw.deserialize() {
                    let pinned_ids = match ev {
                        matrix_sdk::deserialized_responses::SyncOrStrippedState::Sync(
                            matrix_sdk::ruma::events::SyncStateEvent::Original(orig),
                        ) => orig.content.pinned,
                        _ => vec![],
                    };
                    // Fetch all pinned events in parallel (no sequential round trips).
                    let fetch_futs: Vec<_> = pinned_ids.iter().take(5).map(|event_id| {
                        let r = pinned_room.clone();
                        let eid = event_id.to_owned();
                        async move { r.event(&eid, None).await.ok() }
                    }).collect();
                    let fetched = futures_util::future::join_all(fetch_futs).await;
                    let mut entries = Vec::new();
                    for ev_opt in fetched.into_iter().flatten() {
                        if let Ok(timeline_ev) = ev_opt.raw().deserialize() {
                            if let matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                                matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomMessage(
                                    matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(msg),
                                ),
                            ) = timeline_ev
                            {
                                let Some((body, formatted, _media)) = extract_message_content(&msg.content.msgtype) else {
                                    continue;
                                };
                                let sender = resolve_display_name(&pinned_room, &msg.sender).await;
                                entries.push((sender, body, formatted));
                            }
                        }
                    }
                    entries
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    };

        let enc_room = room.clone();
        let enc_future = async move { enc_room.is_encrypted().await.unwrap_or(false) };

        let fully_read_room = room.clone();
        let fully_read_future = async move {
            use matrix_sdk::ruma::events::fully_read::FullyReadEventContent;
            fully_read_room
            .account_data_static::<FullyReadEventContent>()
            .await
            .ok()
            .flatten()
            .and_then(|raw| raw.deserialize().ok())
            .map(|ev| ev.content.event_id.to_string())
        };

        // Member fetch runs in parallel with messages, tombstone, etc.
        // No size cap — for large rooms this may take a second or two, but it runs
        // concurrently so it doesn't add to total latency beyond the message fetch.
        // Result is cached per-session so subsequent room visits are instant.
        let member_room = room.clone();
        let member_cache = timeline_cache.clone();
        let member_room_id_str = room_id.to_string();
        let member_future = async move {
            if let Some(cached) = member_cache.get_cached_members(&member_room_id_str) {
                return (cached, true); // (members+avatars, was_cached)
            }
            use matrix_sdk::RoomMemberships;
            match member_room.members(RoomMemberships::JOIN).await {
                Ok(member_list) => {
                    let members: Vec<(String, String)> = member_list
                        .iter()
                        .map(|m| (
                            m.user_id().to_string(),
                            m.display_name().unwrap_or_else(|| m.user_id().localpart()).to_string(),
                        ))
                        .collect();
                    let member_avatars: Vec<(String, String)> = member_list
                        .iter()
                        .filter_map(|m| {
                            m.avatar_url().map(|url| (m.user_id().to_string(), url.to_string()))
                        })
                        .collect();
                    member_cache.cache_members(
                        &member_room_id_str,
                        members.clone(),
                        member_avatars.clone(),
                    );
                    ((members, member_avatars), false)
                }
                Err(e) => {
                    tracing::warn!("bg_refresh {member_room_id_str}: room.members() failed: {e}");
                    ((vec![], vec![]), false)
                }
            }
        };

        // Run messages, tombstone, pinned, encryption, fully_read, and members in parallel.
        let (
            (all_messages, prev_batch_token),
            (is_tombstoned, replacement_room, replacement_room_name),
            pinned_messages,
            is_encrypted,
            fully_read_event_id,
            ((members, member_avatars), members_were_cached),
        ) = tokio::join!(
            msg_future,
            tombstone_future,
            pinned_future,
            enc_future,
            fully_read_future,
            member_future,
        );
        let members_fetched_flag = !members.is_empty();
        tracing::info!("bg_refresh {room_id}: members fetched={members_fetched_flag} (count={}, cached={members_were_cached})", members.len());
        tracing::info!("bg_refresh {room_id}: tokio::join! done in {:?}", t_inner.elapsed());
        // Backfill reply_to_sender from the existing memory cache for replies
        // that point to events outside the current 25-message batch.  Modern
        // clients often omit the ">" fallback quote, leaving sender unknown.
        let mut all_messages = all_messages;
        {
            if let Some((old_msgs, _, _)) = timeline_cache.get_memory(&room_id.to_string()) {
                let cache_senders: std::collections::HashMap<String, String> = old_msgs
                    .iter()
                    .filter(|m| !m.event_id.is_empty())
                    .map(|m| (m.event_id.clone(), m.sender.clone()))
                    .collect();
                for msg in &mut all_messages {
                    if msg.reply_to_sender.is_none() {
                        if let Some(ref rt) = msg.reply_to {
                            if let Some(name) = cache_senders.get(rt) {
                                msg.reply_to_sender = Some(name.clone());
                            }
                        }
                    }
                }
            }
        }

        // For large rooms (count > 200) the full member fetch was skipped.
        // Fall back to the unique senders from the loaded timeline so nick
        // completion and the room info panel still work for recent participants.
        let members = if members.is_empty() && !all_messages.is_empty() {
            let mut seen = std::collections::HashSet::new();
            let mut timeline_members: Vec<(String, String)> = all_messages.iter()
                .rev()  // most-recent senders first
                .filter(|m| !m.sender_id.is_empty() && seen.insert(m.sender_id.clone()))
                .map(|m| (m.sender_id.clone(), m.sender.clone()))
                .collect();
            timeline_members.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
            tracing::debug!("bg_refresh {room_id}: derived {} members from timeline", timeline_members.len());
            timeline_members
        } else {
            members
        };
        let members_fetched = members_fetched_flag;
        let topic = room.topic().unwrap_or_default();
        // Prefer the fetched member list length — it's accurate even before the
        // first sync populates the SDK's joined_member_count summary field.
        let member_count = if !members.is_empty() {
            members.len() as u64
        } else {
            room.joined_members_count()
        };
        // Derive unread_count from the fetched messages + fully_read marker.
        // The SDK's unread_notification_counts() is 0 until after the first sync
        // has processed notification counts — using it directly produces no divider
        // for rooms with new messages opened before the first sync completes.
        // compute_enter_unread() counts messages after the fully_read marker;
        // falls back to the SDK count (or 0) only when the marker is absent.
        let sdk_unread = room.unread_notification_counts().notification_count as u32;
        let unread_count = compute_enter_unread(
            &all_messages,
            fully_read_event_id.as_deref(),
            sdk_unread,
            known_unread,
        );
        let room_meta = RoomMeta {
            topic,
            is_tombstoned,
            replacement_room,
            replacement_room_name,
            pinned_messages,
            is_encrypted,
            member_count,
            is_favourite: room.is_favourite(),
            members,
            member_avatars,
            members_fetched,
            unread_count,
            fully_read_event_id,
        };

        let rid_str = room_id.to_string();
        let prev_count = timeline_cache.get_memory(&rid_str)
            .map(|(msgs, _, _)| msgs.len())
            .unwrap_or(0);

        if !all_messages.is_empty() {
            // Always merge server result with in-memory cache by event_id union.
            // Never prefer one side by size — the server fetch goes backward
            // (older history) while live sync delivers forward (new messages).
            // Taking the larger side by count would discard the other side's
            // unique events (e.g. user's recent comments not yet in the server's
            // backward window, or older history not yet in the live-sync window).
            let merged_messages = {
                let existing = timeline_cache.get_memory(&rid_str)
                    .map(|(msgs, _, _)| msgs)
                    .unwrap_or_default();
                if existing.is_empty() {
                    all_messages.clone()
                } else {
                    // Union by event_id: memory wins for body/sender so local
                    // edits are preserved, but reactions come from the server
                    // (authoritative source).  Sort chronologically.
                    let mut by_id: std::collections::HashMap<String, crate::matrix::MessageInfo> =
                        std::collections::HashMap::new();
                    // Insert memory messages first.
                    for m in existing.into_iter() {
                        if m.event_id.is_empty() { continue; }
                        by_id.insert(m.event_id.clone(), m);
                    }
                    // Overlay server messages: memory wins for text, server wins
                    // for reactions (which the disk cache never stored until now).
                    for m in all_messages.clone() {
                        if m.event_id.is_empty() { continue; }
                        by_id.entry(m.event_id.clone())
                            .and_modify(|existing| {
                                // Take server reactions if present; keep memory body.
                                if !m.reactions.is_empty() {
                                    existing.reactions = m.reactions.clone();
                                }
                            })
                            .or_insert(m);
                    }
                    let mut merged: Vec<crate::matrix::MessageInfo> =
                        by_id.into_values().collect();
                    merged.sort_by_key(|m| m.timestamp);
                    tracing::info!(
                        "bg_refresh {room_id}: merged server({}) ∪ memory → {} total",
                        all_messages.len(),
                        merged.len(),
                    );
                    merged
                }
            };
            timeline_cache.insert_memory_only(
                &rid_str,
                merged_messages.clone(),
                prev_batch_token.clone(),
                room_meta.clone(),
            );
            timeline_cache.mark_fresh(&rid_str);
            let disk_cache = timeline_cache.clone();
            let disk_token = prev_batch_token.clone();
            let disk_rid = rid_str.clone();
            tokio::task::spawn_blocking(move || {
                disk_cache.save_disk(&disk_rid, &merged_messages, disk_token.as_deref());
            });
        }

        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }

        // Always send RoomMessages.  An earlier Tokio-side "skip re-render"
        // optimisation compared fresh_top == prev_top, but this caused stale
        // data to persist when the disk cache was built from an incomplete
        // sync (sync-gap rooms whose timeline is always limited, e.g.
        // #gnome-hackers).  The GTK-side set_messages already has its own
        // skip logic (event_index comparison) that is accurate and cheap.
        tracing::debug!(
            "bg_refresh for {}: prev_count={} fresh_count={} unread={} fully_read={:?}",
            room_id, prev_count,
            timeline_cache.get_memory(&rid_str).map(|(m,_,_)| m.len()).unwrap_or(0),
            room_meta.unread_count, room_meta.fully_read_event_id
        );

        let _ = event_tx
            .send(MatrixEvent::RoomMessages {
                room_id: room_id.to_string(),
                messages: timeline_cache.get_memory(&room_id.to_string())
                    .map(|(msgs, _, _)| msgs)
                    .unwrap_or(all_messages),
                prev_batch_token,
                room_meta,
                is_background: true,
            })
            .await;
    }); // end tokio::spawn(inner)

    // Wait up to 20 s for the inner task.  If it takes longer (slow homeserver,
    // large encrypted room) we stop the spinner with an empty response and drop
    // the JoinHandle — Tokio detaches rather than cancels the task, so it keeps
    // running in the background.  When it eventually finishes it updates the
    // cache (silent if user moved on) or sends real messages (if still waiting).
    match tokio::time::timeout(
        std::time::Duration::from_secs(20),
        inner,
    ).await {
        Ok(_) => {} // Completed normally.
        Err(_elapsed) => {
            tracing::warn!("bg_refresh {room_id}: >20s, sending placeholder; fetch continues in bg");
            if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = event_tx
                    .send(MatrixEvent::RoomMessages {
                        room_id: room_id.to_string(),
                        messages: vec![],
                        prev_batch_token: None,
                        room_meta: RoomMeta::default(),
                        is_background: true,
                    })
                    .await;
            }
            // JoinHandle dropped here → inner task detached.
            // _store_permit lives inside inner task → released when inner finishes.
        }
    }
}

/// Fetch older messages for a room (pagination).
async fn handle_fetch_older(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    from_token: &str,
) {
    use matrix_sdk::ruma::UInt;

    let Ok(room_id) = RoomId::parse(room_id) else {
        return;
    };
    let Some(room) = client.get_room(&room_id) else {
        return;
    };

    use matrix_sdk::ruma::api::client::filter::RoomEventFilter;
    let mut msg_filter = RoomEventFilter::default();
    // Reactions excluded for the same reason as the initial fetch: they count
    // against the limit but produce no visible messages.
    msg_filter.types = Some(vec![
        "m.room.message".to_string(),
        "m.room.encrypted".to_string(),
    ]);
    let mut options = matrix_sdk::room::MessagesOptions::backward();
    options.limit = UInt::from(50u32);
    options.from = Some(from_token.to_string());
    options.filter = msg_filter;

    let (messages, prev_batch_token) = match room.messages(options).await {
        Ok(response) => {
            let msgs = extract_messages(&room, &response.chunk, true, client.user_id()).await;
            let token = response.end.map(|t| t.to_string());
            (msgs, token)
        }
        Err(e) => {
            tracing::error!("Failed to fetch older messages for {room_id}: {e}");
            (Vec::new(), None)
        }
    };

    let _ = event_tx
        .send(MatrixEvent::OlderMessages {
            room_id: room_id.to_string(),
            messages,
            prev_batch_token,
        })
        .await;
}

/// Send a text message to a room.
async fn handle_send_media(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    file_path: &str,
) {
    use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
    use std::path::Path;

    let Ok(room_id) = RoomId::parse(room_id) else { return };
    let Some(room) = client.get_room(&room_id) else { return };

    let path = Path::new(file_path);
    let filename = path.file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    // Read file.
    let data = match std::fs::read(file_path) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Failed to read file {file_path}: {e}");
            return;
        }
    };

    // Detect MIME type from extension.
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("wav") => "audio/wav",
        Some("flac") => "audio/flac",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    };
    let mime_type: mime::Mime = mime.parse().unwrap_or(mime::APPLICATION_OCTET_STREAM);

    // send_attachment handles all media types based on MIME.
    if let Err(e) = room.send_attachment(&filename, &mime_type, data, Default::default()).await {
        tracing::error!("Failed to send attachment: {e}");
    }
}

async fn handle_download_media(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    mxc_url: &str,
    filename: &str,
    source_json: &str,
) {
    use matrix_sdk::media::MediaFormat;

    // Handle local file:// URLs (from local echo) — already on disk.
    if let Some(path) = mxc_url.strip_prefix("file://") {
        let _ = event_tx
            .send(MatrixEvent::MediaReady {
                url: mxc_url.to_string(),
                path: path.to_string(),
            })
            .await;
        return;
    }

    // Handle HTTPS URLs (e.g. Giphy) — download directly.
    if mxc_url.starts_with("https://") || mxc_url.starts_with("http://") {
        match reqwest::get(mxc_url).await {
            Ok(resp) => {
                // Determine extension from Content-Type header or URL.
                let content_type = resp.headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                if let Ok(data) = resp.bytes().await {
                    let tmp_dir = std::env::temp_dir().join("hikyaku-media");
                    let _ = std::fs::create_dir_all(&tmp_dir);
                    let ext = if content_type.contains("gif") { "gif" }
                        else if content_type.contains("png") { "png" }
                        else if content_type.contains("webp") { "webp" }
                        else if content_type.contains("mp4") { "mp4" }
                        else if content_type.contains("webm") { "webm" }
                        else if content_type.contains("jpeg") || content_type.contains("jpg") { "jpg" }
                        else {
                            // Fall back to URL path extension.
                            mxc_url.rsplit('.').next()
                                .filter(|e| e.len() <= 4)
                                .unwrap_or("gif")
                        };
                    // Use a hash of the URL as filename to avoid collisions.
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    let mut hasher = DefaultHasher::new();
                    mxc_url.hash(&mut hasher);
                    let safe_name = format!("{:x}.{ext}", hasher.finish());
                    let path = tmp_dir.join(&safe_name);
                    if let Err(e) = std::fs::write(&path, &data) {
                        tracing::error!("Failed to write media file: {e}");
                        return;
                    }
                    let _ = event_tx
                        .send(MatrixEvent::MediaReady {
                            url: mxc_url.to_string(),
                            path: path.to_string_lossy().to_string(),
                        })
                        .await;
                }
            }
            Err(e) => tracing::error!("Failed to download {mxc_url}: {e}"),
        }
        return;
    }

    // Reconstruct the MediaSource from JSON if available (handles encrypted media).
    let source = if !source_json.is_empty() {
        match serde_json::from_str::<matrix_sdk::ruma::events::room::MediaSource>(source_json) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to parse media source JSON: {e}");
                return;
            }
        }
    } else {
        let Ok(uri) = <&matrix_sdk::ruma::MxcUri>::try_from(mxc_url) else {
            tracing::error!("Invalid mxc URL: {mxc_url}");
            return;
        };
        matrix_sdk::ruma::events::room::MediaSource::Plain(uri.to_owned())
    };

    let request = matrix_sdk::media::MediaRequestParameters {
        source,
        format: MediaFormat::File,
    };

    // cache=false ensures encrypted media is freshly decrypted.
    match client.media().get_media_content(&request, false).await {
        Ok(data) => {
            let tmp_dir = std::env::temp_dir().join("hikyaku-media");
            let _ = std::fs::create_dir_all(&tmp_dir);

            // Sanitize the filename: some clients set body to an mxc:// URL or
            // a bare name with no extension. Take only the last path component,
            // replace any remaining non-safe chars, then ensure there is an
            // extension by sniffing the downloaded bytes if needed.
            let safe_name = sanitize_media_filename(filename, &data);
            let path = tmp_dir.join(&safe_name);
            if let Err(e) = std::fs::write(&path, &data) {
                tracing::error!("Failed to write media file: {e}");
                return;
            }
            let _ = event_tx
                .send(MatrixEvent::MediaReady {
                    url: mxc_url.to_string(),
                    path: path.to_string_lossy().to_string(),
                })
                .await;
        }
        Err(e) => {
            tracing::error!("Failed to download media: {e}");
        }
    }
}

/// Return a safe local filename for a downloaded media file.
///
/// - Strips mxc:// prefixes and any path separators the body field might contain.
/// - Appends the correct extension derived from magic bytes if the name has none
///   or if the existing extension is wrong (e.g. a PNG sent with body "image").
fn sanitize_media_filename(filename: &str, data: &[u8]) -> String {
    // Take only the last path component and strip control chars / slashes.
    let base = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(filename);
    // Replace any remaining characters that are unsafe in a file path.
    let safe: String = base.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    // Strip leading dots/underscores that could produce hidden or ugly filenames.
    let safe = safe.trim_start_matches(['.', '_']).to_string();
    let safe = if safe.is_empty() { "media".to_string() } else { safe };

    // Sniff a more reliable extension from magic bytes.
    let sniffed_ext = sniff_extension(data);

    // Check whether the current extension (if any) matches the sniffed type.
    let current_ext = std::path::Path::new(&safe)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match (current_ext, sniffed_ext) {
        // Extension present and matches sniffed type — keep as-is.
        (Some(ce), Some(se)) if ce == se => safe,
        // No extension — append the sniffed one.
        (None, Some(se)) => format!("{safe}.{se}"),
        // Extension present but wrong (e.g. ".jpg" for a PNG) — replace it.
        (Some(_), Some(se)) => {
            let stem = std::path::Path::new(&safe)
                .file_stem().and_then(|s| s.to_str()).unwrap_or(&safe);
            format!("{stem}.{se}")
        }
        // Can't sniff (unknown format) — keep whatever name we have.
        (_, None) => safe,
    }
}

/// Derive a file extension from the first few magic bytes of the content.
/// Returns `None` for unknown or unrecognised formats.
fn sniff_extension(data: &[u8]) -> Option<&'static str> {
    match data {
        [0x89, b'P', b'N', b'G', ..] => Some("png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("jpg"),
        [b'G', b'I', b'F', b'8', ..] => Some("gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("webp"),
        // MP4 / MOV: "ftyp" box at offset 4.
        d if d.len() >= 12 && &d[4..8] == b"ftyp" => Some("mp4"),
        // WebM / MKV: EBML magic.
        [0x1A, 0x45, 0xDF, 0xA3, ..] => Some("webm"),
        // OGG (covers .ogv video and .oga audio).
        [b'O', b'g', b'g', b'S', ..] => Some("ogg"),
        // PDF (keep for completeness — opens via system app).
        [b'%', b'P', b'D', b'F', ..] => Some("pdf"),
        _ => None,
    }
}

async fn handle_send_message(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    body: &str,
    formatted_body: Option<&str>,
    reply_to: Option<&str>,
    quote_text: Option<&(String, String)>,
    is_emote: bool,
    mentioned_user_ids: &[String],
) {
    use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;

    let Ok(room_id) = RoomId::parse(room_id) else {
        tracing::error!("Invalid room ID: {room_id}");
        return;
    };

    let Some(room) = client.get_room(&room_id) else {
        tracing::error!("Room not found: {room_id}");
        return;
    };

    // Build the message content. Emotes (from /me or :) use m.emote.
    // For replies, send with m.relates_to — no manual quote fallback.
    let mut content = if is_emote {
        if let Some(html) = formatted_body {
            RoomMessageEventContent::emote_html(body, html)
        } else {
            RoomMessageEventContent::emote_plain(body)
        }
    } else if let Some(html) = formatted_body {
        RoomMessageEventContent::text_html(body, html)
    } else {
        RoomMessageEventContent::text_plain(body)
    };

    // Set m.mentions so the server can route push notifications to mentioned users.
    if !mentioned_user_ids.is_empty() {
        use matrix_sdk::ruma::events::Mentions;
        use matrix_sdk::ruma::OwnedUserId;
        let parsed: Vec<OwnedUserId> = mentioned_user_ids
            .iter()
            .filter_map(|uid| OwnedUserId::try_from(uid.as_str()).ok())
            .collect();
        if !parsed.is_empty() {
            content.mentions = Some(Mentions::with_user_ids(parsed));
        }
    }

    if let Some(reply_event_id) = reply_to {
        if let Ok(eid) = matrix_sdk::ruma::EventId::parse(reply_event_id) {
            content.relates_to = Some(
                matrix_sdk::ruma::events::room::message::Relation::Reply {
                    in_reply_to: matrix_sdk::ruma::events::relation::InReplyTo::new(eid.to_owned()),
                },
            );
        }
    }

    match room.send(content).await {
        Ok(response) => {
            let _ = event_tx.send(MatrixEvent::MessageSent {
                room_id: room_id.to_string(),
                echo_body: body.to_string(),
                event_id: response.event_id.to_string(),
            }).await;
        }
        Err(e) => {
            tracing::error!("Failed to send message to {room_id}: {e}");
        }
    }
}

async fn handle_send_reaction(client: &Client, room_id: &str, event_id: &str, emoji: &str) {
    use matrix_sdk::ruma::events::reaction::ReactionEventContent;
    use matrix_sdk::ruma::events::relation::Annotation;

    let Ok(room_id) = RoomId::parse(room_id) else {
        return;
    };
    let Ok(event_id) = matrix_sdk::ruma::EventId::parse(event_id) else {
        return;
    };
    let Some(room) = client.get_room(&room_id) else {
        return;
    };

    tracing::debug!("Sending reaction {emoji} to {event_id} in {room_id}");

    // Try to send the reaction. If the server returns M_DUPLICATE_ANNOTATION,
    // the user already reacted — find and redact to toggle off.
    let annotation = Annotation::new(event_id.to_owned(), emoji.to_string());
    let content = ReactionEventContent::new(annotation);
    match room.send(content).await {
        Ok(_) => {}
        Err(e) => {
            let err_str = e.to_string();
            if err_str.contains("M_DUPLICATE_ANNOTATION") || err_str.contains("same reaction") {
                // Already reacted — find our reaction event and redact it.
                tracing::debug!("Duplicate reaction, searching to redact...");
                if let Some(user_id) = client.user_id() {
                    // Use the room relations API or search recent events.
                    let mut opts = matrix_sdk::room::MessagesOptions::backward();
                    opts.limit = matrix_sdk::ruma::UInt::from(200u32);
                    let mut filter = matrix_sdk::ruma::api::client::filter::RoomEventFilter::default();
                    filter.types = Some(vec!["m.reaction".to_string()]);
                    filter.senders = Some(vec![user_id.to_owned()]);
                    opts.filter = filter;

                    if let Ok(resp) = room.messages(opts).await {
                        for ev in &resp.chunk {
                            let raw = ev.raw().json().get();
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                                let relates_to = v.get("content")
                                    .and_then(|c| c.get("m.relates_to"));
                                let matches = relates_to
                                    .and_then(|r| {
                                        let eid = r.get("event_id")?.as_str()?;
                                        let key = r.get("key")?.as_str()?;
                                        Some(eid == event_id.as_str() && key == emoji)
                                    })
                                    .unwrap_or(false);
                                if matches {
                                    if let Some(reaction_eid) = v.get("event_id").and_then(|e| e.as_str()) {
                                        if let Ok(rid) = matrix_sdk::ruma::EventId::parse(reaction_eid) {
                                            tracing::debug!("Redacting reaction {rid}");
                                            if let Err(e) = room.redact(&rid, None, None).await {
                                                tracing::error!("Failed to redact: {e}");
                                            }
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    tracing::warn!("Could not find reaction event to redact");
                }
            } else {
                tracing::error!("Failed to send reaction: {e}");
            }
        }
    }
}

async fn handle_browse_public_rooms(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    search_term: Option<&str>,
    spaces_only: bool,
    server: Option<&str>,
) {
    use matrix_sdk::ruma::api::client::directory::get_public_rooms_filtered;
    use matrix_sdk::ruma::directory::{Filter, RoomTypeFilter};

    let title = if spaces_only { "Browse Spaces" } else { "Browse Public Rooms" };

    let mut request = get_public_rooms_filtered::v3::Request::new();
    request.limit = Some(matrix_sdk::ruma::UInt::from(50u32));
    {
        let mut filter = Filter::new();
        if let Some(term) = search_term {
            if !term.is_empty() {
                filter.generic_search_term = Some(term.to_owned());
            }
        }
        if spaces_only {
            filter.room_types = vec![RoomTypeFilter::Space];
        }
        request.filter = filter;
    }
    if let Some(srv) = server {
        if !srv.is_empty() {
            if let Ok(name) = matrix_sdk::ruma::ServerName::parse(srv) {
                request.server = Some(name.to_owned());
            }
        }
    }

    tracing::info!("BrowsePublicRooms: spaces_only={spaces_only} server={server:?}");
    match client.send(request).await {
        Ok(response) => {
            tracing::info!("BrowsePublicRooms: got {} rooms", response.chunk.len());
            let joined_rooms: std::collections::HashSet<String> = client
                .joined_rooms()
                .iter()
                .map(|r| r.room_id().to_string())
                .collect();

            let rooms: Vec<SpaceDirectoryRoom> = response
                .chunk
                .into_iter()
                .map(|r| {
                    let alias = r.canonical_alias.as_ref().map(|a| a.to_string());
                    // Some buggy servers return room IDs without the :server component.
                    // Patch them by appending the directory server so via_servers is never empty.
                    let raw_id = r.room_id.to_string();
                    let room_id = if !raw_id.contains(':') {
                        if let Some(srv) = server {
                            format!("{}:{}", raw_id, srv)
                        } else {
                            raw_id
                        }
                    } else {
                        raw_id
                    };
                    // The via hint must be the room's own server, not the directory server.
                    // The room's server is in the room_id or canonical_alias.  Fall back to
                    // the directory server only if neither has a server component.
                    let via_server = room_id.splitn(2, ':').nth(1).map(|s| s.to_string())
                        .or_else(|| alias.as_ref().and_then(|a| a.splitn(2, ':').nth(1).map(|s| s.to_string())))
                        .or_else(|| server.map(|s| s.to_string()));
                    SpaceDirectoryRoom {
                        already_joined: joined_rooms.contains(&room_id),
                        room_id,
                        name: r.name.clone().unwrap_or_else(|| {
                            alias.clone().unwrap_or_else(|| r.room_id.to_string())
                        }),
                        canonical_alias: alias,
                        topic: r.topic.unwrap_or_default(),
                        member_count: r.num_joined_members.into(),
                        via_server,
                    }
                })
                .collect();

            if spaces_only {
                let _ = event_tx
                    .send(MatrixEvent::PublicSpacesForServer {
                        server: server.unwrap_or("").to_string(),
                        rooms,
                    })
                    .await;
            } else {
                let _ = event_tx
                    .send(MatrixEvent::PublicRoomDirectory { title: title.to_string(), rooms })
                    .await;
            }
        }
        Err(e) => {
            tracing::error!("Failed to fetch public rooms: {e}");
        }
    }
}

async fn handle_browse_space(client: &Client, event_tx: &Sender<MatrixEvent>, space_id: &str) {
    use matrix_sdk::ruma::api::client::space::get_hierarchy;

    let Ok(room_id) = RoomId::parse(space_id) else {
        return;
    };

    let request = get_hierarchy::v1::Request::new(room_id.to_owned());
    match client.send(request).await {
        Ok(response) => {
            let joined_rooms: std::collections::HashSet<String> = client
                .joined_rooms()
                .iter()
                .map(|r| r.room_id().to_string())
                .collect();

            // Fallback server: use the space's own server for rooms that come
            // back with no server component in their room_id (buggy servers).
            let space_server = room_id.server_name().map(|s| s.to_string());

            let rooms: Vec<SpaceDirectoryRoom> = response
                .rooms
                .into_iter()
                .filter(|r| r.room_id != room_id) // Skip the space itself
                .map(|r| {
                    let alias = r.canonical_alias.as_ref().map(|a| a.to_string());
                    // Patch malformed room IDs (no :server part) using the space server.
                    let raw_rid = r.room_id.to_string();
                    let room_id_str = if raw_rid.contains(':') {
                        raw_rid
                    } else if let Some(srv) = &space_server {
                        format!("{}:{}", raw_rid, srv)
                    } else {
                        raw_rid
                    };
                    let rid_server = room_id_str.splitn(2, ':').nth(1).map(|s| s.to_string())
                        .or_else(|| alias.as_ref().and_then(|a| a.splitn(2, ':').nth(1).map(|s| s.to_string())));
                    SpaceDirectoryRoom {
                        already_joined: joined_rooms.contains(&room_id_str),
                        room_id: room_id_str,
                        name: r.name.clone().unwrap_or_else(|| {
                            alias.clone().unwrap_or_else(|| r.room_id.to_string())
                        }),
                        canonical_alias: alias,
                        topic: r.topic.unwrap_or_default(),
                        member_count: r.num_joined_members.into(),
                        via_server: rid_server,
                    }
                })
                .collect();

            let _ = event_tx
                .send(MatrixEvent::SpaceDirectory {
                    space_id: space_id.to_string(),
                    rooms,
                })
                .await;
        }
        Err(e) => {
            tracing::error!("Failed to fetch space hierarchy for {space_id}: {e}");
        }
    }
}

async fn handle_join_room(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id_or_alias: &str,
    extra_via: &[String],
) {
    use matrix_sdk::ruma::{RoomAliasId, RoomOrAliasId, ServerName};

    let Ok(id) = RoomOrAliasId::parse(room_id_or_alias) else {
        let _ = event_tx
            .send(MatrixEvent::JoinFailed {
                error: format!("Invalid room ID or alias: {room_id_or_alias}"),
            })
            .await;
        return;
    };

    // If the input is a room alias, resolve it first to get the live list of
    // servers currently participating in the room.  This is much more reliable
    // than guessing via servers from the room ID alone — the resolution
    // response always includes currently-active federation servers.
    let via: Vec<matrix_sdk::ruma::OwnedServerName> = if id.is_room_alias_id() {
        if let Ok(alias) = RoomAliasId::parse(room_id_or_alias) {
            match client.resolve_room_alias(&alias).await {
                Ok(resolved) => {
                    tracing::debug!(
                        "Resolved alias {} → {} via {:?}",
                        alias, resolved.room_id, resolved.servers
                    );
                    resolved.servers.into_iter().take(3).collect()
                }
                Err(e) => {
                    tracing::warn!("Alias resolution failed for {alias}: {e}, falling back to caller-supplied via hints");
                    extra_via.iter()
                        .filter_map(|s| ServerName::parse(s.as_str()).ok().map(|n| n.to_owned()))
                        .collect()
                }
            }
        } else {
            vec![]
        }
    } else {
        // Room ID: build via from the embedded server + any caller-supplied hints.
        let mut v: Vec<matrix_sdk::ruma::OwnedServerName> = room_id_or_alias
            .splitn(2, ':')
            .nth(1)
            .and_then(|s| ServerName::parse(s).ok().map(|n| n.to_owned()))
            .into_iter()
            .collect();
        for s in extra_via {
            if let Ok(name) = ServerName::parse(s.as_str()) {
                let owned = name.to_owned();
                if !v.contains(&owned) {
                    v.push(owned);
                }
            }
        }
        v
    };

    tracing::warn!("Joining '{}' via {:?}", room_id_or_alias, via);

    match client.join_room_by_id_or_alias(&id, &via).await {
        Ok(room) => {
            let room_id = room.room_id().to_string();
            // display_name() fetches from server if not yet cached — use it
            // so the toast shows the human-readable name, not the room ID.
            let room_name = room
                .display_name()
                .await
                .map(|n| n.to_string())
                .unwrap_or_else(|_| {
                    room.cached_display_name()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| room_id_or_alias.to_string())
                });
            tracing::info!("Joined room: {room_name} ({room_id})");
            let _ = event_tx
                .send(MatrixEvent::RoomJoined { room_id, room_name })
                .await;
        }
        Err(e) => {
            tracing::error!("Failed to join {room_id_or_alias}: {e}");
            let _ = event_tx
                .send(MatrixEvent::JoinFailed {
                    error: e.to_string(),
                })
                .await;
        }
    }
}

async fn handle_invite_user(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    user_id: &str,
) {
    let Ok(room_id) = RoomId::parse(room_id) else {
        let _ = event_tx.send(MatrixEvent::InviteFailed {
            error: format!("Invalid room ID: {room_id}"),
        }).await;
        return;
    };
    let Ok(user_id) = UserId::parse(user_id) else {
        let _ = event_tx.send(MatrixEvent::InviteFailed {
            error: format!("Invalid user ID: {user_id}"),
        }).await;
        return;
    };
    let Some(room) = client.get_room(&room_id) else {
        let _ = event_tx.send(MatrixEvent::InviteFailed {
            error: "Room not found".to_string(),
        }).await;
        return;
    };
    match room.invite_user_by_id(&user_id).await {
        Ok(()) => {
            tracing::info!("Invited {user_id} to {room_id}");
            let _ = event_tx.send(MatrixEvent::InviteSuccess {
                user_id: user_id.to_string(),
            }).await;
        }
        Err(e) => {
            tracing::error!("Failed to invite {user_id} to {room_id}: {e}");
            let _ = event_tx.send(MatrixEvent::InviteFailed {
                error: e.to_string(),
            }).await;
        }
    }
}

/// Search the homeserver's user directory by display name or Matrix ID prefix.
/// Sends `UserSearchResults` with up to 20 matching (display_name, user_id) pairs.
async fn handle_search_users(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    query: &str,
) {
    use matrix_sdk::ruma::api::client::user_directory::search_users::v3::Request as SearchRequest;
    let mut req = SearchRequest::new(query.to_owned());
    req.limit = 20u32.into();
    match client.send(req).await {
        Ok(resp) => {
            let results: Vec<(String, String)> = resp.results
                .iter()
                .map(|u| (
                    u.display_name.clone().unwrap_or_else(|| u.user_id.localpart().to_string()),
                    u.user_id.to_string(),
                ))
                .collect();
            tracing::debug!("SearchUsers \"{query}\": {} results", results.len());
            let _ = event_tx.send(MatrixEvent::UserSearchResults { results }).await;
        }
        Err(e) => {
            tracing::warn!("SearchUsers \"{query}\" failed: {e}");
            let _ = event_tx.send(MatrixEvent::UserSearchResults { results: vec![] }).await;
        }
    }
}

async fn handle_accept_invite(client: &Client, event_tx: &Sender<MatrixEvent>, room_id: &str) {
    let Ok(parsed_id) = RoomId::parse(room_id) else { return };
    let Some(room) = client.get_room(&parsed_id) else { return };
    match room.join().await {
        Ok(()) => {
            tracing::info!("Accepted invite to {room_id}");
            let room_name = room.display_name().await
                .map(|n| n.to_string())
                .unwrap_or_else(|_| room_id.to_string());
            let _ = event_tx.send(MatrixEvent::RoomJoined {
                room_id: room_id.to_string(),
                room_name,
            }).await;
        }
        Err(e) => {
            tracing::error!("Failed to accept invite to {room_id}: {e}");
            let _ = event_tx.send(MatrixEvent::JoinFailed {
                error: format!("{e}"),
            }).await;
        }
    }
}

async fn handle_decline_invite(client: &Client, event_tx: &Sender<MatrixEvent>, room_id: &str) {
    let Ok(parsed_id) = RoomId::parse(room_id) else { return };
    let Some(room) = client.get_room(&parsed_id) else { return };
    match room.leave().await {
        Ok(()) => {
            tracing::info!("Declined invite to {room_id}");
            let _ = event_tx.send(MatrixEvent::RoomLeft {
                room_id: room_id.to_string(),
            }).await;
        }
        Err(e) => tracing::error!("Failed to decline invite to {room_id}: {e}"),
    }
}

async fn handle_leave_room(client: &Client, event_tx: &Sender<MatrixEvent>, room_id: &str) {
    let Ok(room_id) = RoomId::parse(room_id) else {
        let _ = event_tx
            .send(MatrixEvent::LeaveFailed {
                error: format!("Invalid room ID: {room_id}"),
            })
            .await;
        return;
    };

    let Some(room) = client.get_room(&room_id) else {
        let _ = event_tx
            .send(MatrixEvent::LeaveFailed {
                error: "Room not found".to_string(),
            })
            .await;
        return;
    };

    match room.leave().await {
        Ok(()) => {
            tracing::info!("Left room: {room_id}");
            let _ = event_tx
                .send(MatrixEvent::RoomLeft {
                    room_id: room_id.to_string(),
                })
                .await;
        }
        Err(e) => {
            tracing::error!("Failed to leave {room_id}: {e}");
            let _ = event_tx
                .send(MatrixEvent::LeaveFailed {
                    error: e.to_string(),
                })
                .await;
        }
    }
}

/// Send a read receipt for the latest event in a room.
/// Fetch thread replies for a root event.
async fn handle_fetch_thread(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    thread_root_id: &str,
) {
    let Ok(rid) = RoomId::parse(room_id) else { return };
    let Ok(root_eid) = matrix_sdk::ruma::EventId::parse(thread_root_id) else { return };
    let Some(room) = client.get_room(&rid) else { return };

    // Fetch the root message first.
    let root_message = match room.event(&root_eid, None).await {
        Ok(ev) => {
            let chunk = vec![ev];
            let mut msgs = extract_messages(&room, &chunk, false, client.user_id()).await;
            msgs.pop()
        }
        Err(e) => {
            tracing::warn!("Failed to fetch thread root {thread_root_id}: {e}");
            None
        }
    };

    // Fetch thread replies via the relations API.
    use matrix_sdk::ruma::api::client::relations::get_relating_events_with_rel_type::v1::Request;
    use matrix_sdk::ruma::events::relation::RelationType;

    let mut request = Request::new(rid.clone(), root_eid.to_owned(), RelationType::Thread);
    request.limit = Some(matrix_sdk::ruma::UInt::from(50u32));

    let replies = match client.send(request).await {
        Ok(response) => {
            // Parse the raw events into MessageInfo.
            let mut msgs = Vec::new();
            for raw_event in &response.chunk {
                if let Ok(ev) = raw_event.deserialize() {
                    // Extract sender, body, timestamp from the event.
                    if let matrix_sdk::ruma::events::AnyMessageLikeEvent::RoomMessage(
                        matrix_sdk::ruma::events::MessageLikeEvent::Original(msg),
                    ) = ev
                    {
                        let Some((body, formatted_body, media)) = extract_message_content(&msg.content.msgtype) else {
                            continue;
                        };
                        let sender_id = msg.sender.to_string();
                        let display_name = resolve_display_name(&room, &msg.sender).await;
                        msgs.push(MessageInfo {
                            sender: display_name,
                            sender_id,
                            body,
                            formatted_body,
                            timestamp: msg.origin_server_ts.as_secs().into(),
                            event_id: msg.event_id.to_string(),
                            reply_to: None,
                            reply_to_sender: None,
                            thread_root: Some(thread_root_id.to_string()),
                            reactions: Vec::new(),
                            media,
                            is_highlight: false,
                            is_system_event: false,
                        });
                    }
                }
            }
            // Sort chronologically (oldest first).
            msgs.sort_by_key(|m| m.timestamp);
            msgs
        }
        Err(e) => {
            tracing::error!("Failed to fetch thread replies for {thread_root_id}: {e}");
            Vec::new()
        }
    };

    tracing::info!(
        "Fetched thread for {thread_root_id}: root={}, {} replies",
        root_message.is_some(),
        replies.len()
    );

    let _ = event_tx.send(MatrixEvent::ThreadReplies {
        room_id: room_id.to_string(),
        thread_root_id: thread_root_id.to_string(),
        root_message,
        replies,
    }).await;
}

/// Fetch event context for a seek-to-event jump.
async fn handle_seek_to_event(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    event_id: &str,
) {
    use matrix_sdk::ruma::api::client::context::get_context;
    use matrix_sdk::ruma::EventId;

    let Ok(rid) = RoomId::parse(room_id) else { return };
    let Ok(eid) = EventId::parse(event_id) else { return };
    let Some(room) = client.get_room(&rid) else { return };

    let mut request = get_context::v3::Request::new(rid.to_owned(), eid.to_owned());
    request.limit = matrix_sdk::ruma::UInt::from(40u32);

    match client.send(request).await {
        Ok(response) => {
            // events_before is reverse-chronological; reverse to get chronological order.
            // Convert Raw<AnyTimelineEvent> → Raw<AnySyncTimelineEvent> via cast() (same JSON).
            let mut all_raw: Vec<matrix_sdk::deserialized_responses::TimelineEvent> =
                response.events_before
                    .into_iter()
                    .rev()
                    .map(|r| matrix_sdk::deserialized_responses::TimelineEvent::new(r.cast()))
                    .collect();

            if let Some(evt) = response.event {
                all_raw.push(matrix_sdk::deserialized_responses::TimelineEvent::new(evt.cast()));
            }
            all_raw.extend(
                response.events_after
                    .into_iter()
                    .map(|r| matrix_sdk::deserialized_responses::TimelineEvent::new(r.cast())),
            );

            let messages = extract_messages(&room, &all_raw, false, client.user_id()).await;

            tracing::info!(
                "SeekToEvent {event_id}: fetched {} context events → {} messages",
                all_raw.len(),
                messages.len()
            );

            let _ = event_tx.send(MatrixEvent::SeekResult {
                room_id: room_id.to_string(),
                target_event_id: event_id.to_string(),
                messages,
                before_token: response.start,
            }).await;
        }
        Err(e) => {
            tracing::error!("SeekToEvent failed for {event_id}: {e}");
        }
    }
}

/// Find an existing DM room with a user, or create a new one.
async fn handle_create_dm(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    user_id: &str,
) {
    let Ok(target_uid) = <&matrix_sdk::ruma::UserId>::try_from(user_id) else {
        let _ = event_tx.send(MatrixEvent::DmFailed {
            error: format!("Invalid user ID: {user_id}"),
        }).await;
        return;
    };

    // Check for an existing DM room with this user.
    for room in client.joined_rooms() {
        if !room.is_direct().await.unwrap_or(false) {
            continue;
        }
        // Check if the target user is a member of this DM room.
        if let Ok(Some(_member)) = room.get_member_no_sync(target_uid).await {
            let room_id = room.room_id().to_string();
            let name = room.display_name().await
                .ok()
                .map(|n| n.to_string())
                .unwrap_or_else(|| user_id.to_string());
            tracing::info!("Found existing DM with {user_id}: {room_id}");
            let _ = event_tx.send(MatrixEvent::DmReady {
                user_id: user_id.to_string(),
                room_id,
                room_name: name,
            }).await;
            return;
        }
    }

    // Verify the user exists on their homeserver before creating a room.
    // A Matrix homeserver will happily create a room and invite a non-existent
    // user — the invite just silently hangs. Fetching the profile first gives
    // a clear error message instead.
    {
        use matrix_sdk::ruma::api::client::profile::get_profile::v3::Request as ProfileRequest;
        let profile_req = ProfileRequest::new(target_uid.to_owned());
        if let Err(e) = client.send(profile_req).await {
            tracing::warn!("User {user_id} not found: {e}");
            let _ = event_tx.send(MatrixEvent::DmFailed {
                error: format!("User {user_id} not found — check the ID and try again"),
            }).await;
            return;
        }
    }

    // No existing DM — create one with encryption enabled.
    tracing::info!("Creating new encrypted DM room with {user_id}");
    use matrix_sdk::ruma::api::client::room::create_room::v3::Request as CreateRoomRequest;
    use matrix_sdk::ruma::events::room::encryption::RoomEncryptionEventContent;
    use matrix_sdk::ruma::events::InitialStateEvent;

    let enc_event = InitialStateEvent::new(RoomEncryptionEventContent::with_recommended_defaults())
        .to_raw_any();

    let mut request = CreateRoomRequest::new();
    request.is_direct = true;
    request.invite = vec![target_uid.to_owned()];
    request.preset = Some(matrix_sdk::ruma::api::client::room::create_room::v3::RoomPreset::TrustedPrivateChat);
    request.initial_state = vec![enc_event];

    match client.create_room(request).await {
        Ok(response) => {
            let room_id = response.room_id().to_string();
            let name = user_id.to_string();
            tracing::info!("Created encrypted DM room {room_id} with {user_id}");
            let _ = event_tx.send(MatrixEvent::DmReady {
                user_id: user_id.to_string(),
                room_id,
                room_name: name,
            }).await;
        }
        Err(e) => {
            tracing::error!("Failed to create DM with {user_id}: {e}");
            let _ = event_tx.send(MatrixEvent::DmFailed {
                error: e.to_string(),
            }).await;
        }
    }
}

async fn handle_mark_read(client: &Client, room_id: &str, cached_event_id: Option<String>) {
    let Ok(rid) = RoomId::parse(room_id) else {
        tracing::warn!("handle_mark_read: invalid room_id {room_id}");
        return;
    };
    let Some(room) = client.get_room(&rid) else {
        tracing::warn!("handle_mark_read: room not found {room_id}");
        return;
    };

    let unread_before = room.unread_notification_counts().notification_count;

    // Prefer the SDK's latest_event (populated by the sync handler) — it reflects
    // any messages that arrived via sync AFTER the pagination bg_refresh ran.
    // Fall back to the pagination cache for rooms where sync hasn't delivered
    // a new event yet (e.g. a room opened from disk cache with no new activity).
    let eid = if let Some(ev) = room.latest_event().and_then(|e| e.event_id().map(|id| id.to_owned())) {
        ev
    } else {
        match cached_event_id {
            Some(eid_str) => {
                match matrix_sdk::ruma::OwnedEventId::try_from(eid_str.as_str()) {
                    Ok(eid) => eid,
                    Err(e) => {
                        tracing::warn!("handle_mark_read: bad cached event_id '{eid_str}': {e}");
                        return;
                    }
                }
            }
            None => {
                tracing::warn!("handle_mark_read: no event_id available for {room_id}");
                return;
            }
        }
    };

    tracing::info!(
        "handle_mark_read: sending receipt for {room_id}, event={eid}, unread_before={unread_before}"
    );

    use matrix_sdk::room::Receipts;
    let receipts = Receipts::new()
        .fully_read_marker(eid.clone())
        .public_read_receipt(eid);

    match room.send_multiple_receipts(receipts).await {
        Ok(_) => {
            let unread_after = room.unread_notification_counts().notification_count;
            tracing::info!(
                "handle_mark_read: receipt sent for {room_id}, unread_after={unread_after}"
            );
        }
        Err(e) => tracing::warn!("handle_mark_read: failed for {room_id}: {e}"),
    }
}

/// Export messages for `room_id` from the SQLite cache to `path` as JSONL.
/// Each line: {"sender":"…","sender_id":"@…","body":"…"}
/// Safe: reads only from the local cache, never calls room.messages().
async fn handle_export_messages(
    _client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    path: &std::path::Path,
    timeline_cache: &super::room_cache::RoomCache,
) {
    use std::io::Write as _;

    // Read from disk cache on a blocking thread.
    let cache = timeline_cache.clone();
    let rid = room_id.to_string();
    let disk_data = tokio::task::spawn_blocking(move || {
        cache.load_disk(&rid)
    }).await.ok().flatten();

    // Fall back to in-memory cache if disk has nothing.
    let msgs = match disk_data {
        Some((msgs, _token)) => msgs,
        None => {
            match timeline_cache.get_memory(room_id) {
                Some((msgs, _, _)) => msgs,
                None => {
                    let _ = event_tx.send(MatrixEvent::MessagesExportFailed {
                        error: "No cached messages for this room — open the room first to load them.".into(),
                    }).await;
                    return;
                }
            }
        }
    };

    let mut file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => {
            let _ = event_tx.send(MatrixEvent::MessagesExportFailed {
                error: format!("Cannot write to {}: {e}", path.display()),
            }).await;
            return;
        }
    };

    let mut count = 0usize;
    for msg in &msgs {
        if msg.body.is_empty() || msg.is_system_event { continue; }
        let line = serde_json::json!({
            "sender":    msg.sender,
            "sender_id": msg.sender_id,
            "body":      msg.body,
        });
        if writeln!(file, "{line}").is_err() { break; }
        count += 1;
    }

    let _ = event_tx.send(MatrixEvent::MessagesExported {
        path: path.display().to_string(),
        count,
    }).await;
}

async fn handle_export_room_metrics(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id_str: &str,
    days: u32,
) {
    use std::io::Write;

    let cutoff_ms = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        now.saturating_sub(days as u64 * 86_400_000)
    };

    let room_id = match RoomId::parse(room_id_str) {
        Ok(id) => id,
        Err(e) => {
            let _ = event_tx.send(MatrixEvent::MetricsFailed { error: e.to_string() }).await;
            return;
        }
    };

    let room = match client.get_room(&room_id) {
        Some(r) => r,
        None => {
            let _ = event_tx.send(MatrixEvent::MetricsFailed { error: "Room not found".into() }).await;
            return;
        }
    };

    let room_name = room.cached_display_name()
        .map(|n| n.to_string())
        .unwrap_or_else(|| room_id_str.to_string());

    tracing::info!("Exporting metrics for {room_name} — last {days} days");

    struct MemberEvent {
        ts_ms: u64,
        event_type: String,
        sender: String,
        target: String,
        reason: String,
    }
    let mut member_events: Vec<MemberEvent> = Vec::new();
    let mut message_counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut from_token: Option<String> = None;
    let mut done = false;
    let mut pages = 0u32;

    while !done && pages < 200 {
        pages += 1;
        let mut opts = matrix_sdk::room::MessagesOptions::backward();
        opts.limit = matrix_sdk::ruma::UInt::from(100u32);
        if let Some(ref tok) = from_token {
            opts.from = Some(tok.to_string());
        }
        let result = match room.messages(opts).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Metrics pagination error: {e}");
                break;
            }
        };

        let next_token = result.end.clone();

        for event in &result.chunk {
            // Access raw JSON to extract fields.
            let raw_json = event.raw().json().get();
            let v = match serde_json::from_str::<serde_json::Value>(raw_json) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let ts_ms = v.get("origin_server_ts").and_then(|t| t.as_u64()).unwrap_or(0);

            if ts_ms > 0 && ts_ms < cutoff_ms {
                done = true;
                break;
            }

            let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let sender = v.get("sender").and_then(|s| s.as_str()).unwrap_or("").to_string();

            match event_type.as_str() {
                "m.room.member" => {
                    let content = v.get("content").cloned().unwrap_or(serde_json::Value::Null);
                    let membership = content.get("membership")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let reason = content.get("reason")
                        .and_then(|r| r.as_str())
                        .unwrap_or("")
                        .to_string();
                    let target = v.get("state_key")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();

                    let ev_type = if membership == "leave" && sender != target {
                        "kick".to_string()
                    } else {
                        membership
                    };

                    member_events.push(MemberEvent {
                        ts_ms,
                        event_type: ev_type,
                        sender,
                        target,
                        reason,
                    });
                }
                "m.room.message" => {
                    *message_counts.entry(sender).or_insert(0) += 1;
                }
                _ => {}
            }
        }

        from_token = next_token;
        if from_token.is_none() {
            break;
        }
    }

    tracing::info!("Collected {} member events, {} active senders over {pages} pages",
        member_events.len(), message_counts.len());

    let out_dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let safe_name: String = room_name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    let date_str = format_unix_date(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    let csv_path = out_dir.join(format!("{safe_name}_metrics_{date_str}.csv"));

    let mut file = match std::fs::File::create(&csv_path) {
        Ok(f) => f,
        Err(e) => {
            let _ = event_tx.send(MatrixEvent::MetricsFailed { error: e.to_string() }).await;
            return;
        }
    };

    let _ = writeln!(file, "# Membership Events (last {days} days)");
    let _ = writeln!(file, "timestamp_ms,date,event_type,sender,target,reason");
    for ev in &member_events {
        let date = format_unix_date(ev.ts_ms / 1000);
        let reason = ev.reason.replace(',', ";").replace('\n', " ");
        let _ = writeln!(file, "{},{},{},{},{},{}",
            ev.ts_ms, date, ev.event_type, ev.sender, ev.target, reason);
    }

    let _ = writeln!(file, "\n# Message Counts by User (last {days} days)");
    let _ = writeln!(file, "user_id,message_count");
    let mut counts_sorted: Vec<_> = message_counts.iter().collect();
    counts_sorted.sort_by(|a, b| b.1.cmp(a.1));
    for (user, count) in &counts_sorted {
        let _ = writeln!(file, "{user},{count}");
    }

    let bans = member_events.iter().filter(|e| e.event_type == "ban").count();
    let kicks = member_events.iter().filter(|e| e.event_type == "kick").count();
    let joins = member_events.iter().filter(|e| e.event_type == "join").count();
    let leaves = member_events.iter().filter(|e| e.event_type == "leave").count();
    let _ = writeln!(file, "\n# Summary");
    let _ = writeln!(file, "metric,value");
    let _ = writeln!(file, "period_days,{days}");
    let _ = writeln!(file, "bans,{bans}");
    let _ = writeln!(file, "kicks,{kicks}");
    let _ = writeln!(file, "joins,{joins}");
    let _ = writeln!(file, "leaves,{leaves}");
    let _ = writeln!(file, "total_messages,{}", message_counts.values().sum::<u64>());
    let _ = writeln!(file, "active_users,{}", message_counts.len());

    let event_count = member_events.len() + message_counts.len();
    let path_str = csv_path.to_string_lossy().to_string();

    let metrics_text = format!(
        "Room: {room_name}\nPeriod: last {days} days\nBans: {bans}\nKicks: {kicks}\nJoins: {joins}\nLeaves: {leaves}\nTotal messages: {}\nActive users: {}\n\nTop 10 message senders:\n{}",
        message_counts.values().sum::<u64>(),
        message_counts.len(),
        counts_sorted.iter().take(10)
            .map(|(u, c)| format!("  {u}: {c}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let _ = event_tx.send(MatrixEvent::MetricsReady {
        path: path_str,
        event_count,
        metrics_text,
    }).await;
}

fn format_unix_date(unix_secs: u64) -> String {
    let days = unix_secs / 86400;
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Fetch messages from a room for the hover-preview AI summary.
/// Does NOT send any read receipt — this is a silent peek.
/// If `unread_count` > 0, only the most recent `unread_count` messages are
/// summarised (the ones the user hasn't seen); otherwise all fetched messages
/// are used as a general "what's happening" summary.
async fn handle_fetch_room_preview(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    unread_count: u32,
    ollama_endpoint: &str,
    ollama_model: &str,
    extra_instructions: &str,
) {
    use matrix_sdk::ruma::api::client::filter::RoomEventFilter;
    use matrix_sdk::ruma::UInt;

    let Ok(rid) = RoomId::parse(room_id) else { return };
    let Some(room) = client.get_room(&rid) else { return };

    // For unread rooms, fetch the unread window plus some prior context so the
    // LLM can understand references. Context messages are sent in a separate
    // section of the prompt so the model knows which ones are new.
    const CONTEXT_SIZE: u32 = 10;
    let unread_cap = unread_count.min(40);
    let fetch_limit = if unread_count > 0 { unread_cap + CONTEXT_SIZE } else { 20 };

    let mut msg_filter = RoomEventFilter::default();
    msg_filter.types = Some(vec![
        "m.room.message".to_string(),
        "m.room.encrypted".to_string(),
    ]);
    let mut options = matrix_sdk::room::MessagesOptions::backward();
    options.limit = UInt::from(fetch_limit);
    options.filter = msg_filter;

    let Ok(response) = room.messages(options).await else {
        tracing::warn!("FetchRoomPreview: room.messages() failed for {room_id}");
        let _ = event_tx.send(MatrixEvent::RoomPreview { room_id: room_id.to_string(), messages_text: String::new(), is_unread: false }).await;
        return;
    };

    // Format as plain text "sender: body" lines, oldest first.
    // Handles both plaintext rooms (AnySyncTimelineEvent via .raw())
    // and encrypted rooms (AnyTimelineEvent via TimelineEventKind::Decrypted).
    let mut lines: Vec<String> = Vec::new();
    for ev in response.chunk.iter().rev() {
        use matrix_sdk::deserialized_responses::TimelineEventKind;
        use matrix_sdk::ruma::events::room::message::MessageType;
        use matrix_sdk::ruma::events::{
            AnyMessageLikeEvent, AnyTimelineEvent,
            AnySyncMessageLikeEvent, AnySyncTimelineEvent,
            MessageLikeEvent, SyncMessageLikeEvent,
        };

        let (sender, body, ts_ms) = match &ev.kind {
            TimelineEventKind::Decrypted(d) => {
                let Ok(any) = d.event.deserialize() else { continue };
                match any {
                    AnyMessageLikeEvent::RoomMessage(
                        MessageLikeEvent::Original(msg)
                    ) => match msg.content.msgtype {
                        MessageType::Text(t) => {
                            let ts = msg.origin_server_ts.as_secs().into();
                            (msg.sender.to_string(), t.body, ts)
                        }
                        _ => continue,
                    },
                    _ => continue,
                }
            }
            TimelineEventKind::PlainText { .. } => {
                let Ok(any) = ev.raw().deserialize() else { continue };
                match any {
                    AnySyncTimelineEvent::MessageLike(
                        AnySyncMessageLikeEvent::RoomMessage(
                            SyncMessageLikeEvent::Original(msg)
                        )
                    ) => match msg.content.msgtype {
                        MessageType::Text(t) => {
                            let ts = msg.origin_server_ts.as_secs().into();
                            (msg.sender.to_string(), t.body, ts)
                        }
                        _ => continue,
                    },
                    _ => continue,
                }
            }
            TimelineEventKind::UnableToDecrypt { .. } => continue,
        };

        let sender_short = sender.trim_start_matches('@')
            .split(':').next().unwrap_or(&sender);
        // Strip Matrix reply fallback lines ("> quoted text") so the LLM
        // only sees the actual message, not the re-quoted context.
        let clean_body = strip_reply_fallback_simple(&body);
        if clean_body.is_empty() { continue; }
        let ts_str = format_unix_ts(ts_ms);
        lines.push(format!("[{ts_str}] {sender_short}: {clean_body}"));
    }

    if ollama_endpoint.is_empty() || ollama_model.is_empty() {
        tracing::debug!("FetchRoomPreview: Ollama not configured, skipping");
        let _ = event_tx.send(MatrixEvent::OllamaChunk {
            context: format!("preview:{room_id}"),
            chunk: String::new(),
            done: true,
        }).await;
        return;
    }
    if lines.is_empty() {
        tracing::debug!("FetchRoomPreview: no readable messages for {room_id}");
        let _ = event_tx.send(MatrixEvent::OllamaChunk {
            context: format!("preview:{room_id}"),
            chunk: "\u{26a0} No readable messages found (room may be encrypted without keys)".to_string(),
            done: true,
        }).await;
        return;
    }

    let is_unread = unread_count > 0;

    // Split into context (already seen) and new (unread) windows.
    // For non-unread views all lines are treated as the conversation body.
    let prompt = if is_unread && lines.len() > unread_cap as usize {
        let split = lines.len() - unread_cap as usize;
        let context_text = lines[..split].join("\n");
        let new_text = lines[split..].join("\n");
        let extra = if extra_instructions.is_empty() {
            String::new()
        } else {
            format!("\n\nAdditional instructions: {extra_instructions}")
        };
        format!(
            "You are a helpful assistant analyzing a Matrix room conversation.\n\
             Each message is prefixed with its exact timestamp in [YYYY-MM-DD HH:MM UTC] format. \
             Use only those timestamps when referring to timing — do not guess or infer any dates \
             not present in the conversation.\n\
             \n\
             === Prior conversation (context — already read) ===\n\
             {context_text}\n\
             \n\
             === New messages (unread) ===\n\
             {new_text}\n\
             \n\
             Provide a concise summary (2-4 bullet points) of the NEW messages above. For each \
             bullet, include who sent the message and what they said or decided. Use the prior \
             context only to explain references — do not summarize it separately.{extra}"
        )
    } else {
        // No unread split — summarize the full window.
        let messages_text = lines.join("\n");
        let extra = if extra_instructions.is_empty() {
            String::new()
        } else {
            format!("\n\nAdditional instructions: {extra_instructions}")
        };
        format!(
            "You are a helpful assistant. Summarize the following Matrix room conversation \
             in 2-4 bullet points. Focus on topics, decisions, and key contributors. Be concise.\n\
             Each message is prefixed with its exact timestamp in [YYYY-MM-DD HH:MM UTC] format. \
             Use only those timestamps when referring to timing — do not guess or infer any dates \
             not present in the conversation.{extra}\n\nConversation:\n{messages_text}"
        )
    };

    tracing::info!(
        "RoomPreview: calling Ollama for {room_id} ({} msgs, {} prompt chars)",
        lines.len(), prompt.len()
    );
    ollama_stream_to_event(
        ollama_endpoint, ollama_model, &prompt,
        &format!("preview:{room_id}"),
        event_tx,
    ).await;
}

/// Fetch a member avatar and cache it to disk. Sends AvatarReady on success.
/// Skips the download if a cached file already exists.
async fn handle_fetch_avatar(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    user_id: &str,
    mxc_url: &str,
) {
    if mxc_url.is_empty() { return; }

    // Persistent cache: ~/.local/share/hikyaku/avatars/<hash>.jpg
    let cache_dir = glib::user_data_dir()
        .join("hikyaku")
        .join("avatars");
    let _ = std::fs::create_dir_all(&cache_dir);

    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    mxc_url.hash(&mut hasher);
    let cache_path = cache_dir.join(format!("{:x}.jpg", hasher.finish()));

    // Return cached file immediately without hitting the network.
    if cache_path.exists() {
        let _ = event_tx.send(MatrixEvent::AvatarReady {
            user_id: user_id.to_string(),
            path: cache_path.to_string_lossy().to_string(),
        }).await;
        return;
    }

    let Ok(uri) = <&matrix_sdk::ruma::MxcUri>::try_from(mxc_url) else {
        tracing::warn!("FetchAvatar: invalid mxc URL: {mxc_url}");
        return;
    };

    use matrix_sdk::media::{MediaFormat, MediaRequestParameters, MediaThumbnailSettings};
    use matrix_sdk::ruma::UInt;
    let request = MediaRequestParameters {
        source: matrix_sdk::ruma::events::room::MediaSource::Plain(uri.to_owned()),
        format: MediaFormat::Thumbnail(MediaThumbnailSettings::new(
            UInt::from(64u32),
            UInt::from(64u32),
        )),
    };

    match client.media().get_media_content(&request, true).await {
        Ok(data) => {
            if let Err(e) = std::fs::write(&cache_path, &data) {
                tracing::warn!("FetchAvatar: write failed: {e}");
                return;
            }
            let _ = event_tx.send(MatrixEvent::AvatarReady {
                user_id: user_id.to_string(),
                path: cache_path.to_string_lossy().to_string(),
            }).await;
        }
        Err(e) => tracing::warn!("FetchAvatar: download failed for {user_id}: {e}"),
    }
}

/// Fetch a room avatar and cache it to disk. Sends RoomAvatarReady on success.
/// Skips the download if a cached file already exists.
async fn handle_fetch_room_avatar(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    mxc_url: &str,
) {
    if mxc_url.is_empty() { return; }

    let cache_dir = glib::user_data_dir()
        .join("hikyaku")
        .join("avatars");
    let _ = std::fs::create_dir_all(&cache_dir);

    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    mxc_url.hash(&mut hasher);
    let cache_path = cache_dir.join(format!("{:x}.jpg", hasher.finish()));

    if cache_path.exists() {
        let _ = event_tx.send(MatrixEvent::RoomAvatarReady {
            room_id: room_id.to_string(),
            path: cache_path.to_string_lossy().to_string(),
        }).await;
        return;
    }

    let Ok(uri) = <&matrix_sdk::ruma::MxcUri>::try_from(mxc_url) else {
        tracing::warn!("FetchRoomAvatar: invalid mxc URL: {mxc_url}");
        return;
    };

    use matrix_sdk::media::{MediaFormat, MediaRequestParameters, MediaThumbnailSettings};
    use matrix_sdk::ruma::UInt;
    let request = MediaRequestParameters {
        source: matrix_sdk::ruma::events::room::MediaSource::Plain(uri.to_owned()),
        format: MediaFormat::Thumbnail(MediaThumbnailSettings::new(
            UInt::from(64u32),
            UInt::from(64u32),
        )),
    };

    match client.media().get_media_content(&request, true).await {
        Ok(data) => {
            if let Err(e) = std::fs::write(&cache_path, &data) {
                tracing::warn!("FetchRoomAvatar: write failed: {e}");
                return;
            }
            let _ = event_tx.send(MatrixEvent::RoomAvatarReady {
                room_id: room_id.to_string(),
                path: cache_path.to_string_lossy().to_string(),
            }).await;
        }
        Err(e) => tracing::warn!("FetchRoomAvatar: download failed for {room_id}: {e}"),
    }
}

/// Shared reqwest client — avoids creating a new TCP connection for every
/// Ollama request. `reqwest::Client` is cheap to clone (Arc internally).
fn ollama_client() -> &'static reqwest::Client {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
}

/// Check if Ollama endpoint is reachable (fast probe, short timeout).
async fn ollama_reachable(endpoint: &str) -> bool {
    let url = format!("{}/api/tags", endpoint.trim_end_matches('/'));
    ollama_client().get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map(|r| r.status().is_success() || r.status().as_u16() == 200)
        .unwrap_or(false)
}

/// Ensure Ollama is running: probe, then start binary if needed, then poll.
/// Returns true if the endpoint became reachable within the timeout.
async fn ensure_ollama_running_tokio(endpoint: &str) -> bool {
    if ollama_reachable(endpoint).await {
        return true;
    }

    // Try to find and start the binary.
    // Mirror the detection order from ollama_manager::detect():
    //   1. Flatpak extension
    //   2. Managed download path
    //   3. System PATH (non-Flatpak only)
    let in_flatpak = std::env::var("FLATPAK_ID").is_ok();
    let binary = {
        let ext = std::path::PathBuf::from("/app/extensions/Ollama/bin/ollama");
        if in_flatpak && ext.exists() {
            Some(ext)
        } else {
            let managed = {
                let base = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                base.join("hikyaku").join("ollama").join("bin").join("ollama")
            };
            if managed.exists() {
                Some(managed)
            } else if !in_flatpak {
                let path_var = std::env::var("PATH").unwrap_or_default();
                std::env::split_paths(&path_var)
                    .map(|d| d.join("ollama"))
                    .find(|p| p.is_file())
            } else {
                None
            }
        }
    };

    let Some(binary) = binary else {
        tracing::warn!("Ollama binary not found");
        return false;
    };

    let host = endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let models_dir = {
        let base = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        base.join("hikyaku").join("ollama").join("models")
    };

    tracing::info!("Starting Ollama: {}", binary.display());
    let _ = tokio::process::Command::new(&binary)
        .arg("serve")
        .env("OLLAMA_HOST", host)
        .env("OLLAMA_MODELS", &models_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    // Poll up to 10 seconds for it to become reachable.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if ollama_reachable(endpoint).await {
            tracing::info!("Ollama is now reachable at {endpoint}");
            return true;
        }
    }
    tracing::warn!("Ollama did not start within 10s");
    false
}

/// Preload an Ollama model via the chat API so subsequent summaries start fast.
/// Uses /api/chat with an empty messages list — same endpoint as inference, so
/// Ollama loads the model into the correct runner context.
async fn warmup_ollama_model(endpoint: &str, model: &str) {
    if endpoint.is_empty() || model.is_empty() { return; }

    // Ensure Ollama is running before attempting warmup.
    if !ensure_ollama_running_tokio(endpoint).await {
        tracing::warn!("Warmup: Ollama not reachable at {endpoint}");
        return;
    }

    // Check that the model is actually downloaded before trying to load it.
    // Ollama returns 404 on /api/chat when the model doesn't exist locally —
    // this is expected if the user enabled AI but hasn't pulled the model yet.
    let tags_url = format!("{}/api/tags", endpoint.trim_end_matches('/'));
    let model_available = match ollama_client().get(&tags_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            resp.json::<serde_json::Value>().await
                .ok()
                .and_then(|v| v.get("models")?.as_array().cloned())
                .map(|models| models.iter().any(|m| {
                    m.get("name").and_then(|n| n.as_str())
                        .map(|n| n == model || n.starts_with(&format!("{model}:")))
                        .unwrap_or(false)
                }))
                .unwrap_or(false)
        }
        _ => false,
    };

    if !model_available {
        tracing::debug!("Warmup: model '{model}' not yet downloaded, skipping");
        return;
    }

    let url = format!("{}/api/chat", endpoint.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        // Empty messages list: Ollama loads model weights without generating tokens.
        "messages": [],
        "stream": false,
        "keep_alive": "15m",
    });

    match ollama_client().post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            let _ = resp.bytes().await;
            tracing::info!("Ollama warmup complete for model '{model}'");
        }
        Ok(resp) => {
            tracing::warn!("Ollama warmup returned status {}", resp.status());
        }
        Err(e) => {
            tracing::warn!("Ollama warmup failed: {e}");
        }
    }
}

/// Stream Ollama inference via reqwest (tokio-native) and emit OllamaChunk events.
/// `context` is passed through so the GTK side can route chunks to the right widget.
async fn ollama_stream_to_event(
    endpoint: &str,
    model: &str,
    prompt: &str,
    context: &str,
    event_tx: &Sender<MatrixEvent>,
) {
    let client = ollama_client();

    // Start Ollama if needed, bail if unavailable.
    if !ensure_ollama_running_tokio(endpoint).await {
        tracing::warn!("ollama_stream_to_event: Ollama not reachable at {endpoint}");
        let _ = event_tx.send(MatrixEvent::OllamaChunk {
            context: context.to_string(),
            chunk: format!("\u{26a0} Ollama not running at {endpoint}"),
            done: true,
        }).await;
        return;
    }

    let url = format!("{}/api/chat", endpoint.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": true,
        // Keep the model loaded for 15 minutes after last use.
        // Balances fast subsequent requests vs. freeing RAM when idle.
        // (Ollama's default is 5 minutes.)
        "keep_alive": "15m",
    });

    // 120-second hard timeout for the entire request + streaming.
    // Prevents indefinite hangs when Ollama needs to load a large model.
    // Tasks are abortable anyway (CancelRoomPreview), but this caps runaway inference.
    let inference_result = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        async {
    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Ollama HTTP error: {e}");
            let _ = event_tx.send(MatrixEvent::OllamaChunk {
                context: context.to_string(),
                chunk: format!("\u{26a0} Ollama connection error: {e}"),
                done: true,
            }).await;
            return;
        }
    };

    // Check for HTTP errors (e.g. 404 when the model isn't downloaded).
    if !resp.status().is_success() {
        let status = resp.status();
        // Try to extract Ollama's error message from the body.
        let body_text = resp.text().await.unwrap_or_default();
        let ollama_error = serde_json::from_str::<serde_json::Value>(&body_text)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| format!("HTTP {status}"));
        tracing::warn!("Ollama /api/chat error: {ollama_error}");
        let _ = event_tx.send(MatrixEvent::OllamaChunk {
            context: context.to_string(),
            chunk: format!("\u{26a0} {ollama_error}"),
            done: true,
        }).await;
        return;
    }

    use futures_util::StreamExt;
    let mut stream = Box::pin(resp.bytes_stream());
    let mut buf = Vec::new();

    while let Some(item) = stream.next().await {
        let Ok(bytes) = item else { break };
        buf.extend_from_slice(&bytes);
        // Process all complete newline-delimited JSON objects in the buffer.
        while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            let line = line.trim_ascii_start();
            let line: Vec<u8> = line.iter().rev().skip_while(|&&b| b == b'\r' || b == b'\n').cloned().collect::<Vec<_>>().into_iter().rev().collect();
            if line.is_empty() { continue; }
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&line) {
                let text = val["message"]["content"].as_str().unwrap_or("").to_string();
                let done = val["done"].as_bool().unwrap_or(false);
                if !text.is_empty() || done {
                    let _ = event_tx.send(MatrixEvent::OllamaChunk {
                        context: context.to_string(),
                        chunk: text,
                        done,
                    }).await;
                    if done { return; }
                }
            }
        }
    }
    // Ensure done is always sent.
    let _ = event_tx.send(MatrixEvent::OllamaChunk {
        context: context.to_string(), chunk: String::new(), done: true,
    }).await;
    } // end async block passed to tokio::time::timeout
    ).await;

    if inference_result.is_err() {
        tracing::warn!("ollama_stream_to_event: inference timed out after 120s");
        let _ = event_tx.send(MatrixEvent::OllamaChunk {
            context: context.to_string(),
            chunk: "\u{26a0} Inference timed out (model may be loading — try again)".to_string(),
            done: true,
        }).await;
    }
}

/// Strip Matrix reply fallback lines from a message body.
/// Replies include "> <@user:server> quoted text\n\n" at the start —
/// these are noise for LLM summarization.
/// Format a Unix timestamp (seconds) as "YYYY-MM-DD HH:MM UTC" without external deps.
fn format_unix_ts(secs: u64) -> String {
    // Days since 1970-01-01, accounting for leap years.
    let s = secs;
    let days = s / 86400;
    let time = s % 86400;
    let hh = time / 3600;
    let mm = (time % 3600) / 60;

    // Gregorian calendar calculation.
    let mut y = 1970u32;
    let mut d = days as u32;
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let days_in_year = if leap { 366 } else { 365 };
        if d < days_in_year { break; }
        d -= days_in_year;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_days: [u32; 12] = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0u32;
    for (i, &md) in month_days.iter().enumerate() {
        if d < md { m = i as u32 + 1; break; }
        d -= md;
    }
    format!("{y}-{m:02}-{:02} {:02}:{mm:02} UTC", d + 1, hh)
}

fn strip_reply_fallback_simple(body: &str) -> String {
    let mut lines = body.lines().peekable();
    while let Some(line) = lines.peek() {
        if line.starts_with("> ") { lines.next(); } else { break; }
    }
    // Skip one blank separator line after the quote block.
    if lines.peek().map(|l| l.is_empty()).unwrap_or(false) {
        lines.next();
    }
    let result: Vec<&str> = lines.collect();
    let joined = result.join("\n");
    if joined.trim().is_empty() { String::new() } else { joined }
}

#[cfg(test)]
mod disk_cache_tests {
    use super::{merge_disk_unread_counts, RoomInfo, RoomKind};

    fn make_room(room_id: &str, unread: u64, highlight: u64) -> RoomInfo {
        RoomInfo {
            room_id: room_id.to_string(),
            name: room_id.to_string(),
            last_activity_ts: 0,
            kind: RoomKind::Room,
            is_encrypted: false,
            parent_space: None,
            parent_space_id: String::new(),
            is_pinned: false,
            unread_count: unread,
            highlight_count: highlight,
            is_admin: false,
            is_tombstoned: false,
            is_favourite: false,
            avatar_url: String::new(),
            topic: String::new(),
        }
    }

    /// SDK returns 0 pre-sync; disk cache has correct non-zero count.
    /// merge must preserve the disk-cached badge.
    #[test]
    fn merge_preserves_disk_badge_when_sdk_returns_zero() {
        let mut rooms = vec![make_room("!r:m.org", 0, 0)];
        let disk = [("!r:m.org".to_string(), (5u64, 2u64))].into();
        merge_disk_unread_counts(&mut rooms, &disk);
        assert_eq!(rooms[0].unread_count, 5, "badge should come from disk cache");
        assert_eq!(rooms[0].highlight_count, 2);
    }

    /// SDK returns a fresher, higher count than disk cache.
    /// merge must keep the SDK value.
    #[test]
    fn merge_keeps_sdk_value_when_higher_than_disk() {
        let mut rooms = vec![make_room("!r:m.org", 8, 3)];
        let disk = [("!r:m.org".to_string(), (5u64, 1u64))].into();
        merge_disk_unread_counts(&mut rooms, &disk);
        assert_eq!(rooms[0].unread_count, 8);
        assert_eq!(rooms[0].highlight_count, 3);
    }

    /// Room that user had already read: both SDK and disk show 0.
    /// merge must leave it at 0 — no phantom badge.
    #[test]
    fn merge_leaves_zero_for_rooms_user_already_read() {
        let mut rooms = vec![make_room("!r:m.org", 0, 0)];
        let disk = [("!r:m.org".to_string(), (0u64, 0u64))].into();
        merge_disk_unread_counts(&mut rooms, &disk);
        assert_eq!(rooms[0].unread_count, 0);
    }

    /// Room not present in disk cache (new room joined this session).
    /// merge must not touch it.
    #[test]
    fn merge_leaves_new_rooms_unchanged() {
        let mut rooms = vec![make_room("!new:m.org", 3, 0)];
        let disk: std::collections::HashMap<String, (u64, u64)> = std::collections::HashMap::new();
        merge_disk_unread_counts(&mut rooms, &disk);
        assert_eq!(rooms[0].unread_count, 3);
    }

    /// Scenario: user quit with 3 unread rooms; SDK returns 0 pre-sync.
    /// All three badges must survive the pre-sync save.
    #[test]
    fn merge_preserves_multiple_rooms_on_restart() {
        let mut rooms = vec![
            make_room("!a:m.org", 0, 0),
            make_room("!b:m.org", 0, 0),
            make_room("!c:m.org", 2, 0),
        ];
        let disk = [
            ("!a:m.org".to_string(), (4u64, 1u64)),
            ("!b:m.org".to_string(), (7u64, 2u64)),
            ("!c:m.org".to_string(), (1u64, 0u64)),
        ].into();
        merge_disk_unread_counts(&mut rooms, &disk);
        assert_eq!(rooms[0].unread_count, 4, "room a: disk badge preserved");
        assert_eq!(rooms[1].unread_count, 7, "room b: disk badge preserved");
        assert_eq!(rooms[2].unread_count, 2, "room c: SDK value kept (higher)");
    }

    // ── zero_room_unread_in_disk_cache contract ───────────────────────────────
    //
    // SelectRoom calls zero_room_unread_in_disk_cache which sets the opened
    // room's disk entry to 0.  Subsequent merge-saves must NOT resurrect the
    // badge (merge sees disk=0 → max(sdk=0, disk=0) = 0).
    //
    // Rooms the user did NOT open keep their disk entry non-zero, so the same
    // merge-save preserves their badge (max(sdk=0, disk=N) = N).
    //
    // We simulate the disk state with an in-memory HashMap rather than touching
    // the filesystem.

    /// After the user opens a room, its disk entry is 0.
    /// A subsequent merge-save must leave it at 0 (no badge resurrection).
    #[test]
    fn merge_after_zero_does_not_resurrect_badge() {
        // Simulate disk after zero_room_unread_in_disk_cache ran.
        let disk_after_zero: std::collections::HashMap<String, (u64, u64)> =
            [("!r:m.org".to_string(), (0u64, 0u64))].into();

        // SDK also returns 0 (room was read, server agrees).
        let mut rooms = vec![make_room("!r:m.org", 0, 0)];
        merge_disk_unread_counts(&mut rooms, &disk_after_zero);
        assert_eq!(rooms[0].unread_count, 0, "zeroed room must stay 0 after merge");
    }

    /// Room the user never opened keeps its disk badge across a merge-save
    /// even when the SDK returns 0 (push rules may not fire for this room).
    #[test]
    fn merge_preserves_badge_for_unvisited_room_across_60s_sync() {
        // Disk still has the badge (user never opened the room).
        let disk: std::collections::HashMap<String, (u64, u64)> =
            [("!r:m.org".to_string(), (5u64, 1u64))].into();

        // SDK returns 0 — push rules didn't fire.
        let mut rooms = vec![make_room("!r:m.org", 0, 0)];
        merge_disk_unread_counts(&mut rooms, &disk);
        assert_eq!(rooms[0].unread_count, 5, "unvisited room badge must survive merge");
        assert_eq!(rooms[0].highlight_count, 1);
    }

    /// Mixed scenario: user opened room A (disk zeroed) but not room B.
    /// merge-save must zero A and preserve B.
    #[test]
    fn merge_zeros_opened_room_preserves_unopened() {
        let disk: std::collections::HashMap<String, (u64, u64)> = [
            ("!a:m.org".to_string(), (0u64, 0u64)), // zeroed by SelectRoom
            ("!b:m.org".to_string(), (9u64, 3u64)), // not opened
        ].into();

        let mut rooms = vec![
            make_room("!a:m.org", 0, 0),
            make_room("!b:m.org", 0, 0),
        ];
        merge_disk_unread_counts(&mut rooms, &disk);
        assert_eq!(rooms[0].unread_count, 0, "opened room must stay 0");
        assert_eq!(rooms[1].unread_count, 9, "unopened room badge must be preserved");
        assert_eq!(rooms[1].highlight_count, 3);
    }

    /// Quit → restart cycle: badges that were non-zero before quit must
    /// survive through startup merge AND first-sync merge.
    #[test]
    fn badges_survive_quit_restart_two_merge_passes() {
        // Step 1: disk before restart has correct badges.
        let disk_before_restart: std::collections::HashMap<String, (u64, u64)> = [
            ("!x:m.org".to_string(), (4u64, 0u64)),
        ].into();

        // Step 2: startup merge (SDK returns 0 pre-sync).
        let mut rooms = vec![make_room("!x:m.org", 0, 0)];
        merge_disk_unread_counts(&mut rooms, &disk_before_restart);
        assert_eq!(rooms[0].unread_count, 4, "startup merge must preserve badge");

        // Save to disk (simulated — disk now has 4).
        let disk_after_startup_save: std::collections::HashMap<String, (u64, u64)> = [
            ("!x:m.org".to_string(), (rooms[0].unread_count, rooms[0].highlight_count)),
        ].into();

        // Step 3: first-sync merge (SDK still returns 0).
        let mut rooms2 = vec![make_room("!x:m.org", 0, 0)];
        merge_disk_unread_counts(&mut rooms2, &disk_after_startup_save);
        assert_eq!(rooms2[0].unread_count, 4, "first-sync merge must preserve badge");

        // Step 4: 60s sync merge (SDK still returns 0).
        let disk_after_first_sync: std::collections::HashMap<String, (u64, u64)> = [
            ("!x:m.org".to_string(), (rooms2[0].unread_count, rooms2[0].highlight_count)),
        ].into();
        let mut rooms3 = vec![make_room("!x:m.org", 0, 0)];
        merge_disk_unread_counts(&mut rooms3, &disk_after_first_sync);
        assert_eq!(rooms3[0].unread_count, 4, "60s-sync merge must preserve badge");
    }
}

// ── known_unread floor logic ─────────────────────────────────────────────────
//
// When the user selects a room, window.rs captures the UI badge (known_unread)
// BEFORE clear_unread() zeroes it.  The Matrix thread uses max(sdk, known) as
// the unread count so the divider + tinting are correct even when the SDK store
// returns 0 pre-sync.
//
// These tests verify the floor logic in isolation (no GTK needed).

#[cfg(test)]
mod known_unread_floor_tests {
    /// Pure model of max(sdk_unread, known_unread) — the formula used in the
    /// disk-cache and memory-cache hit paths.
    fn resolve_unread(sdk_unread: u32, known_unread: u32) -> u32 {
        sdk_unread.max(known_unread)
    }

    /// SDK returns 0 pre-sync; UI badge was 3 → divider should be at n-3.
    #[test]
    fn sdk_zero_known_three_uses_known() {
        assert_eq!(resolve_unread(0, 3), 3);
    }

    /// SDK returns the authoritative count (post-sync); higher than badge → use SDK.
    #[test]
    fn sdk_higher_than_known_uses_sdk() {
        assert_eq!(resolve_unread(5, 3), 5);
    }

    /// SDK equals known → no change.
    #[test]
    fn sdk_equals_known_unchanged() {
        assert_eq!(resolve_unread(3, 3), 3);
    }

    /// Both zero — user already read room, no divider needed.
    #[test]
    fn both_zero_stays_zero() {
        assert_eq!(resolve_unread(0, 0), 0);
    }

    /// Scenario: restart with N=3 unread badges intact, then user enters room.
    ///
    /// known_unread = 3 (from registry before clear_unread)
    /// sdk_unread   = 0 (SDK store not yet populated post-sync)
    ///
    /// Expected divider position (10 messages): 10 - 3 = 7.
    #[test]
    fn scenario_enter_room_after_restart_divider_at_correct_position() {
        let known_unread = 3u32;
        let sdk_unread   = 0u32;
        let effective = resolve_unread(sdk_unread, known_unread);
        assert_eq!(effective, 3);

        let n_messages = 10usize;
        let divider_pos = n_messages.saturating_sub(effective as usize);
        assert_eq!(divider_pos, 7, "divider must be 3 messages from the end");
    }

    /// Scenario: normal run (not restart), SDK has correct count, known matches.
    ///
    /// sdk_unread=2, known_unread=2 → divider at n-2 regardless of which we use.
    #[test]
    fn scenario_normal_run_sdk_and_known_agree() {
        let effective = resolve_unread(2, 2);
        assert_eq!(effective, 2);
        let divider_pos = 10usize.saturating_sub(effective as usize);
        assert_eq!(divider_pos, 8);
    }

    /// Scenario: new messages arrived while app was closed.
    ///
    /// Disk cache saved 0 (rooms were read before quit).  After first sync the
    /// SDK returns 4 — known_unread from disk badge was 0 but SDK is now higher.
    #[test]
    fn scenario_new_messages_arrived_while_closed_sdk_wins() {
        // After first sync completes, sdk_unread is authoritative.
        let effective = resolve_unread(4, 0);
        assert_eq!(effective, 4, "post-sync SDK count must not be suppressed by known=0");
    }
}

// ── compute_enter_unread ─────────────────────────────────────────────────────
//
// Tests for the fully_read-based unread count derivation used in
// handle_select_room_bg.  The SDK's notification_count is unreliable before
// the first sync; we count messages after the fully_read marker instead.

#[cfg(test)]
mod compute_enter_unread_tests {
    use super::compute_enter_unread;
    use crate::matrix::MessageInfo;

    fn msg(id: &str) -> MessageInfo {
        MessageInfo {
            sender: String::new(),
            sender_id: String::new(),
            body: String::new(),
            formatted_body: None,
            timestamp: 0,
            event_id: id.to_string(),
            reply_to: None,
            reply_to_sender: None,
            thread_root: None,
            reactions: vec![],
            media: None,
            is_highlight: false,
            is_system_event: false,
        }
    }

    fn msgs(ids: &[&str]) -> Vec<MessageInfo> {
        ids.iter().map(|id| msg(id)).collect()
    }

    /// fully_read is the 3rd of 5 messages → 2 new messages after it.
    #[test]
    fn fully_read_in_window_counts_messages_after() {
        let ms = msgs(&["$e0", "$e1", "$e2", "$e3", "$e4"]);
        assert_eq!(compute_enter_unread(&ms, Some("$e2"), 0, 0), 2);
    }

    /// fully_read is the last message → 0 new messages.
    #[test]
    fn fully_read_is_last_message_zero_unread() {
        let ms = msgs(&["$e0", "$e1", "$e2"]);
        assert_eq!(compute_enter_unread(&ms, Some("$e2"), 0, 0), 0);
    }

    /// fully_read is the first message → all remaining messages are new.
    #[test]
    fn fully_read_is_first_message_rest_are_new() {
        let ms = msgs(&["$e0", "$e1", "$e2", "$e3"]);
        assert_eq!(compute_enter_unread(&ms, Some("$e0"), 0, 0), 3);
    }

    /// fully_read is outside the window → fall back to max(sdk, known).
    #[test]
    fn fully_read_not_in_window_falls_back_to_sdk() {
        let ms = msgs(&["$e0", "$e1", "$e2"]);
        assert_eq!(compute_enter_unread(&ms, Some("$old"), 4, 0), 4);
    }

    /// fully_read absent, sdk=0, known=3 → use known (restart scenario).
    #[test]
    fn no_fully_read_sdk_zero_uses_known() {
        let ms = msgs(&["$e0", "$e1", "$e2"]);
        assert_eq!(compute_enter_unread(&ms, None, 0, 3), 3);
    }

    /// fully_read absent, sdk=0, known=0 → 0 unread (room was read).
    #[test]
    fn no_fully_read_both_zero_no_divider() {
        let ms = msgs(&["$e0", "$e1"]);
        assert_eq!(compute_enter_unread(&ms, None, 0, 0), 0);
    }

    /// Scenario: user offline, 4 new messages arrived, fully_read is old event.
    /// fully_read in window at position 6, 10 messages total → 3 new.
    #[test]
    fn scenario_offline_messages_arrived_fully_read_in_window() {
        let ms = msgs(&["$e0","$e1","$e2","$e3","$e4","$e5","$e6","$e7","$e8","$e9"]);
        // fully_read = $e6 (index 6) → new messages are $e7, $e8, $e9 → 3.
        assert_eq!(compute_enter_unread(&ms, Some("$e6"), 0, 0), 3);
    }

    /// Scenario: room was never fully_read-marked, sdk=0 pre-sync, known=2.
    /// Falls back to known.
    #[test]
    fn scenario_no_fully_read_marker_uses_known_unread() {
        let ms = msgs(&["$e0","$e1","$e2","$e3","$e4"]);
        assert_eq!(compute_enter_unread(&ms, None, 0, 2), 2);
    }

    // ── skip-re-render guard ─────────────────────────────────────────────────
    //
    // bg_refresh skips sending RoomMessages when messages haven't changed.
    // But when unread_count > 0 (or fully_read is set), it must still send so
    // the divider gets placed.  Simulate the guard condition:
    //   skip = unread_count == 0 AND fully_read is None
    //   send  = otherwise

    fn should_skip_render(unread_count: u32, fully_read: Option<&str>) -> bool {
        unread_count == 0 && fully_read.is_none()
    }

    /// Messages unchanged, unread=0, no marker → skip re-render (normal).
    #[test]
    fn skip_rerender_when_messages_unchanged_and_no_unreads() {
        assert!(should_skip_render(0, None), "safe to skip: nothing new to show");
    }

    /// Messages unchanged but unread_count=12 → must NOT skip (divider needed).
    #[test]
    fn no_skip_rerender_when_unread_count_positive() {
        assert!(!should_skip_render(12, None), "must send meta to place divider");
    }

    /// Messages unchanged but fully_read is set → must NOT skip (marker placement).
    #[test]
    fn no_skip_rerender_when_fully_read_set() {
        assert!(!should_skip_render(0, Some("$e5")), "must send to place marker");
    }

    /// Scenario: 12 messages arrived via append_memory (cached), bg_refresh
    /// finds same top message but compute_enter_unread=12 → must send RoomMessages.
    #[test]
    fn scenario_cache_fresh_but_unread_count_nonzero_must_send() {
        let ms = msgs(&["$e0","$e1","$e2","$e3","$e4","$e5","$e6","$e7",
                        "$e8","$e9","$e10","$e11","$e12"]);
        // fully_read = $e0 → 12 new messages after it
        let count = compute_enter_unread(&ms, Some("$e0"), 0, 0);
        assert_eq!(count, 12);
        // With messages unchanged but count=12, re-render must NOT be skipped.
        assert!(!should_skip_render(count, Some("$e0")));
    }
}
