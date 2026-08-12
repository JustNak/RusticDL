//! Transfer entry + planner decision tree.
//!
//! When multi qualifies (`multi_connection_enabled`, known size ≥ `multi_min_bytes`,
//! ranges supported) the planner chooses [`TransferMode::Multi`].
//!
//! Normative policy (PR 12):
//! - Convert multi→single **only** when every segment `written == 0`.
//! - Legacy v0 `.part` stays single until Restart.
//! - v1 map missing/inconsistent → Resume error (never invent Range offsets).
//! - Surface `fallback_reason` whenever the planner stays on single-stream.

use super::http::{
    apply_preflight, control_outcome, run_http_download_with_ctx, ProgressUpdate, TransferContext,
};
use super::job::{DownloadError, DownloadOutcome, Job, TransferMode};
use super::multi::{
    multi_resume_policy, resume_restart_required, run_multi_segment_download, MultiResumePolicy,
};
use super::preflight::PreflightInfo;

/// Why the planner chose single-stream, or that multi qualified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferPlanReason {
    MultiDisabled,
    SizeUnknown,
    RangesUnknown,
    RangesUnsupported,
    BelowMinSize,
    /// v0 contiguous `.part` — stay single until Restart.
    LegacyContiguousPartial,
    /// `version >= 1` and no map — Resume error, do not invent ranges.
    MapMissing,
    /// Map present but inconsistent — Resume error, do not invent ranges.
    MapInconsistent,
    /// Multi qualifies — orchestrator should run.
    MultiQualified,
}

/// One-shot toast when a large file cannot use multi (ranges unknown/unsupported).
pub const LARGE_FILE_MULTI_UNAVAILABLE_TOAST: &str =
    "Multi-connection unavailable for this large file; using a single connection.";

impl TransferPlanReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MultiDisabled => "multi_disabled",
            Self::SizeUnknown => "size_unknown",
            Self::RangesUnknown => "ranges_unknown",
            Self::RangesUnsupported => "ranges_unsupported",
            Self::BelowMinSize => "below_multi_min_bytes",
            Self::LegacyContiguousPartial => "legacy_contiguous_partial",
            Self::MapMissing => "map_missing",
            Self::MapInconsistent => "map_inconsistent",
            Self::MultiQualified => "multi_qualified",
        }
    }

    pub fn would_qualify_multi(self) -> bool {
        matches!(self, Self::MultiQualified)
    }

    pub fn is_resume_required(self) -> bool {
        matches!(self, Self::MapMissing | Self::MapInconsistent)
    }
}

/// Planner result. `chosen` is Multi when [`TransferPlanReason::MultiQualified`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferPlan {
    pub chosen: TransferMode,
    pub reason: TransferPlanReason,
}

impl TransferPlan {
    pub fn to_progress_update(self) -> ProgressUpdate {
        ProgressUpdate {
            transfer_mode: self.published_mode(),
            fallback_reason: self.fallback_reason().map(str::to_string),
            ..Default::default()
        }
    }

    /// Resume-required jobs keep their multi identity; do not claim a conversion.
    fn published_mode(self) -> Option<TransferMode> {
        if self.reason.is_resume_required() {
            None
        } else {
            Some(self.chosen)
        }
    }

    /// Last why we stayed single (or why Resume is required).
    pub fn fallback_reason(self) -> Option<&'static str> {
        match self.reason {
            TransferPlanReason::MultiQualified => None,
            other => Some(other.as_str()),
        }
    }

    /// Planner patch plus connections and a one-shot toast for large files.
    ///
    /// Toast only when ranges are unknown/unsupported on a file at or above
    /// `multi_min_bytes`. Disabled multi records the reason without toasting.
    pub fn to_visibility_update(self, size: Option<u64>, multi_min_bytes: u64) -> ProgressUpdate {
        let mut patch = self.to_progress_update();
        if self.chosen == TransferMode::Single {
            patch.active_connections = Some(1);
        }
        if matches!(
            self.reason,
            TransferPlanReason::RangesUnknown | TransferPlanReason::RangesUnsupported
        ) && size.is_some_and(|n| n >= multi_min_bytes)
        {
            patch.toast = Some(LARGE_FILE_MULTI_UNAVAILABLE_TOAST.into());
        }
        patch
    }
}

/// Decide transfer mode. Multi when enabled, size known, ranges supported, size ≥ min.
/// A present consistent `segment_map` always forces Multi so resume reuses bounds/`written`.
/// Legacy v0 partials stay single; v1 map missing/inconsistent is Resume-required.
pub fn plan_transfer(
    job: &Job,
    preflight: Option<&PreflightInfo>,
    multi_connection_enabled: bool,
    multi_min_bytes: u64,
) -> TransferPlan {
    let reason = plan_reason(job, preflight, multi_connection_enabled, multi_min_bytes);
    TransferPlan {
        chosen: if reason.would_qualify_multi() {
            TransferMode::Multi
        } else {
            TransferMode::Single
        },
        reason,
    }
}

fn job_has_resumable_map(job: &Job) -> bool {
    job.segment_map
        .as_ref()
        .is_some_and(|map| map.is_consistent())
}

fn plan_reason(
    job: &Job,
    preflight: Option<&PreflightInfo>,
    multi_connection_enabled: bool,
    multi_min_bytes: u64,
) -> TransferPlanReason {
    match multi_resume_policy(job) {
        MultiResumePolicy::MapMissing => return TransferPlanReason::MapMissing,
        MultiResumePolicy::MapInconsistent => return TransferPlanReason::MapInconsistent,
        MultiResumePolicy::LegacySingle => return TransferPlanReason::LegacyContiguousPartial,
        MultiResumePolicy::Proceed => {}
    }

    // In-progress map must not fall through to single-stream (MultiMap / Restart).
    if job_has_resumable_map(job) {
        return TransferPlanReason::MultiQualified;
    }

    if !multi_connection_enabled {
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

/// Engine transfer entry: preflight → plan → multi orchestrator or single-stream.
pub async fn run_transfer(mut ctx: TransferContext) -> Result<DownloadOutcome, DownloadError> {
    let preflight = apply_preflight(&mut ctx).await;

    if let Some(outcome) = control_outcome(&ctx.control) {
        return Ok(outcome);
    }

    let plan = plan_transfer(
        &ctx.job,
        preflight.as_ref(),
        ctx.multi_connection_enabled,
        ctx.multi_min_bytes,
    );
    if let Some(reason) = plan.fallback_reason() {
        ctx.job.fallback_reason = Some(reason.to_string());
    }
    let size = known_size(&ctx.job, preflight.as_ref());
    (ctx.on_progress)(plan.to_visibility_update(size, ctx.multi_min_bytes));

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
    use crate::download::http::ProgressCallback;
    use crate::download::job::{FailureCategory, Job};
    use crate::download::multi::RESUME_RESTART_MESSAGE;
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
    fn planner_disabled_stays_single() {
        let job = sample_job();
        let pf = preflight(Some(10 * 1024 * 1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), false, 5 * 1024 * 1024);
        assert_eq!(plan.chosen, TransferMode::Single);
        assert_eq!(plan.reason, TransferPlanReason::MultiDisabled);
        assert_eq!(
            plan.to_progress_update().fallback_reason.as_deref(),
            Some("multi_disabled")
        );
        assert_eq!(
            plan.to_progress_update().transfer_mode,
            Some(TransferMode::Single)
        );
    }

    #[test]
    fn planner_unknown_size_stays_single() {
        let job = sample_job();
        let pf = preflight(None, Some(true));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::SizeUnknown);
        assert_eq!(plan.chosen, TransferMode::Single);
    }

    #[test]
    fn planner_uses_job_size_when_preflight_missing() {
        let mut job = sample_job();
        job.total_bytes = 8 * 1024 * 1024;
        job.resume_supported = true;
        let plan = plan_transfer(&job, None, true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
        assert_eq!(plan.chosen, TransferMode::Multi);
        assert_eq!(plan.reason.as_str(), "multi_qualified");
        assert!(plan.to_progress_update().fallback_reason.is_none());
    }

    #[test]
    fn planner_ranges_unknown_stays_single() {
        let job = sample_job();
        let pf = preflight(Some(8 * 1024 * 1024), None);
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::RangesUnknown);
        assert_eq!(
            plan.to_progress_update().fallback_reason.as_deref(),
            Some("ranges_unknown")
        );
    }

    #[test]
    fn planner_ranges_unsupported_stays_single() {
        let job = sample_job();
        let pf = preflight(Some(8 * 1024 * 1024), Some(false));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::RangesUnsupported);
        assert_eq!(plan.chosen, TransferMode::Single);
        assert_eq!(
            plan.to_progress_update().fallback_reason.as_deref(),
            Some("ranges_unsupported")
        );
    }

    #[test]
    fn planner_below_min_stays_single() {
        let job = sample_job();
        let pf = preflight(Some(1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::BelowMinSize);
        assert_eq!(plan.chosen, TransferMode::Single);
        assert_eq!(
            plan.to_progress_update().fallback_reason.as_deref(),
            Some("below_multi_min_bytes")
        );
    }

    #[test]
    fn planner_multi_qualifies_chooses_multi() {
        let job = sample_job();
        let pf = preflight(Some(10 * 1024 * 1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.chosen, TransferMode::Multi);
        assert!(plan.reason.would_qualify_multi());
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
        let patch = plan.to_progress_update();
        assert_eq!(patch.transfer_mode, Some(TransferMode::Multi));
        assert!(patch.fallback_reason.is_none());
    }

    #[test]
    fn planner_handoff_does_not_block_multi_qualification() {
        // Decision tree allows multi with handoff; planner does not inspect auth.
        let mut job = sample_job();
        job.total_bytes = 20 * 1024 * 1024;
        job.resume_supported = true;
        let plan = plan_transfer(&job, None, true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
        assert_eq!(plan.chosen, TransferMode::Multi);
    }

    #[test]
    fn planner_job_resume_supported_counts_as_ranges() {
        let mut job = sample_job();
        job.total_bytes = 9 * 1024 * 1024;
        job.resume_supported = true;
        let pf = preflight(Some(9 * 1024 * 1024), None);
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);
        assert_eq!(plan.chosen, TransferMode::Multi);
    }

    #[test]
    fn planner_existing_map_forces_multi_even_when_preflight_unqualifies() {
        let mut job = sample_job();
        job.transfer_format_version = 1;
        job.total_bytes = 2 * 1024 * 1024;
        job.segment_map = Some(crate::download::segment::partition(2 * 1024 * 1024, 2));
        // Flaky preflight: unknown size / ranges, and min-size raised above total.
        let pf = preflight(None, None);
        let plan = plan_transfer(&job, Some(&pf), true, 50 * 1024 * 1024);
        assert_eq!(plan.chosen, TransferMode::Multi);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualified);

        // Even if the master switch is off, resume must keep the map path.
        let plan = plan_transfer(&job, Some(&pf), false, 50 * 1024 * 1024);
        assert_eq!(plan.chosen, TransferMode::Multi);
    }

    #[test]
    fn planner_legacy_v0_partial_stays_single_until_restart() {
        let mut job = sample_job();
        job.downloaded_bytes = 4096;
        job.total_bytes = 10 * 1024 * 1024;
        job.resume_supported = true;
        job.transfer_format_version = 0;
        let pf = preflight(Some(10 * 1024 * 1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.chosen, TransferMode::Single);
        assert_eq!(plan.reason, TransferPlanReason::LegacyContiguousPartial);
        assert_eq!(
            plan.to_progress_update().fallback_reason.as_deref(),
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
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert!(plan.reason.is_resume_required());
        assert_eq!(plan.reason, TransferPlanReason::MapMissing);
        assert_eq!(plan.chosen, TransferMode::Single);
        let patch = plan.to_progress_update();
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
        let plan = plan_transfer(&job, Some(&pf), true, 1);
        assert_eq!(plan.reason, TransferPlanReason::MapInconsistent);
        assert!(plan.reason.is_resume_required());
        assert_eq!(
            plan.to_progress_update().fallback_reason.as_deref(),
            Some("map_inconsistent")
        );
    }

    #[tokio::test]
    async fn run_transfer_stays_single_when_multi_disabled() {
        let body = vec![b'x'; 64];
        let head = format!(
            "HTTP/1.1 200 OK\r\n\
Connection: close\r\n\
Accept-Ranges: bytes\r\n\
Content-Length: {}\r\n\
\r\n",
            8 * 1024 * 1024
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
        let url = format!("{base}/big.bin");
        let job = Job::new(url, "out.bin".into(), target.clone(), temp);

        let patches: Arc<Mutex<Vec<ProgressUpdate>>> = Arc::new(Mutex::new(Vec::new()));
        let patches_cb = patches.clone();
        let on_progress: ProgressCallback = Arc::new(move |u: ProgressUpdate| {
            patches_cb.lock().unwrap().push(u);
        });

        let mut ctx = TransferContext::new(
            job,
            Arc::new(AtomicU8::new(0)),
            on_progress,
            None,
            GlobalBandwidthLimiter::new(None),
        );
        ctx.multi_connection_enabled = false;
        let outcome = run_transfer(ctx).await.expect("transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));

        let data = std::fs::read(&target).expect("final file");
        assert_eq!(data, body);

        let seen = patches.lock().unwrap();
        let mode_patch = seen.iter().find(|p| p.transfer_mode.is_some());
        let mode_patch = mode_patch.expect("planner should publish transfer_mode");
        assert_eq!(mode_patch.transfer_mode, Some(TransferMode::Single));
        assert_eq!(
            mode_patch.fallback_reason.as_deref(),
            Some("multi_disabled")
        );
        assert!(mode_patch.toast.is_none(), "disabled multi must not toast");
        assert_eq!(mode_patch.active_connections, Some(1));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn visibility_large_file_ranges_unsupported_toasts_once() {
        let job = sample_job();
        let pf = preflight(Some(8 * 1024 * 1024), Some(false));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        let patch = plan.to_visibility_update(Some(8 * 1024 * 1024), 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::RangesUnsupported);
        assert_eq!(patch.transfer_mode, Some(TransferMode::Single));
        assert_eq!(patch.fallback_reason.as_deref(), Some("ranges_unsupported"));
        assert_eq!(
            patch.toast.as_deref(),
            Some(LARGE_FILE_MULTI_UNAVAILABLE_TOAST)
        );
        assert_eq!(patch.active_connections, Some(1));
    }

    #[test]
    fn visibility_small_file_ranges_unsupported_no_toast() {
        let job = sample_job();
        let pf = preflight(Some(1024), Some(false));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        let patch = plan.to_visibility_update(Some(1024), 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::RangesUnsupported);
        assert_eq!(patch.fallback_reason.as_deref(), Some("ranges_unsupported"));
        assert!(patch.toast.is_none());
    }

    #[test]
    fn visibility_large_file_multi_disabled_reason_without_toast() {
        let job = sample_job();
        let pf = preflight(Some(10 * 1024 * 1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), false, 5 * 1024 * 1024);
        let patch = plan.to_visibility_update(Some(10 * 1024 * 1024), 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::MultiDisabled);
        assert_eq!(patch.fallback_reason.as_deref(), Some("multi_disabled"));
        assert!(patch.toast.is_none());
    }

    #[test]
    fn visibility_below_min_no_fallback_reason() {
        let job = sample_job();
        let pf = preflight(Some(1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        let patch = plan.to_visibility_update(Some(1024), 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::BelowMinSize);
        assert_eq!(
            patch.fallback_reason.as_deref(),
            Some("below_multi_min_bytes")
        );
        assert!(patch.toast.is_none());
    }

    #[tokio::test]
    async fn run_transfer_large_file_without_ranges_toasts_once() {
        let body = vec![b'x'; 64];
        // Explicit none so preflight does not fire a Range probe (which would
        // consume the scripted GET). Planner still sees ranges_unsupported.
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

        let patches: Arc<Mutex<Vec<ProgressUpdate>>> = Arc::new(Mutex::new(Vec::new()));
        let patches_cb = patches.clone();
        let on_progress: ProgressCallback = Arc::new(move |u: ProgressUpdate| {
            patches_cb.lock().unwrap().push(u);
        });

        let ctx = TransferContext::new(
            job,
            Arc::new(AtomicU8::new(0)),
            on_progress,
            None,
            GlobalBandwidthLimiter::new(None),
        );
        let outcome = run_transfer(ctx).await.expect("transfer");
        assert!(matches!(outcome, DownloadOutcome::Completed));

        let seen = patches.lock().unwrap();
        let toasts: Vec<_> = seen.iter().filter(|p| p.toast.is_some()).collect();
        assert_eq!(toasts.len(), 1, "toast must be one-shot, got {toasts:?}");
        assert_eq!(
            toasts[0].toast.as_deref(),
            Some(LARGE_FILE_MULTI_UNAVAILABLE_TOAST)
        );
        assert_eq!(
            toasts[0].fallback_reason.as_deref(),
            Some("ranges_unsupported")
        );

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

        let patches: Arc<Mutex<Vec<ProgressUpdate>>> = Arc::new(Mutex::new(Vec::new()));
        let patches_cb = patches.clone();
        let on_progress: ProgressCallback = Arc::new(move |u: ProgressUpdate| {
            patches_cb.lock().unwrap().push(u);
        });

        let mut ctx = TransferContext::new(
            job,
            Arc::new(AtomicU8::new(0)),
            on_progress,
            None,
            GlobalBandwidthLimiter::new(None),
        );
        ctx.multi_connection_enabled = true;
        ctx.multi_min_bytes = 1;
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

        let published = patches.lock().unwrap();
        assert!(published
            .iter()
            .any(|p| p.transfer_mode == Some(TransferMode::Single)));
        assert!(published
            .iter()
            .any(|p| p.fallback_reason.as_deref() == Some("ranges_unsupported")));

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
        // Contiguous bytes on disk must not be used as a Range start.
        std::fs::write(&temp, &body[..16]).unwrap();

        let url = format!("{base}/file.bin");
        let mut job = Job::new(url, "out.bin".into(), target, temp.clone());
        job.transfer_format_version = 1;
        job.total_bytes = body.len() as u64;
        job.downloaded_bytes = 16;
        job.resume_supported = true;

        let patches: Arc<Mutex<Vec<ProgressUpdate>>> = Arc::new(Mutex::new(Vec::new()));
        let patches_cb = patches.clone();
        let on_progress: ProgressCallback = Arc::new(move |u: ProgressUpdate| {
            patches_cb.lock().unwrap().push(u);
        });

        let mut ctx = TransferContext::new(
            job,
            Arc::new(AtomicU8::new(0)),
            on_progress,
            None,
            GlobalBandwidthLimiter::new(None),
        );
        ctx.multi_connection_enabled = true;
        ctx.multi_min_bytes = 1;

        let err = run_transfer(ctx)
            .await
            .expect_err("v1 without map must not invent ranges");
        assert_eq!(err.category, FailureCategory::Resume);
        assert_eq!(err.message, RESUME_RESTART_MESSAGE);
        assert_eq!(std::fs::read(&temp).unwrap(), &body[..16]);

        let published = patches.lock().unwrap();
        assert!(published
            .iter()
            .any(|p| p.fallback_reason.as_deref() == Some("map_missing")));
        assert!(!published.iter().any(|p| p.segment_map.is_some()));

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
        // Single-stream resume: 206 from offset 16.
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

        let patches: Arc<Mutex<Vec<ProgressUpdate>>> = Arc::new(Mutex::new(Vec::new()));
        let patches_cb = patches.clone();
        let on_progress: ProgressCallback = Arc::new(move |u: ProgressUpdate| {
            patches_cb.lock().unwrap().push(u);
        });

        let mut ctx = TransferContext::new(
            job,
            Arc::new(AtomicU8::new(0)),
            on_progress,
            None,
            GlobalBandwidthLimiter::new(None),
        );
        ctx.multi_connection_enabled = true;
        ctx.multi_min_bytes = 1;
        ctx.multi_max_segments = 2;

        let outcome = run_transfer(ctx).await.expect("legacy single");
        assert!(matches!(outcome, DownloadOutcome::Completed));
        assert_eq!(std::fs::read(&target).unwrap(), body);

        let published = patches.lock().unwrap();
        assert!(published
            .iter()
            .any(|p| p.transfer_mode == Some(TransferMode::Single)));
        assert!(published
            .iter()
            .any(|p| p.fallback_reason.as_deref() == Some("legacy_contiguous_partial")));
        assert!(!published.iter().any(|p| p.segment_map.is_some()));

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
