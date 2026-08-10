//! Browser session headers for extension handoff (memory-only).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffAuthHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffAuth {
    pub headers: Vec<HandoffAuthHeader>,
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

/// Apply handoff headers only when the request URL is same-origin as the job URL.
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
