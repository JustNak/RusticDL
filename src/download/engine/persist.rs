//! Engine-owned job persist. `JobStore` is I/O only; the actor serializes writes
//! by re-reading live `EngineInner.jobs` so a stale snapshot cannot win.

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

/// Test double. Records snapshots; never touches disk.
#[derive(Default)]
#[allow(dead_code)]
pub struct MemoryJobStore {
    pub snapshots: std::sync::Mutex<Vec<Vec<Job>>>,
}

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

/// Queue one write of the current live queue. Never pass a caller-built Vec.
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

/// In-memory apply plus disk persist via the engine actor.
pub(crate) struct EngineIdentity {
    pub(super) inner: Arc<Mutex<EngineInner>>,
}

#[async_trait]
impl IdentityCommit for EngineIdentity {
    async fn commit(&self, job: &mut Job, c: CommitIdentity) -> Result<(), String> {
        apply_commit_identity(job, &c);
        let applied = {
            let mut guard = self.inner.lock().await;
            let requeued = guard.requeue_on_cancel.contains_key(&job.id);
            if let Some(canonical) = find_job_mut(&mut guard.jobs, &job.id) {
                // Restart already wiped identity and requeued; do not restore the canceled worker's map.
                if requeued
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
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
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

    struct SlowFirstStore {
        snapshots: StdMutex<Vec<Vec<Job>>>,
        writes: StdMutex<u32>,
    }

    impl SlowFirstStore {
        fn new() -> Self {
            Self {
                snapshots: StdMutex::new(Vec::new()),
                writes: StdMutex::new(0),
            }
        }

        fn last(&self) -> Vec<Job> {
            self.snapshots
                .lock()
                .unwrap()
                .last()
                .cloned()
                .unwrap_or_default()
        }
    }

    impl JobStore for SlowFirstStore {
        fn persist_jobs(&self, jobs: &[Job]) -> Result<(), String> {
            let n = {
                let mut writes = self.writes.lock().unwrap();
                *writes += 1;
                *writes
            };
            // First writer sleeps so a stale snapshot can finish last if the
            // actor is not re-reading live jobs.
            if n == 1 {
                std::thread::sleep(Duration::from_millis(150));
            }
            self.snapshots.lock().unwrap().push(jobs.to_vec());
            Ok(())
        }
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
        let store = Arc::new(SlowFirstStore::new());
        let job_a = sample_job("job-a");
        let job_b = sample_job("job-b");
        let inner = inner_with_store(vec![job_a.clone(), job_b.clone()], store.clone()).await;

        // In-flight persist of the pre-commit queue; without a live re-read this
        // write can finish after A/B apply and drop both later identities.
        let dummy = persist_live_jobs(&inner);
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let ident_a = EngineIdentity {
            inner: inner.clone(),
        };
        let ident_b = EngineIdentity {
            inner: inner.clone(),
        };
        let mut job_a = job_a;
        let mut job_b = job_b;
        let (dummy_res, a_res, b_res) = tokio::join!(
            dummy,
            ident_a.commit(&mut job_a, v1_commit(100)),
            ident_b.commit(&mut job_b, v1_commit(200)),
        );
        dummy_res.unwrap();
        a_res.unwrap();
        b_res.unwrap();

        let last = store.last();
        let saved_a = last.iter().find(|j| j.id == "job-a").expect("job-a");
        let saved_b = last.iter().find(|j| j.id == "job-b").expect("job-b");
        assert_eq!(saved_a.transfer_format_version, 1);
        assert_eq!(saved_a.segment_map.as_ref().unwrap().total_bytes, 100);
        assert_eq!(saved_b.transfer_format_version, 1);
        assert_eq!(saved_b.segment_map.as_ref().unwrap().total_bytes, 200);
    }
}
