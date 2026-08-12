//! Request handlers for extension bridge messages.

use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use serde_json::Value;
use tokio::sync::oneshot;

use super::bridge::{BrowserPrompt, IpcBridge, PromptDecision, DOWNLOAD_PROMPT_TIMEOUT};
use super::protocol::{
    is_side_effect_rate_limited, parse_enqueue_payload, validate_host_request, EnqueuePayload,
    HostRequest, HostResponse, RawHandoffAuth, MAX_METADATA_LENGTH,
};
use crate::download::{
    find_active_duplicate, EngineCommand, EnqueueOutcome, EnqueueStatus, HandoffAuth,
    HandoffAuthHeader,
};
use crate::extension_settings::ExtensionIntegrationSettings;
use crate::persistence::save_settings;

const ENQUEUE_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn handle_request(bridge: &IpcBridge, request: HostRequest) -> HostResponse {
    if let Err(response) = validate_host_request(&request) {
        return response;
    }

    if is_side_effect_rate_limited(&request.message_type) {
        return HostResponse::error(
            request.request_id,
            "RATE_LIMITED",
            "Too many extension bridge requests. Try again shortly.",
        );
    }

    match request.message_type.as_str() {
        "ping" | "get_status" => {
            let Some((_, extension, settings, jobs)) = bridge.snapshot() else {
                return HostResponse::error(
                    request.request_id,
                    "INTERNAL_ERROR",
                    "Could not read app state.",
                );
            };
            HostResponse::ready(request.request_id, &settings, &extension, &jobs)
        }
        "show_window" => {
            bridge.request_show_window();
            let Some((_, extension, settings, jobs)) = bridge.snapshot() else {
                return HostResponse::error(
                    request.request_id,
                    "INTERNAL_ERROR",
                    "Could not read app state.",
                );
            };
            HostResponse::ready(request.request_id, &settings, &extension, &jobs)
        }
        "enqueue_download" => match parse_enqueue_payload(&request.request_id, &request.payload) {
            Ok(payload) => enqueue_download(bridge, request.request_id, payload).await,
            Err(response) => response,
        },
        "prompt_download" => match parse_enqueue_payload(&request.request_id, &request.payload) {
            Ok(payload) => prompt_download(bridge, request.request_id, payload).await,
            Err(response) => response,
        },
        "save_extension_settings" => {
            save_extension_settings(bridge, request.request_id, &request.payload)
        }
        _ => HostResponse::error(
            request.request_id,
            "INVALID_PAYLOAD",
            "Unsupported request type.",
        ),
    }
}

fn save_extension_settings(
    bridge: &IpcBridge,
    request_id: String,
    payload: &Value,
) -> HostResponse {
    match ExtensionIntegrationSettings::from_protocol_json(payload) {
        Ok(extension) => {
            if let Ok(mut guard) = bridge.inner.lock() {
                guard.extension_settings = extension.clone();
                guard.settings.extension = extension.clone();
                let _ = save_settings(&bridge.paths, &guard.settings);
            }
            let Some((_, extension, settings, jobs)) = bridge.snapshot() else {
                return HostResponse::error(
                    request_id,
                    "INTERNAL_ERROR",
                    "Could not read app state.",
                );
            };
            HostResponse::ready(request_id, &settings, &extension, &jobs)
        }
        Err(message) => HostResponse::error(request_id, "INVALID_PAYLOAD", message),
    }
}

fn parse_handoff_auth(raw: Option<RawHandoffAuth>) -> Option<HandoffAuth> {
    raw.map(|auth| HandoffAuth {
        headers: auth
            .headers
            .into_iter()
            .filter(|h| !h.name.trim().is_empty() && !h.value.is_empty())
            .take(32)
            .map(|h| HandoffAuthHeader {
                name: h.name.chars().take(MAX_METADATA_LENGTH).collect(),
                value: h.value.chars().take(16 * 1024).collect(),
            })
            .collect(),
    })
    .filter(|auth| !auth.headers.is_empty())
}

async fn enqueue_download(
    bridge: &IpcBridge,
    request_id: String,
    payload: EnqueuePayload,
) -> HostResponse {
    let Some((directory, _, _, jobs)) = bridge.snapshot() else {
        return HostResponse::error(request_id, "INTERNAL_ERROR", "Could not read app state.");
    };

    if directory.as_os_str().is_empty() {
        return HostResponse::error(
            request_id,
            "DESTINATION_NOT_CONFIGURED",
            "Download directory is not configured.",
        );
    }

    // Active duplicate: same URL still in queue / downloading / paused.
    if let Some(existing) = find_active_duplicate(&jobs, &payload.url) {
        return HostResponse::enqueue_result(
            request_id,
            EnqueueOutcome {
                job_id: existing.id.clone(),
                filename: existing.filename.clone(),
                status: EnqueueStatus::DuplicateExistingJob,
            },
        );
    }

    let handoff_auth = parse_handoff_auth(payload.handoff_auth);
    engine_enqueue(
        bridge,
        request_id,
        payload.url,
        payload.suggested_filename,
        directory,
        handoff_auth,
    )
    .await
}

async fn prompt_download(
    bridge: &IpcBridge,
    request_id: String,
    payload: EnqueuePayload,
) -> HostResponse {
    let Some((directory, _, _, jobs)) = bridge.snapshot() else {
        return HostResponse::error(request_id, "INTERNAL_ERROR", "Could not read app state.");
    };

    if directory.as_os_str().is_empty() {
        return HostResponse::error(
            request_id,
            "DESTINATION_NOT_CONFIGURED",
            "Download directory is not configured.",
        );
    }

    // Still short-circuit exact active duplicates without bothering the user.
    if let Some(existing) = find_active_duplicate(&jobs, &payload.url) {
        return HostResponse::enqueue_result(
            request_id,
            EnqueueOutcome {
                job_id: existing.id.clone(),
                filename: existing.filename.clone(),
                status: EnqueueStatus::DuplicateExistingJob,
            },
        );
    }

    let handoff_auth = parse_handoff_auth(payload.handoff_auth);
    let (reply_tx, reply_rx) = oneshot::channel();
    let prompt_id = uuid::Uuid::new_v4().to_string();
    let prompt = BrowserPrompt {
        id: prompt_id.clone(),
        url: payload.url.clone(),
        suggested_filename: payload.suggested_filename.clone(),
        total_bytes: payload.total_bytes,
        browser: payload.source.browser.clone(),
        entry_point: payload.source.entry_point.clone(),
        page_title: payload.source.page_title.clone(),
        created_at: Instant::now(),
        reply: reply_tx,
    };

    if bridge.enqueue_prompt(prompt).is_err() {
        return HostResponse::error(
            request_id,
            "RATE_LIMITED",
            "Too many pending download prompts. Accept or dismiss existing ones first.",
        );
    }

    let decision = match tokio::time::timeout(DOWNLOAD_PROMPT_TIMEOUT, reply_rx).await {
        Ok(Ok(decision)) => decision,
        Ok(Err(_)) => PromptDecision::Dismiss,
        Err(_) => {
            // Timed out waiting for the user — remove from queue so the dialog can close.
            let _ = bridge.resolve_prompt(&prompt_id, PromptDecision::Dismiss);
            PromptDecision::Dismiss
        }
    };

    match decision {
        PromptDecision::Dismiss => HostResponse::prompt_dismissed(request_id),
        PromptDecision::Accept {
            filename,
            directory: dir_override,
        } => {
            let directory = dir_override.unwrap_or(directory);
            let filename = filename.or(payload.suggested_filename);
            engine_enqueue(
                bridge,
                request_id,
                payload.url,
                filename,
                directory,
                handoff_auth,
            )
            .await
        }
    }
}

async fn engine_enqueue(
    bridge: &IpcBridge,
    request_id: String,
    url: String,
    filename: Option<String>,
    directory: PathBuf,
    handoff_auth: Option<HandoffAuth>,
) -> HostResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    bridge.engine.send(EngineCommand::Add {
        url,
        filename,
        directory,
        handoff_auth,
        reply: Some(reply_tx),
    });

    match tokio::time::timeout(ENQUEUE_REPLY_TIMEOUT, reply_rx).await {
        Ok(Ok(outcome)) => {
            // Open floating progress HUD for newly queued browser handoffs.
            if outcome.status == EnqueueStatus::Queued {
                let show_progress = bridge
                    .inner
                    .lock()
                    .ok()
                    .is_some_and(|g| g.extension_settings.show_progress_after_handoff);
                if show_progress {
                    bridge.enqueue_progress_job(outcome.job_id.clone());
                }
            }
            HostResponse::enqueue_result(request_id, outcome)
        }
        Ok(Err(_)) => HostResponse::error(
            request_id,
            "INTERNAL_ERROR",
            "Download engine closed before accepting the job.",
        ),
        Err(_) => HostResponse::error(
            request_id,
            "INTERNAL_ERROR",
            "Timed out waiting for the download engine.",
        ),
    }
}
