use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, Mutex};

use super::super::job::Job;
use super::super::progress::{apply_commit_identity, CommitIdentity, IdentityCommit, MapUpdate};
use super::{emit_jobs_locked, find_job_mut, EngineInner};

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
        let (jobs, store) = {
            let guard = inner.lock().await;
            (guard.jobs.clone(), guard.store.clone())
        };
        let result = tokio::task::spawn_blocking(move || store.persist_jobs(&jobs))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let _ = req.ack.send(result);
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
                if identity_wiped
                    && canonical.segment_map.is_none()
                    && canonical.transfer_format_version == 0
                    && matches!(c.map, MapUpdate::Set(_) | MapUpdate::Clear)
                {
                    false
                } else {
                    apply_commit_identity(canonical, &c);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::bandwidth::GlobalBandwidthLimiter;
    use crate::download::conn_budget::ConnectionBudget;
    use crate::download::engine::{EngineEvent, EngineRuntimeConfig};
    use crate::download::job::JobState;
    use crate::download::segment::{Segment, SegmentMap, SegmentState};
    use std::collections::HashMap;
    use std::path::PathBuf;
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
            controls: HashMap::new(),
            active: HashMap::new(),
            handoff_auth: HashMap::new(),
            requeue_on_cancel: HashMap::new(),
            pending_partial_deletes: HashMap::new(),
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
}
