use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

pub const PROTOCOL_VERSION: u32 = 1;
const MAX_REQUEST_ID_LENGTH: usize = 128;
const MAX_URL_LENGTH: usize = 2048;
const MAX_METADATA_LENGTH: usize = 512;

type HostResult<T> = Result<T, Box<HostResponseEnvelope>>;

#[derive(Debug, Deserialize)]
pub struct NativeRequestEnvelope {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: u32,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "type")]
    pub message_type: String,
    pub payload: Value,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RequestSource {
    #[serde(rename = "entryPoint")]
    pub entry_point: String,
    pub browser: String,
    #[serde(rename = "extensionVersion")]
    pub extension_version: String,
    #[serde(rename = "pageUrl")]
    pub page_url: Option<String>,
    #[serde(rename = "pageTitle")]
    pub page_title: Option<String>,
    pub referrer: Option<String>,
    pub incognito: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HandoffAuthHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HandoffAuth {
    #[serde(default)]
    pub headers: Vec<HandoffAuthHeader>,
    #[serde(default)]
    pub origin_auth: Vec<OriginHandoffAuth>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OriginHandoffAuth {
    pub origin: String,
    #[serde(default)]
    pub headers: Vec<HandoffAuthHeader>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EnqueueDownloadPayload {
    pub url: String,
    pub source: RequestSource,
    #[serde(rename = "suggestedFilename")]
    pub suggested_filename: Option<String>,
    #[serde(rename = "totalBytes")]
    pub total_bytes: Option<u64>,
    #[serde(rename = "handoffAuth")]
    pub handoff_auth: Option<HandoffAuth>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PromptDownloadPayload {
    pub url: String,
    pub source: RequestSource,
    #[serde(rename = "suggestedFilename")]
    pub suggested_filename: Option<String>,
    #[serde(rename = "totalBytes")]
    pub total_bytes: Option<u64>,
    #[serde(rename = "handoffAuth")]
    pub handoff_auth: Option<HandoffAuth>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OpenAppPayload {
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct AppRequestEnvelope<T>
where
    T: Serialize,
{
    #[serde(rename = "protocolVersion")]
    pub protocol_version: u32,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "type")]
    pub message_type: String,
    pub payload: T,
}

#[derive(Debug, Deserialize)]
pub struct AppResponseEnvelope {
    pub ok: bool,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "type")]
    pub message_type: String,
    pub payload: Option<Value>,
    pub code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HostResponseEnvelope {
    pub ok: bool,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl HostResponseEnvelope {
    pub fn pong(request_id: String, payload: Value) -> Self {
        Self {
            ok: true,
            request_id,
            message_type: "pong".into(),
            payload: Some(payload),
            code: None,
            message: None,
        }
    }

    pub fn accepted_with_status(
        request_id: String,
        job_id: Option<&str>,
        filename: Option<&str>,
        status: &str,
        app_state: &str,
    ) -> Self {
        let mut payload = serde_json::Map::new();
        payload.insert("status".into(), json!(status));
        payload.insert("appState".into(), json!(app_state));
        if let Some(job_id) = job_id {
            payload.insert("jobId".into(), json!(job_id));
        }
        if let Some(filename) = filename {
            payload.insert("filename".into(), json!(filename));
        }

        Self {
            ok: true,
            request_id,
            message_type: "accepted".into(),
            payload: Some(Value::Object(payload)),
            code: None,
            message: None,
        }
    }

    pub fn rejected(
        request_id: String,
        response_type: &str,
        code: &str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            ok: false,
            request_id,
            message_type: response_type.into(),
            payload: None,
            code: Some(code.into()),
            message: Some(message.into()),
        }
    }
}

pub fn validate_protocol(request: &NativeRequestEnvelope) -> HostResult<()> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(Box::new(HostResponseEnvelope::rejected(
            request.request_id.clone(),
            "rejected",
            "HOST_PROTOCOL_MISMATCH",
            format!(
                "Expected protocol version {}, got {}.",
                PROTOCOL_VERSION, request.protocol_version
            ),
        )));
    }

    if !is_valid_request_id(&request.request_id) {
        return Err(invalid_payload(
            &request.request_id,
            "Request id is not supported.",
        ));
    }

    if !is_supported_request_type(&request.message_type) {
        return Err(invalid_payload(
            &request.request_id,
            "Unsupported request type.",
        ));
    }

    Ok(())
}

pub fn parse_enqueue_payload(
    request: &NativeRequestEnvelope,
) -> HostResult<EnqueueDownloadPayload> {
    let mut payload = serde_json::from_value::<EnqueueDownloadPayload>(request.payload.clone())
        .map_err(|error| {
            Box::new(HostResponseEnvelope::rejected(
                request.request_id.clone(),
                "invalid_payload",
                "INVALID_PAYLOAD",
                format!("Payload could not be parsed: {error}"),
            ))
        })?;

    payload.url = validate_http_url(&request.request_id, &payload.url)?;
    validate_request_source(&request.request_id, &payload.source)?;
    validate_metadata_field(
        &request.request_id,
        "suggestedFilename",
        payload.suggested_filename.as_deref(),
    )?;
    Ok(payload)
}

pub fn parse_prompt_download_payload(
    request: &NativeRequestEnvelope,
) -> HostResult<PromptDownloadPayload> {
    let mut payload = serde_json::from_value::<PromptDownloadPayload>(request.payload.clone())
        .map_err(|error| {
            Box::new(HostResponseEnvelope::rejected(
                request.request_id.clone(),
                "invalid_payload",
                "INVALID_PAYLOAD",
                format!("Payload could not be parsed: {error}"),
            ))
        })?;

    payload.url = validate_http_url(&request.request_id, &payload.url)?;
    validate_request_source(&request.request_id, &payload.source)?;
    validate_metadata_field(
        &request.request_id,
        "suggestedFilename",
        payload.suggested_filename.as_deref(),
    )?;
    Ok(payload)
}

pub fn parse_open_app_payload(request: &NativeRequestEnvelope) -> HostResult<OpenAppPayload> {
    let payload =
        serde_json::from_value::<OpenAppPayload>(request.payload.clone()).map_err(|error| {
            Box::new(HostResponseEnvelope::rejected(
                request.request_id.clone(),
                "invalid_payload",
                "INVALID_PAYLOAD",
                format!("Payload could not be parsed: {error}"),
            ))
        })?;

    if !matches!(payload.reason.as_str(), "user_request" | "reconnect") {
        return Err(invalid_payload(
            &request.request_id,
            "Open app reason is not supported.",
        ));
    }

    Ok(payload)
}

pub fn validate_http_url(request_id: &str, raw_url: &str) -> HostResult<String> {
    let trimmed_url = raw_url.trim();
    if trimmed_url.len() > MAX_URL_LENGTH {
        return Err(Box::new(HostResponseEnvelope::rejected(
            request_id.to_string(),
            "invalid_payload",
            "URL_TOO_LONG",
            format!("URL exceeds {MAX_URL_LENGTH} characters."),
        )));
    }

    let parsed = Url::parse(trimmed_url).map_err(|_| {
        Box::new(HostResponseEnvelope::rejected(
            request_id.to_string(),
            "invalid_payload",
            "INVALID_URL",
            "URL is not valid.",
        ))
    })?;

    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        _ => Err(Box::new(HostResponseEnvelope::rejected(
            request_id.to_string(),
            "invalid_payload",
            "UNSUPPORTED_SCHEME",
            "Only http and https URLs are supported.",
        ))),
    }
}

fn validate_request_source(request_id: &str, source: &RequestSource) -> HostResult<()> {
    if !matches!(
        source.entry_point.as_str(),
        "context_menu" | "popup" | "browser_download"
    ) {
        return Err(invalid_payload(
            request_id,
            "Source entry point is not supported.",
        ));
    }

    if !matches!(source.browser.as_str(), "chrome" | "edge" | "firefox") {
        return Err(invalid_payload(request_id, "Browser is not supported."));
    }

    validate_metadata_field(request_id, "entryPoint", Some(source.entry_point.as_str()))?;
    validate_metadata_field(request_id, "browser", Some(source.browser.as_str()))?;
    validate_metadata_field(
        request_id,
        "extensionVersion",
        Some(source.extension_version.as_str()),
    )?;
    validate_metadata_field(request_id, "pageUrl", source.page_url.as_deref())?;
    validate_metadata_field(request_id, "pageTitle", source.page_title.as_deref())?;
    validate_metadata_field(request_id, "referrer", source.referrer.as_deref())
}

fn validate_metadata_field(
    request_id: &str,
    field_name: &str,
    value: Option<&str>,
) -> HostResult<()> {
    if value.is_some_and(|value| value.len() > MAX_METADATA_LENGTH) {
        return Err(Box::new(HostResponseEnvelope::rejected(
            request_id.to_string(),
            "invalid_payload",
            "METADATA_TOO_LARGE",
            format!("{field_name} exceeds {MAX_METADATA_LENGTH} characters."),
        )));
    }

    Ok(())
}

fn invalid_payload(request_id: &str, message: &str) -> Box<HostResponseEnvelope> {
    Box::new(HostResponseEnvelope::rejected(
        request_id.to_string(),
        "invalid_payload",
        "INVALID_PAYLOAD",
        message,
    ))
}

fn is_valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty()
        && request_id.len() <= MAX_REQUEST_ID_LENGTH
        && request_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

fn is_supported_request_type(message_type: &str) -> bool {
    matches!(
        message_type,
        "ping"
            | "get_status"
            | "open_app"
            | "enqueue_download"
            | "prompt_download"
            | "save_extension_settings"
    )
}

pub fn app_status_request(request_id: String) -> AppRequestEnvelope<Value> {
    AppRequestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        message_type: "get_status".into(),
        payload: json!({}),
    }
}

pub fn app_show_window_request(
    request_id: String,
    payload: OpenAppPayload,
) -> AppRequestEnvelope<OpenAppPayload> {
    AppRequestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        message_type: "show_window".into(),
        payload,
    }
}

pub fn app_enqueue_request(
    request_id: String,
    payload: EnqueueDownloadPayload,
) -> AppRequestEnvelope<EnqueueDownloadPayload> {
    AppRequestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        message_type: "enqueue_download".into(),
        payload,
    }
}

pub fn app_prompt_download_request(
    request_id: String,
    payload: PromptDownloadPayload,
) -> AppRequestEnvelope<PromptDownloadPayload> {
    AppRequestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        message_type: "prompt_download".into(),
        payload,
    }
}

pub fn app_save_extension_settings_request(
    request_id: String,
    payload: Value,
) -> AppRequestEnvelope<Value> {
    AppRequestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        message_type: "save_extension_settings".into(),
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(message_type: &str, payload: Value) -> NativeRequestEnvelope {
        NativeRequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".into(),
            message_type: message_type.into(),
            payload,
        }
    }

    fn valid_source() -> Value {
        json!({
            "entryPoint": "context_menu",
            "browser": "firefox",
            "extensionVersion": "0.1.0"
        })
    }

    #[test]
    fn parse_enqueue_payload_rejects_urls_over_protocol_limit() {
        let long_url = format!("https://example.com/{}", "a".repeat(2_048));
        let request = request(
            "enqueue_download",
            json!({
                "url": long_url,
                "source": valid_source()
            }),
        );

        let error = parse_enqueue_payload(&request).expect_err("long URL should be rejected");
        assert_eq!(error.code.as_deref(), Some("URL_TOO_LONG"));
    }

    #[test]
    fn parse_enqueue_payload_rejects_magnet() {
        let request = request(
            "enqueue_download",
            json!({
                "url": "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
                "source": valid_source()
            }),
        );

        let error = parse_enqueue_payload(&request).expect_err("magnet should be rejected");
        assert_eq!(error.code.as_deref(), Some("UNSUPPORTED_SCHEME"));
    }

    #[test]
    fn parse_enqueue_payload_accepts_http_with_auth() {
        let request = request(
            "enqueue_download",
            json!({
                "url": "https://example.com/file.zip",
                "source": {
                    "entryPoint": "browser_download",
                    "browser": "chrome",
                    "extensionVersion": "0.1.0"
                },
                "suggestedFilename": "file.zip",
                "totalBytes": 100,
                "handoffAuth": {
                    "headers": [{ "name": "Cookie", "value": "a=b" }]
                }
            }),
        );

        let payload = parse_enqueue_payload(&request).expect("valid enqueue");
        assert_eq!(payload.suggested_filename.as_deref(), Some("file.zip"));
        assert_eq!(
            payload
                .handoff_auth
                .as_ref()
                .and_then(|a| a.headers.first())
                .map(|h| h.name.as_str()),
            Some("Cookie")
        );
        assert!(payload
            .handoff_auth
            .as_ref()
            .map(|a| a.origin_auth.is_empty())
            .unwrap_or(true));
    }

    #[test]
    fn parse_enqueue_payload_keeps_origin_auth() {
        let request = request(
            "enqueue_download",
            json!({
                "url": "https://school.instructure.com/files/1/download",
                "source": {
                    "entryPoint": "browser_download",
                    "browser": "chrome",
                    "extensionVersion": "0.1.0"
                },
                "handoffAuth": {
                    "headers": [{ "name": "Cookie", "value": "canvas=1" }],
                    "originAuth": [{
                        "origin": "https://drive.google.com",
                        "headers": [{ "name": "Cookie", "value": "SID=drive" }]
                    }]
                }
            }),
        );

        let payload = parse_enqueue_payload(&request).expect("valid enqueue");
        let origin = payload
            .handoff_auth
            .as_ref()
            .and_then(|a| a.origin_auth.first())
            .expect("originAuth");
        assert_eq!(origin.origin, "https://drive.google.com");
        assert_eq!(origin.headers[0].value, "SID=drive");
    }
}
