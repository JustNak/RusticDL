#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod forwarder;
mod framing;
mod protocol;

use forwarder::{AppForwarder, ForwarderError};
use protocol::{
    app_enqueue_request, app_prompt_download_request, app_save_extension_settings_request,
    app_show_window_request, app_status_request, parse_enqueue_payload, parse_open_app_payload,
    parse_prompt_download_payload, validate_protocol, AppResponseEnvelope, HostResponseEnvelope,
    NativeRequestEnvelope,
};
use std::io;

fn main() {
    if let Err(error) = run() {
        eprintln!("native host fatal error: {error}");
    }
}

fn run() -> Result<(), String> {
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        let raw_request = match framing::read_message(&mut stdin) {
            Ok(raw_request) => raw_request,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.to_string()),
        };
        let response = match serde_json::from_slice::<NativeRequestEnvelope>(&raw_request) {
            Ok(request) => handle_request(request),
            Err(error) => HostResponseEnvelope::rejected(
                "invalid_request".into(),
                "invalid_payload",
                "INVALID_PAYLOAD",
                format!("Invalid request payload: {error}"),
            ),
        };
        let response_bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;

        framing::write_message(&mut stdout, &response_bytes).map_err(|error| error.to_string())?;
    }

    Ok(())
}

fn handle_request(request: NativeRequestEnvelope) -> HostResponseEnvelope {
    if let Err(response) = validate_protocol(&request) {
        return *response;
    }

    let forwarder = AppForwarder::from_environment();

    match request.message_type.as_str() {
        "ping" | "get_status" => {
            match forwarder.send(&app_status_request(request.request_id.clone())) {
                Ok(response) => map_status_response(response),
                Err(error) => map_forwarder_error(request.request_id, error),
            }
        }
        "open_app" => match parse_open_app_payload(&request) {
            Ok(payload) => match forwarder.send(&app_show_window_request(
                request.request_id.clone(),
                payload,
            )) {
                Ok(response) => map_status_response(response),
                Err(error) => map_forwarder_error(request.request_id, error),
            },
            Err(response) => *response,
        },
        "enqueue_download" => match parse_enqueue_payload(&request) {
            Ok(payload) => {
                match forwarder.send(&app_enqueue_request(request.request_id.clone(), payload)) {
                    Ok(response) => map_app_response(response),
                    Err(error) => map_forwarder_error(request.request_id, error),
                }
            }
            Err(response) => *response,
        },
        "prompt_download" => match parse_prompt_download_payload(&request) {
            Ok(payload) => match forwarder.send(&app_prompt_download_request(
                request.request_id.clone(),
                payload,
            )) {
                Ok(response) => map_app_response(response),
                Err(error) => map_forwarder_error(request.request_id, error),
            },
            Err(response) => *response,
        },
        "save_extension_settings" => match forwarder.send(&app_save_extension_settings_request(
            request.request_id.clone(),
            request.payload,
        )) {
            Ok(response) => map_status_response(response),
            Err(error) => map_forwarder_error(request.request_id, error),
        },
        _ => HostResponseEnvelope::rejected(
            request.request_id,
            "invalid_payload",
            "INVALID_PAYLOAD",
            "Unsupported request type.",
        ),
    }
}

fn map_status_response(response: AppResponseEnvelope) -> HostResponseEnvelope {
    if response.ok {
        if response.message_type != "ready" {
            return HostResponseEnvelope::rejected(
                response.request_id,
                "rejected",
                "INTERNAL_ERROR",
                format!("Unexpected app response type: {}", response.message_type),
            );
        }

        return HostResponseEnvelope::pong(
            response.request_id,
            response
                .payload
                .unwrap_or_else(|| serde_json::json!({ "appState": "running" })),
        );
    }

    HostResponseEnvelope::rejected(
        response.request_id,
        "rejected",
        response.code.as_deref().unwrap_or("INTERNAL_ERROR"),
        response
            .message
            .unwrap_or_else(|| "Desktop app rejected the request.".to_string()),
    )
}

fn map_app_response(response: AppResponseEnvelope) -> HostResponseEnvelope {
    if response.ok {
        if !matches!(
            response.message_type.as_str(),
            "queued" | "duplicate_existing_job" | "prompt_dismissed"
        ) {
            return HostResponseEnvelope::rejected(
                response.request_id,
                "rejected",
                "INTERNAL_ERROR",
                format!("Unexpected app response type: {}", response.message_type),
            );
        }

        let job_id = response
            .payload
            .as_ref()
            .and_then(|payload| payload.get("jobId"))
            .and_then(|value| value.as_str());
        let filename = response
            .payload
            .as_ref()
            .and_then(|payload| payload.get("filename"))
            .and_then(|value| value.as_str());
        let status = response
            .payload
            .as_ref()
            .and_then(|payload| payload.get("status"))
            .and_then(|value| value.as_str())
            .unwrap_or(response.message_type.as_str());

        return HostResponseEnvelope::accepted_with_status(
            response.request_id,
            job_id,
            filename,
            status,
            "running",
        );
    }

    HostResponseEnvelope::rejected(
        response.request_id,
        "rejected",
        response.code.as_deref().unwrap_or("INTERNAL_ERROR"),
        response
            .message
            .unwrap_or_else(|| "Desktop app rejected the request.".to_string()),
    )
}

fn map_forwarder_error(request_id: String, error: ForwarderError) -> HostResponseEnvelope {
    match error {
        ForwarderError::AppNotInstalled => HostResponseEnvelope::rejected(
            request_id,
            "app_not_installed",
            "APP_NOT_INSTALLED",
            "Desktop app executable not found.",
        ),
        ForwarderError::AppUnreachable => HostResponseEnvelope::rejected(
            request_id,
            "app_unreachable",
            "APP_UNREACHABLE",
            "Desktop app did not respond on the named pipe.",
        ),
        ForwarderError::Serialization(message) | ForwarderError::Transport(message) => {
            HostResponseEnvelope::rejected(request_id, "rejected", "INTERNAL_ERROR", message)
        }
    }
}
