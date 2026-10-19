//! v1 map missing/inconsistent → Resume error (never invent Range offsets).

use super::context::TransferContext;
use super::http::{apply_preflight, control_outcome, run_http_download_with_ctx};
use super::job::{
    download_error, DownloadError, DownloadOutcome, FailureCategory, Job, TransferMode,
};
use super::multi::{resume_restart_required, run_multi_segment_download};
use super::preflight::PreflightInfo;
use super::progress::{CommitIdentity, ProgressTick, TransferEvent};
use super::resume::{resume_oracle, ResumeOracle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferPlanReason {
    SizeUnknown,
    RangesUnknown,
    RangesUnsupported,
    BelowMinSize,
    LegacyContiguousPartial,
    /// `version >= 1` and no map — Resume error, do not invent ranges.
    MapMissing,
    /// Map present but inconsistent — Resume error, do not invent ranges.
    MapInconsistent,
    MultiQualified,
    MultiDisabled,
}

pub const LARGE_FILE_MULTI_UNAVAILABLE_TOAST: &str =
    "Multi-connection unavailable for this large file; using a single connection.";

impl TransferPlanReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SizeUnknown => "size_unknown",
            Self::RangesUnknown => "ranges_unknown",
            Self::RangesUnsupported => "ranges_unsupported",
            Self::BelowMinSize => "below_multi_min_bytes",
            Self::LegacyContiguousPartial => "legacy_contiguous_partial",
            Self::MapMissing => "map_missing",
            Self::MapInconsistent => "map_inconsistent",
            Self::MultiQualified => "multi_qualified",
            Self::MultiDisabled => "multi_disabled",
        }
    }

    pub fn would_qualify_multi(self) -> bool {
        matches!(self, Self::MultiQualified)
    }

    pub fn is_resume_required(self) -> bool {
        matches!(self, Self::MapMissing | Self::MapInconsistent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferPlan {
    pub chosen: TransferMode,
    pub reason: TransferPlanReason,
}

#[derive(Debug, Clone)]
pub struct VisibilityUpdate {
    pub tick: ProgressTick,
    pub toast: Option<String>,
}

impl TransferPlan {
    pub fn to_commit_identity(self) -> CommitIdentity {
        CommitIdentity {
            transfer_mode: self.published_mode(),
            fallback_reason: self.fallback_reason().map(str::to_string),
            ..Default::default()
        }
    }

    fn published_mode(self) -> Option<TransferMode> {
        if self.reason.is_resume_required() {
            None
        } else {
            Some(self.chosen)
        }
    }

    pub fn fallback_reason(self) -> Option<&'static str> {
        match self.reason {
            TransferPlanReason::MultiQualified => None,
            other => Some(other.as_str()),
        }
    }

    pub fn to_visibility_update(
        self,
        size: Option<u64>,
        multi_min_bytes: u64,
        existing_reason: Option<&str>,
    ) -> VisibilityUpdate {
        let mut tick = ProgressTick::default();
        if self.chosen == TransferMode::Single {
            tick.active_connections = Some(1);
        }
        let toast = if matches!(
            self.reason,
            TransferPlanReason::RangesUnknown | TransferPlanReason::RangesUnsupported
        ) && size.is_some_and(|n| n >= multi_min_bytes)
            && existing_reason != Some(self.reason.as_str())
        {
            Some(LARGE_FILE_MULTI_UNAVAILABLE_TOAST.into())
        } else {
            None
        };
        VisibilityUpdate { tick, toast }
    }
}

pub fn plan_transfer(
    job: &Job,
    preflight: Option<&PreflightInfo>,
    multi_min_bytes: u64,
    multi_connection_enabled: bool,
) -> TransferPlan {
    let reason = plan_reason(job, preflight, multi_min_bytes, multi_connection_enabled);
    TransferPlan {
        chosen: if reason.would_qualify_multi() {
            TransferMode::Multi
        } else {
            TransferMode::Single
        },
        reason,
    }
}

fn plan_reason(
    job: &Job,
    preflight: Option<&PreflightInfo>,
    multi_min_bytes: u64,
    multi_connection_enabled: bool,
) -> TransferPlanReason {
    // A rejected `.part` is not a resume source. Its map describes bytes this
    // engine refused, so this attempt is planned as a fresh transfer.
    if !job.rejected_partial {
        let oracle = resume_oracle(job);
        match oracle {
            ResumeOracle::RestartRequired => {
                return if job.segment_map.is_none() {
                    TransferPlanReason::MapMissing
                } else {
                    TransferPlanReason::MapInconsistent
                };
            }
            ResumeOracle::Multi { .. } => return TransferPlanReason::MultiQualified,
            ResumeOracle::LegacySingle | ResumeOracle::FreshSingle => {}
        }

        if !multi_connection_enabled {
            return TransferPlanReason::MultiDisabled;
        }

        if matches!(oracle, ResumeOracle::LegacySingle) {
            return TransferPlanReason::LegacyContiguousPartial;
        }
    } else if !multi_connection_enabled {
        return TransferPlanReason::MultiDisabled;
    }

    let Some(size) = known_size(job, preflight) else {
        return TransferPlanReason::SizeUnknown;
    };

    match range_support(job, preflight) {
        RangeSupport::Unknown => return TransferPlanReason::RangesUnknown,
        RangeSupport::Unsupported => return TransferPlanReason::RangesUnsupported,
        RangeSupport::Supported => {}
    }

    if size < multi_min_bytes {
        return TransferPlanReason::BelowMinSize;
    }

    TransferPlanReason::MultiQualified
}

fn known_size(job: &Job, preflight: Option<&PreflightInfo>) -> Option<u64> {
    preflight
        .and_then(|info| info.total_bytes)
        .filter(|&n| n > 0)
        .or_else(|| (job.total_bytes > 0).then_some(job.total_bytes))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeSupport {
    Supported,
    Unsupported,
    Unknown,
}

fn range_support(job: &Job, preflight: Option<&PreflightInfo>) -> RangeSupport {
    match preflight.and_then(|info| info.accept_ranges) {
        Some(true) => RangeSupport::Supported,
        Some(false) => RangeSupport::Unsupported,
        None if job.resume_supported => RangeSupport::Supported,
        None => RangeSupport::Unknown,
    }
}

pub async fn run_transfer(mut ctx: TransferContext) -> Result<DownloadOutcome, DownloadError> {
    let preflight = apply_preflight(&mut ctx).await;

    if let Some(outcome) = control_outcome(&ctx.control) {
        return Ok(outcome);
    }

    let plan = plan_transfer(
        &ctx.job,
        preflight.as_ref(),
        ctx.multi_min_bytes,
        ctx.multi_connection_enabled,
    );
    let existing_reason = ctx.job.fallback_reason.clone();
    if let Some(reason) = plan.fallback_reason() {
        ctx.job.fallback_reason = Some(reason.to_string());
    }
    ctx.committer
        .commit(&mut ctx.job, plan.to_commit_identity())
        .await
        .map_err(|message| download_error(FailureCategory::Internal, message, false))?;
    let size = known_size(&ctx.job, preflight.as_ref());
    let visibility =
        plan.to_visibility_update(size, ctx.multi_min_bytes, existing_reason.as_deref());
    (ctx.on_progress)(TransferEvent::Tick(visibility.tick));
    if let Some(toast) = visibility.toast {
        (ctx.on_progress)(TransferEvent::Toast(toast));
    }

    // v1 map missing/inconsistent: never invent Range from metadata_len or a fresh partition.
    if plan.reason.is_resume_required() {
        return Err(resume_restart_required());
    }

    if plan.chosen == TransferMode::Multi {
        return run_multi_segment_download(&mut ctx).await;
    }

    run_http_download_with_ctx(&mut ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::io::{Read, Seek, SeekFrom};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(windows)]
    use std::os::windows::fs::OpenOptionsExt;

    use crate::download::bandwidth::GlobalBandwidthLimiter;
    use crate::download::conn_budget::ConnectionBudget;
    use crate::download::engine::EngineRuntimeConfig;
    use crate::download::fetch::CONTROL_PAUSED;
    use crate::download::filesystem::{
        install_rejected_clear_hook, reconcile_partial_progress, remove_rejected_clear_hook,
    };
    use crate::download::job::{fallback_reason_label, FailureCategory, Job};
    use crate::download::multi::RESUME_RESTART_MESSAGE;
    use crate::download::progress::{NoopIdentity, TestProgress, TransferEventCallback};
    use crate::download::segment::{Segment, SegmentMap, SegmentState};
    use crate::download::verify::{sha256_hex, SHA256_EMPTY};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    struct OnDrop<F: FnOnce()>(Option<F>);

    impl<F: FnOnce()> OnDrop<F> {
        fn new(f: F) -> Self {
            Self(Some(f))
        }
    }

    impl<F: FnOnce()> Drop for OnDrop<F> {
        fn drop(&mut self) {
            if let Some(f) = self.0.take() {
                f();
            }
        }
    }

    /// Undo a test lock that blocks both delete and truncate. Missing paths
    /// are ignored so a panic during cleanup can still delete the directory.
    fn restore_reject_lock(dir: &std::path::Path, temp: &std::path::Path) {
        #[cfg(unix)]
        {
            if let Ok(meta) = std::fs::metadata(temp) {
                let mut file_perms = meta.permissions();
                file_perms.set_mode(0o644);
                let _ = std::fs::set_permissions(temp, file_perms);
            }
            if let Ok(meta) = std::fs::metadata(dir) {
                let mut dir_perms = meta.permissions();
                dir_perms.set_mode(0o755);
                let _ = std::fs::set_permissions(dir, dir_perms);
            }
        }
        #[cfg(windows)]
        {
            if let Ok(meta) = std::fs::metadata(temp) {
                let mut perms = meta.permissions();
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(temp, perms);
            }
            let _ = dir;
        }
    }

    fn release_rejected_lock(
        dir: &std::path::Path,
        temp: &std::path::Path,
        held: &Mutex<Option<std::fs::File>>,
    ) {
        remove_rejected_clear_hook(temp);
        *held.lock().unwrap_or_else(|error| error.into_inner()) = None;
        restore_reject_lock(dir, temp);
    }

    /// `FILE_SHARE_NONE` denies a second open, so the bytes are read through
    /// the handle that holds the lock. A mode-0444 file can be read directly.
    fn read_rejected_partial(
        temp: &std::path::Path,
        held: &Mutex<Option<std::fs::File>>,
    ) -> Vec<u8> {
        #[cfg(windows)]
        {
            let mut guard = held.lock().unwrap_or_else(|error| error.into_inner());
            let file = guard
                .as_mut()
                .expect("the sharing lock holds the rejected partial");
            file.seek(SeekFrom::Start(0)).unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            let _ = temp;
            bytes
        }
        #[cfg(not(windows))]
        {
            let _ = held;
            std::fs::read(temp).expect("rejected partial")
        }
    }

    fn sample_job() -> Job {
        Job::new(
            "https://example.com/file.bin".into(),
            "file.bin".into(),
            PathBuf::from("C:\\dl\\file.bin"),
            PathBuf::from("C:\\dl\\file.bin.part"),
        )
    }

    fn test_ctx(job: Job, on_progress: TransferEventCallback) -> TransferContext {
        test_ctx_cfg(job, on_progress, EngineRuntimeConfig::default())
    }

    fn test_ctx_cfg(
        job: Job,
        on_progress: TransferEventCallback,
        config: EngineRuntimeConfig,
    ) -> TransferContext {
        test_ctx_commit(job, on_progress, config, Arc::new(NoopIdentity))
    }

    fn test_ctx_commit(
        job: Job,
        on_progress: TransferEventCallback,
        config: EngineRuntimeConfig,
        committer: Arc<dyn crate::download::progress::IdentityCommit>,
    ) -> TransferContext {
        test_ctx_control(
            job,
            on_progress,
            config,
            committer,
            Arc::new(AtomicU8::new(0)),
        )
    }

    fn test_ctx_control(
        job: Job,
        on_progress: TransferEventCallback,
        config: EngineRuntimeConfig,
        committer: Arc<dyn crate::download::progress::IdentityCommit>,
        control: Arc<AtomicU8>,
    ) -> TransferContext {
        TransferContext::from_runtime(
            job,
            control,
            on_progress,
            None,
            GlobalBandwidthLimiter::new(None),
            ConnectionBudget::new(32, 8),
            committer,
            &config,
        )
    }

    fn preflight(total: Option<u64>, accept_ranges: Option<bool>) -> PreflightInfo {
        PreflightInfo {
            total_bytes: total,
            filename: None,
            accept_ranges,
            etag: None,
            last_modified: None,
            final_url: "https://example.com/file.bin".into(),
        }
    }

    #[test]
    fn planner_unknown_size_stays_single() {
        let job = sample_job();
        let pf = preflight(None, Some(true));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        assert_eq!(plan.reason, TransferPlanReason::SizeUnknown);
        assert_eq!(plan.chosen, TransferMode::Single);
    }

    #[test]
    fn planner_uses_job_size_when_preflight_missing() {
        let mut job = sample_job();
        job.total_bytes = 8 * 1024 * 1024;
        job.resume_supported = true;
        let plan = plan_transfer(&job, None, 5 * 1024 * 1024, true);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
        assert_eq!(plan.chosen, TransferMode::Multi);
        assert_eq!(plan.reason.as_str(), "multi_qualified");
        assert!(plan.to_commit_identity().fallback_reason.is_none());
    }

    #[test]
    fn planner_ranges_unknown_stays_single() {
        let job = sample_job();
        let pf = preflight(Some(8 * 1024 * 1024), None);
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        assert_eq!(plan.reason, TransferPlanReason::RangesUnknown);
        assert_eq!(
            plan.to_commit_identity().fallback_reason.as_deref(),
            Some("ranges_unknown")
        );
    }

    #[test]
    fn planner_ranges_unsupported_stays_single() {
        let job = sample_job();
        let pf = preflight(Some(8 * 1024 * 1024), Some(false));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        assert_eq!(plan.reason, TransferPlanReason::RangesUnsupported);
        assert_eq!(plan.chosen, TransferMode::Single);
        assert_eq!(
            plan.to_commit_identity().fallback_reason.as_deref(),
            Some("ranges_unsupported")
        );
    }

    #[test]
    fn planner_below_min_stays_single() {
        let job = sample_job();
        let pf = preflight(Some(1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        assert_eq!(plan.reason, TransferPlanReason::BelowMinSize);
        assert_eq!(plan.chosen, TransferMode::Single);
        assert_eq!(
            plan.to_commit_identity().fallback_reason.as_deref(),
            Some("below_multi_min_bytes")
        );
    }

    #[test]
    fn planner_multi_qualifies_chooses_multi() {
        let job = sample_job();
        let pf = preflight(Some(10 * 1024 * 1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        assert_eq!(plan.chosen, TransferMode::Multi);
        assert!(plan.reason.would_qualify_multi());
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
        let patch = plan.to_commit_identity();
        assert_eq!(patch.transfer_mode, Some(TransferMode::Multi));
        assert!(patch.fallback_reason.is_none());
    }

    #[test]
    fn planner_handoff_does_not_block_multi_qualification() {
        let mut job = sample_job();
        job.total_bytes = 20 * 1024 * 1024;
        job.resume_supported = true;
        let plan = plan_transfer(&job, None, 5 * 1024 * 1024, true);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
        assert_eq!(plan.chosen, TransferMode::Multi);
    }

    #[test]
    fn planner_job_resume_supported_counts_as_ranges() {
        let mut job = sample_job();
        job.total_bytes = 9 * 1024 * 1024;
        job.resume_supported = true;
        let pf = preflight(Some(9 * 1024 * 1024), None);
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
        assert_eq!(plan.chosen, TransferMode::Multi);
    }

    #[test]
    fn planner_existing_map_forces_multi_even_when_preflight_unqualifies() {
        let mut job = sample_job();
        job.transfer_format_version = 1;
        job.total_bytes = 2 * 1024 * 1024;
        job.segment_map = Some(crate::download::segment::partition(2 * 1024 * 1024, 2));
        let pf = preflight(None, None);
        let plan = plan_transfer(&job, Some(&pf), 50 * 1024 * 1024, true);
        assert_eq!(plan.chosen, TransferMode::Multi);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
    }

    #[test]
    fn planner_one_segment_map_is_multi() {
        let mut job = sample_job();
        job.transfer_format_version = 1;
        job.total_bytes = 1000;
        job.downloaded_bytes = 250;
        job.segment_map = Some(crate::download::segment::SegmentMap {
            total_bytes: 1000,
            segment_count: 1,
            segments: vec![crate::download::segment::Segment {
                index: 0,
                start: 0,
                end: 999,
                written: 250,
                state: crate::download::segment::SegmentState::Active,
            }],
            preallocated: true,
        });
        let pf = preflight(None, None);
        let plan = plan_transfer(&job, Some(&pf), 50 * 1024 * 1024, true);
        assert_eq!(plan.chosen, TransferMode::Multi);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
    }

    #[test]
    fn planner_legacy_v0_partial_stays_single_until_restart() {
        let mut job = sample_job();
        job.downloaded_bytes = 4096;
        job.total_bytes = 10 * 1024 * 1024;
        job.resume_supported = true;
        job.transfer_format_version = 0;
        let pf = preflight(Some(10 * 1024 * 1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        assert_eq!(plan.chosen, TransferMode::Single);
        assert_eq!(plan.reason, TransferPlanReason::LegacyContiguousPartial);
        assert_eq!(
            plan.to_commit_identity().fallback_reason.as_deref(),
            Some("legacy_contiguous_partial")
        );
    }

    #[test]
    fn planner_v1_map_missing_is_resume_required() {
        let mut job = sample_job();
        job.transfer_format_version = 1;
        job.total_bytes = 10 * 1024 * 1024;
        job.resume_supported = true;
        let pf = preflight(Some(10 * 1024 * 1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        assert!(plan.reason.is_resume_required());
        assert_eq!(plan.reason, TransferPlanReason::MapMissing);
        assert_eq!(plan.chosen, TransferMode::Single);
        let patch = plan.to_commit_identity();
        assert_eq!(patch.fallback_reason.as_deref(), Some("map_missing"));
        assert!(patch.transfer_mode.is_none());
    }

    #[test]
    fn planner_v1_map_inconsistent_is_resume_required() {
        let mut job = sample_job();
        job.transfer_format_version = 1;
        job.total_bytes = 1000;
        job.segment_map = Some(crate::download::segment::SegmentMap {
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![],
            preallocated: false,
        });
        let pf = preflight(Some(1000), Some(true));
        let plan = plan_transfer(&job, Some(&pf), 1, true);
        assert_eq!(plan.reason, TransferPlanReason::MapInconsistent);
        assert!(plan.reason.is_resume_required());
        assert_eq!(
            plan.to_commit_identity().fallback_reason.as_deref(),
            Some("map_inconsistent")
        );
    }

    #[test]
    fn planner_v1_map_missing_still_resume_required_when_multi_disabled() {
        let mut job = sample_job();
        job.transfer_format_version = 1;
        job.total_bytes = 10 * 1024 * 1024;
        job.resume_supported = true;
        let pf = preflight(Some(10 * 1024 * 1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, false);
        assert!(plan.reason.is_resume_required());
        assert_eq!(plan.reason, TransferPlanReason::MapMissing);
        assert_eq!(
            plan.to_commit_identity().fallback_reason.as_deref(),
            Some("map_missing")
        );
    }

    #[test]
    fn planner_v1_map_inconsistent_still_resume_required_when_multi_disabled() {
        let mut job = sample_job();
        job.transfer_format_version = 1;
        job.total_bytes = 1000;
        job.segment_map = Some(crate::download::segment::SegmentMap {
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![],
            preallocated: false,
        });
        let pf = preflight(Some(1000), Some(true));
        let plan = plan_transfer(&job, Some(&pf), 1, false);
        assert!(plan.reason.is_resume_required());
        assert_eq!(plan.reason, TransferPlanReason::MapInconsistent);
        assert_eq!(
            plan.to_commit_identity().fallback_reason.as_deref(),
            Some("map_inconsistent")
        );
    }

    #[test]
    fn planner_rejected_partial_ignores_map_and_plans_fresh() {
        let mut job = sample_job();
        job.rejected_partial = true;
        job.transfer_format_version = 1;
        job.total_bytes = 32;
        job.resume_supported = true;
        job.segment_map = Some(SegmentMap {
            total_bytes: 32,
            segment_count: 1,
            segments: vec![Segment {
                index: 0,
                start: 0,
                end: 31,
                written: 32,
                state: SegmentState::Completed,
            }],
            preallocated: true,
        });
        let plan = plan_transfer(&job, None, 5 * 1024 * 1024, true);
        assert!(!plan.reason.is_resume_required());
        assert_ne!(plan.reason, TransferPlanReason::MultiQualified);
        assert_eq!(plan.chosen, TransferMode::Single);

        job.segment_map = Some(SegmentMap {
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![],
            preallocated: false,
        });
        job.total_bytes = 1000;
        let pf = preflight(Some(1000), Some(true));
        let inconsistent = plan_transfer(&job, Some(&pf), 1, true);
        assert!(
            !inconsistent.reason.is_resume_required(),
            "an untrusted map must not block Retry behind Restart"
        );
        assert_eq!(inconsistent.reason, TransferPlanReason::MultiQualified);
        assert_eq!(inconsistent.chosen, TransferMode::Multi);
    }

    #[test]
    fn planner_multi_disabled_publishes_reason_and_label() {
        let job = sample_job();
        let pf = preflight(Some(10 * 1024 * 1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, false);
        assert_eq!(plan.reason, TransferPlanReason::MultiDisabled);
        assert_eq!(plan.chosen, TransferMode::Single);
        assert_eq!(plan.reason.as_str(), "multi_disabled");
        assert_eq!(
            plan.to_commit_identity().fallback_reason.as_deref(),
            Some("multi_disabled")
        );
        assert_eq!(
            fallback_reason_label("multi_disabled"),
            "Multi-connection disabled"
        );
        let patch = plan.to_visibility_update(Some(10 * 1024 * 1024), 5 * 1024 * 1024, None);
        assert!(
            patch.toast.is_none(),
            "user-disabled multi must not fire the unavailable toast"
        );
        assert_eq!(patch.tick.active_connections, Some(1));
    }

    #[test]
    fn planner_disabled_still_forces_multi_when_map_present() {
        let mut job = sample_job();
        job.transfer_format_version = 1;
        job.total_bytes = 2 * 1024 * 1024;
        job.segment_map = Some(crate::download::segment::partition(2 * 1024 * 1024, 2));
        let pf = preflight(None, None);
        let plan = plan_transfer(&job, Some(&pf), 50 * 1024 * 1024, false);
        assert_eq!(plan.chosen, TransferMode::Multi);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
    }

    #[tokio::test]
    async fn run_transfer_stays_single_when_below_min() {
        let body = vec![b'x'; 64];
        let head = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: {}\r\n\
\r\n",
            body.len()
        );
        let get = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: {}\r\n\
Accept-Ranges: bytes\r\n\
\r\n",
            body.len()
        );
        let (base, _handle) = spawn_scripted_server(vec![head, get]).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-xfer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/small.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp);

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
        );
        let outcome = run_transfer(ctx).await.expect("transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));

        let data = std::fs::read(&target).expect("final file");
        assert_eq!(data, body);

        let snaps = progress.snapshots();
        let mode_job = snaps
            .iter()
            .find(|job| job.transfer_mode.is_some())
            .expect("planner should publish transfer_mode");
        assert_eq!(mode_job.transfer_mode, Some(TransferMode::Single));
        assert_eq!(
            mode_job.fallback_reason.as_deref(),
            Some("below_multi_min_bytes")
        );
        assert!(progress
            .events()
            .iter()
            .all(|event| !matches!(event, TransferEvent::Toast(_))));
        assert!(progress.events().iter().any(|event| matches!(
            event,
            TransferEvent::Tick(tick) if tick.active_connections == Some(1)
        )));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn visibility_large_file_ranges_unsupported_toasts_once() {
        let job = sample_job();
        let pf = preflight(Some(8 * 1024 * 1024), Some(false));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        let patch = plan.to_visibility_update(Some(8 * 1024 * 1024), 5 * 1024 * 1024, None);
        assert_eq!(plan.reason, TransferPlanReason::RangesUnsupported);
        assert_eq!(
            plan.to_commit_identity().transfer_mode,
            Some(TransferMode::Single)
        );
        assert_eq!(plan.fallback_reason(), Some("ranges_unsupported"));
        assert_eq!(
            patch.toast.as_deref(),
            Some(LARGE_FILE_MULTI_UNAVAILABLE_TOAST)
        );
        assert_eq!(patch.tick.active_connections, Some(1));
    }

    #[test]
    fn visibility_small_file_ranges_unsupported_no_toast() {
        let job = sample_job();
        let pf = preflight(Some(1024), Some(false));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        let patch = plan.to_visibility_update(Some(1024), 5 * 1024 * 1024, None);
        assert_eq!(plan.reason, TransferPlanReason::RangesUnsupported);
        assert_eq!(plan.fallback_reason(), Some("ranges_unsupported"));
        assert!(patch.toast.is_none());
    }

    #[test]
    fn visibility_below_min_no_fallback_reason() {
        let job = sample_job();
        let pf = preflight(Some(1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        let patch = plan.to_visibility_update(Some(1024), 5 * 1024 * 1024, None);
        assert_eq!(plan.reason, TransferPlanReason::BelowMinSize);
        assert_eq!(plan.fallback_reason(), Some("below_multi_min_bytes"));
        assert!(patch.toast.is_none());
    }

    #[test]
    fn visibility_same_reason_already_set_skips_toast() {
        let job = sample_job();
        let pf = preflight(Some(8 * 1024 * 1024), Some(false));
        let plan = plan_transfer(&job, Some(&pf), 5 * 1024 * 1024, true);
        let first = plan.to_visibility_update(Some(8 * 1024 * 1024), 5 * 1024 * 1024, None);
        assert_eq!(
            first.toast.as_deref(),
            Some(LARGE_FILE_MULTI_UNAVAILABLE_TOAST)
        );
        let second = plan.to_visibility_update(
            Some(8 * 1024 * 1024),
            5 * 1024 * 1024,
            plan.fallback_reason(),
        );
        assert_eq!(plan.fallback_reason(), Some("ranges_unsupported"));
        assert!(second.toast.is_none(), "retry/resume must not re-toast");
        assert_eq!(second.tick.active_connections, Some(1));
    }

    #[tokio::test]
    async fn run_transfer_large_file_without_ranges_toasts_once() {
        let body = vec![b'x'; 64];
        let head = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: none\r\n\
Content-Length: {}\r\n\
\r\n",
            body.len()
        );
        let get = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: {}\r\n\
\r\n",
            body.len()
        );
        let (base, _handle) = spawn_scripted_server(vec![head, get]).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-xfer-nr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/big.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp);

        let progress = TestProgress::new();
        let config = EngineRuntimeConfig {
            multi_min_bytes: body.len() as u64,
            ..EngineRuntimeConfig::default()
        };
        let ctx = test_ctx_commit(job, progress.callback(), config, progress.identity.clone());
        let outcome = run_transfer(ctx).await.expect("transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));

        let toasts: Vec<_> = progress
            .events()
            .iter()
            .filter_map(|event| match event {
                TransferEvent::Toast(msg) => Some(msg.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(toasts.len(), 1, "toast must be one-shot, got {toasts:?}");
        assert_eq!(toasts[0], LARGE_FILE_MULTI_UNAVAILABLE_TOAST);
        assert!(progress
            .snapshots()
            .iter()
            .any(|job| job.fallback_reason.as_deref() == Some("ranges_unsupported")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_transfer_skips_toast_when_fallback_reason_already_matches() {
        let body = vec![b'x'; 64];
        let head = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: none\r\n\
Content-Length: {}\r\n\
\r\n",
            body.len()
        );
        let get = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: {}\r\n\
\r\n",
            body.len()
        );
        let (base, _handle) = spawn_scripted_server(vec![head, get]).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-xfer-nr2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/big.bin");
        let mut job = Job::new(url, "out.bin".into(), target, temp);
        job.fallback_reason = Some("ranges_unsupported".into());

        let progress = TestProgress::new();
        let config = EngineRuntimeConfig {
            multi_min_bytes: body.len() as u64,
            ..EngineRuntimeConfig::default()
        };
        let ctx = test_ctx_commit(job, progress.callback(), config, progress.identity.clone());
        let outcome = run_transfer(ctx).await.expect("transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));

        assert!(
            progress
                .events()
                .iter()
                .all(|event| !matches!(event, TransferEvent::Toast(_))),
            "retry/resume with same reason must not toast"
        );
        assert!(progress
            .snapshots()
            .iter()
            .any(|job| job.fallback_reason.as_deref() == Some("ranges_unsupported")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fresh_200_shorter_than_head_content_length_is_size_mismatch() {
        let body = vec![b'a'; 4096];
        let err = head_then_get_error(65_536, Some(body.len() as u64), &body).await;
        assert_size_mismatch(&err, body.len() as u64, 65_536);
    }

    #[tokio::test]
    async fn fresh_200_larger_than_head_content_length_is_size_mismatch() {
        let body = vec![b'b'; 65_536];
        let err = head_then_get_error(4096, Some(body.len() as u64), &body).await;
        assert_size_mismatch(&err, body.len() as u64, 4096);
    }

    #[tokio::test]
    async fn fresh_200_matching_head_content_length_completes() {
        let body = vec![b'c'; 4096];
        let (dir, target, outcome) =
            head_then_get(body.len() as u64, Some(body.len() as u64), &body).await;
        let outcome = outcome.expect("matching Content-Length must complete");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stored_expected_size_rejects_shorter_200_content_length() {
        let body = vec![b'd'; 4096];
        let get = response_200(Some(body.len() as u64), false);
        let (base, _seen, _handle) =
            spawn_scripted_server_recording_with_body(vec![get], body.clone()).await;
        let (dir, target, _temp, mut job) = download_paths(&base);
        job.validators.expected_size = Some(65_536);
        let progress = TestProgress::new();
        let mut ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
        );
        ctx.preflight_done = true;

        let err = transfer_within(ctx)
            .await
            .expect_err("stored expected_size must reject a shorter 200");
        assert_size_mismatch(&err, body.len() as u64, 65_536);
        assert!(
            !target.exists(),
            "a conflicting 200 must not be renamed to the finished file"
        );
        assert!(
            progress.snapshots().iter().all(|job| {
                job.total_bytes != body.len() as u64
                    && job.validators.expected_size != Some(body.len() as u64)
            }),
            "must not overwrite the stored size with the shorter Content-Length"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fresh_200_without_content_length_short_of_known_size_is_not_completed() {
        let head_len = 65_536u64;
        let body = vec![b'e'; 4096];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.clone();
        let _handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = response_200(Some(head_len), true);
                    let _ = socket.write_all(reply.as_bytes()).await;
                } else if req.starts_with("GET ") {
                    let reply = response_200(None, false);
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&payload).await;
                }
                let _ = socket.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let (dir, target, _temp, job) = download_paths(&base);
        let progress = TestProgress::new();
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
        );
        let err = transfer_within(ctx)
            .await
            .expect_err("a short body with no Content-Length must not complete");
        assert_size_mismatch(&err, body.len() as u64, head_len);
        assert!(
            !target.exists(),
            "short body must not become the finished file"
        );
        assert!(
            progress.snapshots().iter().all(|job| job.progress < 100.0),
            "short body must not be committed as finished"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fresh_200_without_content_length_matching_known_size_completes() {
        let body = vec![b'f'; 4096];
        let head = response_200(Some(body.len() as u64), true);
        let get = response_200(None, false);
        let (base, _seen, _handle) =
            spawn_scripted_server_recording_with_body(vec![head, get], body.clone()).await;
        let (dir, target, _temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = transfer_within(ctx)
            .await
            .expect("a body that matches the known size must complete");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fresh_200_without_content_length_past_known_size_is_size_mismatch() {
        let head_len = 65_536u64;
        let body = vec![b'g'; 131_072];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.clone();
        let _handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = response_200(Some(head_len), true);
                    let _ = socket.write_all(reply.as_bytes()).await;
                } else if req.starts_with("GET ") {
                    let reply = response_200(None, false);
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&payload).await;
                }
                let _ = socket.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let (dir, target, temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let err = transfer_within(ctx)
            .await
            .expect_err("a body longer than the known size must not complete");
        let observed = observed_mismatch_size(&err);
        assert!(
            observed > head_len,
            "overrun must report a size past {head_len}, got {err:?}"
        );
        assert!(
            err.message.contains(&head_len.to_string()),
            "expected the known size in {err:?}"
        );
        assert!(
            !target.exists(),
            "overrun must not become the finished file"
        );
        assert!(
            !temp.exists(),
            "rejected overrun bytes must not remain for a later Retry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn full_replace_200_with_same_identity_and_shorter_content_length_is_size_mismatch() {
        let entity = 262_144u64;
        let dropped = 100_000usize;
        let replacement = vec![b'h'; 4096];
        let identity = "ETag: \"v1\"\r\nLast-Modified: Wed, 01 Jan 2020 00:00:00 GMT\r\n";
        let base = serve_drop_then_range(
            entity,
            identity,
            dropped,
            &format!("Content-Length: {}\r\n{identity}", replacement.len()),
            replacement.clone(),
        )
        .await;
        let (dir, target, temp, job) = download_paths(&base);
        let progress = TestProgress::new();
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
        );
        let err = transfer_within(ctx)
            .await
            .expect_err("a full-replace 200 must match the known size");
        assert_size_mismatch(&err, replacement.len() as u64, entity);
        assert!(
            !target.exists(),
            "short replacement must not be the finished file"
        );
        assert_eq!(
            std::fs::metadata(&temp).expect("original partial").len(),
            dropped as u64,
            "a rejected full-replace must keep the bytes already downloaded"
        );
        assert!(
            progress.snapshots().iter().all(|job| {
                job.total_bytes != replacement.len() as u64
                    && job.validators.expected_size != Some(replacement.len() as u64)
            }),
            "must not adopt the shorter Content-Length"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn full_replace_200_without_content_length_short_of_known_size_is_size_mismatch() {
        let entity = 262_144u64;
        let replacement = vec![b'i'; 4096];
        let identity = "ETag: \"v1\"\r\nLast-Modified: Wed, 01 Jan 2020 00:00:00 GMT\r\n";
        let base =
            serve_drop_then_range(entity, identity, 100_000, identity, replacement.clone()).await;
        let (dir, target, _temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let err = transfer_within(ctx)
            .await
            .expect_err("a short full-replace body must not complete");
        assert_size_mismatch(&err, replacement.len() as u64, entity);
        assert!(
            !target.exists(),
            "short body must not become the finished file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn full_replace_200_with_changed_last_modified_accepts_new_size() {
        let entity = 262_144u64;
        let replacement = vec![b'j'; 4096];
        let head_identity = "ETag: \"v1\"\r\nLast-Modified: Wed, 01 Jan 2020 00:00:00 GMT\r\n";
        let resume_identity = "ETag: \"v1\"\r\nLast-Modified: Thu, 02 Jan 2020 00:00:00 GMT\r\n";
        let base = serve_drop_then_range(
            entity,
            head_identity,
            100_000,
            &format!("Content-Length: {}\r\n{resume_identity}", replacement.len()),
            replacement.clone(),
        )
        .await;
        let (dir, target, _temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = transfer_within(ctx)
            .await
            .expect("a changed Last-Modified may replace the entity");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), replacement);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn full_replace_200_matching_known_size_completes() {
        let body = vec![b'k'; 80];
        let identity = "ETag: \"v1\"\r\nLast-Modified: Wed, 01 Jan 2020 00:00:00 GMT\r\n";
        let base = serve_drop_then_range(
            body.len() as u64,
            identity,
            40,
            &format!("Content-Length: {}\r\n{identity}", body.len()),
            body.clone(),
        )
        .await;
        let (dir, target, _temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = transfer_within(ctx)
            .await
            .expect("a full-replace 200 of the known size must complete");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn retry_after_lengthless_overrun_does_not_keep_rejected_bytes() {
        let head_len = 65_536u64;
        let correct = vec![b'A'; head_len as usize];
        let rejected = vec![b'B'; head_len as usize + 1];
        let file = retry_after_size_mismatch(head_len, rejected, correct.clone()).await;
        assert_eq!(file, correct, "Retry must not keep the rejected prefix");
    }

    #[tokio::test]
    async fn retry_after_lengthless_short_body_does_not_splice() {
        let head_len = 65_536u64;
        let correct = vec![b'A'; head_len as usize];
        let rejected = vec![b'B'; 4096];
        let file = retry_after_size_mismatch(head_len, rejected, correct.clone()).await;
        assert_eq!(
            file, correct,
            "Retry must not splice the rejected prefix onto the real file"
        );
    }

    /// Delete of a rejected `.part` can fail while the file stays writable
    /// (a mode-0555 directory, or a Windows share that denies delete). The
    /// next attempt still trusts the on-disk length, so the rejected bytes
    /// have to be truncated or that length is spliced onto the real file.
    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn size_mismatch_truncates_partial_when_delete_fails() {
        let head_len = 4096u64;
        let rejected = vec![b'B'; 128];
        let correct = vec![b'A'; head_len as usize];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let rejected_body = rejected.clone();
        let correct_body = correct.clone();
        tokio::spawn(async move {
            let mut rejected_sent = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {head_len}\r\n\r\n"
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0);
                if start == 0 && !rejected_sent {
                    rejected_sent = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&rejected_body).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if start > 0 {
                    let start_us = start as usize;
                    let suffix = &correct_body[start_us.min(correct_body.len())..];
                    let end = correct_body.len().saturating_sub(1);
                    let reply = format!(
                        "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{head_len}\r\nContent-Length: {}\r\n\r\n",
                        suffix.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(suffix).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {head_len}\r\n\r\n"
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&correct_body).await;
                let _ = socket.shutdown().await;
            }
        });

        let dir =
            std::env::temp_dir().join(format!("rusticdl-reject-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        std::fs::write(&temp, b"").unwrap();
        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(&dir).unwrap().permissions();
            perms.set_mode(0o555);
            std::fs::set_permissions(&dir, perms).unwrap();
        }
        // Share read and write, but not delete. Rust's own opens share delete,
        // so this holder is what makes `remove_file` fail while truncate still works.
        #[cfg(windows)]
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0x0000_0001 | 0x0000_0002)
            .open(&temp)
            .unwrap();

        let url = format!("http://{addr}/file.bin");
        let job = Job::new(url.clone(), "out.bin".into(), target.clone(), temp.clone());
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let err = transfer_within(ctx)
            .await
            .expect_err("a short lengthless body must fail");
        let len = std::fs::metadata(&temp).map(|meta| meta.len()).ok();
        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(&dir).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&dir, perms).unwrap();
        }
        #[cfg(windows)]
        drop(held);

        assert_size_mismatch(&err, rejected.len() as u64, head_len);
        assert_eq!(
            len,
            Some(0),
            "a failed delete must truncate the rejected partial"
        );
        assert!(!target.exists());

        let job = Job::new(url, "out.bin".into(), target.clone(), temp);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = transfer_within(ctx)
            .await
            .expect("Retry after a truncated reject downloads the real file");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(
            std::fs::read(&target).expect("final file"),
            correct,
            "the rejected prefix must not be spliced onto the retry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Larger than `AppendSink`'s buffer, so the rejected body is flushed
    /// before discard closes the writer and the test locks the file.
    const SINK_FLUSH_BYTES: usize = 256 * 1024;

    #[derive(Clone, Copy)]
    enum LockedReject {
        Overrun,
        Short,
    }

    fn locked_reject_known(kind: LockedReject) -> usize {
        match kind {
            LockedReject::Overrun => SINK_FLUSH_BYTES + 64 * 1024,
            LockedReject::Short => 512 * 1024,
        }
    }

    fn locked_reject_kept(kind: LockedReject) -> usize {
        match kind {
            LockedReject::Overrun => locked_reject_known(kind),
            LockedReject::Short => SINK_FLUSH_BYTES + 128 * 1024,
        }
    }

    /// Delete and truncate both fail while the rejected body is still the
    /// `.part`. `release_before_retry` drops that lock before the next attempt.
    #[cfg(any(unix, windows))]
    async fn locked_reject_then_retry(
        kind: LockedReject,
        release_before_retry: bool,
    ) -> Result<Vec<u8>, DownloadError> {
        let known = locked_reject_known(kind);
        let kept = locked_reject_kept(kind);
        let correct = vec![b'A'; known];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let correct_body = correct.clone();
        tokio::spawn(async move {
            let mut rejected_sent = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {known}\r\n\r\n"
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0);
                if start == 0 && !rejected_sent {
                    rejected_sent = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let payload = match kind {
                        LockedReject::Overrun => vec![b'B'; known + 1],
                        LockedReject::Short => vec![b'B'; kept],
                    };
                    let _ = socket.write_all(&payload).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if start > 0 {
                    let start_us = start as usize;
                    let suffix = &correct_body[start_us.min(correct_body.len())..];
                    let end = correct_body.len().saturating_sub(1);
                    let reply = format!(
                        "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{known}\r\nContent-Length: {}\r\n\r\n",
                        suffix.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(suffix).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {known}\r\n\r\n"
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&correct_body).await;
                let _ = socket.shutdown().await;
            }
        });

        let dir =
            std::env::temp_dir().join(format!("rusticdl-reject-both-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let locked = Arc::new(AtomicBool::new(false));
        // The download handle is closed before discard. Only then can a test
        // take the lock that blocks both delete and a later write open.
        // On Windows that lock is FILE_SHARE_NONE. The read-only attribute
        // does not stop Rust's delete disposition.
        let held: Arc<Mutex<Option<std::fs::File>>> = Arc::new(Mutex::new(None));
        install_rejected_clear_hook(
            &temp,
            Arc::new({
                let dir = dir.clone();
                let temp = temp.clone();
                let locked = locked.clone();
                #[cfg(windows)]
                let held = held.clone();
                move || {
                    if locked.swap(true, Ordering::Relaxed) {
                        return;
                    }
                    #[cfg(unix)]
                    {
                        let mut file_perms = std::fs::metadata(&temp).unwrap().permissions();
                        file_perms.set_mode(0o444);
                        std::fs::set_permissions(&temp, file_perms).unwrap();
                        let mut dir_perms = std::fs::metadata(&dir).unwrap().permissions();
                        dir_perms.set_mode(0o555);
                        std::fs::set_permissions(&dir, dir_perms).unwrap();
                    }
                    #[cfg(windows)]
                    {
                        let file = std::fs::OpenOptions::new()
                            .read(true)
                            .share_mode(0)
                            .open(&temp)
                            .expect("the writer is closed, so the partial can be locked");
                        *held.lock().unwrap_or_else(|error| error.into_inner()) = Some(file);
                        let _ = dir;
                    }
                }
            }),
        );
        let _restore_on_drop = OnDrop::new({
            let dir = dir.clone();
            let temp = temp.clone();
            let held = held.clone();
            move || release_rejected_lock(&dir, &temp, &held)
        });

        let url = format!("http://{addr}/file.bin");
        let progress = TestProgress::new();
        let job = Job::new(url, "out.bin".into(), target.clone(), temp.clone());
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
        );
        let err = tokio::time::timeout(Duration::from_secs(15), run_transfer(ctx))
            .await
            .expect("the rejected body must settle")
            .expect_err("the rejected body must not complete");
        assert!(
            locked.load(Ordering::Relaxed),
            "the lock must be held before discard"
        );
        match kind {
            LockedReject::Overrun => assert_size_mismatch(&err, (known + 1) as u64, known as u64),
            LockedReject::Short => assert_size_mismatch(&err, kept as u64, known as u64),
        }
        assert!(
            !target.exists(),
            "the rejected body must not be the finished file"
        );
        let partial = read_rejected_partial(&temp, &held);
        assert_eq!(
            partial,
            vec![b'B'; kept],
            "rejected bytes must survive both failures"
        );
        let mut saved = progress
            .snapshots()
            .into_iter()
            .next_back()
            .expect("discard commits the cleared offset");
        assert!(saved.rejected_partial, "the surviving partial is untrusted");
        assert_eq!(saved.downloaded_bytes, 0);
        let json = serde_json::to_string(&saved).expect("the untrusted flag must persist");
        assert!(json.contains("\"rejectedPartial\":true"));
        saved = serde_json::from_str(&json).expect("reload the saved job");
        let reconciled = reconcile_partial_progress(&mut saved).await;
        assert_eq!(reconciled.downloaded_bytes, 0);
        assert!(!reconciled.used_metadata_len);
        assert!(saved.rejected_partial);

        if release_before_retry {
            release_rejected_lock(&dir, &temp, &held);
        }

        let ctx = test_ctx(saved, Arc::new(|_: TransferEvent| {}));
        let second = tokio::time::timeout(Duration::from_secs(15), run_transfer(ctx))
            .await
            .expect("the retry must settle");
        if !release_before_retry {
            assert!(!target.exists(), "a held lock must not finish the file");
            assert_eq!(
                read_rejected_partial(&temp, &held),
                partial,
                "a held lock must leave the rejected bytes in place"
            );
        }
        let outcome = match second {
            Ok(DownloadOutcome::Completed) => Ok(std::fs::read(&target).unwrap_or_default()),
            Ok(other) => panic!("retry ended as {other:?}"),
            Err(error) => Err(error),
        };

        release_rejected_lock(&dir, &temp, &held);
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn rejected_overrun_is_not_resumed_after_delete_and_truncate_fail() {
        let file = locked_reject_then_retry(LockedReject::Overrun, true)
            .await
            .expect("releasing the lock lets Retry replace the rejected partial");
        assert_eq!(file, vec![b'A'; locked_reject_known(LockedReject::Overrun)]);
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn rejected_short_body_is_not_spliced_after_delete_and_truncate_fail() {
        let file = locked_reject_then_retry(LockedReject::Short, true)
            .await
            .expect("releasing the lock lets Retry replace the rejected partial");
        assert_eq!(file, vec![b'A'; locked_reject_known(LockedReject::Short)]);
    }

    /// Control: the lock is still held, so Retry cannot open the file. It must
    /// say so, and it must not finish the rejected bytes.
    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn rejected_partial_errors_while_the_lock_is_still_held() {
        let err = locked_reject_then_retry(LockedReject::Short, false)
            .await
            .expect_err("a lock that is still held must not complete");
        assert!(
            err.message.contains("another program"),
            "the error must name the program holding the file, got {err:?}"
        );
        assert!(err.message.contains("Retry"), "{err:?}");
        assert_eq!(err.category, FailureCategory::Disk);
        assert!(!err.retryable);
    }

    /// A segment map that counts the rejected file must not finalize those bytes.
    #[tokio::test]
    async fn rejected_segment_map_does_not_finalize_the_partial() {
        let body = vec![b'A'; 32];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0) as usize;
                if start == 0 {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&payload).await;
                } else {
                    let suffix = &payload[start.min(payload.len())..];
                    let end = payload.len().saturating_sub(1);
                    let reply = format!(
                        "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\n\r\n",
                        payload.len(),
                        suffix.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(suffix).await;
                }
                let _ = socket.shutdown().await;
            }
        });

        let (dir, target, temp, mut job) = download_paths(&format!("http://{addr}"));
        std::fs::write(&temp, vec![0xFFu8; body.len()]).unwrap();
        job.downloaded_bytes = body.len() as u64;
        job.total_bytes = body.len() as u64;
        job.resume_supported = true;
        job.transfer_format_version = 1;
        job.rejected_partial = true;
        job.segment_map = Some(SegmentMap {
            total_bytes: body.len() as u64,
            segment_count: 1,
            segments: vec![Segment {
                index: 0,
                start: 0,
                end: body.len() as u64 - 1,
                written: body.len() as u64,
                state: SegmentState::Completed,
            }],
            preallocated: true,
        });
        let reconciled = reconcile_partial_progress(&mut job).await;
        assert_eq!(reconciled.downloaded_bytes, 0);
        assert!(job.segment_map.is_some());
        assert!(job.rejected_partial);

        // Below the real multi minimum this map would stay single. Force the
        // multi plan so a completed map cannot finalize the rejected bytes.
        let config = EngineRuntimeConfig {
            multi_min_bytes: 1,
            ..EngineRuntimeConfig::default()
        };
        let mut ctx = test_ctx_cfg(job, Arc::new(|_: TransferEvent| {}), config);
        ctx.preflight_done = true;
        let outcome = tokio::time::timeout(Duration::from_secs(8), run_transfer(ctx))
            .await
            .expect("the rejected map must settle")
            .expect("Retry downloads the file instead of finalizing the rejected bytes");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn etag_change_without_content_length_finishes_at_the_new_size() {
        let old_len = 64u64;
        let new_body: Vec<u8> = (0..96u8).collect();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = new_body.clone();
        let _handle = tokio::spawn(async move {
            let mut saw_changed = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"v1\"\r\nContent-Length: {old_len}\r\n\r\n"
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0);
                if start == 0 && !saw_changed {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {old_len}\r\n\r\n"
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&[b'o'; 20]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !saw_changed {
                    saw_changed = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nETag: \"v2\"\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&payload).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nETag: \"v2\"\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&payload).await;
                let _ = socket.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let (dir, target, _temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = transfer_within(ctx)
            .await
            .expect("a changed ETag with no Content-Length must finish after reconnect");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), new_body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn etag_change_range_ignored_without_length_finishes_new_file() {
        let file = changed_entity_through_reconnect(ChangedEntityFollowup::RangeIgnored).await;
        assert_eq!(file.len(), NEW_ENTITY_LEN);
        assert!(file.iter().all(|byte| *byte == b'N'));
    }

    #[tokio::test]
    async fn etag_change_if_range_without_length_finishes_new_file() {
        let file = changed_entity_through_reconnect(ChangedEntityFollowup::Standards).await;
        assert_eq!(file.len(), NEW_ENTITY_LEN);
        assert!(file.iter().all(|byte| *byte == b'N'));
    }

    #[tokio::test]
    async fn last_modified_change_without_length_finishes_new_file() {
        let file = changed_entity_through_reconnect(ChangedEntityFollowup::LastModifiedOnly).await;
        assert_eq!(file.len(), NEW_ENTITY_LEN);
        assert!(file.iter().all(|byte| *byte == b'L'));
    }

    /// HEAD 262144 e1, the first body drops, then Range gets e2 with no length and
    /// the full 524288-byte file. A later Range is a 200 with the same e2, no
    /// length, and only 1000 bytes. That follow-up must not replace the file.
    /// A 416 `bytes */524288` with the new ETag then confirms the saved body.
    #[tokio::test]
    async fn truncated_unchanged_etag_after_lengthless_change_is_not_the_file() {
        let full_len = 524_288usize;
        let full = vec![b'N'; full_len];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = full.clone();
        let saw_trap = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_trap_task = saw_trap.clone();
        tokio::spawn(async move {
            let mut dropped_original = false;
            let mut sent_changed = false;
            let mut sent_trap = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e1\"\r\nContent-Length: 262144\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0);
                if start == 0 && !dropped_original {
                    dropped_original = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: 262144\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&[b'o'; 100_000]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !sent_changed {
                    sent_changed = true;
                    let reply =
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e2\"\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&payload).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !sent_trap {
                    sent_trap = true;
                    saw_trap_task.store(true, std::sync::atomic::Ordering::SeqCst);
                    let trap = vec![b'X'; 1000];
                    let reply =
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e2\"\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&trap).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\nETag: \"e2\"\r\nContent-Range: bytes */{full_len}\r\nContent-Length: 0\r\n\r\n"
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let (dir, target, _temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = tokio::time::timeout(Duration::from_secs(20), run_transfer(ctx))
            .await
            .expect("the saved file must be confirmed after the short follow-up");
        let outcome = outcome.expect("416 confirms the full body, not the 1000-byte follow-up");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), full);
        assert!(
            saw_trap.load(std::sync::atomic::Ordering::SeqCst),
            "the short follow-up must be received before the file is finished"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A 416 `bytes */N` for a short partial must not finish the job when the
    /// changed response never closed cleanly.
    #[tokio::test]
    async fn early_416_of_short_partial_does_not_complete() {
        let full = vec![b'N'; 8192];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = full.clone();
        let saw_416 = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_416_task = saw_416.clone();
        tokio::spawn(async move {
            let mut dropped_original = false;
            let mut sent_prefix = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e1\"\r\nContent-Length: 8192\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0);
                if start == 0 && !dropped_original {
                    dropped_original = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: 8192\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&[b'o'; 4000]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if start > 0 && !sent_prefix {
                    sent_prefix = true;
                    let reply =
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e2\"\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&[b'X'; 1000]).await;
                    let _ = socket.flush().await;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    // Abort so the short body is a reset, not a clean close.
                    #[allow(deprecated)]
                    let _ = socket.set_linger(Some(Duration::from_secs(0)));
                    continue;
                }
                if start > 0 {
                    saw_416_task.store(true, std::sync::atomic::Ordering::SeqCst);
                    let reply = format!(
                        "HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\nContent-Range: bytes */{start}\r\nContent-Length: 0\r\n\r\n"
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e2\"\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&payload).await;
                let _ = socket.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let (dir, target, _temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = tokio::time::timeout(Duration::from_secs(20), run_transfer(ctx))
            .await
            .expect("416 of a short partial must settle");
        let outcome = outcome.expect("restart after the 416 downloads the real file");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        let file = std::fs::read(&target).expect("final file");
        assert_eq!(file, full, "a 416 of the short prefix must not be the file");
        assert!(
            saw_416.load(std::sync::atomic::Ordering::SeqCst),
            "the short prefix must be answered with 416 before the real file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A clean close of a short body after the validators change is not the file.
    #[tokio::test]
    async fn truncated_first_body_after_validator_change_does_not_complete() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut dropped_original = false;
            let mut sent_short = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e1\"\r\nContent-Length: 8192\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0);
                if start == 0 && !dropped_original {
                    dropped_original = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: 8192\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&[b'o'; 2000]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !sent_short {
                    sent_short = true;
                    let reply =
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e2\"\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&[b'X'; 1000]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply =
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e2\"\r\n\r\n";
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&[b'Y'; 4000]).await;
                let _ = socket.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let (dir, target, temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let err = tokio::time::timeout(Duration::from_secs(20), run_transfer(ctx))
            .await
            .expect("a truncated changed body must settle")
            .expect_err("a truncated changed body must not complete");
        assert!(!err.retryable, "{err:?}");
        assert!(
            !target.exists(),
            "the short body must not be the finished file"
        );
        assert_eq!(
            std::fs::read(&temp).expect("candidate partial"),
            vec![b'X'; 1000],
            "the longer follow-up must not replace or splice the partial"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After the ETag changes, a server that ignores If-Range and sends the same
    /// full body again still finishes. A different length does not.
    #[tokio::test]
    async fn ignored_if_range_with_changed_etag_accepts_matching_length() {
        let new_body = vec![b'I'; 4096];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = new_body.clone();
        let ignored = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ignored_task = ignored.clone();
        tokio::spawn(async move {
            let mut dropped_original = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e1\"\r\nContent-Length: 8192\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let lower = req.to_ascii_lowercase();
                let start = range_start_from(&lower).unwrap_or(0);
                if start == 0 && !dropped_original {
                    dropped_original = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: 8192\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&[b'o'; 1000]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if header_value(&lower, "if-range").is_some_and(|value| value.contains("\"e2\"")) {
                    ignored_task.store(true, Ordering::SeqCst);
                }
                let reply =
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e2\"\r\n\r\n";
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&payload).await;
                let _ = socket.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let (dir, target, _temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = tokio::time::timeout(Duration::from_secs(20), run_transfer(ctx))
            .await
            .expect("an ignored If-Range of the same length must settle");
        let outcome = outcome.expect("two equal full bodies finish the new file");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), new_body);
        assert!(
            ignored.load(Ordering::SeqCst),
            "the confirm request must carry If-Range for the new ETag"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The changed response and the confirming 416 carry only an ETag.
    #[tokio::test]
    async fn single_etag_lengthless_change_confirms_on_416() {
        let new_body = vec![b'S'; 4096];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = new_body.clone();
        let saw_416 = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_416_task = saw_416.clone();
        tokio::spawn(async move {
            let mut dropped_original = false;
            let mut sent_changed = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e1\"\r\nContent-Length: 8192\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0);
                if start == 0 && !dropped_original {
                    dropped_original = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e1\"\r\nContent-Length: 8192\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&[b'o'; 1000]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !sent_changed {
                    sent_changed = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e2\"\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&payload).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                saw_416_task.store(true, Ordering::SeqCst);
                let reply = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\nETag: \"e2\"\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\n\r\n",
                    payload.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let (dir, target, _temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = tokio::time::timeout(Duration::from_secs(20), run_transfer(ctx))
            .await
            .expect("one validator must be enough to confirm");
        let outcome = outcome.expect("416 with the new ETag finishes the file");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), new_body);
        assert!(
            saw_416.load(Ordering::SeqCst),
            "the lengthless close must wait for the 416"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stop after the changed body is saved, then confirm that saved job with 416.
    #[tokio::test]
    async fn reloaded_job_confirms_lengthless_change_on_416() {
        let new_body = vec![b'R'; 8192];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = new_body.clone();
        let phase = Arc::new(AtomicU8::new(0));
        let phase_task = phase.clone();
        tokio::spawn(async move {
            let mut dropped_original = false;
            let mut sent_changed = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = if phase_task.load(Ordering::SeqCst) == 0 {
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e1\"\r\nContent-Length: 4096\r\n\r\n"
                    } else {
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e2\"\r\n\r\n"
                    };
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0);
                if start == 0 && !dropped_original {
                    dropped_original = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: 4096\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&[b'o'; 500]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !sent_changed {
                    sent_changed = true;
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nETag: \"e2\"\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&payload).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if phase_task.load(Ordering::SeqCst) == 0 {
                    let mut buf = [0u8; 8];
                    let _ =
                        tokio::time::timeout(Duration::from_secs(20), socket.read(&mut buf)).await;
                    continue;
                }
                let reply = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\nETag: \"e2\"\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\n\r\n",
                    payload.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let (dir, target, temp, job) = download_paths(&base);
        let progress = TestProgress::new();
        let control = Arc::new(AtomicU8::new(0));
        let ctx = test_ctx_control(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
            control.clone(),
        );
        let runner = tokio::spawn(async move { run_transfer(ctx).await });
        let saved = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(job) = progress.snapshots().into_iter().rev().find(|job| {
                    job.downloaded_bytes == new_body.len() as u64
                        && job.total_bytes == 0
                        && job.validators.expected_size.is_none()
                        && job.validators.etag.as_deref() == Some("\"e2\"")
                        && job.validators.last_modified.is_none()
                }) {
                    return job;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the changed body must be saved before it is confirmed");
        control.store(CONTROL_PAUSED, Ordering::SeqCst);
        let paused = tokio::time::timeout(Duration::from_secs(5), runner)
            .await
            .expect("pause must settle")
            .expect("transfer task")
            .expect("pause is not a download error");
        assert!(matches!(paused, DownloadOutcome::Paused));
        assert_eq!(
            std::fs::metadata(&temp).expect("partial").len(),
            new_body.len() as u64
        );

        phase.store(1, Ordering::SeqCst);
        let ctx = test_ctx(saved, Arc::new(|_: TransferEvent| {}));
        let outcome = tokio::time::timeout(Duration::from_secs(20), run_transfer(ctx))
            .await
            .expect("the reloaded job must settle");
        let outcome = outcome.expect("416 confirms the saved partial");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), new_body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn seed_lengthless_candidate(base: &str, bytes: &[u8]) -> (PathBuf, PathBuf, PathBuf, Job) {
        let (dir, target, temp, mut job) = download_paths(base);
        std::fs::write(&temp, bytes).unwrap();
        job.downloaded_bytes = bytes.len() as u64;
        job.total_bytes = 0;
        job.resume_supported = true;
        job.validators.etag = Some("\"e2\"".into());
        job.validators.expected_size = None;
        (dir, target, temp, job)
    }

    /// A 416 finishes the candidate only when it names that length and repeats
    /// the saved validators. Anything else must leave the file unfinished.
    #[tokio::test]
    async fn unconfirmed_416_does_not_complete_lengthless_candidate() {
        let cases = [
            ("416 without validators", "bytes */100", ""),
            (
                "416 with a different etag",
                "bytes */100",
                "ETag: \"e9\"\r\n",
            ),
            (
                "416 total differs from the candidate",
                "bytes */101",
                "ETag: \"e2\"\r\n",
            ),
            ("416 star total", "bytes */*", "ETag: \"e2\"\r\n"),
            ("416 numeric range", "bytes 0-99/100", "ETag: \"e2\"\r\n"),
        ];
        for (label, content_range, extra) in cases {
            let candidate = vec![b'Q'; 100];
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let range_line = content_range.to_string();
            let extra = extra.to_string();
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let req = read_http_headers(&mut socket).await;
                    if !req.starts_with("GET ") {
                        let _ = socket.shutdown().await;
                        continue;
                    }
                    let reply = format!(
                        "HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\n{extra}Content-Range: {range_line}\r\nContent-Length: 0\r\n\r\n"
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                }
            });

            let (dir, target, _temp, job) =
                seed_lengthless_candidate(&format!("http://{addr}"), &candidate);
            let mut ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
            ctx.preflight_done = true;
            let outcome = tokio::time::timeout(Duration::from_secs(12), run_transfer(ctx))
                .await
                .unwrap_or_else(|_| panic!("{label} must settle"));
            assert!(
                !matches!(outcome, Ok(DownloadOutcome::Completed)),
                "{label} must not finish the candidate, got {outcome:?}"
            );
            assert!(!target.exists(), "{label} must not rename the partial");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A second full body with no validators is not confirmation, even when
    /// its length matches the candidate.
    #[tokio::test]
    async fn second_lengthless_body_without_validators_does_not_complete() {
        let candidate = vec![b'Q'; 48];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let other = vec![b'Z'; candidate.len()];
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&other).await;
                let _ = socket.shutdown().await;
            }
        });

        let (dir, target, temp, job) =
            seed_lengthless_candidate(&format!("http://{addr}"), &candidate);
        let mut ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        ctx.preflight_done = true;
        let err = tokio::time::timeout(Duration::from_secs(12), run_transfer(ctx))
            .await
            .expect("a validator-less repeat must settle")
            .expect_err("a validator-less repeat must not complete");
        assert!(!err.retryable, "{err:?}");
        assert!(!target.exists());
        assert_eq!(
            std::fs::read(&temp).expect("candidate partial"),
            candidate,
            "the unconfirmed body must not replace the partial"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The confirm read stops once the repeated body passes the candidate.
    /// A tail that never closes must not hold the attempt open.
    #[tokio::test]
    async fn confirm_body_stops_once_it_passes_the_candidate() {
        let candidate = vec![b'C'; 32];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let req = read_http_headers(&mut socket).await;
                    if !req.starts_with("GET ") {
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let reply = "HTTP/1.1 200 OK\r\nConnection: close\r\nETag: \"e2\"\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let mut body = vec![b'C'; 32];
                    body.push(b'Z');
                    let _ = socket.write_all(&body).await;
                    let _ = socket.flush().await;
                    tokio::time::sleep(Duration::from_secs(20)).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        let (dir, target, temp, job) =
            seed_lengthless_candidate(&format!("http://{addr}"), &candidate);
        let mut ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        ctx.preflight_done = true;
        let err = tokio::time::timeout(Duration::from_secs(8), run_transfer(ctx))
            .await
            .expect("the confirm read must stop once the body passes the candidate")
            .expect_err("a longer confirm body must not complete");
        assert!(!err.retryable, "{err:?}");
        assert!(!target.exists());
        assert_eq!(std::fs::read(&temp).expect("candidate partial"), candidate);
        let _ = std::fs::remove_dir_all(&dir);
    }

    const OLD_ENTITY_LEN: u64 = 262_144;
    const NEW_ENTITY_LEN: usize = 270_336;

    #[derive(Clone, Copy)]
    enum ChangedEntityFollowup {
        /// Every ranged GET is a full 200. The server never honors Range.
        RangeIgnored,
        /// If-Range mismatch is a 200 of the new file. A matching Range is 206 or 416.
        Standards,
        /// Same as standards, but only Last-Modified changes.
        LastModifiedOnly,
    }

    /// HEAD advertises the old size. The first body drops, forcing a Range
    /// reconnect. The resource has changed and the replacement 200 has no
    /// Content-Length. Later requests keep answering for the whole reconnect
    /// loop: ignored ranges, or 206/416 after the validator matches.
    async fn changed_entity_through_reconnect(followup: ChangedEntityFollowup) -> Vec<u8> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let new_body = match followup {
            ChangedEntityFollowup::LastModifiedOnly => vec![b'L'; NEW_ENTITY_LEN],
            _ => vec![b'N'; NEW_ENTITY_LEN],
        };
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = requests.clone();
        tokio::spawn(async move {
            let mut dropped_original = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if let Ok(mut guard) = requests.lock() {
                    guard.push(req.clone());
                }
                if req.starts_with("HEAD ") {
                    let reply = head_for(followup);
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let lower = req.to_ascii_lowercase();
                let start = range_start_from(&lower).unwrap_or(0);
                if start == 0 && !dropped_original {
                    dropped_original = true;
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {OLD_ENTITY_LEN}\r\n\r\n"
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&vec![b'o'; 100_000]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let if_range = header_value(&lower, "if-range");
                let validator_matches = followup_validator_matches(followup, if_range.as_deref());
                let send_full = match followup {
                    ChangedEntityFollowup::RangeIgnored => true,
                    ChangedEntityFollowup::Standards | ChangedEntityFollowup::LastModifiedOnly => {
                        start == 0 || !validator_matches
                    }
                };
                if send_full {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\n{}\r\n\r\n",
                        current_validator(followup)
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&new_body).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if start as usize >= new_body.len() {
                    let reply = format!(
                        "HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\n{}\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\n\r\n",
                        current_validator(followup),
                        new_body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let suffix = &new_body[start as usize..];
                let end = new_body.len() - 1;
                let reply = format!(
                    "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\n{}\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\n\r\n",
                    current_validator(followup),
                    new_body.len(),
                    suffix.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(suffix).await;
                let _ = socket.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let (dir, target, _temp, job) = download_paths(&base);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = tokio::time::timeout(Duration::from_secs(45), run_transfer(ctx)).await;
        let file = match outcome {
            Ok(Ok(DownloadOutcome::Completed)) => std::fs::read(&target).unwrap_or_default(),
            Ok(Ok(other)) => panic!("download ended as {other:?}"),
            Ok(Err(err)) => {
                let seen = recorded.lock().unwrap().len();
                panic!("changed entity failed after {seen} requests: {err:?}")
            }
            Err(_) => {
                let seen = recorded.lock().unwrap().len();
                panic!("changed entity timed out after {seen} requests")
            }
        };
        let range_gets = recorded
            .lock()
            .unwrap()
            .iter()
            .filter(|req| {
                let lower = req.to_ascii_lowercase();
                lower.starts_with("get ") && range_start_from(&lower).is_some()
            })
            .count();
        assert_eq!(
            range_gets, 2,
            "the changed body must be confirmed by one follow-up, saw {range_gets} range GETs"
        );
        let _ = std::fs::remove_dir_all(&dir);
        file
    }

    fn head_for(followup: ChangedEntityFollowup) -> String {
        let identity = match followup {
            ChangedEntityFollowup::LastModifiedOnly => {
                "Last-Modified: Wed, 01 Jan 2020 00:00:00 GMT"
            }
            ChangedEntityFollowup::RangeIgnored | ChangedEntityFollowup::Standards => {
                "ETag: \"v1\""
            }
        };
        format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\n{identity}\r\nContent-Length: {OLD_ENTITY_LEN}\r\n\r\n"
        )
    }

    fn current_validator(followup: ChangedEntityFollowup) -> &'static str {
        match followup {
            ChangedEntityFollowup::LastModifiedOnly => {
                "Last-Modified: Thu, 02 Jan 2020 00:00:00 GMT"
            }
            ChangedEntityFollowup::RangeIgnored | ChangedEntityFollowup::Standards => {
                "ETag: \"v2\""
            }
        }
    }

    fn followup_validator_matches(followup: ChangedEntityFollowup, if_range: Option<&str>) -> bool {
        let Some(value) = if_range else {
            return false;
        };
        match followup {
            ChangedEntityFollowup::LastModifiedOnly => value.contains("02 jan 2020"),
            ChangedEntityFollowup::RangeIgnored | ChangedEntityFollowup::Standards => {
                value.contains("\"v2\"")
            }
        }
    }

    fn header_value(lower_req: &str, name: &str) -> Option<String> {
        let prefix = format!("{name}:");
        lower_req
            .lines()
            .find(|line| line.starts_with(&prefix))
            .map(|line| line[prefix.len()..].trim().to_string())
    }

    fn response_200(content_length: Option<u64>, accept_ranges: bool) -> String {
        let mut response = String::from("HTTP/1.1 200 OK\r\nConnection: close\r\n");
        if accept_ranges {
            response.push_str("Accept-Ranges: bytes\r\n");
        }
        if let Some(len) = content_length {
            response.push_str(&format!("Content-Length: {len}\r\n"));
        }
        response.push_str("\r\n");
        response
    }

    fn download_paths(base: &str) -> (PathBuf, PathBuf, PathBuf, Job) {
        let dir = std::env::temp_dir().join(format!("rusticdl-size-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp.clone());
        (dir, target, temp, job)
    }

    async fn head_then_get(
        head_len: u64,
        get_len: Option<u64>,
        body: &[u8],
    ) -> (PathBuf, PathBuf, Result<DownloadOutcome, DownloadError>) {
        let head = response_200(Some(head_len), true);
        let get = response_200(get_len, false);
        let (base, _seen, _handle) =
            spawn_scripted_server_recording_with_body(vec![head, get], body.to_vec()).await;
        let (dir, target, _temp, job) = download_paths(&base);
        let progress = TestProgress::new();
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
        );
        let outcome = transfer_within(ctx).await;
        if matches!(&outcome, Ok(DownloadOutcome::Completed)) {
            return (dir, target, outcome);
        }
        assert!(
            !target.exists(),
            "a rejected download must not be renamed to the finished file"
        );
        let wrong = get_len.filter(|len| *len != head_len);
        assert!(
            progress.snapshots().iter().all(|job| {
                wrong.is_none_or(|len| {
                    job.total_bytes != len && job.validators.expected_size != Some(len)
                })
            }),
            "must not overwrite the known size with a conflicting Content-Length: {:?}",
            progress.snapshots()
        );
        (dir, target, outcome)
    }

    async fn head_then_get_error(
        head_len: u64,
        get_len: Option<u64>,
        body: &[u8],
    ) -> DownloadError {
        let (dir, _target, outcome) = head_then_get(head_len, get_len, body).await;
        let err = outcome.expect_err("conflicting 200 must fail the job");
        let _ = std::fs::remove_dir_all(&dir);
        err
    }

    async fn transfer_within(ctx: TransferContext) -> Result<DownloadOutcome, DownloadError> {
        tokio::time::timeout(Duration::from_secs(8), run_transfer(ctx))
            .await
            .expect("download must settle without exhausting the reconnect budget")
    }

    fn assert_size_mismatch(err: &DownloadError, actual: u64, expected: u64) {
        assert_eq!(err.category, FailureCategory::Resume);
        assert!(!err.retryable, "size mismatch must not be retried: {err:?}");
        assert!(
            err.message.contains(&actual.to_string())
                && err.message.contains(&expected.to_string())
                && err.message.to_ascii_lowercase().contains("size"),
            "expected a size-mismatch error naming {actual} vs {expected}, got {err:?}"
        );
    }

    fn observed_mismatch_size(err: &DownloadError) -> u64 {
        assert_eq!(err.category, FailureCategory::Resume);
        assert!(!err.retryable, "size mismatch must not be retried: {err:?}");
        let rest = err
            .message
            .split_once("Remote size changed (")
            .and_then(|(_, rest)| rest.split_once(" bytes"))
            .map(|(size, _)| size)
            .unwrap_or_else(|| panic!("expected a size-mismatch error, got {err:?}"));
        rest.parse()
            .unwrap_or_else(|_| panic!("expected a byte count, got {err:?}"))
    }

    /// First GET has no Content-Length and is the rejected body. A later Retry
    /// must download `correct` from byte 0. A Range resume would splice.
    async fn retry_after_size_mismatch(
        head_len: u64,
        rejected: Vec<u8>,
        correct: Vec<u8>,
    ) -> Vec<u8> {
        assert_eq!(correct.len() as u64, head_len);
        let base = serve_reject_then_correct(head_len, rejected, correct).await;
        let (dir, target, temp, job) = download_paths(&base);
        let progress = TestProgress::new();
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
        );
        let err = transfer_within(ctx)
            .await
            .expect_err("the rejected body must fail before Retry");
        assert_eq!(err.category, FailureCategory::Resume);
        assert!(!err.retryable, "{err:?}");
        assert!(
            !target.exists(),
            "the rejected body must not be the finished file"
        );
        assert!(
            !temp.exists(),
            "rejected bytes must be dropped before Retry"
        );
        let job = progress
            .snapshots()
            .last()
            .cloned()
            .expect("size mismatch commits the cleared offset");
        assert_eq!(
            job.downloaded_bytes, 0,
            "Retry must not resume from a rejected offset"
        );

        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
        );
        let outcome = transfer_within(ctx)
            .await
            .expect("Retry starts clean and downloads the real file");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        let file = std::fs::read(&target).expect("final file");
        let _ = std::fs::remove_dir_all(&dir);
        file
    }

    async fn serve_reject_then_correct(
        head_len: u64,
        rejected: Vec<u8>,
        correct: Vec<u8>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut rejected_sent = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {head_len}\r\n\r\n"
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0);
                if start == 0 && !rejected_sent {
                    rejected_sent = true;
                    let reply = response_200(None, false);
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&rejected).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if start > 0 {
                    let start_us = start as usize;
                    if start_us < correct.len() {
                        let suffix = &correct[start_us..];
                        let end = correct.len() - 1;
                        let reply = format!(
                            "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{head_len}\r\nContent-Length: {}\r\n\r\n",
                            suffix.len()
                        );
                        let _ = socket.write_all(reply.as_bytes()).await;
                        let _ = socket.write_all(suffix).await;
                    } else {
                        let reply = format!(
                            "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{start}/{head_len}\r\nContent-Length: 0\r\n\r\n"
                        );
                        let _ = socket.write_all(reply.as_bytes()).await;
                    }
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                    correct.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&correct).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    /// HEAD advertises `entity_len`. The first body GET declares that length and
    /// closes after `dropped` bytes. A later Range GET is answered with
    /// `resume_headers` and `resume_body`.
    async fn serve_drop_then_range(
        entity_len: u64,
        head_extra: &str,
        dropped: usize,
        resume_headers: &str,
        resume_body: Vec<u8>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let head_extra = head_extra.to_string();
        let resume_headers = resume_headers.to_string();
        let filler = vec![b'p'; entity_len as usize];
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let req = read_http_headers(&mut socket).await;
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {entity_len}\r\n{head_extra}\r\n"
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0);
                if start == 0 {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {entity_len}\r\n\r\n"
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let end = dropped.min(filler.len());
                    let _ = socket.write_all(&filler[..end]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{resume_headers}\r\n");
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&resume_body).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    async fn read_http_headers(socket: &mut TcpStream) -> String {
        let mut buf = vec![0u8; 8192];
        let mut collected = Vec::new();
        loop {
            let n = match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            collected.extend_from_slice(&buf[..n]);
            if collected.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&collected).into_owned()
    }

    #[tokio::test]
    async fn run_transfer_accept_ranges_none_stays_single() {
        let body = vec![b'y'; 64];
        let head = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: none\r\n\
Content-Length: {}\r\n\
\r\n",
            body.len()
        );
        let get = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: {}\r\n\
Accept-Ranges: none\r\n\
\r\n",
            body.len()
        );
        let (base, seen, _handle) =
            spawn_scripted_server_recording_with_body(vec![head, get], body.clone()).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-xfer-none-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/big.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp);

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig {
                multi_min_bytes: 1,
                ..Default::default()
            },
            progress.identity.clone(),
        );
        let outcome = run_transfer(ctx).await.expect("transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).expect("final file"), body);

        let seen = seen.lock().unwrap();
        let body_ranges: Vec<_> = seen
            .iter()
            .filter(|r| {
                let l = r.to_ascii_lowercase();
                l.starts_with("get ") && l.contains("range: bytes=")
            })
            .cloned()
            .collect();
        assert!(
            body_ranges.is_empty(),
            "Accept-Ranges: none must not issue multi Range GETs, got {seen:?}"
        );

        let snaps = progress.snapshots();
        assert!(snaps
            .iter()
            .any(|job| job.transfer_mode == Some(TransferMode::Single)));
        assert!(snaps
            .iter()
            .any(|job| job.fallback_reason.as_deref() == Some("ranges_unsupported")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_transfer_v1_map_missing_is_resume_error() {
        let body = vec![b'z'; 64];
        let head = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: {}\r\n\
\r\n",
            body.len()
        );
        let (base, seen, _handle) = spawn_scripted_server_recording(vec![head]).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-xfer-v1-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        std::fs::write(&temp, &body[..16]).unwrap();

        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target, temp.clone());
        job.transfer_format_version = 1;
        job.total_bytes = body.len() as u64;
        job.downloaded_bytes = 16;
        job.resume_supported = true;

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig {
                multi_min_bytes: 1,
                ..Default::default()
            },
            progress.identity.clone(),
        );

        let err = run_transfer(ctx)
            .await
            .expect_err("v1 without map must not invent ranges");
        assert_eq!(err.category, FailureCategory::Resume);
        assert_eq!(err.message, RESUME_RESTART_MESSAGE);
        assert_eq!(std::fs::read(&temp).unwrap(), &body[..16]);

        let snaps = progress.snapshots();
        assert!(snaps
            .iter()
            .any(|job| job.fallback_reason.as_deref() == Some("map_missing")));
        assert!(snaps.iter().all(|job| job.segment_map.is_none()));

        let seen = seen.lock().unwrap();
        let gets: Vec<_> = seen
            .iter()
            .filter(|r| r.starts_with("GET "))
            .cloned()
            .collect();
        assert!(
            gets.is_empty(),
            "must not fetch a body when the v1 map is missing, got {seen:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_transfer_legacy_v0_part_stays_single() {
        let body = vec![b'q'; 64];
        let head = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: {}\r\n\
\r\n",
            body.len()
        );
        let get = format!(
            "HTTP/1.1 206 Partial Content\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Range: bytes 16-63/{}\r\n\
Content-Length: 48\r\n\
\r\n",
            body.len()
        );
        let (base, seen, _handle) =
            spawn_scripted_server_recording_with_body(vec![head, get], body[16..].to_vec()).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-xfer-leg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        std::fs::write(&temp, &body[..16]).unwrap();

        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target.clone(), temp);
        job.transfer_format_version = 0;
        job.downloaded_bytes = 16;
        job.total_bytes = body.len() as u64;
        job.resume_supported = true;

        let progress = TestProgress::new();
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig {
                multi_min_bytes: 1,
                multi_max_segments: 2,
                ..Default::default()
            },
            progress.identity.clone(),
        );

        let outcome = run_transfer(ctx).await.expect("legacy single");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);

        let snaps = progress.snapshots();
        assert!(snaps
            .iter()
            .any(|job| job.transfer_mode == Some(TransferMode::Single)));
        assert!(snaps
            .iter()
            .any(|job| job.fallback_reason.as_deref() == Some("legacy_contiguous_partial")));
        assert!(snaps.iter().all(|job| job.segment_map.is_none()));

        let seen = seen.lock().unwrap();
        let body_gets: Vec<_> = seen
            .iter()
            .filter(|r| {
                let l = r.to_ascii_lowercase();
                l.starts_with("get ") && !l.contains("range: bytes=0-0")
            })
            .cloned()
            .collect();
        assert_eq!(
            body_gets.len(),
            1,
            "legacy v0 must stay one stream, got {seen:?}"
        );
        assert!(
            body_gets[0]
                .to_ascii_lowercase()
                .contains("range: bytes=16-"),
            "expected resume from .part length, got {body_gets:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn single_stream_sha256_match_renames() {
        let body = vec![b'x'; 64];
        let expected = sha256_hex(&body);
        let (dir, target, temp, ctx) = single_stream_ctx(&body, Some(expected)).await;

        let outcome = run_transfer(ctx).await.expect("match should complete");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);
        assert!(!temp.exists(), "successful verify must rename .part away");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn single_stream_sha256_mismatch_keeps_part() {
        let body = vec![b'x'; 64];
        let (dir, target, temp, ctx) =
            single_stream_ctx(&body, Some(SHA256_EMPTY.to_string())).await;

        let err = run_transfer(ctx)
            .await
            .expect_err("mismatch must fail before rename");
        assert_eq!(err.category, FailureCategory::Internal);
        assert!(!err.retryable);
        assert!(err.message.contains("SHA-256 mismatch"));
        assert!(temp.exists(), "hash fail must keep .part");
        assert!(!target.exists(), "hash fail must not rename to final");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn single_stream_exhausts_last_budget_slot() {
        use crate::download::conn_budget::host_key_for_budget;
        use crate::download::job::DownloadOutcome;
        use std::time::Duration;
        use tokio::sync::Notify;

        let body = vec![b'x'; 64];
        let release_body = Arc::new(Notify::new());
        let release_body_srv = release_body.clone();
        let got_get = Arc::new(Notify::new());
        let got_get_srv = got_get.clone();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let body_srv = body.clone();
        let _handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let mut collected = Vec::new();
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    collected.extend_from_slice(&buf[..n]);
                    if collected.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&collected);
                let lower = req.to_ascii_lowercase();
                let is_body_get = req.starts_with("GET ")
                    && !lower.contains("range: bytes=0-0")
                    && !lower.contains("range: bytes=1-1");
                if is_body_get {
                    got_get_srv.notify_one();
                    release_body_srv.notified().await;
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: {}\r\n\
\r\n",
                        body_srv.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&body_srv).await;
                } else {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: none\r\n\
Content-Length: {}\r\n\
\r\n",
                        body_srv.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                }
                let _ = socket.shutdown().await;
            }
        });

        let dir = std::env::temp_dir().join(format!("rusticdl-slot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let host = host_key_for_budget(&url);
        let budget = ConnectionBudget::new(2, 2);
        let dummy = budget.acquire(&host).await;

        let job = Job::new(url, "out.bin".into(), target.clone(), temp);
        let ctx = TransferContext::from_runtime(
            job,
            Arc::new(AtomicU8::new(0)),
            Arc::new(|_: TransferEvent| {}),
            None,
            GlobalBandwidthLimiter::new(None),
            budget.clone(),
            Arc::new(NoopIdentity),
            &EngineRuntimeConfig::default(),
        );

        let download = tokio::spawn(async move { run_transfer(ctx).await });
        tokio::time::timeout(Duration::from_secs(5), got_get.notified())
            .await
            .expect("body GET should start after taking the last slot");
        assert!(
            budget.try_acquire(&host).await.is_none(),
            "single-stream must exhaust the last slot while a dummy holder exists"
        );

        release_body.notify_one();
        drop(dummy);
        let outcome = tokio::time::timeout(Duration::from_secs(5), download)
            .await
            .expect("download finished")
            .expect("task")
            .expect("transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);

        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn single_stream_ctx(
        body: &[u8],
        expected_sha256: Option<String>,
    ) -> (PathBuf, PathBuf, PathBuf, TransferContext) {
        let head = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: {}\r\n\
\r\n",
            body.len()
        );
        let get = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Content-Length: {}\r\n\
Accept-Ranges: bytes\r\n\
\r\n",
            body.len()
        );
        let (base, _handle) = spawn_scripted_server(vec![head, get]).await;

        let dir = std::env::temp_dir().join(format!("rusticdl-sha-xfer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target.clone(), temp.clone());
        job.expected_sha256 = expected_sha256;

        let on_progress: TransferEventCallback = Arc::new(|_: TransferEvent| {});
        let ctx = test_ctx(job, on_progress);
        (dir, target, temp, ctx)
    }

    #[tokio::test]
    async fn partial_body_without_accept_ranges_resumes_over_range() {
        let body: Vec<u8> = (0..80u8).collect();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_task = seen.clone();
        let _handle = tokio::spawn(async move {
            let mut truncated = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let mut collected = Vec::new();
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    collected.extend_from_slice(&buf[..n]);
                    if collected.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&collected).into_owned();
                seen_task.lock().unwrap().push(req.clone());
                let lower = req.to_ascii_lowercase();
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: none\r\nContent-Length: {}\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&lower).unwrap_or(0) as usize;
                if start == 0 && !truncated {
                    truncated = true;
                    let half = payload.len() / 2;
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&payload[..half]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let slice = if start < payload.len() {
                    &payload[start..]
                } else {
                    &[]
                };
                let end = payload.len().saturating_sub(1);
                let reply = format!(
                    "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\n\r\n",
                    payload.len(),
                    slice.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(slice).await;
                let _ = socket.shutdown().await;
            }
        });

        let dir = std::env::temp_dir().join(format!("rusticdl-poor-part-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("http://{addr}/file.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = run_transfer(ctx)
            .await
            .expect("truncated body must resume instead of failing");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter().any(|req| {
                let lower = req.to_ascii_lowercase();
                lower.contains("range: bytes=") && !lower.contains("range: bytes=0-0")
            }),
            "resume must send Range after the truncated body, got {seen:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn http_503_then_success_keeps_the_download() {
        let body = b"retry-me-please-0123456789";
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.to_vec();
        let _handle = tokio::spawn(async move {
            let mut failures = 0u32;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let mut collected = Vec::new();
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    collected.extend_from_slice(&buf[..n]);
                    if collected.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&collected);
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if req.starts_with("GET ") && failures < 2 {
                    failures += 1;
                    let reply = "HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&payload).await;
                let _ = socket.shutdown().await;
            }
        });

        let dir = std::env::temp_dir().join(format!("rusticdl-poor-503-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("http://{addr}/file.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp);
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = run_transfer(ctx)
            .await
            .expect("503 must be retried, not fail the transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn http_416_once_keeps_partial_and_resumes() {
        let body: Vec<u8> = (0..80u8).collect();
        let prefix = 40usize;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.clone();
        let _handle = tokio::spawn(async move {
            let mut rejected = false;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let mut collected = Vec::new();
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    collected.extend_from_slice(&buf[..n]);
                    if collected.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&collected);
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0) as usize;
                if start == prefix && !rejected {
                    rejected = true;
                    let reply = "HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if start == 0 {
                    let wrong = vec![0xFFu8; payload.len()];
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                        wrong.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&wrong).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let slice = &payload[start.min(payload.len())..];
                let end = payload.len().saturating_sub(1);
                let reply = format!(
                    "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\n\r\n",
                    payload.len(),
                    slice.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(slice).await;
                let _ = socket.shutdown().await;
            }
        });

        let dir = std::env::temp_dir().join(format!("rusticdl-poor-416-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        std::fs::write(&temp, &body[..prefix]).unwrap();
        let url = format!("http://{addr}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target.clone(), temp);
        job.resume_supported = true;
        job.total_bytes = body.len() as u64;
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = run_transfer(ctx)
            .await
            .expect("one 416 must retry the same offset");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn http_416_persistent_restarts_from_zero() {
        let body: Vec<u8> = (0..80u8).collect();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.clone();
        let _handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let mut collected = Vec::new();
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    collected.extend_from_slice(&buf[..n]);
                    if collected.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&collected);
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0) as usize;
                if start > 0 {
                    let reply = "HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&payload).await;
                let _ = socket.shutdown().await;
            }
        });

        let dir = std::env::temp_dir().join(format!(
            "rusticdl-poor-416-restart-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        std::fs::write(&temp, &body[..40]).unwrap();
        let url = format!("http://{addr}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target.clone(), temp);
        job.resume_supported = true;
        job.total_bytes = body.len() as u64;
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = tokio::time::timeout(Duration::from_secs(15), run_transfer(ctx))
            .await
            .expect("persistent 416 must restart from zero before the timeout")
            .expect("transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn speculative_range_answered_with_200_keeps_partial() {
        let body: Vec<u8> = (0..80u8).collect();
        let prefix = 40usize;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_task = seen.clone();
        let _handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let mut collected = Vec::new();
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    collected.extend_from_slice(&buf[..n]);
                    if collected.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&collected).into_owned();
                seen_task.lock().unwrap().push(req.clone());
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: none\r\nContent-Length: {}\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let lower = req.to_ascii_lowercase();
                let start = range_start_from(&lower).unwrap_or(0) as usize;
                if start == 0 {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&payload[..prefix]).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let replacement = vec![0xABu8; payload.len()];
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                    replacement.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&replacement).await;
                let _ = socket.shutdown().await;
            }
        });

        let dir =
            std::env::temp_dir().join(format!("rusticdl-poor-200-keep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        let url = format!("http://{addr}/file.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp.clone());
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let err = tokio::time::timeout(Duration::from_secs(10), run_transfer(ctx))
            .await
            .expect("a 200 to a speculative Range must stop without spinning")
            .expect_err("a 200 must not replace the partial");
        assert!(
            err.message.contains("partial kept"),
            "unexpected error: {err:?}"
        );
        assert_eq!(
            std::fs::read(&temp).expect("partial must stay on disk"),
            &body[..prefix]
        );
        assert!(
            !target.exists(),
            "ignored Range must not finish as a new file"
        );
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter().any(|req| {
                let lower = req.to_ascii_lowercase();
                lower.contains("range: bytes=") && !lower.contains("range: bytes=0-0")
            }),
            "the dropped body must still attempt Range, got {seen:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn http_416_after_from_zero_restart_keeps_new_partial() {
        let body: Vec<u8> = (0..80u8).collect();
        let original = 40usize;
        let restarted = 20usize;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_task = seen.clone();
        let _handle = tokio::spawn(async move {
            let mut from_zero = 0u32;
            let mut rejects_at_restart = 0u32;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let mut collected = Vec::new();
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    collected.extend_from_slice(&buf[..n]);
                    if collected.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&collected).into_owned();
                seen_task.lock().unwrap().push(req.clone());
                if req.starts_with("HEAD ") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if !req.starts_with("GET ") {
                    let _ = socket.shutdown().await;
                    continue;
                }
                let start = range_start_from(&req.to_ascii_lowercase()).unwrap_or(0) as usize;
                if start == 0 {
                    from_zero += 1;
                    if from_zero == 1 {
                        let reply = format!(
                            "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                            payload.len()
                        );
                        let _ = socket.write_all(reply.as_bytes()).await;
                        let _ = socket.write_all(&payload[..restarted]).await;
                        let _ = socket.shutdown().await;
                        continue;
                    }
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(&payload).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                if start == restarted {
                    rejects_at_restart += 1;
                    if rejects_at_restart == 1 {
                        let reply = "HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
                        let _ = socket.write_all(reply.as_bytes()).await;
                        let _ = socket.shutdown().await;
                        continue;
                    }
                    let slice = &payload[restarted..];
                    let end = payload.len().saturating_sub(1);
                    let reply = format!(
                        "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {restarted}-{end}/{}\r\nContent-Length: {}\r\n\r\n",
                        payload.len(),
                        slice.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.write_all(slice).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let reply = "HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        let dir =
            std::env::temp_dir().join(format!("rusticdl-poor-416-budget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("out.bin");
        let temp = PathBuf::from(format!("{}.part", target.display()));
        std::fs::write(&temp, &body[..original]).unwrap();
        let url = format!("http://{addr}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target.clone(), temp);
        job.resume_supported = true;
        job.downloaded_bytes = original as u64;
        job.total_bytes = body.len() as u64;
        let ctx = test_ctx(job, Arc::new(|_: TransferEvent| {}));
        let outcome = tokio::time::timeout(Duration::from_secs(20), run_transfer(ctx))
            .await
            .expect("416 after a from-zero restart must finish from the new partial")
            .expect("transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);
        let seen = seen.lock().unwrap();
        let starts: Vec<Option<u64>> = seen
            .iter()
            .filter(|req| req.starts_with("GET "))
            .map(|req| range_start_from(&req.to_ascii_lowercase()))
            .collect();
        assert_eq!(
            starts.iter().filter(|start| start.is_none()).count(),
            1,
            "the from-zero body must be fetched once, got {starts:?}"
        );
        assert!(
            starts
                .windows(2)
                .any(|pair| pair == [Some(restarted as u64), Some(restarted as u64)]),
            "one 416 must retry the new partial, got {starts:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn range_start_from(lower_req: &str) -> Option<u64> {
        let line = lower_req.lines().find(|line| line.starts_with("range:"))?;
        let spec = line.split_once(':')?.1.trim();
        let bytes = spec.strip_prefix("bytes=")?;
        bytes.split(['-', ',']).next()?.parse().ok()
    }

    async fn spawn_scripted_server(replies: Vec<String>) -> (String, tokio::task::JoinHandle<()>) {
        let (base, _seen, handle) = spawn_scripted_server_recording(replies).await;
        (base, handle)
    }

    async fn spawn_scripted_server_recording(
        replies: Vec<String>,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        spawn_scripted_server_recording_with_body(replies, Vec::new()).await
    }

    async fn spawn_scripted_server_recording_with_body(
        replies: Vec<String>,
        extra_body: Vec<u8>,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_task = seen.clone();
        let handle = tokio::spawn(async move {
            let mut replies = replies.into_iter();
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let mut collected = Vec::new();
                loop {
                    let n = match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    collected.extend_from_slice(&buf[..n]);
                    if collected.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&collected).into_owned();
                seen_task.lock().unwrap().push(req.clone());
                let Some(reply) = replies.next() else {
                    break;
                };
                let _ = socket.write_all(reply.as_bytes()).await;
                let is_get = req.starts_with("GET ");
                if is_get && !extra_body.is_empty() {
                    let _ = socket.write_all(&extra_body).await;
                } else if is_get && extra_body.is_empty() && reply.contains("Content-Length: 64") {
                    let _ = socket.write_all(&[b'x'; 64]).await;
                }
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://{addr}"), seen, handle)
    }
}
