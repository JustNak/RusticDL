//! Global + per-host connection budget for concurrent HTTP transfer bodies.
//!
//! Job scheduler still limits concurrent jobs; this limits simultaneous
//! request bodies (single-stream job or multi-segment workers).
//!
//! # Acquire order
//! Host permit first, then global. Waiters blocked on the global pool only hold a
//! host slot (same-host backpressure), so one saturated host cannot exhaust the
//! process-wide pool and starve other hosts. On `try_acquire`, a failed global
//! attempt drops the host permit via RAII.
//!
//! # Host map growth
//! Per-host semaphores are retained for the process lifetime (v0.2). Idle entries
//! are small; pruning is deferred polish.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// Process-wide connection budget: one global pool plus per-host caps.
pub struct ConnectionBudget {
    global: Arc<Semaphore>,
    #[allow(dead_code)]
    max_total: usize,
    max_per_host: usize,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
}

/// RAII permits for both global and per-host slots. Drop to release.
#[must_use = "the permit is released when dropped"]
pub struct ConnectionPermit {
    _host: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

impl ConnectionBudget {
    /// Build a budget from runtime config caps.
    ///
    /// Clamps match [`crate::download::engine::EngineRuntimeConfig::sanitize`]:
    /// total ∈ [1, 256], per-host ∈ [1, 64], per-host ≤ total.
    pub fn new(max_total: u32, max_per_host: u32) -> Arc<Self> {
        let max_total = max_total.clamp(1, 256) as usize;
        let max_per_host = max_per_host.clamp(1, 64).min(max_total as u32) as usize;
        Arc::new(Self {
            global: Arc::new(Semaphore::new(max_total)),
            max_total,
            max_per_host,
            hosts: Mutex::new(HashMap::new()),
        })
    }

    #[allow(dead_code)]
    pub fn max_total(&self) -> usize {
        self.max_total
    }

    #[allow(dead_code)]
    pub fn max_per_host(&self) -> usize {
        self.max_per_host
    }

    #[allow(dead_code)]
    pub fn available_global(&self) -> usize {
        self.global.available_permits()
    }

    /// Normalize host for the per-host map (HTTP hostnames are case-insensitive).
    ///
    /// Lowercases the whole key. Port, if present (`host:port`), is preserved so
    /// different ports stay independent pools.
    pub fn normalize_host(host: &str) -> String {
        host.trim().to_ascii_lowercase()
    }

    /// Block until both a per-host and a global slot are held for `host`.
    ///
    /// Host is acquired first, then global (see module docs).
    #[must_use = "the permit is released when dropped"]
    pub async fn acquire(self: &Arc<Self>, host: &str) -> ConnectionPermit {
        let key = Self::normalize_host(host);
        let host_sem = self.host_semaphore(key).await;

        let host = host_sem
            .acquire_owned()
            .await
            .expect("connection budget host semaphore closed");

        let global = self
            .global
            .clone()
            .acquire_owned()
            .await
            .expect("connection budget global semaphore closed");

        ConnectionPermit {
            _host: host,
            _global: global,
        }
    }

    /// Non-blocking attempt. Returns `None` if either pool is exhausted.
    ///
    /// Host first: if global is exhausted after host succeeds, the host permit
    /// is dropped and released immediately.
    #[must_use = "the permit is released when dropped"]
    pub async fn try_acquire(self: &Arc<Self>, host: &str) -> Option<ConnectionPermit> {
        let key = Self::normalize_host(host);
        let host_sem = self.host_semaphore(key).await;
        let host = host_sem.try_acquire_owned().ok()?;

        match self.global.clone().try_acquire_owned() {
            Ok(global) => Some(ConnectionPermit {
                _host: host,
                _global: global,
            }),
            Err(_) => None, // host dropped → released
        }
    }

    async fn host_semaphore(&self, key: String) -> Arc<Semaphore> {
        let mut hosts = self.hosts.lock().await;
        hosts
            .entry(key)
            .or_insert_with(|| Arc::new(Semaphore::new(self.max_per_host)))
            .clone()
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
    async fn host_fail_releases_does_not_leak_slots() {
        // max_total=2, max_per_host=1: fill host a, fail second a, still allow b.
        let budget = ConnectionBudget::new(2, 1);
        let a1 = budget.try_acquire("a.com").await.expect("first a");
        assert_eq!(budget.available_global(), 1);

        assert!(
            budget.try_acquire("a.com").await.is_none(),
            "host a saturated"
        );
        // Host-first: host miss never took global. Global-first bug would also
        // release global on host miss; this tight total makes a leak visible if
        // someone reintroduces global-first without release.
        assert_eq!(
            budget.available_global(),
            1,
            "failed host acquire must not consume global"
        );

        let b1 = budget
            .try_acquire("b.com")
            .await
            .expect("b.com must get the remaining global slot");
        assert_eq!(budget.available_global(), 0);
        drop(a1);
        drop(b1);
        assert_eq!(budget.available_global(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn global_fail_releases_host_permit() {
        // max_total=1, max_per_host=1: host-first takes b.com's only host slot, then
        // global fails. If the host permit leaked, b.com would stay saturated forever
        // and the post-drop try_acquire would still fail.
        let budget = ConnectionBudget::new(1, 1);
        let held = budget.try_acquire("a.com").await.expect("hold global");
        assert!(
            budget.try_acquire("b.com").await.is_none(),
            "global exhausted"
        );
        drop(held);
        assert!(
            budget.try_acquire("b.com").await.is_some(),
            "host b must be free after global miss released its host permit"
        );
    }

    #[tokio::test]
    async fn host_key_is_case_insensitive() {
        let budget = ConnectionBudget::new(4, 1);
        let a = budget.try_acquire("Example.COM").await.expect("first");
        assert!(
            budget.try_acquire("example.com").await.is_none(),
            "mixed case must share per-host cap"
        );
        assert!(budget.try_acquire("EXAMPLE.com:443").await.is_some());
        drop(a);
        assert!(budget.try_acquire("example.com").await.is_some());
    }

    #[test]
    fn normalize_host_lowercases() {
        assert_eq!(
            ConnectionBudget::normalize_host("  CDN.Example.COM "),
            "cdn.example.com"
        );
        assert_eq!(
            ConnectionBudget::normalize_host("Host.Example:8443"),
            "host.example:8443"
        );
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
    async fn clamps_zero_to_one_and_upper_bounds() {
        let budget = ConnectionBudget::new(0, 0);
        assert_eq!(budget.max_per_host(), 1);
        assert_eq!(budget.max_total(), 1);
        let p = budget.try_acquire("x").await;
        assert!(p.is_some());
        assert!(budget.try_acquire("x").await.is_none());

        let hi = ConnectionBudget::new(10_000, 10_000);
        assert_eq!(hi.max_total(), 256);
        assert_eq!(hi.max_per_host(), 64);
    }
}
