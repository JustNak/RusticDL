//! Engine command handlers (Add, Pause, Remove, settings, …).

use std::sync::Arc;

use tokio::sync::Mutex;

use super::{EngineCommand, EngineInner};

mod add;
mod bulk;
mod job_control;
mod settings;

pub(super) async fn handle_command(inner: &Arc<Mutex<EngineInner>>, cmd: EngineCommand) {
    match cmd {
        EngineCommand::Add {
            url,
            filename,
            directory,
            handoff_auth,
            reply,
        } => {
            add::handle(inner, url, filename, directory, handoff_auth, reply).await;
        }
        EngineCommand::Pause(id) => {
            job_control::pause(inner, id).await;
        }
        EngineCommand::Resume(id) => {
            job_control::resume(inner, id).await;
        }
        EngineCommand::Cancel { id, delete_partial } => {
            job_control::cancel(inner, id, delete_partial).await;
        }
        EngineCommand::Retry(id) => {
            job_control::retry(inner, id).await;
        }
        EngineCommand::Restart(id) => {
            job_control::restart(inner, id).await;
        }
        EngineCommand::Remove {
            id,
            delete_partial,
            delete_file,
        } => {
            job_control::remove(inner, id, delete_partial, delete_file).await;
        }
        EngineCommand::PauseAll => {
            bulk::pause_all(inner).await;
        }
        EngineCommand::ResumeAll => {
            bulk::resume_all(inner).await;
        }
        EngineCommand::RetryAll => {
            bulk::retry_all(inner).await;
        }
        EngineCommand::UpdateSettings(config) => {
            settings::update_settings(inner, config).await;
        }
        EngineCommand::ReplaceJobs(jobs) => {
            settings::replace_jobs(inner, jobs).await;
        }
        EngineCommand::Shutdown => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::engine::{spawn_engine, EngineEvent, EngineRuntimeConfig};
    use crate::download::handoff::EnqueueStatus;
    use crate::download::job::{ContentValidators, FailureCategory, Job, JobState, TransferMode};
    use crate::download::multi::RESUME_RESTART_MESSAGE;
    use crate::download::segment::{Segment, SegmentMap, SegmentState};
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::sync::oneshot;

    fn test_config() -> EngineRuntimeConfig {
        let mut cfg = EngineRuntimeConfig::default();
        cfg.max_concurrent = 1;
        cfg.auto_retry = 0;
        cfg.speed_limit_kib = 0;
        cfg
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rusticdl-dup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn sample_job(url: &str, state: JobState, dir: &PathBuf) -> Job {
        let name = "file.bin";
        let mut job = Job::new(
            url.to_string(),
            name.into(),
            dir.join(name),
            dir.join(format!("{name}.part")),
        );
        job.state = state;
        job
    }

    fn sample_map() -> SegmentMap {
        SegmentMap {
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![
                Segment {
                    index: 0,
                    start: 0,
                    end: 499,
                    written: 100,
                    state: SegmentState::Active,
                },
                Segment {
                    index: 1,
                    start: 500,
                    end: 999,
                    written: 0,
                    state: SegmentState::Pending,
                },
            ],
            preallocated: true,
        }
    }

    async fn next_toast(events: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>) -> String {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match events.recv().await {
                    Some(EngineEvent::Toast(msg)) => break msg,
                    Some(EngineEvent::JobsChanged(_)) => continue,
                    None => panic!("event channel closed before toast"),
                }
            }
        })
        .await
        .expect("timed out waiting for toast")
    }

    async fn next_jobs(events: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>) -> Vec<Job> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match events.recv().await {
                    Some(EngineEvent::JobsChanged(jobs)) => break jobs,
                    Some(EngineEvent::Toast(_)) => continue,
                    None => panic!("event channel closed before jobs"),
                }
            }
        })
        .await
        .expect("timed out waiting for jobs")
    }

    #[tokio::test]
    async fn pure_dup_single_always_replies_duplicate_existing_job() {
        let dir = temp_dir();
        let existing = sample_job("https://example.com/a.zip", JobState::Paused, &dir);
        let existing_id = existing.id.clone();
        let existing_name = existing.filename.clone();

        let (engine, mut events) = spawn_engine(vec![existing], test_config());
        let (reply_tx, reply_rx) = oneshot::channel();
        engine.send(EngineCommand::Add {
            url: "https://example.com/a.zip".into(),
            filename: None,
            directory: dir.clone(),
            handoff_auth: None,
            reply: Some(reply_tx),
        });

        let outcome = tokio::time::timeout(Duration::from_secs(2), reply_rx)
            .await
            .expect("reply timeout")
            .expect("reply dropped on pure-dup");
        assert_eq!(outcome.status, EnqueueStatus::DuplicateExistingJob);
        assert_eq!(outcome.job_id, existing_id);
        assert_eq!(outcome.filename, existing_name);

        let toast = next_toast(&mut events).await;
        assert_eq!(toast, format!("Already downloading: {existing_name}"));

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pure_dup_multi_url_skips_all_and_replies_first_dup() {
        let dir = temp_dir();
        let a = sample_job("https://example.com/a.zip", JobState::Paused, &dir);
        let b = sample_job("https://example.com/b.zip", JobState::Queued, &dir);
        // Queued becomes Starting via scheduler — still active. Use Paused for both to avoid network.
        let mut b = b;
        b.state = JobState::Paused;
        let first_id = a.id.clone();

        let (engine, mut events) = spawn_engine(vec![a, b], test_config());
        let (reply_tx, reply_rx) = oneshot::channel();
        engine.send(EngineCommand::Add {
            url: "https://example.com/a.zip\nhttps://example.com/b.zip".into(),
            filename: None,
            directory: dir.clone(),
            handoff_auth: None,
            reply: Some(reply_tx),
        });

        let outcome = tokio::time::timeout(Duration::from_secs(2), reply_rx)
            .await
            .expect("reply timeout")
            .expect("reply dropped on pure multi-dup");
        assert_eq!(outcome.status, EnqueueStatus::DuplicateExistingJob);
        assert_eq!(outcome.job_id, first_id);

        let toast = next_toast(&mut events).await;
        assert_eq!(toast, "Skipped 2 duplicate(s).");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mixed_multi_url_skips_dups_adds_new_and_toasts() {
        let dir = temp_dir();
        let existing = sample_job("https://example.com/old.zip", JobState::Paused, &dir);

        let (engine, mut events) = spawn_engine(vec![existing], test_config());
        let (reply_tx, reply_rx) = oneshot::channel();
        engine.send(EngineCommand::Add {
            url: "https://example.com/old.zip\nhttps://example.com/new.zip".into(),
            filename: None,
            directory: dir.clone(),
            handoff_auth: None,
            reply: Some(reply_tx),
        });

        let outcome = tokio::time::timeout(Duration::from_secs(2), reply_rx)
            .await
            .expect("reply timeout")
            .expect("reply");
        assert_eq!(outcome.status, EnqueueStatus::Queued);
        assert_eq!(outcome.filename, "new.zip");

        let jobs = next_jobs(&mut events).await;
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().any(|j| j.url == "https://example.com/new.zip"));
        assert_eq!(
            jobs.iter()
                .filter(|j| j.url == "https://example.com/old.zip")
                .count(),
            1,
            "must not insert a second active job for the old URL"
        );

        let toast = next_toast(&mut events).await;
        assert_eq!(toast, "Skipped 1 duplicate(s); added 1.");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn paused_blocks_same_request_url_redownload() {
        let dir = temp_dir();
        let existing = sample_job("https://example.com/paused.bin", JobState::Paused, &dir);
        let (engine, mut events) = spawn_engine(vec![existing], test_config());
        let (reply_tx, reply_rx) = oneshot::channel();
        engine.send(EngineCommand::Add {
            url: "https://example.com/paused.bin".into(),
            filename: None,
            directory: dir.clone(),
            handoff_auth: None,
            reply: Some(reply_tx),
        });
        let outcome = reply_rx.await.expect("reply");
        assert_eq!(outcome.status, EnqueueStatus::DuplicateExistingJob);
        let toast = next_toast(&mut events).await;
        assert!(toast.starts_with("Already downloading:"));
        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn terminal_state_allows_same_request_url_redownload(state: JobState) {
        let dir = temp_dir();
        let url = "https://example.com/done.bin";
        let existing = sample_job(url, state, &dir);
        let old_id = existing.id.clone();

        let (engine, mut events) = spawn_engine(vec![existing], test_config());
        let (reply_tx, reply_rx) = oneshot::channel();
        engine.send(EngineCommand::Add {
            url: url.into(),
            filename: None,
            directory: dir.clone(),
            handoff_auth: None,
            reply: Some(reply_tx),
        });

        let outcome = reply_rx.await.expect("reply");
        assert_eq!(
            outcome.status,
            EnqueueStatus::Queued,
            "terminal {state:?} must allow re-add"
        );
        assert_ne!(outcome.job_id, old_id);

        let jobs = next_jobs(&mut events).await;
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs.iter().filter(|j| j.url == url).count(), 2);

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn completed_allows_same_request_url_redownload() {
        terminal_state_allows_same_request_url_redownload(JobState::Completed).await;
    }

    #[tokio::test]
    async fn failed_allows_same_request_url_redownload() {
        terminal_state_allows_same_request_url_redownload(JobState::Failed).await;
    }

    #[tokio::test]
    async fn canceled_allows_same_request_url_redownload() {
        terminal_state_allows_same_request_url_redownload(JobState::Canceled).await;
    }

    #[tokio::test]
    async fn same_batch_non_adjacent_duplicate_skips_second_a() {
        // extract_http_urls only collapses consecutive exact dups, so A\nB\nA reaches the engine.
        let dir = temp_dir();
        let (engine, mut events) = spawn_engine(vec![], test_config());
        let (reply_tx, reply_rx) = oneshot::channel();
        engine.send(EngineCommand::Add {
            url: "https://example.com/a.zip\nhttps://example.com/b.zip\nhttps://example.com/a.zip"
                .into(),
            filename: None,
            directory: dir.clone(),
            handoff_auth: None,
            reply: Some(reply_tx),
        });

        let outcome = tokio::time::timeout(Duration::from_secs(2), reply_rx)
            .await
            .expect("reply timeout")
            .expect("reply");
        assert_eq!(outcome.status, EnqueueStatus::Queued);
        assert_eq!(outcome.filename, "a.zip");

        let jobs = next_jobs(&mut events).await;
        assert_eq!(jobs.len(), 2, "second A must not create a third job");
        assert_eq!(
            jobs.iter()
                .filter(|j| j.url == "https://example.com/a.zip")
                .count(),
            1
        );
        assert!(jobs.iter().any(|j| j.url == "https://example.com/b.zip"));

        let toast = next_toast(&mut events).await;
        assert_eq!(toast, "Skipped 1 duplicate(s); added 2.");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn suggested_filename_applies_to_first_successfully_enqueued_job() {
        // First paste URL is an active dup; suggested name should land on the first *new* job.
        let dir = temp_dir();
        let existing = sample_job("https://example.com/old.zip", JobState::Paused, &dir);
        let (engine, mut events) = spawn_engine(vec![existing], test_config());
        let (reply_tx, reply_rx) = oneshot::channel();
        engine.send(EngineCommand::Add {
            url: "https://example.com/old.zip\nhttps://example.com/new-resource".into(),
            filename: Some("from-caller.bin".into()),
            directory: dir.clone(),
            handoff_auth: None,
            reply: Some(reply_tx),
        });

        let outcome = reply_rx.await.expect("reply");
        assert_eq!(outcome.status, EnqueueStatus::Queued);
        assert_eq!(outcome.filename, "from-caller.bin");

        let jobs = next_jobs(&mut events).await;
        let added = jobs
            .iter()
            .find(|j| j.url == "https://example.com/new-resource")
            .expect("new job");
        assert_eq!(added.filename, "from-caller.bin");

        let _ = next_toast(&mut events).await;
        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn request_url_only_different_original_not_treated_as_dup_of_active() {
        // Redirect finals are never on Job::url; distinct request strings always enqueue.
        let dir = temp_dir();
        let existing = sample_job("https://short.example/abc", JobState::Paused, &dir);

        let (engine, mut events) = spawn_engine(vec![existing], test_config());
        let (reply_tx, reply_rx) = oneshot::channel();
        engine.send(EngineCommand::Add {
            url: "https://cdn.example/real/file.bin".into(),
            filename: None,
            directory: dir.clone(),
            handoff_auth: None,
            reply: Some(reply_tx),
        });

        let outcome = reply_rx.await.expect("reply");
        assert_eq!(outcome.status, EnqueueStatus::Queued);

        let jobs = next_jobs(&mut events).await;
        assert_eq!(jobs.len(), 2);

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cancel_queued_with_delete_partial_removes_part_file() {
        let dir = temp_dir();
        let mut job = sample_job("https://example.com/partial.bin", JobState::Paused, &dir);
        // Simulate a leftover partial.
        std::fs::write(&job.temp_path, b"partial-bytes").expect("write part");
        assert!(job.temp_path.exists());
        let id = job.id.clone();
        let part = job.temp_path.clone();
        job.state = JobState::Queued;

        let (engine, mut events) = spawn_engine(vec![job], test_config());
        // Consume initial JobsChanged from spawn if any; then cancel.
        engine.send(EngineCommand::Cancel {
            id: id.clone(),
            delete_partial: true,
        });

        let jobs = next_jobs(&mut events).await;
        let canceled = jobs.iter().find(|j| j.id == id).expect("job remains");
        assert_eq!(canceled.state, JobState::Canceled);
        // Give async remove_partial a moment.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!part.exists(), ".part must be deleted on cancel cleanup");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cancel_without_delete_partial_keeps_part_file() {
        let dir = temp_dir();
        let job = sample_job("https://example.com/keep.bin", JobState::Paused, &dir);
        std::fs::write(&job.temp_path, b"keep-me").expect("write part");
        let id = job.id.clone();
        let part = job.temp_path.clone();

        let (engine, mut events) = spawn_engine(vec![job], test_config());
        engine.send(EngineCommand::Cancel {
            id: id.clone(),
            delete_partial: false,
        });

        let jobs = next_jobs(&mut events).await;
        assert_eq!(
            jobs.iter().find(|j| j.id == id).map(|j| j.state),
            Some(JobState::Canceled)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            part.exists(),
            "partial retained when delete_partial is false"
        );

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn double_cancel_is_idempotent() {
        let dir = temp_dir();
        let job = sample_job("https://example.com/once.bin", JobState::Paused, &dir);
        let id = job.id.clone();
        let (engine, mut events) = spawn_engine(vec![job], test_config());

        engine.send(EngineCommand::Cancel {
            id: id.clone(),
            delete_partial: true,
        });
        let _ = next_jobs(&mut events).await;

        engine.send(EngineCommand::Cancel {
            id: id.clone(),
            delete_partial: true,
        });
        // Should not panic; job stays Canceled.
        let jobs = tokio::time::timeout(Duration::from_millis(200), next_jobs(&mut events))
            .await
            .unwrap_or_else(|_| Vec::new());
        if let Some(j) = jobs.iter().find(|j| j.id == id) {
            assert_eq!(j.state, JobState::Canceled);
        }

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn restart_clears_validators_and_transfer_format_version() {
        let dir = temp_dir();
        let mut job = sample_job("https://example.com/restart.bin", JobState::Paused, &dir);
        job.downloaded_bytes = 500;
        job.total_bytes = 1000;
        job.progress = 50.0;
        job.validators = ContentValidators {
            etag: Some("\"x\"".into()),
            last_modified: Some("Tue, 15 Nov 1994 12:45:26 GMT".into()),
            expected_size: Some(1000),
        };
        job.transfer_format_version = 1;
        job.segment_map = Some(sample_map());
        job.active_connections = 3;
        job.reconnect_count = 2;
        job.transfer_mode = Some(TransferMode::Multi);
        job.fallback_reason = Some("test".into());
        job.resume_supported = true;
        std::fs::write(&job.temp_path, b"partial").expect("write part");
        let id = job.id.clone();
        let part = job.temp_path.clone();

        let (engine, mut events) = spawn_engine(vec![job], test_config());
        engine.send(EngineCommand::Restart(id.clone()));

        let jobs = next_jobs(&mut events).await;
        let restarted = jobs.iter().find(|j| j.id == id).expect("job remains");
        assert_eq!(restarted.state, JobState::Queued);
        assert_eq!(restarted.downloaded_bytes, 0);
        assert_eq!(restarted.total_bytes, 0);
        assert_eq!(restarted.progress, 0.0);
        assert!(restarted.validators.is_empty());
        assert_eq!(restarted.transfer_format_version, 0);
        assert!(restarted.segment_map.is_none());
        assert_eq!(restarted.active_connections, 0);
        assert_eq!(restarted.reconnect_count, 0);
        assert!(restarted.transfer_mode.is_none());
        assert!(restarted.fallback_reason.is_none());
        assert!(!restarted.resume_supported);

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!part.exists(), "Restart deletes .part");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cancel_with_delete_partial_clears_map_and_reconnects() {
        let dir = temp_dir();
        let mut job = sample_job("https://example.com/map.bin", JobState::Paused, &dir);
        job.downloaded_bytes = 100;
        job.total_bytes = 1000;
        job.progress = 10.0;
        job.validators = ContentValidators {
            etag: Some("\"keep-unless-delete\"".into()),
            last_modified: None,
            expected_size: Some(1000),
        };
        job.transfer_format_version = 1;
        job.segment_map = Some(sample_map());
        job.active_connections = 4;
        job.reconnect_count = 7;
        job.transfer_mode = Some(TransferMode::Multi);
        std::fs::write(&job.temp_path, b"partial").expect("write part");
        let id = job.id.clone();
        let part = job.temp_path.clone();

        let (engine, mut events) = spawn_engine(vec![job], test_config());
        engine.send(EngineCommand::Cancel {
            id: id.clone(),
            delete_partial: true,
        });

        let jobs = next_jobs(&mut events).await;
        let canceled = jobs.iter().find(|j| j.id == id).expect("job remains");
        assert_eq!(canceled.state, JobState::Canceled);
        assert!(canceled.segment_map.is_none());
        assert_eq!(canceled.transfer_format_version, 0);
        assert_eq!(canceled.reconnect_count, 0);
        assert_eq!(canceled.downloaded_bytes, 0);
        assert_eq!(canceled.progress, 0.0);
        assert!(canceled.validators.is_empty());
        assert_eq!(canceled.active_connections, 0);

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!part.exists(), "Cancel+delete removes .part");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cancel_keep_partial_retains_map() {
        let dir = temp_dir();
        let mut job = sample_job("https://example.com/keep-map.bin", JobState::Paused, &dir);
        job.downloaded_bytes = 100;
        job.transfer_format_version = 1;
        job.segment_map = Some(sample_map());
        job.reconnect_count = 3;
        job.validators = ContentValidators {
            etag: Some("\"keep\"".into()),
            last_modified: None,
            expected_size: Some(1000),
        };
        let id = job.id.clone();

        let (engine, mut events) = spawn_engine(vec![job], test_config());
        engine.send(EngineCommand::Cancel {
            id: id.clone(),
            delete_partial: false,
        });

        let jobs = next_jobs(&mut events).await;
        let canceled = jobs.iter().find(|j| j.id == id).expect("job remains");
        assert_eq!(canceled.state, JobState::Canceled);
        assert!(canceled.segment_map.is_some());
        assert_eq!(canceled.transfer_format_version, 1);
        assert_eq!(canceled.reconnect_count, 3);
        assert_eq!(canceled.downloaded_bytes, 100);
        assert_eq!(canceled.validators.etag.as_deref(), Some("\"keep\""));
        assert_eq!(canceled.active_connections, 0);

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resume_v1_map_missing_fails_without_inventing_ranges() {
        let dir = temp_dir();
        let mut job = sample_job("https://example.com/v1-missing.bin", JobState::Paused, &dir);
        job.transfer_format_version = 1;
        job.segment_map = None;
        job.downloaded_bytes = 250;
        job.total_bytes = 1000;
        job.progress = 25.0;
        std::fs::write(&job.temp_path, vec![0u8; 250]).expect("write part");
        let id = job.id.clone();
        let part = job.temp_path.clone();

        let (engine, mut events) = spawn_engine(vec![job], test_config());
        engine.send(EngineCommand::Resume(id.clone()));

        let jobs = next_jobs(&mut events).await;
        let failed = jobs.iter().find(|j| j.id == id).expect("job remains");
        assert_eq!(failed.state, JobState::Failed);
        assert_eq!(failed.failure_category, Some(FailureCategory::Resume));
        assert_eq!(failed.error.as_deref(), Some(RESUME_RESTART_MESSAGE));
        assert_eq!(failed.fallback_reason.as_deref(), Some("map_missing"));
        assert_eq!(failed.transfer_format_version, 1);
        assert!(failed.segment_map.is_none());
        assert_eq!(failed.downloaded_bytes, 250);
        assert!(part.exists(), "Resume error must not delete the .part");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn retry_v1_inconsistent_map_fails_resume() {
        let dir = temp_dir();
        let mut job = sample_job("https://example.com/v1-bad.bin", JobState::Failed, &dir);
        job.transfer_format_version = 1;
        job.segment_map = Some(SegmentMap {
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![],
            preallocated: false,
        });
        job.downloaded_bytes = 100;
        let id = job.id.clone();

        let (engine, mut events) = spawn_engine(vec![job], test_config());
        engine.send(EngineCommand::Retry(id.clone()));

        let jobs = next_jobs(&mut events).await;
        let failed = jobs.iter().find(|j| j.id == id).expect("job remains");
        assert_eq!(failed.state, JobState::Failed);
        assert_eq!(failed.failure_category, Some(FailureCategory::Resume));
        assert_eq!(failed.fallback_reason.as_deref(), Some("map_inconsistent"));
        assert!(failed.segment_map.is_some(), "inconsistent map is retained");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn remove_without_delete_file_keeps_completed_file() {
        let dir = temp_dir();
        let job = sample_job("https://example.com/keep.bin", JobState::Completed, &dir);
        std::fs::write(&job.target_path, b"keep-me").expect("write target");
        let id = job.id.clone();
        let target = job.target_path.clone();

        let (engine, mut events) = spawn_engine(vec![job], test_config());
        engine.send(EngineCommand::Remove {
            id: id.clone(),
            delete_partial: true,
            delete_file: false,
        });

        let jobs = next_jobs(&mut events).await;
        assert!(jobs.iter().all(|j| j.id != id), "job removed from queue");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(target.exists(), "completed file kept on Remove");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn remove_with_delete_file_deletes_completed_file() {
        let dir = temp_dir();
        let job = sample_job("https://example.com/gone.bin", JobState::Completed, &dir);
        std::fs::write(&job.target_path, b"delete-me").expect("write target");
        std::fs::write(&job.temp_path, b"leftover").expect("write part");
        let id = job.id.clone();
        let target = job.target_path.clone();
        let part = job.temp_path.clone();

        let (engine, mut events) = spawn_engine(vec![job], test_config());
        engine.send(EngineCommand::Remove {
            id: id.clone(),
            delete_partial: true,
            delete_file: true,
        });

        let jobs = next_jobs(&mut events).await;
        assert!(jobs.iter().all(|j| j.id != id), "job removed from queue");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!target.exists(), "completed file deleted");
        assert!(!part.exists(), "leftover .part also deleted");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn remove_with_delete_file_ok_when_target_missing() {
        let dir = temp_dir();
        let job = sample_job("https://example.com/missing.bin", JobState::Completed, &dir);
        let id = job.id.clone();
        assert!(!job.target_path.exists());

        let (engine, mut events) = spawn_engine(vec![job], test_config());
        engine.send(EngineCommand::Remove {
            id,
            delete_partial: true,
            delete_file: true,
        });

        let jobs = next_jobs(&mut events).await;
        assert!(jobs.is_empty(), "missing target still drops the job");

        engine.send(EngineCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
