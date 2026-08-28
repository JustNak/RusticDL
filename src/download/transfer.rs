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
    use crate::download::bandwidth::GlobalBandwidthLimiter;
    use crate::download::conn_budget::ConnectionBudget;
    use crate::download::engine::EngineRuntimeConfig;
    use crate::download::job::{fallback_reason_label, FailureCategory, Job};
    use crate::download::multi::RESUME_RESTART_MESSAGE;
    use crate::download::progress::{NoopIdentity, TestProgress, TransferEventCallback};
    use crate::download::verify::{sha256_hex, SHA256_EMPTY};
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU8;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

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
        TransferContext::from_runtime(
            job,
            Arc::new(AtomicU8::new(0)),
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
            8 * 1024 * 1024
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
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
        );
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
            8 * 1024 * 1024
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
        let ctx = test_ctx_commit(
            job,
            progress.callback(),
            EngineRuntimeConfig::default(),
            progress.identity.clone(),
        );
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
