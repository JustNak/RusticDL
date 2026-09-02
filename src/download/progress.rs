use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::job::{ContentValidators, Job, JobState, TransferMode};
use super::segment::SegmentMap;

#[derive(Debug, Clone)]
pub enum TransferEvent {
    Tick(ProgressTick),
    Toast(String),
}

#[derive(Debug, Clone, Default)]
pub struct ProgressTick {
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub speed: Option<u64>,
    pub eta_secs: Option<u64>,
    pub progress: Option<f64>,
    pub state_hint: Option<ProgressHint>,
    pub active_connections: Option<u32>,
    pub reconnect_count: Option<u32>,
    pub segment_written: Option<Vec<u64>>,
}

impl ProgressTick {
    pub fn downloading(downloaded: u64, total: u64, speed: u64, eta: u64, progress: f64) -> Self {
        Self {
            downloaded_bytes: Some(downloaded),
            total_bytes: Some(total),
            speed: Some(speed),
            eta_secs: Some(eta),
            progress: Some(progress),
            state_hint: Some(ProgressHint::Downloading),
            ..Default::default()
        }
    }

    pub fn merge(self, later: Self) -> Self {
        Self {
            downloaded_bytes: later.downloaded_bytes.or(self.downloaded_bytes),
            total_bytes: later.total_bytes.or(self.total_bytes),
            speed: later.speed.or(self.speed),
            eta_secs: later.eta_secs.or(self.eta_secs),
            progress: later.progress.or(self.progress),
            state_hint: later.state_hint.or(self.state_hint),
            active_connections: later.active_connections.or(self.active_connections),
            reconnect_count: later.reconnect_count.or(self.reconnect_count),
            segment_written: later.segment_written.or(self.segment_written),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressHint {
    Starting,
    Downloading,
}

#[derive(Debug, Clone, Default)]
pub struct CommitIdentity {
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub progress: Option<f64>,
    pub filename: Option<String>,
    pub target_path: Option<PathBuf>,
    pub temp_path: Option<PathBuf>,
    pub resume_supported: Option<bool>,
    pub validators: Option<ContentValidators>,
    pub replace_validators: bool,
    pub transfer_format_version: Option<u32>,
    pub transfer_mode: Option<TransferMode>,
    pub fallback_reason: Option<String>,
    pub map: MapUpdate,
}

#[derive(Debug, Clone, Default)]
pub enum MapUpdate {
    #[default]
    Unchanged,
    Set(SegmentMap),
    Clear,
}

pub type TransferEventCallback = Arc<dyn Fn(TransferEvent) + Send + Sync>;

#[async_trait]
pub trait IdentityCommit: Send + Sync {
    async fn commit(&self, job: &mut Job, c: CommitIdentity) -> Result<(), String>;

    /// Restart, Remove+delete_file, or Cancel+delete_partial raced with completion.
    async fn output_discarded(&self, _job_id: &str) -> bool {
        false
    }

    /// Record the path this transfer actually created (after uniquify/replace).
    async fn note_produced_file(&self, _job_id: &str, _path: PathBuf) {}
}

#[derive(Default)]
pub struct MemoryIdentity {
    pub snapshots: Mutex<Vec<Job>>,
}

pub struct NoopIdentity;

#[async_trait]
impl IdentityCommit for MemoryIdentity {
    async fn commit(&self, job: &mut Job, c: CommitIdentity) -> Result<(), String> {
        apply_commit_identity(job, &c);
        self.snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(job.clone());
        Ok(())
    }
}

#[async_trait]
impl IdentityCommit for NoopIdentity {
    async fn commit(&self, job: &mut Job, c: CommitIdentity) -> Result<(), String> {
        apply_commit_identity(job, &c);
        Ok(())
    }
}

pub fn apply_commit_identity(job: &mut Job, c: &CommitIdentity) {
    if let Some(v) = c.downloaded_bytes {
        job.downloaded_bytes = v;
    }
    if let Some(v) = c.total_bytes {
        job.total_bytes = v;
    }
    if let Some(v) = c.progress {
        job.progress = v;
    }
    if let Some(name) = c.filename.clone() {
        job.filename = name;
    }
    if let Some(path) = c.target_path.clone() {
        job.target_path = path;
    }
    if let Some(path) = c.temp_path.clone() {
        job.temp_path = path;
    }
    if let Some(resume) = c.resume_supported {
        job.resume_supported = resume;
    }
    if c.replace_validators {
        job.validators = c.validators.clone().unwrap_or_default();
    } else if let Some(validators) = c.validators.clone() {
        job.validators.merge_present(validators);
    }
    if let Some(version) = c.transfer_format_version {
        job.transfer_format_version = version;
    }
    if let Some(mode) = c.transfer_mode {
        job.transfer_mode = Some(mode);
    }
    if let Some(reason) = c.fallback_reason.clone() {
        job.fallback_reason = Some(reason);
    }
    match &c.map {
        MapUpdate::Unchanged => {}
        MapUpdate::Set(map) => job.segment_map = Some(map.clone()),
        MapUpdate::Clear => job.segment_map = None,
    }
}

pub fn apply_tick(job: &mut Job, tick: ProgressTick) -> bool {
    if !matches!(job.state, JobState::Starting | JobState::Downloading) {
        return false;
    }

    if let Some(hint) = tick.state_hint {
        match hint {
            ProgressHint::Starting => {
                job.state = JobState::Starting;
            }
            ProgressHint::Downloading => {
                job.state = JobState::Downloading;
            }
        }
    }

    if let Some(v) = tick.downloaded_bytes {
        job.downloaded_bytes = v;
    }
    if let Some(v) = tick.total_bytes {
        job.total_bytes = v;
    }
    if let Some(v) = tick.speed {
        job.speed = v;
    }
    if let Some(v) = tick.eta_secs {
        job.eta_secs = v;
    }
    if let Some(v) = tick.progress {
        job.progress = v;
    }
    if let Some(n) = tick.active_connections {
        job.active_connections = n;
    }
    if let Some(n) = tick.reconnect_count {
        job.reconnect_count = n;
    }
    if let Some(written) = tick.segment_written {
        if let Some(map) = job.segment_map.as_mut() {
            if written.len() == map.segments.len() {
                for (segment, n) in map.segments.iter_mut().zip(written) {
                    if n > segment.written {
                        segment.written = n;
                    }
                }
            }
        }
    }

    true
}

#[cfg(test)]
pub(crate) struct TestProgress {
    pub events: Arc<Mutex<Vec<TransferEvent>>>,
    pub identity: Arc<MemoryIdentity>,
}

#[cfg(test)]
impl TestProgress {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            identity: Arc::new(MemoryIdentity::default()),
        }
    }

    pub fn callback(&self) -> TransferEventCallback {
        let events = self.events.clone();
        Arc::new(move |event: TransferEvent| {
            events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(event);
        })
    }

    pub fn events(&self) -> Vec<TransferEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn snapshots(&self) -> Vec<Job> {
        self.identity
            .snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::job::Job;
    use crate::download::segment::{Segment, SegmentState};
    use std::path::PathBuf;

    fn sample_job(state: JobState) -> Job {
        let mut job = Job::new(
            "https://example.com/file.bin".into(),
            "file.bin".into(),
            PathBuf::from("C:\\downloads\\file.bin"),
            PathBuf::from("C:\\downloads\\file.bin.part"),
        );
        job.state = state;
        job.downloaded_bytes = 10;
        job.total_bytes = 100;
        job.speed = 1;
        job.eta_secs = 90;
        job.progress = 10.0;
        job
    }

    fn sample_map() -> SegmentMap {
        SegmentMap {
            total_bytes: 1000,
            segment_count: 1,
            segments: vec![Segment {
                index: 0,
                start: 0,
                end: 999,
                written: 10,
                state: SegmentState::Active,
            }],
            preallocated: true,
        }
    }

    #[test]
    fn tick_merge_later_wins() {
        let earlier = ProgressTick {
            downloaded_bytes: Some(10),
            total_bytes: Some(100),
            speed: Some(1),
            eta_secs: Some(90),
            progress: Some(10.0),
            state_hint: Some(ProgressHint::Starting),
            segment_written: Some(vec![1, 2]),
            ..Default::default()
        };
        let later = ProgressTick {
            downloaded_bytes: Some(50),
            speed: Some(5),
            progress: Some(50.0),
            state_hint: Some(ProgressHint::Downloading),
            segment_written: Some(vec![9, 8]),
            ..Default::default()
        };
        let merged = earlier.merge(later);
        assert_eq!(merged.downloaded_bytes, Some(50));
        assert_eq!(merged.total_bytes, Some(100));
        assert_eq!(merged.speed, Some(5));
        assert_eq!(merged.eta_secs, Some(90));
        assert_eq!(merged.progress, Some(50.0));
        assert_eq!(merged.state_hint, Some(ProgressHint::Downloading));
        assert_eq!(merged.segment_written.as_deref(), Some(&[9, 8][..]));
    }

    #[test]
    fn merge_zero_speed_clears_stale_live_sample() {
        let live = ProgressTick {
            speed: Some(1_140_000),
            eta_secs: Some(12),
            ..Default::default()
        };
        let reconnect = ProgressTick {
            speed: Some(0),
            eta_secs: Some(0),
            reconnect_count: Some(1),
            state_hint: Some(ProgressHint::Starting),
            ..Default::default()
        };
        let merged = live.merge(reconnect);
        assert_eq!(merged.speed, Some(0));
        assert_eq!(merged.eta_secs, Some(0));
        assert_eq!(merged.reconnect_count, Some(1));
        assert_eq!(merged.state_hint, Some(ProgressHint::Starting));
    }

    #[test]
    fn downloading_tick_sets_scalars_only() {
        let tick = ProgressTick::downloading(25, 100, 10, 7, 25.0);
        assert_eq!(tick.downloaded_bytes, Some(25));
        assert_eq!(tick.total_bytes, Some(100));
        assert_eq!(tick.speed, Some(10));
        assert_eq!(tick.eta_secs, Some(7));
        assert_eq!(tick.progress, Some(25.0));
        assert_eq!(tick.state_hint, Some(ProgressHint::Downloading));
        assert!(tick.segment_written.is_none());
        assert!(tick.active_connections.is_none());
        assert!(tick.reconnect_count.is_none());
    }

    #[test]
    fn apply_commit_identity_on_queued() {
        let mut job = sample_job(JobState::Queued);
        job.downloaded_bytes = 0;
        apply_commit_identity(
            &mut job,
            &CommitIdentity {
                downloaded_bytes: Some(40),
                transfer_format_version: Some(1),
                map: MapUpdate::Set(sample_map()),
                ..Default::default()
            },
        );
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.downloaded_bytes, 40);
        assert_eq!(job.transfer_format_version, 1);
        assert!(job.segment_map.is_some());
    }

    #[test]
    fn apply_commit_identity_on_canceled() {
        let mut job = sample_job(JobState::Canceled);
        apply_commit_identity(
            &mut job,
            &CommitIdentity {
                map: MapUpdate::Clear,
                transfer_format_version: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(job.state, JobState::Canceled);
        assert!(job.segment_map.is_none());
        assert_eq!(job.transfer_format_version, 0);
    }

    #[test]
    fn apply_commit_does_not_change_state() {
        let mut job = sample_job(JobState::Queued);
        apply_commit_identity(
            &mut job,
            &CommitIdentity {
                progress: Some(50.0),
                transfer_mode: Some(TransferMode::Single),
                ..Default::default()
            },
        );
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.progress, 50.0);
        assert_eq!(job.transfer_mode, Some(TransferMode::Single));
    }

    #[test]
    fn apply_commit_replace_validators_clears_stale_identity() {
        let mut job = sample_job(JobState::Downloading);
        job.validators = ContentValidators {
            etag: Some("\"stale-strong\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            expected_size: Some(1000),
        };
        job.transfer_format_version = 1;

        apply_commit_identity(
            &mut job,
            &CommitIdentity {
                validators: Some(ContentValidators {
                    etag: None,
                    last_modified: Some("Tue, 02 Jan 2024 00:00:00 GMT".into()),
                    expected_size: Some(2000),
                }),
                replace_validators: true,
                transfer_format_version: Some(0),
                ..Default::default()
            },
        );

        assert!(job.validators.etag.is_none());
        assert_eq!(
            job.validators.last_modified.as_deref(),
            Some("Tue, 02 Jan 2024 00:00:00 GMT")
        );
        assert_eq!(job.validators.expected_size, Some(2000));
        assert_eq!(job.transfer_format_version, 0);
    }

    #[test]
    fn apply_commit_empty_or_sparse_validators_preserve_identity() {
        let mut job = sample_job(JobState::Downloading);
        job.validators = ContentValidators {
            etag: Some("\"keep\"".into()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
            expected_size: Some(100),
        };

        apply_commit_identity(&mut job, &CommitIdentity::default());
        assert_eq!(job.validators.etag.as_deref(), Some("\"keep\""));

        apply_commit_identity(
            &mut job,
            &CommitIdentity {
                validators: Some(ContentValidators::default()),
                ..Default::default()
            },
        );
        assert_eq!(job.validators.etag.as_deref(), Some("\"keep\""));
        assert_eq!(
            job.validators.last_modified.as_deref(),
            Some("Wed, 21 Oct 2015 07:28:00 GMT")
        );

        apply_commit_identity(
            &mut job,
            &CommitIdentity {
                validators: Some(ContentValidators {
                    etag: None,
                    last_modified: None,
                    expected_size: Some(999),
                }),
                ..Default::default()
            },
        );
        assert_eq!(job.validators.etag.as_deref(), Some("\"keep\""));
        assert_eq!(job.validators.expected_size, Some(999));
    }

    #[test]
    fn apply_tick_writes_segment_written_in_place() {
        let mut job = sample_job(JobState::Downloading);
        let map = sample_map();
        job.segment_map = Some(map.clone());
        let ok = apply_tick(
            &mut job,
            ProgressTick {
                downloaded_bytes: Some(40),
                segment_written: Some(vec![40]),
                ..Default::default()
            },
        );
        assert!(ok);
        let after = job.segment_map.expect("map kept");
        assert_eq!(after.segments[0].written, 40);
        assert_eq!(after.segments[0].start, map.segments[0].start);
        assert_eq!(after.segments[0].end, map.segments[0].end);
        assert_eq!(job.downloaded_bytes, 40);
        assert_eq!(job.validators, ContentValidators::default());
    }

    #[test]
    fn apply_tick_does_not_roll_written_backward_after_commit() {
        let mut job = sample_job(JobState::Downloading);
        let mut map = sample_map();
        map.segments[0].written = 100;
        job.segment_map = Some(map);
        job.downloaded_bytes = 100;

        let pending = ProgressTick {
            segment_written: Some(vec![80]),
            ..Default::default()
        };
        let persist = ProgressTick {
            downloaded_bytes: Some(100),
            segment_written: None,
            ..Default::default()
        };
        let merged = pending.merge(persist);
        assert_eq!(merged.segment_written.as_deref(), Some(&[80][..]));
        apply_tick(&mut job, merged);
        assert_eq!(job.segment_map.as_ref().unwrap().segments[0].written, 100);
        assert_eq!(job.downloaded_bytes, 100);
    }

    #[test]
    fn apply_tick_ignores_segment_written_len_mismatch() {
        let mut job = sample_job(JobState::Downloading);
        job.segment_map = Some(sample_map());
        apply_tick(
            &mut job,
            ProgressTick {
                segment_written: Some(vec![1, 2, 3]),
                ..Default::default()
            },
        );
        assert_eq!(job.segment_map.as_ref().unwrap().segments[0].written, 10);
    }

    #[tokio::test]
    async fn memory_identity_applies_and_snapshots() {
        let mut job = sample_job(JobState::Queued);
        let identity = MemoryIdentity::default();
        identity
            .commit(
                &mut job,
                CommitIdentity {
                    transfer_format_version: Some(1),
                    map: MapUpdate::Set(sample_map()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.transfer_format_version, 1);
        let snaps = identity.snapshots.lock().unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].transfer_format_version, 1);
        assert!(snaps[0].segment_map.is_some());
    }

    #[tokio::test]
    async fn noop_identity_applies_without_snapshot() {
        let mut job = sample_job(JobState::Canceled);
        NoopIdentity
            .commit(
                &mut job,
                CommitIdentity {
                    map: MapUpdate::Clear,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(job.state, JobState::Canceled);
        assert!(job.segment_map.is_none());
    }
}
