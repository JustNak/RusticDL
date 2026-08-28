//! Host request/response protocol types and validation for the extension bridge.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

use crate::appearance::appearance_settings_dto;
use crate::branding::APP_VERSION;
use crate::download::{EnqueueOutcome, Job, JobState};
use crate::extension_settings::ExtensionIntegrationSettings;
use crate::settings::Settings;

pub const PROTOCOL_VERSION: u32 = 1;

pub(crate) const MAX_REQUEST_ID_LENGTH: usize = 128;
pub(crate) const MAX_URL_LENGTH: usize = 2048;
pub(crate) const MAX_METADATA_LENGTH: usize = 512;
const SIDE_EFFECT_REQUEST_LIMIT: usize = 30;
const SIDE_EFFECT_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(10);

static SIDE_EFFECT_REQUEST_TIMES: OnceLock<Mutex<VecDeque<Instant>>> = OnceLock::new();

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostRequest {
    pub protocol_version: u32,
    pub request_id: String,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnqueueSource {
    pub entry_point: String,
    pub browser: String,
    pub extension_version: String,
    pub page_url: Option<String>,
    pub page_title: Option<String>,
    pub referrer: Option<String>,
    #[allow(dead_code)]
    pub incognito: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnqueuePayload {
    pub url: String,
    pub source: EnqueueSource,
    pub suggested_filename: Option<String>,
    #[allow(dead_code)]
    pub total_bytes: Option<u64>,
    pub handoff_auth: Option<RawHandoffAuth>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawHandoffAuth {
    #[serde(default)]
    pub headers: Vec<RawHandoffAuthHeader>,
    #[serde(default)]
    pub origin_auth: Vec<RawOriginHandoffAuth>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawOriginHandoffAuth {
    pub origin: String,
    #[serde(default)]
    pub headers: Vec<RawHandoffAuthHeader>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawHandoffAuthHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostResponse {
    pub ok: bool,
    pub request_id: String,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl HostResponse {
    pub(crate) fn ready(
        request_id: String,
        settings: &Settings,
        extension: &ExtensionIntegrationSettings,
        jobs: &[Job],
    ) -> Self {
        Self {
            ok: true,
            request_id,
            message_type: "ready".into(),
            payload: Some(json!({
                "appState": "running",
                "appVersion": APP_VERSION,
                "connectionState": "connected",
                "queueSummary": queue_summary(jobs),
                "extensionSettings": extension.to_protocol_json(),
                "appearanceSettings": appearance_settings_dto(settings),
            })),
            code: None,
            message: None,
        }
    }

    pub(crate) fn enqueue_result(request_id: String, outcome: EnqueueOutcome) -> Self {
        Self {
            ok: true,
            request_id,
            message_type: outcome.status.as_protocol().into(),
            payload: Some(json!({
                "jobId": outcome.job_id,
                "filename": outcome.filename,
                "status": outcome.status.as_protocol(),
            })),
            code: None,
            message: None,
        }
    }

    pub(crate) fn prompt_dismissed(request_id: String) -> Self {
        Self {
            ok: true,
            request_id,
            message_type: "prompt_dismissed".into(),
            payload: Some(json!({
                "status": "dismissed",
            })),
            code: None,
            message: None,
        }
    }

    pub(crate) fn error(
        request_id: String,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            ok: false,
            request_id,
            message_type: "rejected".into(),
            payload: None,
            code: Some(code),
            message: Some(message.into()),
        }
    }
}

pub(crate) fn queue_summary(jobs: &[Job]) -> Value {
    let mut queued = 0u32;
    let mut downloading = 0u32;
    let mut completed = 0u32;
    let mut failed = 0u32;
    let mut attention = 0u32;
    for job in jobs {
        match job.state {
            JobState::Queued | JobState::Starting | JobState::Paused => queued += 1,
            JobState::Downloading => downloading += 1,
            JobState::Completed => completed += 1,
            JobState::Failed => {
                failed += 1;
                attention += 1;
            }
            JobState::Canceled => {}
        }
    }
    let active = queued + downloading;
    json!({
        "total": jobs.len(),
        "active": active,
        "attention": attention,
        "queued": queued,
        "downloading": downloading,
        "completed": completed,
        "failed": failed,
    })
}

pub(crate) fn parse_enqueue_payload(
    request_id: &str,
    payload: &Value,
) -> Result<EnqueuePayload, HostResponse> {
    let mut parsed: EnqueuePayload = serde_json::from_value(payload.clone()).map_err(|error| {
        HostResponse::error(
            request_id.to_string(),
            "INVALID_PAYLOAD",
            format!("Payload could not be parsed: {error}"),
        )
    })?;

    parsed.url = validate_http_url(request_id, &parsed.url)?;
    validate_source(request_id, &parsed.source)?;
    if let Some(name) = parsed.suggested_filename.as_deref() {
        if name.len() > MAX_METADATA_LENGTH {
            return Err(HostResponse::error(
                request_id.to_string(),
                "METADATA_TOO_LARGE",
                "suggestedFilename exceeds limit.",
            ));
        }
    }
    let _ = (
        &parsed.source.page_url,
        &parsed.source.page_title,
        &parsed.source.referrer,
        &parsed.source.extension_version,
        &parsed.source.browser,
        &parsed.source.entry_point,
    );
    Ok(parsed)
}

pub(crate) fn validate_source(
    request_id: &str,
    source: &EnqueueSource,
) -> Result<(), HostResponse> {
    if !matches!(
        source.entry_point.as_str(),
        "context_menu" | "popup" | "browser_download"
    ) {
        return Err(HostResponse::error(
            request_id.to_string(),
            "INVALID_PAYLOAD",
            "Source entry point is not supported.",
        ));
    }
    if !matches!(source.browser.as_str(), "chrome" | "edge" | "firefox") {
        return Err(HostResponse::error(
            request_id.to_string(),
            "INVALID_PAYLOAD",
            "Browser is not supported.",
        ));
    }
    for (field, value) in [
        ("extensionVersion", Some(source.extension_version.as_str())),
        ("pageUrl", source.page_url.as_deref()),
        ("pageTitle", source.page_title.as_deref()),
        ("referrer", source.referrer.as_deref()),
    ] {
        if value.is_some_and(|v| v.len() > MAX_METADATA_LENGTH) {
            return Err(HostResponse::error(
                request_id.to_string(),
                "METADATA_TOO_LARGE",
                format!("{field} exceeds limit."),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_http_url(request_id: &str, raw_url: &str) -> Result<String, HostResponse> {
    let trimmed = raw_url.trim();
    if trimmed.len() > MAX_URL_LENGTH {
        return Err(HostResponse::error(
            request_id.to_string(),
            "URL_TOO_LONG",
            format!("URL exceeds {MAX_URL_LENGTH} characters."),
        ));
    }
    let parsed = Url::parse(trimmed).map_err(|_| {
        HostResponse::error(request_id.to_string(), "INVALID_URL", "URL is not valid.")
    })?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        _ => Err(HostResponse::error(
            request_id.to_string(),
            "UNSUPPORTED_SCHEME",
            "Only http and https URLs are supported.",
        )),
    }
}

pub(crate) fn validate_host_request(request: &HostRequest) -> Result<(), HostResponse> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(HostResponse::error(
            request.request_id.clone(),
            "HOST_PROTOCOL_MISMATCH",
            format!(
                "Expected protocol version {}, got {}.",
                PROTOCOL_VERSION, request.protocol_version
            ),
        ));
    }
    if !is_valid_request_id(&request.request_id) {
        return Err(HostResponse::error(
            request.request_id.clone(),
            "INVALID_PAYLOAD",
            "Request id is not supported.",
        ));
    }
    if !matches!(
        request.message_type.as_str(),
        "ping"
            | "get_status"
            | "show_window"
            | "enqueue_download"
            | "prompt_download"
            | "save_extension_settings"
    ) {
        return Err(HostResponse::error(
            request.request_id.clone(),
            "INVALID_PAYLOAD",
            "Unsupported request type.",
        ));
    }
    Ok(())
}

fn is_valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty()
        && request_id.len() <= MAX_REQUEST_ID_LENGTH
        && request_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

pub(crate) fn is_side_effect_rate_limited(message_type: &str) -> bool {
    if !matches!(
        message_type,
        "enqueue_download" | "prompt_download" | "save_extension_settings"
    ) {
        return false;
    }
    let times = SIDE_EFFECT_REQUEST_TIMES.get_or_init(|| Mutex::new(VecDeque::new()));
    let Ok(mut guard) = times.lock() else {
        return false;
    };
    let now = Instant::now();
    while guard
        .front()
        .is_some_and(|t| now.duration_since(*t) > SIDE_EFFECT_RATE_LIMIT_WINDOW)
    {
        guard.pop_front();
    }
    if guard.len() >= SIDE_EFFECT_REQUEST_LIMIT {
        return true;
    }
    guard.push_back(now);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_scheme() {
        let err = validate_http_url("r1", "ftp://example.com/a").unwrap_err();
        assert_eq!(err.code, Some("UNSUPPORTED_SCHEME"));
    }

    #[test]
    fn accepts_https() {
        let url = validate_http_url("r1", "https://example.com/file.zip").unwrap();
        assert!(url.starts_with("https://"));
    }
}
