use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, Mutex};

use super::super::job::Job;
use super::super::progress::{apply_commit_identity, CommitIdentity, IdentityCommit, MapUpdate};
use super::{bump_jobs, emit_jobs_locked, find_job_mut, EngineInner};

pub trait JobStore: Send + Sync {
    fn persist_jobs(&self, jobs: &[Job]) -> Result<(), String>;
}

pub struct FileJobStore {
    pub paths: crate::persistence::AppPaths,
}

impl JobStore for FileJobStore {
    fn persist_jobs(&self, jobs: &[Job]) -> Result<(), String> {
        crate::persistence::save_jobs(&self.paths, jobs)
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryJobStore {
    pub snapshots: std::sync::Mutex<Vec<Vec<Job>>>,
}

#[cfg(test)]
impl JobStore for MemoryJobStore {
    fn persist_jobs(&self, jobs: &[Job]) -> Result<(), String> {
        self.snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(jobs.to_vec());
        Ok(())
    }
}

pub(super) struct PersistReq {
    ack: oneshot::Sender<Result<(), String>>,
}

pub(super) async fn persist_actor(
    inner: Arc<Mutex<EngineInner>>,
    mut rx: mpsc::Receiver<PersistReq>,
) {
    while let Some(req) = rx.recv().await {
        let result = persist_until_generation_stable(&inner).await;
        let _ = req.ack.send(result);
    }
}

/// Snapshot, write, and if a durable mutation landed during the write, snapshot
/// again before ack. One actor is the only `state.json` writer.
async fn persist_until_generation_stable(inner: &Arc<Mutex<EngineInner>>) -> Result<(), String> {
    loop {
        let (jobs, store, generation) = {
            let guard = inner.lock().await;
            (
                guard.jobs.clone(),
                guard.store.clone(),
                guard.jobs_generation,
            )
        };
        let write = tokio::task::spawn_blocking(move || store.persist_jobs(&jobs))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        if let Err(error) = write {
            return Err(error);
        }
        let current = inner.lock().await.jobs_generation;
        if current == generation {
            return Ok(());
        }
    }
}

pub(super) async fn persist_live_jobs(inner: &Arc<Mutex<EngineInner>>) -> Result<(), String> {
    let tx = {
        let guard = inner.lock().await;
        guard.persist_tx.clone()
    };
    let (ack, rx) = oneshot::channel();
    tx.send(PersistReq { ack })
        .await
        .map_err(|_| "persist worker stopped".to_string())?;
    rx.await
        .unwrap_or_else(|_| Err("persist worker dropped".into()))
}

pub(crate) struct EngineIdentity {
    pub(super) inner: Arc<Mutex<EngineInner>>,
}

#[async_trait]
impl IdentityCommit for EngineIdentity {
    async fn commit(&self, job: &mut Job, c: CommitIdentity) -> Result<(), String> {
        apply_commit_identity(job, &c);
        let applied = {
            let mut guard = self.inner.lock().await;
            let identity_wiped = guard.requeue_on_cancel.contains_key(&job.id)
                || guard.pending_partial_deletes.contains_key(&job.id);
            if let Some(canonical) = find_job_mut(&mut guard.jobs, &job.id) {
                // Restart/cancel-delete wipes identity under these flags. In-flight
                // commits must not restore progress, validators, paths, or a map —
                // including MapUpdate::Unchanged payloads from single-stream GETs.
                if identity_wiped {
                    false
                } else {
                    apply_commit_identity(canonical, &c);
                    bump_jobs(&mut guard);
                    emit_jobs_locked(&guard);
                    true
                }
            } else {
                false
            }
        };
        if applied {
            persist_live_jobs(&self.inner).await
        } else {
            Ok(())
        }
    }

    async fn output_discarded(&self, job_id: &str) -> bool {
        let guard = self.inner.lock().await;
        if guard.requeue_on_cancel.contains_key(job_id)
            || guard.pending_final_deletes.contains_key(job_id)
        {
            return true;
        }
        // Cancel+delete_partial keeps the job row. Remove+keep-file does not.
        guard.pending_partial_deletes.contains_key(job_id)
            && guard.jobs.iter().any(|job| job.id == job_id)
    }

    async fn note_produced_file(&self, job_id: &str, path: PathBuf) {
        let mut guard = self.inner.lock().await;
        guard.produced_files.insert(job_id.to_string(), path);
    }

    async fn clear_produced_file(&self, job_id: &str) {
        let mut guard = self.inner.lock().await;
        guard.produced_files.remove(job_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::bandwidth::GlobalBandwidthLimiter;
    use crate::download::conn_budget::ConnectionBudget;
    use crate::download::engine::{EngineEvent, EngineRuntimeConfig};
    use crate::download::job::{ContentValidators, JobState, TransferMode};
    use crate::download::segment::{Segment, SegmentMap, SegmentState};
    use std::collections::HashMap;
    use tokio::sync::Notify;

    fn sample_job(id: &str) -> Job {
        let mut job = Job::new(
            "https://example.com/file.bin".into(),
            "file.bin".into(),
            PathBuf::from("C:\\downloads\\file.bin"),
            PathBuf::from("C:\\downloads\\file.bin.part"),
        );
        job.id = id.into();
        job.state = JobState::Queued;
        job
    }

    fn v1_map(total: u64) -> SegmentMap {
        SegmentMap {
            total_bytes: total,
            segment_count: 1,
            segments: vec![Segment {
                index: 0,
                start: 0,
                end: total.saturating_sub(1),
                written: 0,
                state: SegmentState::Pending,
            }],
            preallocated: false,
        }
    }

    fn v1_commit(total: u64) -> CommitIdentity {
        CommitIdentity {
            transfer_format_version: Some(1),
            map: MapUpdate::Set(v1_map(total)),
            ..Default::default()
        }
    }

    fn last_snapshot(store: &MemoryJobStore) -> Vec<Job> {
        store
            .snapshots
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    fn assert_later_identities(jobs: &[Job]) {
        let saved_a = jobs.iter().find(|j| j.id == "job-a").expect("job-a");
        let saved_b = jobs.iter().find(|j| j.id == "job-b").expect("job-b");
        assert_eq!(saved_a.transfer_format_version, 1);
        assert_eq!(saved_a.segment_map.as_ref().unwrap().total_bytes, 100);
        assert_eq!(saved_b.transfer_format_version, 1);
        assert_eq!(saved_b.segment_map.as_ref().unwrap().total_bytes, 200);
    }

    async fn inner_with_store(jobs: Vec<Job>, store: Arc<dyn JobStore>) -> Arc<Mutex<EngineInner>> {
        let (event_tx, _event_rx) = mpsc::unbounded_channel::<EngineEvent>();
        let (persist_tx, persist_rx) = mpsc::channel(32);
        let inner = Arc::new(Mutex::new(EngineInner {
            jobs,
            jobs_generation: 0,
            controls: HashMap::new(),
            active: HashMap::new(),
            handoff_auth: HashMap::new(),
            requeue_on_cancel: HashMap::new(),
            pending_partial_deletes: HashMap::new(),
            pending_final_deletes: HashMap::new(),
            produced_files: HashMap::new(),
            config: EngineRuntimeConfig::default(),
            limiter: GlobalBandwidthLimiter::new(None),
            conn_budget: ConnectionBudget::new(32, 8),
            event_tx,
            wake: Arc::new(Notify::new()),
            store,
            persist_tx,
        }));
        tokio::spawn(persist_actor(inner.clone(), persist_rx));
        inner
    }

    #[tokio::test]
    async fn commit_persists_version_and_map() {
        let store = Arc::new(MemoryJobStore::default());
        let mut job = sample_job("id-a");
        let inner = inner_with_store(vec![job.clone()], store.clone()).await;
        let committer = EngineIdentity {
            inner: inner.clone(),
        };
        committer.commit(&mut job, v1_commit(1000)).await.unwrap();

        let snaps = store.snapshots.lock().unwrap();
        let last = snaps.last().expect("persist after commit");
        let saved = last.iter().find(|j| j.id == "id-a").unwrap();
        assert_eq!(saved.transfer_format_version, 1);
        assert!(saved.segment_map.is_some());
        assert!(!saved
            .segment_map
            .as_ref()
            .unwrap()
            .structure_eq(&v1_map(999)));
        assert!(saved
            .segment_map
            .as_ref()
            .unwrap()
            .structure_eq(&v1_map(1000)));
    }

    #[tokio::test]
    async fn overlapping_commits_keep_later_identity_of_each_job() {
        let store = Arc::new(MemoryJobStore::default());
        let job_a = sample_job("job-a");
        let job_b = sample_job("job-b");
        let inner = inner_with_store(vec![job_a, job_b], store.clone()).await;

        let persist_tx = {
            let guard = inner.lock().await;
            guard.persist_tx.clone()
        };

        let (ack_tx, ack_rx) = oneshot::channel();
        {
            let mut guard = inner.lock().await;
            persist_tx.send(PersistReq { ack: ack_tx }).await.unwrap();
            tokio::task::yield_now().await;

            if let Some(job) = find_job_mut(&mut guard.jobs, "job-a") {
                apply_commit_identity(job, &v1_commit(100));
            }
            if let Some(job) = find_job_mut(&mut guard.jobs, "job-b") {
                apply_commit_identity(job, &v1_commit(200));
            }
        }
        ack_rx.await.unwrap().unwrap();

        let snaps = store.snapshots.lock().unwrap().clone();
        assert!(
            !snaps.is_empty(),
            "queued persist must write after the lock drops"
        );
        for snap in &snaps {
            assert_later_identities(snap);
        }
    }

    #[tokio::test]
    async fn in_flight_snapshot_rewrites_when_generation_moves() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        struct GateStore {
            entered: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
            release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
            snaps: MemoryJobStore,
        }
        impl JobStore for GateStore {
            fn persist_jobs(&self, jobs: &[Job]) -> Result<(), String> {
                if let Some(tx) = self
                    .entered
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                {
                    let _ = tx.send(());
                    let rx = self
                        .release
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                        .expect("release channel");
                    let _ = rx.recv();
                }
                self.snaps.persist_jobs(jobs)
            }
        }

        let store = std::sync::Arc::new(GateStore {
            entered: std::sync::Mutex::new(Some(entered_tx)),
            release: std::sync::Mutex::new(Some(release_rx)),
            snaps: MemoryJobStore::default(),
        });
        let inner = inner_with_store(vec![sample_job("kept")], store.clone()).await;
        let persist_tx = inner.lock().await.persist_tx.clone();
        let (ack_tx, ack_rx) = oneshot::channel();
        persist_tx.send(PersistReq { ack: ack_tx }).await.unwrap();

        tokio::task::spawn_blocking(move || {
            entered_rx.recv().expect("persist entered");
        })
        .await
        .unwrap();

        {
            let mut guard = inner.lock().await;
            guard.jobs.push(sample_job("added"));
            bump_jobs(&mut guard);
        }
        release_tx.send(()).expect("release persist");

        ack_rx.await.unwrap().unwrap();
        let snaps = store.snaps.snapshots.lock().unwrap().clone();
        assert!(
            snaps.len() >= 2,
            "stale snapshot must be followed by a rewrite, got {}",
            snaps.len()
        );
        let last = snaps.last().unwrap();
        assert!(last.iter().any(|job| job.id == "kept"));
        assert!(
            last.iter().any(|job| job.id == "added"),
            "rewrite must include the job inserted during the write"
        );
    }

    #[tokio::test]
    async fn follow_up_snapshot_failure_returns_the_error() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        struct GateThenFail {
            entered: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
            release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
            snaps: MemoryJobStore,
            writes: std::sync::atomic::AtomicUsize,
        }
        impl JobStore for GateThenFail {
            fn persist_jobs(&self, jobs: &[Job]) -> Result<(), String> {
                let n = self
                    .writes
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    if let Some(tx) = self
                        .entered
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                    {
                        let _ = tx.send(());
                        let rx = self
                            .release
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .take()
                            .expect("release channel");
                        let _ = rx.recv();
                    }
                    return self.snaps.persist_jobs(jobs);
                }
                Err("follow-up snapshot failed".into())
            }
        }

        let store = std::sync::Arc::new(GateThenFail {
            entered: std::sync::Mutex::new(Some(entered_tx)),
            release: std::sync::Mutex::new(Some(release_rx)),
            snaps: MemoryJobStore::default(),
            writes: std::sync::atomic::AtomicUsize::new(0),
        });
        let inner = inner_with_store(vec![sample_job("kept")], store.clone()).await;
        let persist_task = tokio::spawn({
            let inner = inner.clone();
            async move { persist_live_jobs(&inner).await }
        });

        tokio::task::spawn_blocking(move || {
            entered_rx.recv().expect("persist entered");
        })
        .await
        .unwrap();

        {
            let mut guard = inner.lock().await;
            guard.jobs.push(sample_job("added"));
            bump_jobs(&mut guard);
        }
        release_tx.send(()).expect("release persist");

        let error = persist_task
            .await
            .unwrap()
            .expect_err("a failed follow-up write must not ack");
        assert!(error.contains("follow-up"), "{error}");
        let snaps = store.snaps.snapshots.lock().unwrap().clone();
        assert_eq!(snaps.len(), 1, "only the first snapshot may land");
        assert!(snaps[0].iter().all(|job| job.id != "added"));
    }

    #[tokio::test]
    async fn cancel_delete_partial_keeps_store_cleared_after_worker_map_commit() {
        let store = Arc::new(MemoryJobStore::default());
        let mut job = sample_job("cancel-active");
        job.state = JobState::Downloading;
        job.transfer_format_version = 1;
        job.segment_map = Some(v1_map(1000));
        let job_id = job.id.clone();
        let temp_path = job.temp_path.clone();
        let inner = inner_with_store(vec![job.clone()], store.clone()).await;

        {
            let mut guard = inner.lock().await;
            guard
                .pending_partial_deletes
                .insert(job_id.clone(), temp_path);
            if let Some(canonical) = find_job_mut(&mut guard.jobs, &job_id) {
                canonical.clear_transfer_identity();
            }
        }
        persist_live_jobs(&inner).await.unwrap();

        let committer = EngineIdentity {
            inner: inner.clone(),
        };
        committer.commit(&mut job, v1_commit(1000)).await.unwrap();

        {
            let mut guard = inner.lock().await;
            guard.pending_partial_deletes.remove(&job_id);
            if let Some(canonical) = find_job_mut(&mut guard.jobs, &job_id) {
                canonical.state = JobState::Canceled;
                canonical.clear_transfer_identity();
            }
        }

        let saved = last_snapshot(&store)
            .into_iter()
            .find(|j| j.id == job_id)
            .expect("job in store");
        assert_eq!(saved.transfer_format_version, 0);
        assert!(saved.segment_map.is_none());
    }

    #[tokio::test]
    async fn unchanged_commit_after_restart_does_not_restore_or_persist() {
        let store = Arc::new(MemoryJobStore::default());
        let mut job = sample_job("restart-unchanged");
        job.clear_transfer_identity();
        let job_id = job.id.clone();
        let original_target = job.target_path.clone();
        let inner = inner_with_store(vec![job.clone()], store.clone()).await;

        {
            let mut guard = inner.lock().await;
            guard.requeue_on_cancel.insert(job_id.clone(), ());
        }
        persist_live_jobs(&inner).await.unwrap();
        let snaps_after_wipe = store.snapshots.lock().unwrap().len();

        let committer = EngineIdentity {
            inner: inner.clone(),
        };
        committer
            .commit(
                &mut job,
                CommitIdentity {
                    downloaded_bytes: Some(50),
                    total_bytes: Some(1000),
                    progress: Some(5.0),
                    filename: Some("renamed.bin".into()),
                    target_path: Some(PathBuf::from("C:\\downloads\\renamed.bin")),
                    resume_supported: Some(true),
                    validators: Some(ContentValidators {
                        etag: Some("\"poison\"".into()),
                        last_modified: None,
                        expected_size: Some(1000),
                    }),
                    transfer_mode: Some(TransferMode::Single),
                    fallback_reason: Some("size_unknown".into()),
                    map: MapUpdate::Unchanged,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        {
            let guard = inner.lock().await;
            let canonical = guard.jobs.iter().find(|j| j.id == job_id).unwrap();
            assert_eq!(canonical.downloaded_bytes, 0);
            assert_eq!(canonical.total_bytes, 0);
            assert_eq!(canonical.progress, 0.0);
            assert_eq!(canonical.filename, "file.bin");
            assert_eq!(canonical.target_path, original_target);
            assert!(!canonical.resume_supported);
            assert!(canonical.validators.is_empty());
            assert!(canonical.transfer_mode.is_none());
            assert!(canonical.fallback_reason.is_none());
            assert_eq!(canonical.transfer_format_version, 0);
            assert!(canonical.segment_map.is_none());
        }

        assert_eq!(
            store.snapshots.lock().unwrap().len(),
            snaps_after_wipe,
            "skipped Unchanged commit must not persist a restored identity"
        );
    }

    #[tokio::test]
    async fn output_discarded_follows_restart_and_final_delete_flags() {
        let store = Arc::new(MemoryJobStore::default());
        let job = sample_job("discard-id");
        let job_id = job.id.clone();
        let inner = inner_with_store(vec![job], store).await;
        let committer = EngineIdentity {
            inner: inner.clone(),
        };
        assert!(!committer.output_discarded(&job_id).await);

        {
            let mut guard = inner.lock().await;
            guard.requeue_on_cancel.insert(job_id.clone(), ());
        }
        assert!(committer.output_discarded(&job_id).await);

        {
            let mut guard = inner.lock().await;
            guard.requeue_on_cancel.remove(&job_id);
            guard.pending_final_deletes.insert(job_id.clone(), ());
        }
        assert!(committer.output_discarded(&job_id).await);

        {
            let mut guard = inner.lock().await;
            guard.pending_final_deletes.remove(&job_id);
            guard.pending_partial_deletes.insert(
                job_id.clone(),
                PathBuf::from("C:\\downloads\\file.bin.part"),
            );
        }
        assert!(
            committer.output_discarded(&job_id).await,
            "Cancel+delete_partial must discard while the job row remains"
        );

        {
            let mut guard = inner.lock().await;
            guard.jobs.clear();
        }
        assert!(
            !committer.output_discarded(&job_id).await,
            "Remove+keep-file must not discard just because a .part delete is pending"
        );
    }

    #[tokio::test]
    async fn clear_produced_file_drops_recorded_path() {
        let store = Arc::new(MemoryJobStore::default());
        let job = sample_job("produced-id");
        let job_id = job.id.clone();
        let inner = inner_with_store(vec![job], store).await;
        let committer = EngineIdentity {
            inner: inner.clone(),
        };
        let path = PathBuf::from("C:\\downloads\\file.bin");
        committer.note_produced_file(&job_id, path.clone()).await;
        {
            let guard = inner.lock().await;
            assert_eq!(guard.produced_files.get(&job_id), Some(&path));
        }
        committer.clear_produced_file(&job_id).await;
        {
            let guard = inner.lock().await;
            assert!(
                !guard.produced_files.contains_key(&job_id),
                "transfer-side delete must drop the produced path so finalize cannot delete it again"
            );
        }
    }
}
