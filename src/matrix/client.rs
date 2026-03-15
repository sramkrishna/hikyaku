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
}

/// A single message sent to the UI.
#[derive(Debug, Clone)]
pub struct MessageInfo {
    pub sender: String,
    pub body: String,
    pub timestamp: u64,
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
    LoginSuccess { display_name: String },
    LoginFailed { error: String },
    SyncStarted,
    SyncError { error: String },
    RoomListUpdated { rooms: Vec<RoomInfo> },
    RoomMessages {
        room_id: String,
        messages: Vec<MessageInfo>,
    },
    NewMessage {
        room_id: String,
        message: MessageInfo,
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
    /// Verification was cancelled.
    VerificationCancelled { flow_id: String, reason: String },
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
    },
    /// Accept an incoming verification request and start SAS.
    AcceptVerification { flow_id: String },
    /// Confirm that the displayed emojis match.
    ConfirmVerification { flow_id: String },
    /// Cancel a verification.
    CancelVerification { flow_id: String },
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
                            let _ = event_tx.send(MatrixEvent::LoginSuccess { display_name }).await;
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
    setup_encryption(&client).await;

    // Spawn sync in a separate task so we can keep processing commands.
    let sync_event_tx = event_tx.clone();
    let sync_client = client.clone();
    let sync_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        start_sync(sync_client, &sync_event_tx, sync_shutdown).await;
    });

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
                        handle_select_room(&client, &event_tx, &room_id).await;
                    }
                    Ok(MatrixCommand::SendMessage { room_id, body }) => {
                        handle_send_message(&client, &room_id, &body).await;
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

        let _ = event_tx
            .send(MatrixEvent::LoginSuccess { display_name })
            .await;
        Some(client)
    } else {
        tracing::info!("Stored session is invalid, cleaning up");
        cleanup_session(&persisted.homeserver).await;
        None
    }
}


/// Bootstrap cross-signing and enable key backup so we can decrypt
/// messages in encrypted rooms.
async fn setup_encryption(client: &Client) {
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

    // Enable key backup if not already enabled, so room keys are
    // uploaded and available for restore on other devices.
    let backups = enc.backups();
    if !backups.are_enabled().await {
        if backups.exists_on_server().await.unwrap_or(false) {
            tracing::info!("Key backup exists on server, recovery needed to access keys");
        } else {
            match backups.create().await {
                Ok(()) => tracing::info!("Key backup created"),
                Err(e) => tracing::warn!("Failed to create key backup: {e}"),
            }
        }
    } else {
        tracing::info!("Key backup already enabled");
    }
}

/// Collect current room info from the client's joined rooms.
///
/// First builds a map of space → child room IDs by iterating spaces
/// (typically ~20) and reading their m.space.child state events. Then
/// classifies each non-space room as DM or Room, attaching the parent
/// space name so the UI can group them.
async fn collect_room_info(client: &Client) -> Vec<RoomInfo> {
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

    // Step 2: Classify all non-space rooms.
    let cfg = crate::config::settings();
    let mut with_unread = Vec::new();
    let mut direct = Vec::new();
    let mut rest = Vec::new();

    for room in joined.iter() {
        if room.is_space() {
            continue;
        }

        let room_id = room.room_id().to_string();
        let name = room
            .display_name()
            .await
            .ok()
            .map(|n| n.to_string())
            .unwrap_or_else(|| room_id.clone());
        let unread = room.unread_notification_counts();
        let is_dm = room.is_direct().await.unwrap_or(false);
        let is_encrypted = room.is_encrypted().await.unwrap_or(false);
        let parent_space = child_to_space.get(&room_id).cloned();
        if let Some(ref space) = parent_space {
            tracing::info!("Room '{}' belongs to space '{}'", name, space);
        }

        let kind = if is_dm {
            RoomKind::DirectMessage
        } else {
            RoomKind::Room
        };

        // Get last activity timestamp from the latest event in the room.
        let last_activity_ts = room
            .latest_event()
            .and_then(|e| e.event().raw().deserialize().ok())
            .map(|e: matrix_sdk::ruma::events::AnySyncTimelineEvent| {
                e.origin_server_ts().as_secs().into()
            })
            .unwrap_or(0u64);

        let is_pinned = cfg.rooms.pinned_rooms.contains(&room_id);

        let info = RoomInfo {
            room_id,
            name,
            last_activity_ts,
            kind,
            is_encrypted,
            parent_space,
            is_pinned,
        };

        if unread.notification_count > 0 || unread.highlight_count > 0 {
            with_unread.push(info);
        } else if is_dm {
            direct.push(info);
        } else {
            rest.push(info);
        }
    }

    tracing::info!(
        "Room buckets: {} with unread, {} DMs, {} other rooms, {} spaces (of {} total joined)",
        with_unread.len(), direct.len(), rest.len(), space_count, total
    );

    // Cap each category so DMs don't crowd out rooms.
    direct.truncate(cfg.rooms.max_dms);
    rest.truncate(cfg.rooms.max_rooms);

    // Combine: unread first, then DMs, then rooms.
    let mut rooms = Vec::with_capacity(with_unread.len() + direct.len() + rest.len());
    rooms.extend(with_unread);
    rooms.extend(direct);
    rooms.extend(rest);

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

    // Load rooms from the local store immediately so the UI populates
    // without waiting for the first sync response from the server.
    let cached_rooms = collect_room_info(&client).await;
    if !cached_rooms.is_empty() {
        tracing::info!("Loaded {} rooms from local store", cached_rooms.len());
        let _ = event_tx
            .send(MatrixEvent::RoomListUpdated { rooms: cached_rooms })
            .await;
    }

    // Register a handler for new messages.
    let msg_tx = event_tx.clone();
    client.add_event_handler(
        move |event: OriginalSyncRoomMessageEvent,
              room: matrix_sdk::room::Room| {
            let tx = msg_tx.clone();
            async move {
                let body = match &event.content.msgtype {
                    MessageType::Text(text) => text.body.clone(),
                    _ => return,
                };
                let sender = event.sender.to_string();
                let timestamp = event
                    .origin_server_ts
                    .as_secs()
                    .into();

                let _ = tx
                    .send(MatrixEvent::NewMessage {
                        room_id: room.room_id().to_string(),
                        message: MessageInfo {
                            sender,
                            body,
                            timestamp,
                        },
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
        let result = client
            .sync_with_callback(settings, move |_response| {
                let tx = tx.clone();
                let client = sync_client.clone();
                let mut shutdown = sync_shutdown.clone();
                let is_first = !initial_flag.swap(true, std::sync::atomic::Ordering::Relaxed);
                async move {
                    // Only collect the full room list on initial sync.
                    if is_first {
                        tracing::info!("Initial sync complete, collecting room list");
                        let rooms = collect_room_info(&client).await;
                        let _ = tx.send(MatrixEvent::RoomListUpdated { rooms }).await;
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

/// Fetch recent messages for a room and send them to the UI.
async fn handle_select_room(client: &Client, event_tx: &Sender<MatrixEvent>, room_id: &str) {
    use matrix_sdk::ruma::events::room::message::MessageType;
    use matrix_sdk::ruma::UInt;

    let Ok(room_id) = RoomId::parse(room_id) else {
        tracing::error!("Invalid room ID: {room_id}");
        return;
    };

    let Some(room) = client.get_room(&room_id) else {
        tracing::error!("Room not found: {room_id}");
        return;
    };

    tracing::debug!("Fetching messages for {room_id}");

    let mut messages = Vec::new();

    // Fetch the last 50 messages. Use a small limit to keep it fast.
    let mut options = matrix_sdk::room::MessagesOptions::backward();
    options.limit = UInt::from(50u32);

    match room.messages(options).await {
        Ok(response) => {
            tracing::debug!("Got {} events for {room_id}", response.chunk.len());
            for timeline_event in response.chunk.iter().rev() {
                let event = match timeline_event.raw().deserialize() {
                    Ok(ev) => ev,
                    Err(_) => {
                        // Likely an encrypted event we can't decrypt (UTD).
                        messages.push(MessageInfo {
                            sender: String::new(),
                            body: "\u{1f512} Unable to decrypt message".to_string(),
                            timestamp: 0,
                        });
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
                        let body = match &msg_event.content.msgtype {
                            MessageType::Text(text) => text.body.clone(),
                            _ => continue,
                        };
                        messages.push(MessageInfo {
                            sender: msg_event.sender.to_string(),
                            body,
                            timestamp: msg_event.origin_server_ts.as_secs().into(),
                        });
                    }
                    matrix_sdk::ruma::events::AnySyncTimelineEvent::MessageLike(
                        matrix_sdk::ruma::events::AnySyncMessageLikeEvent::RoomEncrypted(enc),
                    ) => {
                        // Encrypted event that wasn't decrypted — missing keys.
                        let sender = match &enc {
                            matrix_sdk::ruma::events::SyncMessageLikeEvent::Original(o) =>
                                o.sender.to_string(),
                            _ => String::new(),
                        };
                        messages.push(MessageInfo {
                            sender,
                            body: "\u{1f512} Unable to decrypt message".to_string(),
                            timestamp: 0,
                        });
                    }
                    _ => continue,
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to fetch messages for {room_id}: {e}");
        }
    }

    tracing::debug!("Sending {} messages to UI for {room_id}", messages.len());
    let _ = event_tx
        .send(MatrixEvent::RoomMessages {
            room_id: room_id.to_string(),
            messages,
        })
        .await;
}

/// Send a text message to a room.
async fn handle_send_message(client: &Client, room_id: &str, body: &str) {
    use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;

    let Ok(room_id) = RoomId::parse(room_id) else {
        tracing::error!("Invalid room ID: {room_id}");
        return;
    };

    let Some(room) = client.get_room(&room_id) else {
        tracing::error!("Room not found: {room_id}");
        return;
    };

    let content = RoomMessageEventContent::text_plain(body);
    if let Err(e) = room.send(content).await {
        tracing::error!("Failed to send message to {room_id}: {e}");
    }
}
