use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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

    #[allow(dead_code)]
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
        }
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
