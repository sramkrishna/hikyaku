// Verification — handles the interactive device verification flow.
//
// When another device (e.g., Element) requests verification, we:
// 1. Notify the UI of the incoming request
// 2. On user acceptance, start SAS (emoji) verification
// 3. Show 7 emojis for the user to compare with the other device
// 4. On confirmation, complete verification → device is now trusted
//    and can decrypt E2EE messages

use async_channel::Sender;
use matrix_sdk::encryption::verification::{
    SasState, SasVerification, VerificationRequest,
    VerificationRequestState,
};
use matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEvent;
use matrix_sdk::Client;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::client::{MatrixEvent, VerificationEmoji};

/// Stores active verification requests/SAS sessions so we can look them
/// up when the user responds from the UI.
pub struct VerificationState {
    pub requests: HashMap<String, VerificationRequest>,
    pub sas_sessions: HashMap<String, SasVerification>,
}

impl VerificationState {
    pub fn new() -> Self {
        Self {
            requests: HashMap::new(),
            sas_sessions: HashMap::new(),
        }
    }
}

pub type SharedVerificationState = Arc<Mutex<VerificationState>>;

/// Register event handlers for incoming verification requests.
pub fn register_verification_handlers(
    client: &Client,
    event_tx: Sender<MatrixEvent>,
    state: SharedVerificationState,
) {
    // Handle to-device verification requests (direct device-to-device).
    let tx = event_tx.clone();
    let vs = state.clone();
    client.add_event_handler(
        move |event: ToDeviceKeyVerificationRequestEvent, client: Client| {
            let tx = tx.clone();
            let vs = vs.clone();
            async move {
                let flow_id = event.content.transaction_id.to_string();
                let other_user = event.sender.to_string();

                tracing::info!(
                    "Incoming verification request from {other_user}, flow_id: {flow_id}"
                );

                // Get the verification request object from the client.
                if let Some(request) =
                    client.encryption().get_verification_request(&event.sender, &flow_id).await
                {
                    let other_device = match request.state() {
                        VerificationRequestState::Requested {
                            other_device_data, ..
                        } => other_device_data.device_id().to_string(),
                        _ => "unknown".to_string(),
                    };

                    vs.lock().await.requests.insert(flow_id.clone(), request);

                    let _ = tx
                        .send(MatrixEvent::VerificationRequest {
                            flow_id,
                            other_user,
                            other_device,
                        })
                        .await;
                }
            }
        },
    );
}

/// Accept a verification request and start SAS verification.
pub async fn accept_verification(
    state: &SharedVerificationState,
    event_tx: &Sender<MatrixEvent>,
    flow_id: &str,
) {
    let request = {
        let vs = state.lock().await;
        vs.requests.get(flow_id).cloned()
    };

    let Some(request) = request else {
        tracing::error!("No verification request found for flow_id: {flow_id}");
        return;
    };

    // Accept the request.
    if let Err(e) = request.accept().await {
        tracing::error!("Failed to accept verification: {e}");
        return;
    }

    // Start SAS verification.
    match request.start_sas().await {
        Ok(Some(sas)) => {
            tracing::info!("SAS verification started for {flow_id}");

            // Accept the SAS.
            if let Err(e) = sas.accept().await {
                tracing::error!("Failed to accept SAS: {e}");
                return;
            }

            // Watch for state changes.
            let tx = event_tx.clone();
            let vs = state.clone();
            let fid = flow_id.to_string();
            tokio::spawn(async move {
                watch_sas_state(sas, &tx, &vs, &fid).await;
            });
        }
        Ok(None) => {
            tracing::warn!("SAS verification not available for {flow_id}");
        }
        Err(e) => {
            tracing::error!("Failed to start SAS: {e}");
        }
    }
}

/// Watch a SAS verification session for state changes and notify the UI.
async fn watch_sas_state(
    sas: SasVerification,
    event_tx: &Sender<MatrixEvent>,
    state: &SharedVerificationState,
    flow_id: &str,
) {
    use futures_util::StreamExt;

    // Store the SAS session so the UI can confirm/cancel it.
    state
        .lock()
        .await
        .sas_sessions
        .insert(flow_id.to_string(), sas.clone());

    let mut stream = sas.changes();
    while let Some(sas_state) = stream.next().await {
        match sas_state {
            SasState::KeysExchanged { emojis, .. } => {
                if let Some(emojis) = emojis {
                    let emoji_list: Vec<VerificationEmoji> = emojis
                        .emojis
                        .iter()
                        .map(|e| VerificationEmoji {
                            symbol: e.symbol.to_string(),
                            description: e.description.to_string(),
                        })
                        .collect();

                    let _ = event_tx
                        .send(MatrixEvent::VerificationEmojis {
                            flow_id: flow_id.to_string(),
                            emojis: emoji_list,
                        })
                        .await;
                }
            }
            SasState::Done { .. } => {
                tracing::info!("Verification {flow_id} completed successfully!");
                let _ = event_tx
                    .send(MatrixEvent::VerificationDone {
                        flow_id: flow_id.to_string(),
                    })
                    .await;

                // Clean up.
                let mut vs = state.lock().await;
                vs.requests.remove(flow_id);
                vs.sas_sessions.remove(flow_id);
                break;
            }
            SasState::Cancelled(info) => {
                let reason = info.reason().to_string();
                tracing::warn!("Verification {flow_id} cancelled: {reason}");
                let _ = event_tx
                    .send(MatrixEvent::VerificationCancelled {
                        flow_id: flow_id.to_string(),
                        reason,
                    })
                    .await;

                let mut vs = state.lock().await;
                vs.requests.remove(flow_id);
                vs.sas_sessions.remove(flow_id);
                break;
            }
            _ => {}
        }
    }
}

/// Confirm that the emojis match.
pub async fn confirm_verification(state: &SharedVerificationState, flow_id: &str) {
    let sas = {
        let vs = state.lock().await;
        vs.sas_sessions.get(flow_id).cloned()
    };

    if let Some(sas) = sas {
        if let Err(e) = sas.confirm().await {
            tracing::error!("Failed to confirm verification: {e}");
        }
    } else {
        tracing::error!("No SAS session found for flow_id: {flow_id}");
    }
}

/// Cancel a verification.
pub async fn cancel_verification(state: &SharedVerificationState, flow_id: &str) {
    let sas = {
        let vs = state.lock().await;
        vs.sas_sessions.get(flow_id).cloned()
    };

    if let Some(sas) = sas {
        if let Err(e) = sas.cancel().await {
            tracing::error!("Failed to cancel verification: {e}");
        }
    }

    // Also try cancelling the request.
    let request = {
        let vs = state.lock().await;
        vs.requests.get(flow_id).cloned()
    };

    if let Some(request) = request {
        if let Err(e) = request.cancel().await {
            tracing::error!("Failed to cancel verification request: {e}");
        }
    }

    let mut vs = state.lock().await;
    vs.requests.remove(flow_id);
    vs.sas_sessions.remove(flow_id);
}
