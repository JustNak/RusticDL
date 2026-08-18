//! Browser session headers for extension handoff (memory-only).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffAuthHeader {
    pub name: String,
    pub value: String,
}

/// Cookie / Authorization captured for one origin (Canvas vs Drive after redirect).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OriginHandoffAuth {
    pub origin: String,
    pub headers: Vec<HandoffAuthHeader>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffAuth {
    #[serde(default)]
    pub headers: Vec<HandoffAuthHeader>,
    /// Per-origin secrets so a Canvas → Google Drive hop keeps Drive cookies
    /// without sending the Canvas session to a different site.
    #[serde(default, rename = "originAuth")]
    pub origin_auth: Vec<OriginHandoffAuth>,
}

#[derive(Debug, Clone)]
pub struct EnqueueOutcome {
    pub job_id: String,
    pub filename: String,
    pub status: EnqueueStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueStatus {
    Queued,
    DuplicateExistingJob,
}

impl EnqueueStatus {
    pub fn as_protocol(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::DuplicateExistingJob => "duplicate_existing_job",
        }
    }
}

/// Headers the desktop app will apply from a browser capture (case-insensitive names).
const ALLOWED_HANDOFF_HEADERS: &[&str] = &[
    "cookie",
    "authorization",
    "referer",
    "origin",
    "user-agent",
    "accept",
    "accept-language",
];

pub fn is_allowed_handoff_header(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    ALLOWED_HANDOFF_HEADERS.contains(&lower.as_str())
        || lower.starts_with("sec-fetch-")
        || lower.starts_with("sec-ch-ua")
}

fn http_origin(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    match parsed.origin() {
        url::Origin::Tuple(..) => Some(parsed.origin().ascii_serialization()),
        url::Origin::Opaque(_) => None,
    }
}

fn is_secret_handoff_header(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    lower == "cookie" || lower == "authorization"
}

/// Headers to send on this hop: identity headers stay job-origin-only;
/// Cookie / Authorization come from matching `origin_auth`, else the legacy
/// job-origin Cookie header.
pub fn handoff_headers_for_request<'a>(
    job_url: &str,
    request_url: &str,
    handoff_auth: Option<&'a HandoffAuth>,
) -> Vec<&'a HandoffAuthHeader> {
    let Some(auth) = handoff_auth else {
        return Vec::new();
    };
    if auth.headers.is_empty() && auth.origin_auth.is_empty() {
        return Vec::new();
    }

    let job_origin = http_origin(job_url);
    let req_origin = http_origin(request_url);
    let same_origin = job_origin.is_some() && job_origin == req_origin;

    let origin_entry = req_origin.as_ref().and_then(|origin| {
        auth.origin_auth
            .iter()
            .find(|entry| !entry.origin.is_empty() && &entry.origin == origin)
    });

    let mut out = Vec::new();
    if same_origin {
        for header in &auth.headers {
            if origin_entry.is_some() && is_secret_handoff_header(&header.name) {
                continue;
            }
            out.push(header);
        }
    }
    if let Some(entry) = origin_entry {
        for header in &entry.headers {
            if is_allowed_handoff_header(&header.name) {
                out.push(header);
            }
        }
    }
    out
}

/// Preflight must not pin a hop it already fetched when a browser session
/// minted that Location. Inst-FS / Drive tokens are one-use; the real GET
/// then 401s with the "fresh token" message.
pub fn should_pin_preflight_url(handoff_auth: Option<&HandoffAuth>) -> bool {
    handoff_auth.is_none()
}

/// After 401/403 on a redirected hop, replay the original session URL once
/// so Canvas can mint a new Location. Same URL cannot remint itself.
pub fn session_url_after_auth_denied<'a>(
    job_url: &'a str,
    current_url: &'a str,
    handoff_auth: Option<&HandoffAuth>,
    already_replayed: bool,
) -> Option<&'a str> {
    if already_replayed || handoff_auth.is_none() {
        return None;
    }
    if job_url == current_url {
        return None;
    }
    Some(job_url)
}

/// Apply handoff headers only when the request URL is same-origin as the job URL.
///
/// Prefer [`handoff_headers_for_request`] when origin-scoped cookies are present.
pub fn handoff_auth_for_request_url<'a>(
    job_url: &str,
    request_url: &str,
    handoff_auth: Option<&'a HandoffAuth>,
) -> Option<&'a HandoffAuth> {
    let auth = handoff_auth?;
    if auth.headers.is_empty() {
        return None;
    }
    let job = url::Url::parse(job_url).ok()?;
    let req = url::Url::parse(request_url).ok()?;
    if job.origin() == req.origin() {
        Some(auth)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(value: &str) -> HandoffAuthHeader {
        HandoffAuthHeader {
            name: "Cookie".into(),
            value: value.into(),
        }
    }

    #[test]
    fn same_origin_keeps_legacy_cookie_when_origin_auth_absent() {
        let auth = HandoffAuth {
            headers: vec![cookie("canvas=1")],
            ..Default::default()
        };
        let headers = handoff_headers_for_request(
            "https://school.instructure.com/files/1/download",
            "https://school.instructure.com/files/1/download",
            Some(&auth),
        );
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].value, "canvas=1");
    }

    #[test]
    fn cross_origin_strips_legacy_cookie() {
        let auth = HandoffAuth {
            headers: vec![cookie("canvas=secret")],
            ..Default::default()
        };
        let headers = handoff_headers_for_request(
            "https://school.instructure.com/files/1/download",
            "https://drive.google.com/uc?id=abc",
            Some(&auth),
        );
        assert!(headers.is_empty());
    }

    #[test]
    fn origin_auth_cookie_follows_matching_redirect_origin() {
        let auth = HandoffAuth {
            headers: vec![cookie("canvas=secret")],
            origin_auth: vec![
                OriginHandoffAuth {
                    origin: "https://school.instructure.com".into(),
                    headers: vec![cookie("canvas=secret")],
                },
                OriginHandoffAuth {
                    origin: "https://drive.google.com".into(),
                    headers: vec![cookie("SID=drive")],
                },
            ],
        };
        let canvas = handoff_headers_for_request(
            "https://school.instructure.com/files/1/download",
            "https://school.instructure.com/files/1/download?download_frd=1",
            Some(&auth),
        );
        assert_eq!(canvas.len(), 1);
        assert_eq!(canvas[0].value, "canvas=secret");

        let drive = handoff_headers_for_request(
            "https://school.instructure.com/files/1/download",
            "https://drive.google.com/uc?export=download&id=abc",
            Some(&auth),
        );
        assert_eq!(drive.len(), 1);
        assert_eq!(drive[0].value, "SID=drive");
    }

    #[test]
    fn legacy_whole_set_filter_still_same_origin_only() {
        let auth = HandoffAuth {
            headers: vec![cookie("canvas=secret")],
            ..Default::default()
        };
        assert!(handoff_auth_for_request_url(
            "https://school.instructure.com/files/1/download",
            "https://school.instructure.com/files/1/download",
            Some(&auth),
        )
        .is_some());
        assert!(handoff_auth_for_request_url(
            "https://school.instructure.com/files/1/download",
            "https://drive.google.com/uc?id=abc",
            Some(&auth),
        )
        .is_none());
    }

    #[test]
    fn preflight_pin_skipped_when_handoff_present() {
        let auth = HandoffAuth {
            headers: vec![cookie("canvas=1")],
            ..Default::default()
        };
        assert!(!should_pin_preflight_url(Some(&auth)));
        assert!(should_pin_preflight_url(None));
    }

    #[test]
    fn auth_denied_replays_session_url_once() {
        let auth = HandoffAuth {
            headers: vec![cookie("canvas=1")],
            ..Default::default()
        };
        let job = "https://school.instructure.com/files/1/download";
        let burned = "https://inst-fs.example/files/abc?token=used";
        assert_eq!(
            session_url_after_auth_denied(job, burned, Some(&auth), false),
            Some(job)
        );
        assert_eq!(
            session_url_after_auth_denied(job, burned, Some(&auth), true),
            None
        );
        assert_eq!(
            session_url_after_auth_denied(job, job, Some(&auth), false),
            None
        );
        assert_eq!(
            session_url_after_auth_denied(job, burned, None, false),
            None
        );
    }
}
