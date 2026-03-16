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
    ruma::RoomId,
    Client, ServerName,
    authentication::matrix::MatrixSession,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What kind of room this is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomKind {
    /// A direct message (1:1 or small group DM).
    DirectMessage,
    /// A regular room (channel).
    Room,
    /// A space (container for other rooms).
    Space,
}

/// A snapshot of one room's state, sent from the Matrix thread to the UI.
#[derive(Debug, Clone)]
pub struct RoomInfo {
    pub room_id: String,
    pub name: String,
    pub last_activity_ts: u64,
    pub kind: RoomKind,
    pub is_encrypted: bool,
    /// If this room belongs to a space, the space's display name.
    pub parent_space: Option<String>,
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
}

/// A single message sent to the UI.
#[derive(Debug, Clone)]
pub struct MessageInfo {
    pub sender: String,
    pub sender_id: String,
    pub body: String,
    pub timestamp: u64,
    pub event_id: String,
    /// If this message is a reply, the event ID it replies to.
    pub reply_to: Option<String>,
    /// If this message is part of a thread, the thread root event ID.
    pub thread_root: Option<String>,
    /// Aggregated emoji reactions: (emoji, count).
    pub reactions: Vec<(String, u64)>,
    /// Media attachment info (if this is an image/file/video/audio message).
    pub media: Option<MediaInfo>,
}

/// Media attachment on a message.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MediaInfo {
    pub kind: MediaKind,
    pub filename: String,
    pub size: Option<u64>,
    /// Matrix content URI (mxc://).
    pub url: String,
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
    /// Pinned messages: (sender, body) pairs.
    pub pinned_messages: Vec<(String, String)>,
    /// Whether the room is encrypted.
    pub is_encrypted: bool,
    /// Number of joined members.
    pub member_count: u64,
    /// Whether this room is bookmarked (m.favourite).
    pub is_favourite: bool,
    /// Room member display names (for nick completion).
    pub members: Vec<(String, String)>, // (user_id, display_name)
}

/// A room entry from a space directory listing.
#[derive(Debug, Clone)]
pub struct SpaceDirectoryRoom {
    pub room_id: String,
    pub name: String,
    pub topic: String,
    pub member_count: u64,
    pub already_joined: bool,
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
    LoginSuccess { display_name: String, user_id: String },
    LoginFailed { error: String },
    SyncStarted,
    SyncError { error: String },
    RoomListUpdated { rooms: Vec<RoomInfo> },
    RoomMessages {
        room_id: String,
        messages: Vec<MessageInfo>,
        /// Pagination token — pass to FetchOlderMessages to get the next batch.
        prev_batch_token: Option<String>,
        /// Room metadata for the content header.
        room_meta: RoomMeta,
    },
    /// Older messages prepended at the top (pagination result).
    OlderMessages {
        room_id: String,
        messages: Vec<MessageInfo>,
        prev_batch_token: Option<String>,
    },
    NewMessage {
        room_id: String,
        room_name: String,
        sender_id: String,
        message: MessageInfo,
        is_mention: bool,
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
    /// Recovery key import succeeded.
    RecoveryComplete,
    /// Recovery key import failed.
    RecoveryFailed { error: String },
    /// Public room directory from the homeserver.
    PublicRoomDirectory {
        rooms: Vec<SpaceDirectoryRoom>,
    },
    /// Space directory rooms for the "Join Room" browser.
    SpaceDirectory {
        space_id: String,
        rooms: Vec<SpaceDirectoryRoom>,
    },
    /// Media downloaded to a temp file — show preview.
    MediaReady { url: String, path: String },
    /// Successfully joined a room.
    RoomJoined { room_id: String, room_name: String },
    /// Failed to join a room.
    JoinFailed { error: String },
    /// Successfully left a room.
    RoomLeft { room_id: String },
    /// Failed to leave a room.
    LeaveFailed { error: String },
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
    },
    SendMessage {
        room_id: String,
        body: String,
        /// Event ID to reply to (if replying).
        reply_to: Option<String>,
        /// Original sender + body for quote fallback (if replying).
        quote_text: Option<(String, String)>,
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
    /// Fetch public room directory on the user's homeserver.
    BrowsePublicRooms { search_term: Option<String> },
    /// Fetch the room directory for a space.
    BrowseSpaceRooms { space_id: String },
    /// Join a room by ID or alias.
    JoinRoom { room_id_or_alias: String },
    /// Delete (redact) a message.
    RedactMessage { room_id: String, event_id: String },
    /// Edit a message (send replacement).
    EditMessage { room_id: String, event_id: String, new_body: String },
    /// Upload and send a media file.
    SendMedia { room_id: String, file_path: String },
    /// Download media and open with system viewer.
    DownloadMedia { url: String, filename: String },
    /// Toggle bookmark (m.favourite tag) on a room.
    SetFavourite { room_id: String, is_favourite: bool },
    /// Leave a room.
    LeaveRoom { room_id: String },
}

/// Session data serialized into the keyring. The access token and auth
/// details are stored in GNOME Keyring via the Secret Service D-Bus API
/// (oo7 crate), not in a plaintext file on disk.
#[derive(Serialize, Deserialize)]
struct PersistedSession {
    homeserver: String,
    session: MatrixSession,
}

/// The attributes used to look up our secret in the keyring.
const KEYRING_LABEL: &str = "Matx Matrix session";

fn db_dir_path(homeserver: &str) -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("matx");
    path.push("db");
    path.push(homeserver);
    path
}

/// Save session to GNOME Keyring via Secret Service.
async fn save_session_to_keyring(
    persisted: &PersistedSession,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let keyring = oo7::Keyring::new().await?;
    let json = serde_json::to_string(persisted)?;
    let attributes = vec![("application", "com.github.matx")];
    keyring
        .create_item(KEYRING_LABEL, &attributes, json, true)
        .await?;
    tracing::info!("Session saved to GNOME Keyring");
    Ok(())
}

/// Load session from GNOME Keyring.
async fn load_session_from_keyring() -> Option<PersistedSession> {
    let keyring = oo7::Keyring::new().await.ok()?;
    let attributes = vec![("application", "com.github.matx")];
    let items = keyring.search_items(&attributes).await.ok()?;
    let item = items.first()?;
    let secret = item.secret().await.ok()?;
    let json = std::str::from_utf8(&secret).ok()?;
    serde_json::from_str(json).ok()
}

/// Delete session from GNOME Keyring.
async fn delete_session_from_keyring() {
    if let Ok(keyring) = oo7::Keyring::new().await {
        let attributes = vec![("application", "com.github.matx")];
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
/// Returns a shutdown handle — call `send(())` to signal graceful shutdown.
pub fn spawn_matrix_thread(
    event_tx: Sender<MatrixEvent>,
    command_rx: Receiver<MatrixCommand>,
) -> tokio::sync::watch::Sender<bool> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");

        rt.block_on(async move {
            matrix_task(event_tx, command_rx, shutdown_rx).await;
        });

        tracing::info!("Matrix thread shut down cleanly");
    });

    shutdown_tx
}

async fn matrix_task(
    event_tx: Sender<MatrixEvent>,
    command_rx: Receiver<MatrixCommand>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    // Try to restore a previous session first.
    let client = if let Some(client) = try_restore_session(&event_tx).await {
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
                            let display_name = username.clone();
                            let user_id = client.user_id().map(|u| u.to_string()).unwrap_or_default();
                            let _ = event_tx.send(MatrixEvent::LoginSuccess { display_name, user_id }).await;
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

    // Set up E2E encryption: bootstrap cross-signing and enable key backup.
    setup_encryption(&client, &event_tx).await;

    // Spawn sync in a separate task so we can keep processing commands.
    let sync_event_tx = event_tx.clone();
    let sync_client = client.clone();
    let sync_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        start_sync(sync_client, &sync_event_tx, sync_shutdown).await;
    });

    // Track rooms we've already downloaded encryption keys for to avoid
    // re-downloading on every room select. Persisted to disk so keys
    // aren't re-fetched across restarts (the SDK's crypto store already
    // has the actual keys; this just tracks which rooms we've fetched for).
    let key_cache_path = {
        let mut p = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
        p.push("matx");
        p.push("key_fetched_rooms.json");
        p
    };
    let mut rooms_with_keys: std::collections::HashSet<String> = std::fs::read_to_string(&key_cache_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    // Process commands while sync runs in the background.
    let mut shutdown_rx = shutdown_rx;
    loop {
        tokio::select! {
            cmd = command_rx.recv() => {
                match cmd {
                    Ok(MatrixCommand::Login { .. }) => {
                        tracing::warn!("Already logged in, ignoring duplicate login command");
                    }
                    Ok(MatrixCommand::SelectRoom { room_id }) => {
                        handle_select_room(&client, &event_tx, &room_id, &mut rooms_with_keys).await;
                    }
                    Ok(MatrixCommand::SendMessage { room_id, body, reply_to, quote_text }) => {
                        handle_send_message(&client, &room_id, &body, reply_to.as_deref(), quote_text.as_ref()).await;
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
                        handle_recover_keys(&client, &event_tx, &recovery_key).await;
                    }
                    Ok(MatrixCommand::BrowsePublicRooms { search_term }) => {
                        handle_browse_public_rooms(&client, &event_tx, search_term.as_deref()).await;
                    }
                    Ok(MatrixCommand::BrowseSpaceRooms { space_id }) => {
                        handle_browse_space(&client, &event_tx, &space_id).await;
                    }
                    Ok(MatrixCommand::JoinRoom { room_id_or_alias }) => {
                        handle_join_room(&client, &event_tx, &room_id_or_alias).await;
                    }
                    Ok(MatrixCommand::RedactMessage { room_id, event_id }) => {
                        if let (Ok(rid), Ok(eid)) = (RoomId::parse(&room_id), matrix_sdk::ruma::EventId::parse(&event_id)) {
                            if let Some(room) = client.get_room(&rid) {
                                if let Err(e) = room.redact(&eid, None, None).await {
                                    tracing::error!("Failed to redact: {e}");
                                }
                            }
                        }
                    }
                    Ok(MatrixCommand::EditMessage { room_id, event_id, new_body }) => {
                        if let (Ok(rid), Ok(eid)) = (RoomId::parse(&room_id), matrix_sdk::ruma::EventId::parse(&event_id)) {
                            if let Some(room) = client.get_room(&rid) {
                                use matrix_sdk::ruma::events::room::message::{
                                    RoomMessageEventContent, ReplacementMetadata,
                                };
                                let metadata = ReplacementMetadata::new(eid.to_owned(), None);
                                let content = RoomMessageEventContent::text_plain(&new_body)
                                    .make_replacement(metadata, None);
                                if let Err(e) = room.send(content).await {
                                    tracing::error!("Failed to edit message: {e}");
                                }
                            }
                        }
                    }
                    Ok(MatrixCommand::SendMedia { room_id, file_path }) => {
                        handle_send_media(&client, &event_tx, &room_id, &file_path).await;
                    }
                    Ok(MatrixCommand::DownloadMedia { url, filename }) => {
                        handle_download_media(&client, &event_tx, &url, &filename).await;
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
                    Ok(MatrixCommand::LeaveRoom { room_id }) => {
                        handle_leave_room(&client, &event_tx, &room_id).await;
                    }
                    Err(_) => break,
                }
            }
            _ = shutdown_rx.changed() => {
                tracing::info!("Shutdown signal received, stopping command loop");
                break;
            }
        }
    }
}

async fn do_login(
    homeserver: &str,
    username: &str,
    password: &str,
) -> Result<Client, Box<dyn std::error::Error + Send + Sync>> {
    let db_path = db_dir_path(homeserver);
    std::fs::create_dir_all(&db_path)?;

    // Parse homeserver as a server name (e.g., "matrix.org").
    let server_name = ServerName::parse(homeserver)?;

    let client = Client::builder()
        .server_name(&server_name)
        .sqlite_store(&db_path, None)
        .build()
        .await?;

    client
        .matrix_auth()
        .login_username(username, password)
        .initial_device_display_name("Matx")
        .await?;

    // Persist session info so we can restore it next launch.
    use matrix_sdk::AuthSession;
    let matrix_session = match client.session().expect("just logged in, session must exist") {
        AuthSession::Matrix(s) => s,
        #[allow(unreachable_patterns)]
        _ => panic!("we used password login, not OIDC"),
    };

    let persisted = PersistedSession {
        homeserver: homeserver.to_string(),
        session: matrix_session,
    };
    save_session_to_keyring(&persisted).await?;
    Ok(client)
}

async fn try_restore_session(
    event_tx: &Sender<MatrixEvent>,
) -> Option<Client> {
    let persisted = load_session_from_keyring().await?;

    tracing::info!("Restoring session from GNOME Keyring");

    let server_name = ServerName::parse(&persisted.homeserver).ok()?;
    let db_path = db_dir_path(&persisted.homeserver);

    let client = match Client::builder()
        .server_name(&server_name)
        .sqlite_store(&db_path, None)
        .build()
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to restore client: {e}");
            cleanup_session(&persisted.homeserver).await;
            return None;
        }
    };

    // Restore the auth session (access token, user ID, device ID).
    // The SQLite store only persists crypto state, not the auth session itself.
    if let Err(e) = client.restore_session(persisted.session).await {
        tracing::warn!("Failed to restore session: {e}");
        cleanup_session(&persisted.homeserver).await;
        return None;
    }

    if client.logged_in() {
        let display_name = client
            .account()
            .get_display_name()
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "User".to_string());

        let user_id = client.user_id().map(|u| u.to_string()).unwrap_or_default();
        let _ = event_tx
            .send(MatrixEvent::LoginSuccess { display_name, user_id })
            .await;
        Some(client)
    } else {
        tracing::info!("Stored session is invalid, cleaning up");
        cleanup_session(&persisted.homeserver).await;
        None
    }
}


/// Bootstrap cross-signing and enable key backup so we can decrypt
/// messages in encrypted rooms. Returns true if the device is verified.
async fn setup_encryption(client: &Client, event_tx: &Sender<MatrixEvent>) {
    let enc = client.encryption();

    // Bootstrap cross-signing if not already set up.
    // This creates the signing keys that let other devices verify us.
    match enc.bootstrap_cross_signing_if_needed(None).await {
        Ok(()) => tracing::info!("Cross-signing ready"),
        Err(e) => {
            // UIAA auth may be required — log but don't block.
            // The user can still verify interactively.
            tracing::warn!("Cross-signing bootstrap skipped: {e}");
        }
    }

    // Enable key backup so room keys are uploaded and available for
    // restore on other devices. If a backup already exists, try to
    // download room keys from it so we can decrypt old messages.
    let backups = enc.backups();
    if backups.are_enabled().await {
        tracing::info!("Key backup already enabled");
    } else if backups.exists_on_server().await.unwrap_or(false) {
        tracing::info!("Key backup exists on server, attempting to download keys");
        // The SDK can access the backup after verification because it
        // has the backup decryption key via cross-signing secrets.
        match backups.create().await {
            Ok(()) => tracing::info!("Connected to existing key backup"),
            Err(e) => tracing::warn!("Failed to connect to key backup: {e}"),
        }
    } else {
        match backups.create().await {
            Ok(()) => tracing::info!("Key backup created"),
            Err(e) => tracing::warn!("Failed to create key backup: {e}"),
        }
    }

    // Try to recover room keys using the recovery module. This uses
    // the secret storage (4S) to get the backup decryption key, which
    // is available after cross-signing verification.
    let recovery = enc.recovery();
    match recovery.state() {
        matrix_sdk::encryption::recovery::RecoveryState::Enabled => {
            tracing::info!("Recovery is enabled, room keys should be available");
        }
        matrix_sdk::encryption::recovery::RecoveryState::Incomplete => {
            tracing::info!("Recovery is incomplete — some secrets missing");
        }
        state => {
            tracing::info!("Recovery state: {state:?}");
        }
    }

    // Check if our identity is verified by another device. The device
    // that bootstraps cross-signing auto-trusts itself, but that doesn't
    // mean we've actually done interactive verification with another
    // session. Without that, we can't decrypt messages from other devices.
    let user_id = client.user_id().expect("must be logged in");
    let is_verified = match enc.get_user_identity(user_id).await {
        Ok(Some(identity)) => {
            // For our own identity, check if it's verified — this is
            // only true if another device has confirmed us via SAS or
            // we've restored from a recovery key/passphrase.
            let verified = identity.is_verified();
            tracing::info!("Own identity verified: {verified}");
            verified
        }
        Ok(None) => {
            tracing::warn!("No identity found for own user");
            false
        }
        Err(e) => {
            tracing::warn!("Failed to check identity verification: {e}");
            false
        }
    };

    if !is_verified {
        tracing::info!("Device not cross-verified — prompting user");
        let _ = event_tx.send(MatrixEvent::DeviceUnverified).await;
    }
}

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
    path.push("matx");
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

/// Extract the mxc:// URL from a MediaSource.
fn media_source_url(source: &matrix_sdk::ruma::events::room::MediaSource) -> String {
    use matrix_sdk::ruma::events::room::MediaSource;
    match source {
        MediaSource::Plain(uri) => uri.to_string(),
        MediaSource::Encrypted(file) => file.url.to_string(),
    }
}

/// Extract body text and optional media info from a MessageType.
fn extract_message_content(
    msgtype: &matrix_sdk::ruma::events::room::message::MessageType,
) -> Option<(String, Option<MediaInfo>)> {
    use matrix_sdk::ruma::events::room::message::MessageType;
    match msgtype {
        MessageType::Text(text) => Some((text.body.clone(), None)),
        MessageType::Notice(notice) => Some((notice.body.clone(), None)),
        MessageType::Image(image) => {
            let url = media_source_url(&image.source);
            let size = image.info.as_ref().and_then(|i| i.size).map(|s| s.into());
            Some((
                image.body.clone(),
                Some(MediaInfo {
                    kind: MediaKind::Image,
                    filename: image.filename.clone().unwrap_or_else(|| image.body.clone()),
                    size,
                    url,
                }),
            ))
        }
        MessageType::Video(video) => {
            let url = media_source_url(&video.source);
            let size = video.info.as_ref().and_then(|i| i.size).map(|s| s.into());
            Some((
                video.body.clone(),
                Some(MediaInfo {
                    kind: MediaKind::Video,
                    filename: video.filename.clone().unwrap_or_else(|| video.body.clone()),
                    size,
                    url,
                }),
            ))
        }
        MessageType::Audio(audio) => {
            let url = media_source_url(&audio.source);
            let size = audio.info.as_ref().and_then(|i| i.size).map(|s| s.into());
            Some((
                audio.body.clone(),
                Some(MediaInfo {
                    kind: MediaKind::Audio,
                    filename: audio.body.clone(),
                    size,
                    url,
                }),
            ))
        }
        MessageType::File(file) => {
            let url = media_source_url(&file.source);
            let size = file.info.as_ref().and_then(|i| i.size).map(|s| s.into());
            Some((
                file.body.clone(),
                Some(MediaInfo {
                    kind: MediaKind::File,
                    filename: file.filename.clone().unwrap_or_else(|| file.body.clone()),
                    size,
                    url,
                }),
            ))
        }
        _ => None,
    }
}

/// Aggregate a list of emoji strings into (emoji, count) pairs using a HashMap.
fn aggregate_reactions(emojis: Option<&Vec<String>>) -> Vec<(String, u64)> {
    let Some(emojis) = emojis else {
        return Vec::new();
    };
    let mut counts: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for emoji in emojis {
        *counts.entry(emoji.as_str()).or_insert(0) += 1;
    }
    counts.into_iter().map(|(e, c)| (e.to_string(), c)).collect()
}

/// Resolve a user's display name from room membership, falling back to user ID.
async fn resolve_display_name(
    room: &matrix_sdk::room::Room,
    user_id: &matrix_sdk::ruma::UserId,
) -> String {
    room.get_member_no_sync(user_id)
        .await
        .ok()
        .flatten()
        .and_then(|m| m.display_name().map(|s| s.to_string()))
        .unwrap_or_else(|| user_id.to_string())
}


/// Fetch the timestamp of the most recent message for rooms where we
/// don't have one yet. Runs up to 20 requests in parallel.
async fn backfill_timestamps(client: &Client, rooms: &mut [RoomInfo]) {
    use futures_util::stream::{FuturesUnordered, StreamExt};

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

    tracing::info!("Backfilling timestamps for {} rooms", missing.len());
    let start = std::time::Instant::now();
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(20));

    let mut futures = FuturesUnordered::new();
    for &idx in &missing {
        let room_id_str = rooms[idx].room_id.clone();
        let client = client.clone();
        let sem = sem.clone();
        futures.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let room_id = match RoomId::parse(&room_id_str) {
                Ok(id) => id,
                Err(_) => return (idx, 0u64),
            };
            let Some(room) = client.get_room(&room_id) else {
                return (idx, 0u64);
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
            (idx, ts)
        }));
    }

    let mut filled = 0u32;
    while let Some(result) = futures.next().await {
        if let Ok((idx, ts)) = result {
            if ts > 0 {
                rooms[idx].last_activity_ts = ts;
                filled += 1;
            }
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

    // Step 1: Build a map of child_room_id → space_name by iterating spaces.
    let mut child_to_space: HashMap<String, String> = HashMap::new();
    let mut space_count = 0u32;
    for room in joined.iter() {
        if !room.is_space() {
            continue;
        }
        space_count += 1;

        let space_name = room
            .display_name()
            .await
            .ok()
            .map(|n| n.to_string())
            .unwrap_or_else(|| room.room_id().to_string());

        match room.get_state_events_static::<SpaceChildEventContent>().await {
            Ok(events) => {
                tracing::info!(
                    "Space '{}' has {} child state events",
                    space_name, events.len()
                );
                for raw_event in events {
                    match raw_event.deserialize() {
                        Ok(event) => {
                            let child_id = event.state_key().to_string();
                            child_to_space.insert(child_id, space_name.clone());
                        }
                        Err(e) => {
                            tracing::warn!("Failed to deserialize space child event: {e}");
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to get child events for space '{}': {e}", space_name);
            }
        }
    }

    tracing::info!(
        "Found {} spaces with {} child mappings",
        space_count, child_to_space.len()
    );

    // Step 2: Classify all rooms, including spaces.
    let cfg = crate::config::settings();
    let mut with_unread = Vec::new();
    let mut direct = Vec::new();
    let mut rest = Vec::new();
    let mut spaces = Vec::new();

    for room in joined.iter() {
        let room_id = room.room_id().to_string();
        let name = room
            .display_name()
            .await
            .ok()
            .map(|n| n.to_string())
            .unwrap_or_else(|| room_id.clone());
        let is_encrypted = room.is_encrypted().await.unwrap_or(false);
        let is_pinned = cfg.rooms.pinned_rooms.contains(&room_id);

        // Get last activity timestamp. Try recency_stamp first (Sliding Sync),
        // then latest_event filtered to real activity only. Rooms with only
        // state events get ts=0 here — the persistent disk cache and backfill
        // handle those separately.
        let last_activity_ts = room
            .recency_stamp()
            .or_else(|| {
                room.latest_event()
                    .and_then(|e| {
                        let raw = e.event().raw().json().get();
                        let v = serde_json::from_str::<serde_json::Value>(raw).ok()?;
                        if is_room_activity(&v) {
                            v.get("origin_server_ts")?.as_u64().map(|ms| ms / 1000)
                        } else {
                            None
                        }
                    })
            })
            .unwrap_or(0u64);

        tracing::debug!(
            "Room '{}' activity_ts={} recency={:?}",
            name, last_activity_ts, room.recency_stamp()
        );

        let unread = room.unread_notification_counts();

        // Check if room is tombstoned (upgraded to a new room).
        use matrix_sdk::ruma::events::room::tombstone::RoomTombstoneEventContent;
        let is_tombstoned = room
            .get_state_event_static::<RoomTombstoneEventContent>()
            .await
            .ok()
            .flatten()
            .is_some();

        let is_favourite = room.is_favourite();

        // Check if current user is admin (power level >= state_default).
        let is_admin = if let Some(user_id) = client.user_id() {
            room.get_member_no_sync(user_id)
                .await
                .ok()
                .flatten()
                .map(|m| m.power_level() >= 100)
                .unwrap_or(false)
        } else {
            false
        };

        if room.is_space() {
            spaces.push(RoomInfo {
                room_id,
                name,
                last_activity_ts,
                kind: RoomKind::Space,
                is_encrypted,
                parent_space: None,
                is_pinned,
                unread_count: unread.notification_count.into(),
                highlight_count: unread.highlight_count.into(),
                is_admin,
                is_tombstoned,
                is_favourite,
            });
            continue;
        }

        let is_dm = room.is_direct().await.unwrap_or(false);
        let parent_space = child_to_space.get(&room_id).cloned();

        let kind = if is_dm {
            RoomKind::DirectMessage
        } else {
            RoomKind::Room
        };

        let info = RoomInfo {
            room_id,
            name,
            last_activity_ts,
            kind,
            is_encrypted,
            parent_space,
            is_pinned,
            unread_count: unread.notification_count.into(),
            highlight_count: unread.highlight_count.into(),
            is_admin,
            is_tombstoned,
            is_favourite,
        };

        if unread.notification_count > 0 || unread.highlight_count > 0 {
            with_unread.push(info);
        } else if is_dm {
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

    // Cap DMs and ungrouped rooms. Space children are never truncated.
    direct.truncate(cfg.rooms.max_dms);
    ungrouped.truncate(cfg.rooms.max_rooms);

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
) {
    use matrix_sdk::ruma::events::room::message::{
        MessageType, OriginalSyncRoomMessageEvent,
    };

    let _ = event_tx.send(MatrixEvent::SyncStarted).await;

    // Persistent map of room_id → most recent message timestamp (seconds).
    // Loaded from disk so rooms appear in the right order immediately on
    // restart, without waiting for backfill.
    let room_timestamps: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
        std::sync::Arc::new(std::sync::Mutex::new(load_timestamp_cache()));

    // Load rooms from the local store immediately so the UI populates
    // without waiting for the first sync response from the server.
    let mut cached_rooms = collect_room_info(&client, Some(&room_timestamps)).await;
    if !cached_rooms.is_empty() {
        // Apply cached timestamps so rooms sort correctly from the start.
        // Fresh data from collect_room_info wins; cache is fallback only.
        {
            let mut ts_map = room_timestamps.lock().unwrap();
            for room in &mut cached_rooms {
                if room.last_activity_ts > 0 {
                    ts_map.insert(room.room_id.clone(), room.last_activity_ts);
                } else if let Some(&cached) = ts_map.get(&room.room_id) {
                    room.last_activity_ts = cached;
                }
            }
        }
        tracing::info!("Loaded {} rooms from local store", cached_rooms.len());
        let _ = event_tx
            .send(MatrixEvent::RoomListUpdated {
                rooms: cached_rooms,
            })
            .await;

        // Backfill timestamps in the background for rooms with ts=0.
        // Only sends an updated room list if new timestamps were found.
        let backfill_client = client.clone();
        let backfill_tx = event_tx.clone();
        let backfill_ts = room_timestamps.clone();
        tokio::spawn(async move {
            let mut rooms = collect_room_info(&backfill_client, Some(&backfill_ts)).await;
            let before_count = rooms.iter().filter(|r| r.last_activity_ts == 0).count();
            backfill_timestamps(&backfill_client, &mut rooms).await;
            // Merge backfilled timestamps into the persistent cache.
            {
                let mut ts_map = backfill_ts.lock().unwrap();
                for room in &mut rooms {
                    if room.last_activity_ts > 0 {
                        // Fresh data always wins — overwrite cache.
                        ts_map.insert(room.room_id.clone(), room.last_activity_ts);
                    } else if let Some(&cached) = ts_map.get(&room.room_id) {
                        // No fresh data — fall back to cache.
                        room.last_activity_ts = cached;
                    }
                }
                let room_ids: Vec<String> = rooms.iter().map(|r| r.room_id.clone()).collect();
                save_timestamp_cache(&ts_map, Some(&room_ids));
            }
            let after_count = rooms.iter().filter(|r| r.last_activity_ts == 0).count();
            // Only re-send if backfill actually discovered new timestamps.
            if after_count < before_count {
                let _ = backfill_tx
                    .send(MatrixEvent::RoomListUpdated { rooms })
                    .await;
            }
        });
    }

    // Register handlers for new messages (both decrypted and encrypted).
    // The SDK auto-decrypts when keys are available and fires the
    // RoomMessage handler. When decryption fails, the RoomEncrypted
    // handler fires instead.
    let msg_tx = event_tx.clone();
    let msg_client = client.clone();
    client.add_event_handler(
        move |event: OriginalSyncRoomMessageEvent,
              room: matrix_sdk::room::Room| {
            let tx = msg_tx.clone();
            let client = msg_client.clone();
            async move {
                let Some((body, media)) = extract_message_content(&event.content.msgtype) else {
                    return;
                };
                let timestamp = event
                    .origin_server_ts
                    .as_secs()
                    .into();

                let display_name = resolve_display_name(&room, &event.sender).await;

                // Check if this message mentions the current user.
                let is_mention = if let Some(user_id) = client.user_id() {
                    let uid = user_id.as_str();
                    let local = user_id.localpart();
                    body.contains(uid) || body.to_lowercase().contains(&local.to_lowercase())
                } else {
                    false
                };

                let room_name = room
                    .display_name()
                    .await
                    .ok()
                    .map(|n| n.to_string())
                    .unwrap_or_default();

                let sender_id = event.sender.to_string();
                let _ = tx
                    .send(MatrixEvent::NewMessage {
                        room_id: room.room_id().to_string(),
                        room_name,
                        sender_id: sender_id.clone(),
                        message: MessageInfo {
                            sender: display_name,
                            sender_id: sender_id.clone(),
                            body,
                            timestamp,
                            event_id: event.event_id.to_string(),
                            reply_to: None,
                            thread_root: None,
                            reactions: Vec::new(),
                            media,
                        },
                        is_mention,
                    })
                    .await;
            }
        },
    );

    // Handler for encrypted messages that couldn't be decrypted.
    use matrix_sdk::ruma::events::room::encrypted::OriginalSyncRoomEncryptedEvent;
    let enc_tx = event_tx.clone();
    client.add_event_handler(
        move |event: OriginalSyncRoomEncryptedEvent,
              room: matrix_sdk::room::Room| {
            let tx = enc_tx.clone();
            async move {
                let display_name = resolve_display_name(&room, &event.sender).await;

                let room_name = room
                    .display_name()
                    .await
                    .ok()
                    .map(|n| n.to_string())
                    .unwrap_or_default();

                let sender_id = event.sender.to_string();
                let _ = tx
                    .send(MatrixEvent::NewMessage {
                        room_id: room.room_id().to_string(),
                        room_name,
                        sender_id: sender_id.clone(),
                        message: MessageInfo {
                            sender: display_name,
                            sender_id: sender_id.clone(),
                            body: "\u{1f512} Unable to decrypt message".to_string(),
                            timestamp: event.origin_server_ts.as_secs().into(),
                            event_id: event.event_id.to_string(),
                            reply_to: None,
                            thread_root: None,
                            reactions: Vec::new(),
                            media: None,
                        },
                        is_mention: false,
                    })
                    .await;
            }
        },
    );

    // Sync loop with retry.
    // Send full room list only on the first sync response (initial sync),
    // not on every incremental sync — that was causing major slowness.
    loop {
        let tx = event_tx.clone();
        let sync_client = client.clone();
        let sync_shutdown = shutdown_rx.clone();
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

        let result = client
            .sync_with_callback(settings, move |response| {
                // Extract timestamps from the sync response for every room
                // that had timeline events. Only count real user messages,
                // not state events or bot notices.
                for (room_id, joined_room) in &response.rooms.join {
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
                async move {
                    // Refresh room list on initial sync and periodically after
                    // (throttled to at most once every 10 seconds to avoid lag).
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let prev = last_update.load(std::sync::atomic::Ordering::Relaxed);
                    let should_update = is_first || (now_secs - prev >= 10);

                    if should_update {
                        if is_first {
                            tracing::info!("Initial sync complete, collecting room list");
                        }
                        let mut rooms = collect_room_info(&client, Some(&timestamps)).await;
                        // Patch in timestamps from sync responses for rooms
                        // where latest_event() returned None.
                        // On initial sync, backfill any remaining rooms with ts=0.
                        if is_first {
                            backfill_timestamps(&client, &mut rooms).await;
                        }
                        // Merge: fresh sync data wins, cache is fallback only.
                        {
                            let mut ts_map = timestamps.lock().unwrap();
                            let mut patched = 0u32;
                            for room in &mut rooms {
                                if room.last_activity_ts > 0 {
                                    // Fresh data — update cache.
                                    ts_map.insert(room.room_id.clone(), room.last_activity_ts);
                                } else if let Some(&cached) = ts_map.get(&room.room_id) {
                                    // No fresh data — fall back to cache.
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
                        } // MutexGuard dropped here, before await.
                        let _ = tx.send(MatrixEvent::RoomListUpdated { rooms }).await;
                        last_update.store(now_secs, std::sync::atomic::Ordering::Relaxed);
                    }

                    if *shutdown.borrow_and_update() {
                        tracing::info!("Shutdown requested, stopping sync");
                        matrix_sdk::LoopCtrl::Break
                    } else {
                        matrix_sdk::LoopCtrl::Continue
                    }
                }
            })
            .await;

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
) -> Vec<MessageInfo> {
    use matrix_sdk::ruma::events::room::message::MessageType;

    let mut messages = Vec::new();
    let iter: Box<dyn Iterator<Item = &matrix_sdk::deserialized_responses::TimelineEvent>> =
        if reverse {
            Box::new(chunk.iter().rev())
        } else {
            Box::new(chunk.iter())
        };

    // First pass: collect reactions and replacements (edits).
    let mut reaction_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    // Map of original_event_id → (latest replacement body, replacement event_id).
    let mut replacement_map: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    // Set of event_ids that are replacement events (to skip in main loop).
    let mut replacement_event_ids: std::collections::HashSet<String> =
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
                    reaction_map.entry(target).or_default().push(emoji);
                }
                matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                    matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomMessage(
                        matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(msg),
                    ),
                ) => {
                    // Check if this is a replacement (edit) event.
                    use matrix_sdk::ruma::events::room::message::Relation;
                    if let Some(Relation::Replacement(replacement)) = &msg.content.relates_to {
                        let original_id = replacement.event_id.to_string();
                        let new_body = extract_message_content(&msg.content.msgtype)
                            .map(|(b, _)| b)
                            .unwrap_or_default();
                        replacement_map.insert(original_id, (new_body, msg.event_id.to_string()));
                        replacement_event_ids.insert(msg.event_id.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    for timeline_event in iter {
        let event = match timeline_event.raw().deserialize() {
            Ok(ev) => ev,
            Err(_) => {
                // Skip events that can't be deserialized (redacted,
                // unknown types, or corrupted). Don't show blank rows.
                continue;
            }
        };
        match event {
            matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomMessage(msg_event),
            ) => {
                let msg_event = match msg_event {
                    matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(orig) => orig,
                    _ => continue,
                };
                let event_id = msg_event.event_id.to_string();

                // Skip replacement events — they're edits displayed
                // via the original message with updated body.
                if replacement_event_ids.contains(&event_id) {
                    continue;
                }

                // Skip events that are replacements (relates_to = Replacement).
                use matrix_sdk::ruma::events::room::message::Relation;
                if matches!(&msg_event.content.relates_to, Some(Relation::Replacement(_))) {
                    continue;
                }

                let Some((mut body, media)) = extract_message_content(&msg_event.content.msgtype) else {
                    continue;
                };

                // Apply the latest edit if this message was replaced.
                if let Some((new_body, _)) = replacement_map.get(&event_id) {
                    body = new_body.clone();
                }

                let display_name = resolve_display_name(room, &msg_event.sender).await;

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

                // Aggregate reactions for this event.
                let reactions = aggregate_reactions(reaction_map.get(&event_id));

                messages.push(MessageInfo {
                    sender: display_name,
                    sender_id: msg_event.sender.to_string(),
                    body,
                    timestamp: msg_event.origin_server_ts.as_secs().into(),
                    event_id,
                    reply_to,
                    thread_root,
                    reactions,
                    media,
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
                // Skip replacement events, redacted events, and encrypted
                // events that are edits (check raw JSON for m.relates_to).
                if replacement_event_ids.contains(&event_id) || event_id.is_empty() {
                    continue;
                }
                // Check raw JSON for replacement relation (encrypted edits).
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
                messages.push(MessageInfo {
                    sender: sender.clone(),
                    sender_id: sender,
                    body: "\u{1f512} Unable to decrypt message".to_string(),
                    timestamp: 0,
                    event_id,
                    reply_to: None,
                    thread_root: None,
                    reactions: Vec::new(),
                    media: None,
                });
            }
            _ => continue,
        }
    }
    messages
}

/// Fetch recent messages for a room and send them to the UI.
/// `key_fetched_rooms` tracks room IDs for which we've already triggered a
/// key download from backup this session (to avoid redundant network calls).
/// This set contains only room ID strings, NOT encryption keys.
async fn handle_select_room(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    room_id: &str,
    key_fetched_rooms: &mut std::collections::HashSet<String>,
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

    // If the room is encrypted and we haven't fetched keys this session,
    // download room keys from backup so we can decrypt messages.
    if room.is_encrypted().await.unwrap_or(false)
        && !key_fetched_rooms.contains(&room_id.to_string())
    {
        let backups = client.encryption().backups();
        match backups.download_room_keys_for_room(&room_id).await {
            Ok(()) => tracing::debug!("Downloaded room keys for {room_id}"),
            Err(e) => tracing::debug!("Could not download room keys for {room_id}: {e}"),
        }
        key_fetched_rooms.insert(room_id.to_string());
        // Persist to disk.
        let path = {
            let mut p = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
            p.push("matx");
            p.push("key_fetched_rooms.json");
            p
        };
        if let Ok(json) = serde_json::to_string(key_fetched_rooms as &std::collections::HashSet<String>) {
            let _ = std::fs::write(&path, json);
        }
    }

    tracing::debug!("Fetching messages for {room_id}");

    // Filter the /messages API to only return message-like events, skipping
    // state events (membership churn) server-side. This avoids paginating
    // through hundreds of m.room.member events to find actual messages.
    use matrix_sdk::ruma::api::client::filter::RoomEventFilter;
    let mut msg_filter = RoomEventFilter::default();
    msg_filter.types = Some(vec![
        "m.room.message".to_string(),
        "m.room.encrypted".to_string(),
        "m.reaction".to_string(),
    ]);

    let mut options = matrix_sdk::room::MessagesOptions::backward();
    options.limit = UInt::from(50u32);
    options.filter = msg_filter;

    let (all_messages, prev_batch_token) = match room.messages(options).await {
        Ok(response) => {
            tracing::debug!("Got {} events for {room_id}", response.chunk.len());
            let msgs = extract_messages(&room, &response.chunk, true).await;
            let token = response.end.map(|t| t.to_string());

            // Send a read receipt for the most recent event to clear unread count.
            if let Some(latest) = response.chunk.first() {
                if let Ok(ev) = latest.raw().deserialize() {
                    let event_id = ev.event_id().to_owned();
                    if let Err(e) = room
                        .send_single_receipt(
                            matrix_sdk::ruma::api::client::receipt::create_receipt::v3::ReceiptType::Read,
                            matrix_sdk::ruma::events::receipt::ReceiptThread::Unthreaded,
                            event_id,
                        )
                        .await
                    {
                        tracing::debug!("Failed to send read receipt: {e}");
                    }
                }
            }

            (msgs, token)
        }
        Err(e) => {
            tracing::error!("Failed to fetch messages for {room_id}: {e}");
            (Vec::new(), None)
        }
    };

    // Collect room metadata for the content header.
    let topic = room
        .topic()
        .unwrap_or_default();

    let is_encrypted = room.is_encrypted().await.unwrap_or(false);
    let member_count = room.joined_members_count();

    // Check tombstone status.
    use matrix_sdk::ruma::events::room::tombstone::RoomTombstoneEventContent;
    let tombstone = room
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

    let (is_tombstoned, replacement_room, replacement_room_name) = match tombstone {
        Some(content) => {
            let rid = content.replacement_room.to_string();
            // Try to resolve the human-readable name from joined rooms.
            let name = client
                .get_room(&content.replacement_room)
                .and_then(|r| {
                    // Use cached display name (no network call).
                    r.cached_display_name().map(|n| n.to_string())
                });
            (true, Some(rid), name)
        }
        None => (false, None, None),
    };

    // Fetch pinned messages.
    use matrix_sdk::ruma::events::room::pinned_events::RoomPinnedEventsEventContent;
    let pinned_messages = match room
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
                // Fetch the actual event content for each pinned event (up to 5).
                let mut entries = Vec::new();
                for event_id in pinned_ids.iter().take(5) {
                    if let Ok(ev) = room.event(event_id, None).await {
                        if let Ok(timeline_ev) = ev.raw().deserialize() {
                            if let matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                                matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomMessage(
                                    matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(msg),
                                ),
                            ) = timeline_ev
                            {
                                let Some((body, _media)) = extract_message_content(&msg.content.msgtype) else {
                                    continue;
                                };
                                let sender = resolve_display_name(&room, &msg.sender).await;
                                entries.push((sender, body));
                            }
                        }
                    }
                }
                entries
            } else {
                vec![]
            }
        }
        _ => vec![],
    };

    // Fetch room members for nick completion (lazy-loaded, so this
    // may only return members we've seen in timeline events).
    let members: Vec<(String, String)> = room
        .members_no_sync(matrix_sdk::RoomMemberships::ACTIVE)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            let uid = m.user_id().to_string();
            let name = m.display_name().map(|n| n.to_string()).unwrap_or_else(|| uid.clone());
            (uid, name)
        })
        .collect();

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
    };

    tracing::debug!("Sending {} messages to UI for {room_id}", all_messages.len());
    let _ = event_tx
        .send(MatrixEvent::RoomMessages {
            room_id: room_id.to_string(),
            messages: all_messages,
            prev_batch_token,
            room_meta,
        })
        .await;
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
    msg_filter.types = Some(vec![
        "m.room.message".to_string(),
        "m.room.encrypted".to_string(),
        "m.reaction".to_string(),
    ]);
    let mut options = matrix_sdk::room::MessagesOptions::backward();
    options.limit = UInt::from(50u32);
    options.from = Some(from_token.to_string());
    options.filter = msg_filter;

    let (messages, prev_batch_token) = match room.messages(options).await {
        Ok(response) => {
            let msgs = extract_messages(&room, &response.chunk, true).await;
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
) {
    use matrix_sdk::media::MediaFormat;

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
                    let tmp_dir = std::env::temp_dir().join("matx-media");
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

    let Ok(uri) = <&matrix_sdk::ruma::MxcUri>::try_from(mxc_url) else {
        tracing::error!("Invalid mxc URL: {mxc_url}");
        return;
    };

    let request = matrix_sdk::media::MediaRequestParameters {
        source: matrix_sdk::ruma::events::room::MediaSource::Plain(uri.to_owned()),
        format: MediaFormat::File,
    };

    match client.media().get_media_content(&request, true).await {
        Ok(data) => {
            // Write to temp file with the original filename.
            let tmp_dir = std::env::temp_dir().join("matx-media");
            let _ = std::fs::create_dir_all(&tmp_dir);
            let path = tmp_dir.join(filename);
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

async fn handle_send_message(
    client: &Client,
    room_id: &str,
    body: &str,
    reply_to: Option<&str>,
    quote_text: Option<&(String, String)>,
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

    // Build body with quote fallback if replying.
    let full_body = if let Some((quote_sender, quote_body)) = quote_text {
        // Matrix fallback format: "> <sender> original\n\nreply"
        let quoted_lines: String = quote_body
            .lines()
            .map(|l| format!("> {l}\n"))
            .collect();
        format!("> <{quote_sender}>\n{quoted_lines}\n{body}")
    } else {
        body.to_string()
    };

    let mut content = RoomMessageEventContent::text_plain(&full_body);

    // If replying, set the in_reply_to relation.
    if let Some(reply_event_id) = reply_to {
        if let Ok(eid) = matrix_sdk::ruma::EventId::parse(reply_event_id) {
            content.relates_to = Some(
                matrix_sdk::ruma::events::room::message::Relation::Reply {
                    in_reply_to: matrix_sdk::ruma::events::relation::InReplyTo::new(eid.to_owned()),
                },
            );
        }
    }

    if let Err(e) = room.send(content).await {
        tracing::error!("Failed to send message to {room_id}: {e}");
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

/// Import secrets from server-side secret storage using a recovery key/passphrase.
/// This gives us access to the backup decryption key, enabling room key download.
async fn handle_recover_keys(client: &Client, event_tx: &Sender<MatrixEvent>, recovery_key: &str) {
    let recovery = client.encryption().recovery();

    tracing::info!("Attempting key recovery...");
    match recovery.recover(recovery_key).await {
        Ok(()) => {
            tracing::info!("Recovery successful! Room keys should now be available.");
            let _ = event_tx.send(MatrixEvent::RecoveryComplete).await;
        }
        Err(e) => {
            tracing::error!("Recovery failed: {e}");
            let _ = event_tx
                .send(MatrixEvent::RecoveryFailed {
                    error: e.to_string(),
                })
                .await;
        }
    }
}

async fn handle_browse_public_rooms(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    search_term: Option<&str>,
) {
    use matrix_sdk::ruma::api::client::directory::get_public_rooms_filtered;
    use matrix_sdk::ruma::directory::Filter;

    let mut request = get_public_rooms_filtered::v3::Request::new();
    request.limit = Some(matrix_sdk::ruma::UInt::from(50u32));
    if let Some(term) = search_term {
        if !term.is_empty() {
            request.filter = Filter::new();
            request.filter.generic_search_term = Some(term.to_owned());
        }
    }

    match client.send(request).await {
        Ok(response) => {
            let joined_rooms: std::collections::HashSet<String> = client
                .joined_rooms()
                .iter()
                .map(|r| r.room_id().to_string())
                .collect();

            let rooms: Vec<SpaceDirectoryRoom> = response
                .chunk
                .into_iter()
                .map(|r| SpaceDirectoryRoom {
                    already_joined: joined_rooms.contains(&r.room_id.to_string()),
                    room_id: r.room_id.to_string(),
                    name: r.name.unwrap_or_else(|| {
                        r.canonical_alias
                            .map(|a| a.to_string())
                            .unwrap_or_else(|| r.room_id.to_string())
                    }),
                    topic: r.topic.unwrap_or_default(),
                    member_count: r.num_joined_members.into(),
                })
                .collect();

            let _ = event_tx
                .send(MatrixEvent::PublicRoomDirectory { rooms })
                .await;
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

            let rooms: Vec<SpaceDirectoryRoom> = response
                .rooms
                .into_iter()
                .filter(|r| r.room_id != room_id) // Skip the space itself
                .map(|r| SpaceDirectoryRoom {
                    already_joined: joined_rooms.contains(&r.room_id.to_string()),
                    room_id: r.room_id.to_string(),
                    name: r.name.unwrap_or_else(|| r.room_id.to_string()),
                    topic: r.topic.unwrap_or_default(),
                    member_count: r.num_joined_members.into(),
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

async fn handle_join_room(client: &Client, event_tx: &Sender<MatrixEvent>, room_id_or_alias: &str) {
    use matrix_sdk::ruma::RoomOrAliasId;

    let Ok(id) = RoomOrAliasId::parse(room_id_or_alias) else {
        let _ = event_tx
            .send(MatrixEvent::JoinFailed {
                error: format!("Invalid room ID or alias: {room_id_or_alias}"),
            })
            .await;
        return;
    };

    match client.join_room_by_id_or_alias(&id, &[]).await {
        Ok(room) => {
            let room_id = room.room_id().to_string();
            let room_name = room
                .cached_display_name()
                .map(|n| n.to_string())
                .unwrap_or_else(|| room_id.clone());
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
