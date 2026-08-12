//! Global + per-host connection budget for concurrent HTTP transfer bodies.
//!
//! Job scheduler still limits concurrent jobs; this limits simultaneous
//! request bodies (single-stream job or multi-segment workers).
//!
//! Wired into the multi-segment orchestrator in a later PR; keep public API live.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// Process-wide connection budget: one global pool plus per-host caps.
pub struct ConnectionBudget {
    global: Arc<Semaphore>,
    max_total: usize,
    max_per_host: usize,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
}

/// RAII permits for both global and per-host slots. Drop to release.
pub struct ConnectionPermit {
    _global: OwnedSemaphorePermit,
    _host: OwnedSemaphorePermit,
}

impl ConnectionBudget {
    /// Build a budget from runtime config caps (clamped to ≥1).
    pub fn new(max_total: u32, max_per_host: u32) -> Arc<Self> {
        let max_total = max_total.max(1) as usize;
        let max_per_host = (max_per_host.max(1) as usize).min(max_total);
        Arc::new(Self {
            global: Arc::new(Semaphore::new(max_total)),
            max_total,
            max_per_host,
            hosts: Mutex::new(HashMap::new()),
        })
    }

    pub fn max_total(&self) -> usize {
        self.max_total
    }

    pub fn max_per_host(&self) -> usize {
        self.max_per_host
    }

    pub fn available_global(&self) -> usize {
        self.global.available_permits()
    }

    /// Block until both a global and a per-host slot are held for `host`.
    ///
    /// Global is acquired first, then per-host, so total in-flight never exceeds
    /// the process-wide cap even while waiting on a busy host.
    pub async fn acquire(self: &Arc<Self>, host: &str) -> ConnectionPermit {
        let global = self
            .global
            .clone()
            .acquire_owned()
            .await
            .expect("connection budget global semaphore closed");

        let host_sem = {
            let mut hosts = self.hosts.lock().await;
            hosts
                .entry(host.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(self.max_per_host)))
                .clone()
        };

        let host = host_sem
            .acquire_owned()
            .await
            .expect("connection budget host semaphore closed");

        ConnectionPermit {
            _global: global,
            _host: host,
        }
    }

    /// Non-blocking attempt. Returns `None` if either pool is exhausted.
    pub async fn try_acquire(self: &Arc<Self>, host: &str) -> Option<ConnectionPermit> {
        let global = self.global.clone().try_acquire_owned().ok()?;

        let host_sem = {
            let mut hosts = self.hosts.lock().await;
            hosts
                .entry(host.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(self.max_per_host)))
                .clone()
        };

        match host_sem.try_acquire_owned() {
            Ok(host) => Some(ConnectionPermit {
                _global: global,
                _host: host,
            }),
            Err(_) => None, // global dropped → released
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn global_cap_limits_total() {
        let budget = ConnectionBudget::new(2, 2);
        let p1 = budget.try_acquire("a.com").await;
        let p2 = budget.try_acquire("b.com").await;
        assert!(p1.is_some());
        assert!(p2.is_some());
        assert!(budget.try_acquire("c.com").await.is_none());
        drop(p1);
        assert!(budget.try_acquire("c.com").await.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn per_host_cap_independent_of_other_hosts() {
        let budget = ConnectionBudget::new(4, 1);
        let a1 = budget.try_acquire("a.com").await;
        assert!(a1.is_some());
        assert!(
            budget.try_acquire("a.com").await.is_none(),
            "second connection to same host must block"
        );
        let b1 = budget.try_acquire("b.com").await;
        assert!(b1.is_some(), "other host still free under global cap");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn acquire_waits_until_release() {
        let budget = ConnectionBudget::new(1, 1);
        let held = budget.acquire("host.example").await;
        let counter = Arc::new(AtomicUsize::new(0));

        let budget2 = budget.clone();
        let counter2 = counter.clone();
        let waiter = tokio::spawn(async move {
            let _p = budget2.acquire("host.example").await;
            counter2.fetch_add(1, Ordering::SeqCst);
        });

        sleep(Duration::from_millis(50)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        drop(held);

        timeout(Duration::from_secs(2), waiter)
            .await
            .expect("waiter should finish")
            .expect("task");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn clamps_zero_to_one() {
        let budget = ConnectionBudget::new(0, 0);
        assert_eq!(budget.max_per_host(), 1);
        assert_eq!(budget.max_total(), 1);
        let p = budget.try_acquire("x").await;
        assert!(p.is_some());
        assert!(budget.try_acquire("x").await.is_none());
    }
}
