use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::sleep;

use super::job::DownloadOutcome;

pub struct ConnectionBudget {
    global: Arc<Semaphore>,
    max_total: AtomicUsize,
    max_per_host: AtomicUsize,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
}

const CONTROL_POLL: Duration = Duration::from_millis(50);

#[must_use = "the permit is released when dropped"]
pub struct ConnectionPermit {
    _host: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

pub fn host_key_for_budget(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let host = parsed.host_str().unwrap_or("unknown");
            match parsed.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            }
        }
        Err(_) => "unknown".into(),
    }
}

fn clamp_limits(max_total: u32, max_per_host: u32) -> (usize, usize) {
    let max_total = max_total.clamp(1, 256) as usize;
    let max_per_host = max_per_host.clamp(1, 64).min(max_total as u32) as usize;
    (max_total, max_per_host)
}

impl ConnectionBudget {
    pub fn new(max_total: u32, max_per_host: u32) -> Arc<Self> {
        let (max_total, max_per_host) = clamp_limits(max_total, max_per_host);
        Arc::new(Self {
            global: Arc::new(Semaphore::new(max_total)),
            max_total: AtomicUsize::new(max_total),
            max_per_host: AtomicUsize::new(max_per_host),
            hosts: Mutex::new(HashMap::new()),
        })
    }

    /// Resize this budget in place so in-flight workers keep sharing one cap.
    pub async fn update_limits(&self, max_total: u32, max_per_host: u32) {
        let (max_total, max_per_host) = clamp_limits(max_total, max_per_host);
        let old_total = self.max_total.swap(max_total, Ordering::SeqCst);
        resize_semaphore(&self.global, old_total, max_total);

        let hosts = self.hosts.lock().await;
        let old_host = self.max_per_host.swap(max_per_host, Ordering::SeqCst);
        if old_host != max_per_host {
            for sem in hosts.values() {
                resize_semaphore(sem, old_host, max_per_host);
            }
        }
    }

    pub fn max_total(&self) -> usize {
        self.max_total.load(Ordering::SeqCst)
    }

    pub fn max_per_host(&self) -> usize {
        self.max_per_host.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn available_global(&self) -> usize {
        self.global.available_permits()
    }

    pub fn normalize_host(host: &str) -> String {
        host.trim().to_ascii_lowercase()
    }

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

    #[must_use = "the permit is released when dropped"]
    pub async fn acquire_interruptible(
        self: &Arc<Self>,
        host: &str,
        control: &AtomicU8,
    ) -> Result<ConnectionPermit, DownloadOutcome> {
        if let Some(outcome) = control_outcome(control) {
            return Err(outcome);
        }

        let key = Self::normalize_host(host);
        let host_sem = self.host_semaphore(key).await;
        let host = acquire_owned_interruptible(host_sem, control).await?;
        let global = acquire_owned_interruptible(self.global.clone(), control).await?;

        Ok(ConnectionPermit {
            _host: host,
            _global: global,
        })
    }

    #[allow(dead_code)]
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
        let max_per_host = self.max_per_host.load(Ordering::SeqCst);
        hosts
            .entry(key)
            .or_insert_with(|| Arc::new(Semaphore::new(max_per_host)))
            .clone()
    }
}

fn resize_semaphore(sem: &Arc<Semaphore>, old: usize, new: usize) {
    if new == old {
        return;
    }
    if new > old {
        sem.add_permits(new - old);
        return;
    }
    let mut remaining = old - new;
    while remaining > 0 {
        match sem.try_acquire() {
            Ok(permit) => {
                permit.forget();
                remaining -= 1;
            }
            Err(_) => break,
        }
    }
    if remaining == 0 {
        return;
    }
    let sem = sem.clone();
    tokio::spawn(async move {
        for _ in 0..remaining {
            match sem.acquire().await {
                Ok(permit) => permit.forget(),
                Err(_) => break,
            }
        }
    });
}

async fn acquire_owned_interruptible(
    sem: Arc<Semaphore>,
    control: &AtomicU8,
) -> Result<OwnedSemaphorePermit, DownloadOutcome> {
    loop {
        if let Some(outcome) = control_outcome(control) {
            return Err(outcome);
        }
        tokio::select! {
            result = sem.clone().acquire_owned() => {
                return Ok(result.expect("connection budget semaphore closed"));
            }
            _ = sleep(CONTROL_POLL) => {}
        }
    }
}

fn control_outcome(control: &AtomicU8) -> Option<DownloadOutcome> {
    match control.load(Ordering::Relaxed) {
        1 => Some(DownloadOutcome::Paused),
        2 => Some(DownloadOutcome::Canceled),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
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
        let budget = ConnectionBudget::new(2, 1);
        let a1 = budget.try_acquire("a.com").await.expect("first a");
        assert_eq!(budget.available_global(), 1);

        assert!(
            budget.try_acquire("a.com").await.is_none(),
            "host a saturated"
        );
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acquire_interruptible_returns_paused_when_control_set() {
        let budget = ConnectionBudget::new(1, 1);
        let held = budget.acquire("host.example").await;
        let control = Arc::new(AtomicU8::new(0));
        let control2 = control.clone();
        let budget2 = budget.clone();

        let blocked = tokio::spawn(async move {
            budget2
                .acquire_interruptible("host.example", &control2)
                .await
        });

        sleep(Duration::from_millis(80)).await;
        control.store(1, Ordering::Relaxed);

        let result = timeout(Duration::from_secs(2), blocked)
            .await
            .expect("should not hang on pause")
            .expect("task");
        assert!(
            matches!(result, Err(DownloadOutcome::Paused)),
            "acquire_interruptible must return Paused when control is set"
        );
        drop(held);
        assert!(
            budget.try_acquire("host.example").await.is_some(),
            "paused waiter must not keep a host or global slot"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acquire_interruptible_drops_partial_host_permit_on_cancel() {
        let budget = ConnectionBudget::new(1, 1);
        let held = budget.acquire("a.com").await;
        let control = Arc::new(AtomicU8::new(0));
        let control2 = control.clone();
        let budget2 = budget.clone();

        let blocked =
            tokio::spawn(async move { budget2.acquire_interruptible("b.com", &control2).await });

        sleep(Duration::from_millis(80)).await;
        control.store(2, Ordering::Relaxed);

        let result = timeout(Duration::from_secs(2), blocked)
            .await
            .expect("should not hang on cancel")
            .expect("task");
        assert!(
            matches!(result, Err(DownloadOutcome::Canceled)),
            "acquire_interruptible must return Canceled when control is set"
        );
        drop(held);
        assert!(
            budget.try_acquire("b.com").await.is_some(),
            "canceled waiter must release the host slot taken before the global wait"
        );
    }

    #[test]
    fn host_key_for_budget_uses_host_and_port() {
        assert_eq!(
            host_key_for_budget("https://CDN.Example.COM/file.bin"),
            "cdn.example.com"
        );
        assert_eq!(
            host_key_for_budget("http://127.0.0.1:8080/x"),
            "127.0.0.1:8080"
        );
        assert_eq!(host_key_for_budget("not a url"), "unknown");
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn update_limits_raises_cap_on_same_budget() {
        let budget = ConnectionBudget::new(1, 1);
        let held = budget.try_acquire("a.com").await.expect("first");
        assert!(budget.try_acquire("b.com").await.is_none());
        budget.update_limits(2, 1).await;
        assert!(
            budget.try_acquire("b.com").await.is_some(),
            "raising total connections must add permits to the live budget"
        );
        drop(held);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn update_limits_lowers_cap_on_same_budget() {
        let budget = ConnectionBudget::new(4, 4);
        let a = budget.try_acquire("a.com").await.expect("a");
        let b = budget.try_acquire("b.com").await.expect("b");
        budget.update_limits(2, 2).await;
        assert!(
            budget.try_acquire("c.com").await.is_none(),
            "lowering total connections must not issue a third permit"
        );
        drop(a);
        drop(b);
        let c = budget.try_acquire("c.com").await.expect("after release");
        let d = budget.try_acquire("d.com").await.expect("new cap is two");
        assert!(
            budget.try_acquire("e.com").await.is_none(),
            "cap must stay at the lowered total after in-flight permits drop"
        );
        drop(c);
        drop(d);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn update_limits_new_host_sees_new_per_host_cap_once() {
        let budget = ConnectionBudget::new(4, 1);
        budget.update_limits(4, 2).await;
        let a = budget.try_acquire("fresh.com").await.expect("first");
        let b = budget.try_acquire("fresh.com").await.expect("second");
        assert!(
            budget.try_acquire("fresh.com").await.is_none(),
            "a host created after resize must not get the delta applied twice"
        );
        drop(a);
        drop(b);
    }
}
