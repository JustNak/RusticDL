//! Transfer entry + planner decision tree.
//!
//! PR 10: always routes single-stream. The planner still evaluates multi
//! qualification (`multi_connection_enabled`, known size ≥ `multi_min_bytes`,
//! resume/ranges) and records `transfer_mode` / `fallback_reason` when multi
//! *would* have been selected. Multi workers land in PR 11.

use super::http::{
    apply_preflight, control_outcome, run_http_download_with_ctx, ProgressUpdate, TransferContext,
};
use super::job::{DownloadError, DownloadOutcome, Job, TransferMode};
use super::preflight::PreflightInfo;

/// Why the planner stayed on single-stream (PR 10 always does).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferPlanReason {
    MultiDisabled,
    SizeUnknown,
    RangesUnknown,
    RangesUnsupported,
    BelowMinSize,
    /// Multi *would* qualify; PR 10 still routes single-stream.
    MultiQualifiedSingleOnly,
}

impl TransferPlanReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MultiDisabled => "multi_disabled",
            Self::SizeUnknown => "size_unknown",
            Self::RangesUnknown => "ranges_unknown",
            Self::RangesUnsupported => "ranges_unsupported",
            Self::BelowMinSize => "below_multi_min_bytes",
            Self::MultiQualifiedSingleOnly => "multi_qualified_single_only",
        }
    }

    pub fn would_qualify_multi(self) -> bool {
        matches!(self, Self::MultiQualifiedSingleOnly)
    }
}

/// Planner result. `chosen` is always [`TransferMode::Single`] in PR 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferPlan {
    pub chosen: TransferMode,
    pub reason: TransferPlanReason,
}

impl TransferPlan {
    pub fn to_progress_update(self) -> ProgressUpdate {
        ProgressUpdate {
            transfer_mode: Some(self.chosen),
            // Only a *fallback* when multi would have been selected.
            fallback_reason: self
                .reason
                .would_qualify_multi()
                .then(|| self.reason.as_str().to_string()),
            ..Default::default()
        }
    }
}

/// Decide transfer mode. Always returns single-stream; `reason` says whether
/// multi would have qualified (and why not, otherwise).
pub fn plan_transfer(
    job: &Job,
    preflight: Option<&PreflightInfo>,
    multi_connection_enabled: bool,
    multi_min_bytes: u64,
) -> TransferPlan {
    let reason = plan_reason(job, preflight, multi_connection_enabled, multi_min_bytes);
    TransferPlan {
        chosen: TransferMode::Single,
        reason,
    }
}

fn plan_reason(
    job: &Job,
    preflight: Option<&PreflightInfo>,
    multi_connection_enabled: bool,
    multi_min_bytes: u64,
) -> TransferPlanReason {
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

    TransferPlanReason::MultiQualifiedSingleOnly
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

/// Engine transfer entry: preflight → plan (always single) → single-stream body.
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
    (ctx.on_progress)(plan.to_progress_update());

    // PR 11 will branch on `plan.reason.would_qualify_multi()` here.
    run_http_download_with_ctx(&mut ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::bandwidth::GlobalBandwidthLimiter;
    use crate::download::http::ProgressCallback;
    use crate::download::job::Job;
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
        assert!(plan.to_progress_update().fallback_reason.is_none());
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
        assert_eq!(plan.reason, TransferPlanReason::MultiQualifiedSingleOnly);
        assert_eq!(
            plan.to_progress_update().fallback_reason.as_deref(),
            Some("multi_qualified_single_only")
        );
    }

    #[test]
    fn planner_ranges_unknown_stays_single() {
        let job = sample_job();
        let pf = preflight(Some(8 * 1024 * 1024), None);
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::RangesUnknown);
        assert!(plan.to_progress_update().fallback_reason.is_none());
    }

    #[test]
    fn planner_ranges_unsupported_stays_single() {
        let job = sample_job();
        let pf = preflight(Some(8 * 1024 * 1024), Some(false));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::RangesUnsupported);
    }

    #[test]
    fn planner_below_min_stays_single() {
        let job = sample_job();
        let pf = preflight(Some(1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::BelowMinSize);
        assert_eq!(plan.chosen, TransferMode::Single);
        assert!(plan.to_progress_update().fallback_reason.is_none());
    }

    #[test]
    fn planner_multi_would_qualify_still_single() {
        let job = sample_job();
        let pf = preflight(Some(10 * 1024 * 1024), Some(true));
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.chosen, TransferMode::Single);
        assert!(plan.reason.would_qualify_multi());
        assert_eq!(plan.reason, TransferPlanReason::MultiQualifiedSingleOnly);
        let patch = plan.to_progress_update();
        assert_eq!(patch.transfer_mode, Some(TransferMode::Single));
        assert_eq!(
            patch.fallback_reason.as_deref(),
            Some("multi_qualified_single_only")
        );
    }

    #[test]
    fn planner_handoff_does_not_block_multi_qualification() {
        // Decision tree allows multi with handoff; planner does not inspect auth.
        let mut job = sample_job();
        job.total_bytes = 20 * 1024 * 1024;
        job.resume_supported = true;
        let plan = plan_transfer(&job, None, true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualifiedSingleOnly);
        assert_eq!(plan.chosen, TransferMode::Single);
    }

    #[test]
    fn planner_job_resume_supported_counts_as_ranges() {
        let mut job = sample_job();
        job.total_bytes = 9 * 1024 * 1024;
        job.resume_supported = true;
        let pf = preflight(Some(9 * 1024 * 1024), None);
        let plan = plan_transfer(&job, Some(&pf), true, 5 * 1024 * 1024);
        assert_eq!(plan.reason, TransferPlanReason::MultiQualifiedSingleOnly);
    }

    #[tokio::test]
    async fn run_transfer_always_single_and_sets_mode_when_multi_would_qualify() {
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

        let ctx = TransferContext::new(
            job,
            Arc::new(AtomicU8::new(0)),
            on_progress,
            None,
            GlobalBandwidthLimiter::new(None),
        );
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
            Some("multi_qualified_single_only")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn spawn_scripted_server(replies: Vec<String>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
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
                let Some(reply) = replies.next() else {
                    break;
                };
                let _ = socket.write_all(reply.as_bytes()).await;
                if reply.contains("Content-Length: 64") {
                    let _ = socket.write_all(&[b'x'; 64]).await;
                }
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://{addr}"), handle)
    }
}
