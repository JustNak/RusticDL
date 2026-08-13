use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::segment::SegmentMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Starting,
    Downloading,
    Paused,
    Completed,
    Failed,
    Canceled,
}

impl JobState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Starting => "Starting",
            Self::Downloading => "Downloading",
            Self::Paused => "Paused",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Canceled => "Canceled",
        }
    }

    /// 0 muted, 1 primary, 2 success, 3 warning, 4 destructive
    pub fn tone(self) -> i32 {
        match self {
            Self::Queued | Self::Canceled => 0,
            Self::Starting | Self::Downloading => 1,
            Self::Completed => 2,
            Self::Paused => 3,
            Self::Failed => 4,
        }
    }

    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Starting | Self::Downloading | Self::Paused
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    Network,
    Http,
    Disk,
    Resume,
    Internal,
}

#[derive(Debug, Clone)]
pub struct DownloadError {
    pub category: FailureCategory,
    pub message: String,
    pub retryable: bool,
}

impl From<String> for DownloadError {
    fn from(message: String) -> Self {
        Self {
            category: FailureCategory::Internal,
            message,
            retryable: false,
        }
    }
}

impl From<&str> for DownloadError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

pub fn download_error(
    category: FailureCategory,
    message: String,
    retryable: bool,
) -> DownloadError {
    DownloadError {
        category,
        message,
        retryable,
    }
}

/// HTTP content validators captured from response headers for resume identity checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentValidators {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_size: Option<u64>,
}

impl ContentValidators {
    /// True when no ETag / Last-Modified / expected_size is stored.
    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none() && self.expected_size.is_none()
    }

    /// Field-wise merge: only overwrite fields present as `Some` in `incoming`.
    /// Never clears an existing field via a sparse capture (lifecycle clears only).
    pub fn merge_present(&mut self, incoming: ContentValidators) {
        if let Some(etag) = incoming.etag {
            self.etag = Some(etag);
        }
        if let Some(lm) = incoming.last_modified {
            self.last_modified = Some(lm);
        }
        if let Some(size) = incoming.expected_size {
            self.expected_size = Some(size);
        }
    }
}

/// Transfer strategy hint for UI / multi-connection planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferMode {
    Single,
    Multi,
}

impl TransferMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::Multi => "Multi",
        }
    }
}

/// Human label for planner / multi-fallback reason keys (detail panel).
pub fn fallback_reason_label(reason: &str) -> &str {
    match reason {
        "multi_disabled" => "Multi-connection disabled",
        "size_unknown" => "File size unknown",
        "ranges_unknown" => "Range support unknown",
        "ranges_unsupported" => "Server does not support ranges",
        "below_multi_min_bytes" => "Below multi-connection size",
        "multi_qualified" => "Multi-connection",
        "legacy_contiguous_partial" => "Existing single-stream partial",
        "map_missing" => "Segment map missing",
        "map_inconsistent" => "Segment map inconsistent",
        "multi_start_failed" => "Multi-connection start failed",
        "multi_http_fallback" => "HTTP error during multi-connection",
        "multi_network_fallback" => "Network error during multi-connection",
        "multi_resume_fallback" => "Resume error during multi-connection",
        "multi_disk_fallback" => "Disk error during multi-connection",
        "multi_internal_fallback" => "Internal error during multi-connection",
        other => other,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub state: JobState,
    pub created_at: u64,
    pub progress: f64,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    pub speed: u64,
    pub eta_secs: u64,
    pub error: Option<String>,
    pub failure_category: Option<FailureCategory>,
    pub target_path: PathBuf,
    pub temp_path: PathBuf,
    pub resume_supported: bool,
    pub retry_attempts: u32,
    /// ETag / Last-Modified / size from successful responses.
    #[serde(default)]
    pub validators: ContentValidators,
    /// 0 = single-stream contiguous `.part`; 1 = multi-segment map-authoritative.
    #[serde(default)]
    pub transfer_format_version: u32,
    /// Multi-segment resume map. Authoritative for progress when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_map: Option<SegmentMap>,
    /// Live connection count (UI / metrics placeholder).
    #[serde(default)]
    pub active_connections: u32,
    /// Cumulative short reconnects for this job until Restart.
    #[serde(default)]
    pub reconnect_count: u32,
    /// Single vs multi transfer mode when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_mode: Option<TransferMode>,
    /// Last multi→single or planner failure reason (UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// Optional SHA-256 hex; verified after transfer, before finalize rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
}

impl Job {
    pub fn new(url: String, filename: String, target_path: PathBuf, temp_path: PathBuf) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            url,
            filename,
            state: JobState::Queued,
            created_at: now_unix_secs(),
            progress: 0.0,
            total_bytes: 0,
            downloaded_bytes: 0,
            speed: 0,
            eta_secs: 0,
            error: None,
            failure_category: None,
            target_path,
            temp_path,
            resume_supported: false,
            retry_attempts: 0,
            validators: ContentValidators::default(),
            transfer_format_version: 0,
            segment_map: None,
            active_connections: 0,
            reconnect_count: 0,
            transfer_mode: None,
            fallback_reason: None,
            expected_sha256: None,
        }
    }

    /// Clear validators, map, transfer format, and metrics (Restart / Cancel+delete).
    pub fn clear_transfer_identity(&mut self) {
        self.validators = ContentValidators::default();
        self.transfer_format_version = 0;
        self.segment_map = None;
        self.active_connections = 0;
        self.reconnect_count = 0;
        self.transfer_mode = None;
        self.fallback_reason = None;
    }

    /// Cancel+delete: drop map/identity and zero downloaded progress.
    pub fn clear_partial_and_identity(&mut self) {
        self.downloaded_bytes = 0;
        self.progress = 0.0;
        self.clear_transfer_identity();
    }

    /// Completed: slim state.json — drop map, version 0, mode; keep validators / reconnects.
    pub fn on_completed(&mut self) {
        self.segment_map = None;
        self.transfer_format_version = 0;
        self.active_connections = 0;
        self.transfer_mode = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn job_serde_defaults_legacy_payload() {
        // Older state.json without validators / version fields must deserialize cleanly.
        let json = r#"{
            "id":"abc",
            "url":"https://example.com/f.bin",
            "filename":"f.bin",
            "state":"queued",
            "createdAt":1,
            "progress":0.0,
            "totalBytes":0,
            "downloadedBytes":0,
            "speed":0,
            "etaSecs":0,
            "error":null,
            "failureCategory":null,
            "targetPath":"C:\\dl\\f.bin",
            "tempPath":"C:\\dl\\f.bin.part",
            "resumeSupported":false,
            "retryAttempts":0
        }"#;
        let job: Job = serde_json::from_str(json).expect("legacy Job deserializes");
        assert!(job.validators.is_empty());
        assert_eq!(job.transfer_format_version, 0);
        assert!(job.segment_map.is_none());
        assert_eq!(job.active_connections, 0);
        assert_eq!(job.reconnect_count, 0);
        assert!(job.transfer_mode.is_none());
        assert!(job.fallback_reason.is_none());
        assert!(job.expected_sha256.is_none());
    }

    #[test]
    fn job_new_defaults_transfer_fields() {
        let job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        assert!(job.validators.is_empty());
        assert_eq!(job.transfer_format_version, 0);
        assert!(job.segment_map.is_none());
        assert_eq!(job.active_connections, 0);
        assert_eq!(job.reconnect_count, 0);
        assert!(job.transfer_mode.is_none());
        assert!(job.fallback_reason.is_none());
        assert!(job.expected_sha256.is_none());
    }

    #[test]
    fn clear_transfer_identity_resets_validators_and_version() {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        job.validators = ContentValidators {
            etag: Some("\"abc\"".into()),
            last_modified: Some("Wed, 01 Jan 2020 00:00:00 GMT".into()),
            expected_size: Some(1024),
        };
        job.transfer_format_version = 1;
        job.segment_map = Some(crate::download::segment::partition(2 * 1024 * 1024, 2));
        job.active_connections = 4;
        job.reconnect_count = 2;
        job.transfer_mode = Some(TransferMode::Multi);
        job.fallback_reason = Some("test".into());

        job.clear_transfer_identity();

        assert!(job.validators.is_empty());
        assert_eq!(job.transfer_format_version, 0);
        assert!(job.segment_map.is_none());
        assert_eq!(job.active_connections, 0);
        assert_eq!(job.reconnect_count, 0);
        assert!(job.transfer_mode.is_none());
        assert!(job.fallback_reason.is_none());
    }

    #[test]
    fn on_completed_clears_map_keeps_validators() {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        job.validators = ContentValidators {
            etag: Some("\"abc\"".into()),
            last_modified: None,
            expected_size: Some(1024),
        };
        job.transfer_format_version = 1;
        job.segment_map = Some(crate::download::segment::partition(2 * 1024 * 1024, 2));
        job.active_connections = 4;
        job.reconnect_count = 3;
        job.transfer_mode = Some(TransferMode::Multi);

        job.on_completed();

        assert!(job.segment_map.is_none());
        assert_eq!(job.transfer_format_version, 0);
        assert_eq!(job.active_connections, 0);
        assert!(job.transfer_mode.is_none());
        assert_eq!(job.reconnect_count, 3);
        assert_eq!(job.validators.etag.as_deref(), Some("\"abc\""));
    }

    #[test]
    fn job_serde_roundtrip_validators_and_version() {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        job.validators = ContentValidators {
            etag: Some("\"abc\"".into()),
            last_modified: Some("Wed, 01 Jan 2020 00:00:00 GMT".into()),
            expected_size: Some(1024),
        };
        job.transfer_format_version = 1;
        job.segment_map = Some(crate::download::segment::partition(2 * 1024 * 1024, 2));
        job.active_connections = 4;
        job.reconnect_count = 2;
        job.transfer_mode = Some(TransferMode::Multi);
        job.fallback_reason = Some("planner".into());
        job.expected_sha256 =
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into());

        let json = serde_json::to_string(&job).expect("serialize");
        // camelCase keys for new fields
        assert!(json.contains("\"transferFormatVersion\":1"));
        assert!(json.contains("\"segmentMap\""));
        assert!(json.contains("\"preallocated\""));
        assert!(json.contains("\"lastModified\""));
        assert!(json.contains("\"expectedSize\":1024"));
        assert!(json.contains("\"activeConnections\":4"));
        assert!(json.contains("\"reconnectCount\":2"));
        assert!(json.contains("\"transferMode\":\"multi\""));
        assert!(json.contains("\"fallbackReason\":\"planner\""));
        assert!(json.contains("\"expectedSha256\""));
        assert!(json.contains("\"etag\""));

        let back: Job = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.validators, job.validators);
        assert_eq!(back.transfer_format_version, 1);
        assert_eq!(back.segment_map, job.segment_map);
        assert_eq!(back.active_connections, 4);
        assert_eq!(back.reconnect_count, 2);
        assert_eq!(back.transfer_mode, Some(TransferMode::Multi));
        assert_eq!(back.fallback_reason.as_deref(), Some("planner"));
        assert_eq!(back.expected_sha256, job.expected_sha256);
    }

    #[test]
    fn content_validators_skip_serializing_none_fields() {
        let v = ContentValidators {
            etag: Some("\"x\"".into()),
            last_modified: None,
            expected_size: None,
        };
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains("etag"));
        assert!(!json.contains("lastModified"));
        assert!(!json.contains("expectedSize"));
    }

    #[test]
    fn content_validators_merge_present_never_clears() {
        let mut stored = ContentValidators {
            etag: Some("\"old\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            expected_size: Some(100),
        };
        stored.merge_present(ContentValidators {
            etag: None,
            last_modified: None,
            expected_size: Some(200),
        });
        assert_eq!(stored.etag.as_deref(), Some("\"old\""));
        assert_eq!(
            stored.last_modified.as_deref(),
            Some("Mon, 01 Jan 2024 00:00:00 GMT")
        );
        assert_eq!(stored.expected_size, Some(200));
    }

    #[test]
    fn transfer_mode_labels() {
        assert_eq!(TransferMode::Single.label(), "Single");
        assert_eq!(TransferMode::Multi.label(), "Multi");
    }

    #[test]
    fn fallback_reason_labels_known_keys() {
        assert_eq!(
            fallback_reason_label("ranges_unsupported"),
            "Server does not support ranges"
        );
        assert_eq!(
            fallback_reason_label("legacy_contiguous_partial"),
            "Existing single-stream partial"
        );
        assert_eq!(fallback_reason_label("map_missing"), "Segment map missing");
        assert_eq!(
            fallback_reason_label("map_inconsistent"),
            "Segment map inconsistent"
        );
        assert_eq!(
            fallback_reason_label("unknown_custom_reason"),
            "unknown_custom_reason"
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerControl {
    Continue,
    Paused,
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadOutcome {
    Completed,
    Paused,
    Canceled,
}

pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
