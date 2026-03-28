// Encryption — E2EE setup, key backup, and recovery.
//
// Centralises all encryption lifecycle logic that was previously scattered
// across client.rs.  The Matrix client calls into these functions; they
// communicate results back via the shared MatrixEvent channel.

use async_channel::Sender;
use matrix_sdk::ruma::api::client::uiaa::{AuthData, Password, UserIdentifier};
use matrix_sdk::Client;

use super::{MatrixEvent, ROOM_LOAD_IN_PROGRESS};
use super::room_cache::RoomCache;

async fn yield_if_room_loading() {
    while ROOM_LOAD_IN_PROGRESS.load(std::sync::atomic::Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Bootstrap cross-signing and check key backup state.
///
/// Called once after login.  `login_creds` carries the (username, password)
/// from a fresh interactive login so we can satisfy the UIAA challenge that
/// new accounts require when uploading cross-signing keys for the first time.
/// For restored sessions it is `None` — existing accounts with keys already on
/// the server don't need UIAA, and brand-new accounts on a restored session
/// are rare enough that we emit `CrossSigningNeedsPassword` instead.
///
/// Deliberately does NOT auto-create a new backup — creating one would orphan
/// every other client's recovery key.  If no backup is connected the user is
/// prompted to run "Recover Keys".
pub(super) async fn setup_encryption(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    login_creds: Option<(String, String)>,
) {
    let enc = client.encryption();

    // First attempt without auth.  For existing accounts this succeeds
    // immediately (identity already on server, nothing to do).  For brand-new
    // accounts the server returns a UIAA 401 so we retry with the password.
    let mut was_bootstrapped = false;
    match enc.bootstrap_cross_signing_if_needed(None).await {
        Ok(()) => tracing::info!("Cross-signing ready (identity already existed)"),
        Err(e) => {
            if let Some(uiaa) = e.as_uiaa_response() {
                if let Some((username, password)) = login_creds {
                    let mut pw = Password::new(
                        UserIdentifier::UserIdOrLocalpart(username),
                        password,
                    );
                    pw.session = uiaa.session.clone();
                    match enc.bootstrap_cross_signing_if_needed(Some(AuthData::Password(pw))).await {
                        Ok(()) => {
                            tracing::info!("Cross-signing bootstrapped (new account, UIAA satisfied)");
                            was_bootstrapped = true;
                        }
                        Err(e2) => tracing::warn!("Cross-signing bootstrap failed with auth: {e2}"),
                    }
                } else {
                    // Restored session + no identity = account was created but
                    // cross-signing was never set up.  User needs to re-login.
                    tracing::warn!("Cross-signing needs UIAA but no password available");
                    let _ = event_tx.send(MatrixEvent::CrossSigningNeedsPassword).await;
                }
            } else {
                tracing::warn!("Cross-signing bootstrap skipped: {e}");
            }
        }
    }

    let backups = enc.backups();
    let backup_enabled = backups.are_enabled().await;
    let backup_on_server = backups.fetch_exists_on_server().await.unwrap_or(false);
    tracing::info!("Backup state: enabled={backup_enabled}, on_server={backup_on_server}");
    if !backup_enabled {
        tracing::info!("Key backup not connected — user must use Recover Keys");
        let _ = event_tx.send(MatrixEvent::BackupVersionMismatch).await;
    }

    let user_id = client.user_id().expect("must be logged in");
    let is_verified = match enc.get_user_identity(user_id).await {
        Ok(Some(identity)) => {
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

    if was_bootstrapped {
        // New account: we just created the cross-signing keys; device is now
        // self-signed.  No verification prompt needed — show a success hint instead.
        tracing::info!("Encryption bootstrapped — sending CrossSigningBootstrapped");
        let _ = event_tx.send(MatrixEvent::CrossSigningBootstrapped).await;
    } else if !is_verified {
        tracing::info!("Device not cross-verified — prompting user");
        let _ = event_tx.send(MatrixEvent::DeviceUnverified).await;
    }
}

/// Import secrets from server-side secret storage using a recovery key or
/// passphrase.  On success, kicks off a background bulk key download so UTD
/// messages in all encrypted rooms decrypt without the user opening each one.
pub(super) async fn handle_recover_keys(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    recovery_key: &str,
    cache: &RoomCache,
) {
    let recovery = client.encryption().recovery();
    let backups = client.encryption().backups();

    tracing::info!("Recovery: starting (decrypting SSSS with passphrase/key)...");
    let _ = event_tx.send(MatrixEvent::RecoveryStarted).await;
    match recovery.recover(recovery_key).await {
        Ok(()) => {
            let backup_ready = backups.are_enabled().await;
            if backup_ready {
                tracing::info!("Recovery successful — backup connected, downloading room keys.");
                let dl_client = client.clone();
                let dl_cache = cache.clone();
                let dl_tx = event_tx.clone();
                tokio::spawn(async move {
                    use futures_util::StreamExt;
                    let backups = dl_client.encryption().backups();
                    let rooms = dl_client.joined_rooms();
                    let mut encrypted = Vec::new();
                    for room in rooms {
                        if room.is_encrypted().await.unwrap_or(false) {
                            encrypted.push(room);
                        }
                    }
                    tracing::info!(
                        "Downloading keys for {} encrypted rooms after recovery",
                        encrypted.len()
                    );
                    futures_util::stream::iter(encrypted)
                        .for_each_concurrent(1, |room| {
                            let backups = backups.clone();
                            async move {
                                // Sequential (concurrency=1) so yield_if_room_loading()
                                // can interrupt between rooms without write-lock bursts.
                                yield_if_room_loading().await;
                                if let Err(e) =
                                    backups.download_room_keys_for_room(room.room_id()).await
                                {
                                    tracing::warn!(
                                        "Key download failed for {}: {e}",
                                        room.room_id()
                                    );
                                }
                            }
                        })
                        .await;
                    // Clear message cache — cached rooms may have UTD messages
                    // that can now decrypt on the next fresh fetch.
                    dl_cache.clear_memory();
                    tracing::info!("Post-recovery key download complete — message cache cleared");
                    let _ = dl_tx.send(MatrixEvent::RoomKeysReceived { room_ids: vec![] }).await;
                });
            } else {
                tracing::warn!(
                    "Recovery ran but backup is not enabled. \
                     The recovery key may not match the current backup version on the server."
                );
            }
            let _ = event_tx
                .send(MatrixEvent::RecoveryComplete { backup_connected: backup_ready })
                .await;
        }
        Err(e) => {
            tracing::error!("Recovery failed: {e}");
            let _ = event_tx
                .send(MatrixEvent::RecoveryFailed { error: e.to_string() })
                .await;
        }
    }
}

/// Handle the case where a stale empty backup version exists on the server
/// (created by a previous session when it shouldn't have been).
///
/// 1. Queries the current backup version.
/// 2. Deletes the stale version (safe — no other client ever used it).
/// 3. Signals the UI to prompt the user to click Recover Keys once more.
pub(super) async fn handle_download_from_ssss_backup(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
) {
    use matrix_sdk::ruma::api::client::backup::{
        delete_backup_version, get_latest_backup_info,
    };

    tracing::info!("Querying current backup version from server...");

    let current_version = match client
        .send(get_latest_backup_info::v3::Request::new())
        .await
    {
        Ok(resp) => resp.version,
        Err(e) => {
            tracing::warn!("No current backup on server (or error): {e}");
            run_recover_and_download(client, event_tx).await;
            return;
        }
    };

    tracing::info!(
        "Current server backup version: {current_version} — \
         deleting (it's the stale empty one we created)"
    );

    let del_req = delete_backup_version::v3::Request::new(current_version.clone());
    match client.send(del_req).await {
        Ok(_) => tracing::info!("Deleted stale backup version {current_version}"),
        Err(e) => tracing::warn!(
            "Could not delete stale backup version: {e} — trying recover anyway"
        ),
    }

    run_recover_and_download(client, event_tx).await;
}

async fn run_recover_and_download(client: &Client, event_tx: &Sender<MatrixEvent>) {
    // The SSSS key is already in SQLite from the first recover() call.
    // Emit StaleBackupDeleted so the UI prompts the user to click Recover Keys
    // one more time — the second attempt will succeed now that the stale
    // version has been removed and the original is current.
    let _ = client; // suppress unused warning — kept for potential future use
    tracing::info!(
        "Stale backup removed — please use Recover Keys one more time \
         to connect to the original backup."
    );
    let _ = event_tx.send(MatrixEvent::StaleBackupDeleted).await;
}

/// Import room keys from an export file (legacy key export format).
pub(super) async fn handle_import_room_keys(
    client: &Client,
    event_tx: &Sender<MatrixEvent>,
    path: std::path::PathBuf,
    passphrase: &str,
    cache: &RoomCache,
) {
    tracing::info!("Importing room keys from {:?}", path);
    match client.encryption().import_room_keys(path, passphrase).await {
        Ok(result) => {
            let imported = result.imported_count as u64;
            let total = result.total_count as u64;
            tracing::info!("Imported {imported}/{total} room keys from file");
            cache.clear_memory();
            let _ = event_tx.send(MatrixEvent::KeysImported { imported, total }).await;
            let _ = event_tx.send(MatrixEvent::RoomKeysReceived { room_ids: vec![] }).await;
        }
        Err(e) => {
            tracing::error!("Key import failed: {e}");
            let _ = event_tx.send(MatrixEvent::KeyImportFailed { error: e.to_string() }).await;
        }
    }
}

/// Spawn a background task that watches for newly imported room keys and
/// notifies the UI to re-fetch affected rooms so UTD messages re-render.
pub(super) fn spawn_keys_watcher(
    client: &Client,
    event_tx: Sender<MatrixEvent>,
    cache: super::room_cache::RoomCache,
) {
    use futures_util::StreamExt;
    let keys_client = client.clone();
    tokio::spawn(async move {
        let Some(mut stream) = keys_client.encryption().room_keys_received_stream().await else {
            return;
        };
        while let Some(Ok(infos)) = stream.next().await {
            if infos.is_empty() {
                continue;
            }
            let room_ids: Vec<String> = {
                let mut seen = std::collections::HashSet::new();
                infos
                    .iter()
                    .map(|i| i.room_id.to_string())
                    .filter(|id| seen.insert(id.clone()))
                    .collect()
            };
            tracing::info!("New room keys for {} room(s): {:?}", room_ids.len(), room_ids);
            // Evict these rooms from the timeline cache so the next SelectRoom
            // (or the RoomKeysReceived handler for the current room) forces a
            // full bg_refresh with two-phase decryption instead of serving the
            // stale UTD messages from the warm cache.
            for id in &room_ids {
                cache.invalidate_room(id);
            }
            let _ = event_tx.send(MatrixEvent::RoomKeysReceived { room_ids }).await;
        }
    });
}
