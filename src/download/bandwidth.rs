//! Process-wide token-bucket bandwidth limiter shared by all transfer bodies.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify};
use tokio::time::sleep;

/// Shared global download bandwidth limiter.
pub struct GlobalBandwidthLimiter {
    state: Mutex<LimiterState>,
    notify: Notify,
}

struct LimiterState {
    /// Bytes per second. `None` or `0` means unlimited.
    rate: Option<u64>,
    tokens: f64,
    capacity: f64,
    last_refill: Instant,
}

impl GlobalBandwidthLimiter {
    /// Max bytes a single wait loop iteration may charge (callers may pass larger `n`).
    pub const MAX_ACQUIRE_QUANTUM: usize = 64 * 1024;

    pub fn new(bytes_per_second: Option<u64>) -> Arc<Self> {
        let rate = normalize_rate(bytes_per_second);
        let capacity = capacity_for(rate);
        Arc::new(Self {
            state: Mutex::new(LimiterState {
                rate,
                tokens: capacity,
                capacity,
                last_refill: Instant::now(),
            }),
            notify: Notify::new(),
        })
    }

    /// Hot-update the rate. Wakes all waiters so unlimited / higher limits apply promptly.
    pub async fn set_limit(&self, bytes_per_second: Option<u64>) {
        {
            let mut state = self.state.lock().await;
            let rate = normalize_rate(bytes_per_second);
            state.rate = rate;
            state.capacity = capacity_for(rate);
            // Do not wipe tokens on raise; cap if capacity shrinks.
            if state.tokens > state.capacity {
                state.tokens = state.capacity;
            }
            state.last_refill = Instant::now();
        }
        self.notify.notify_waiters();
    }

    /// Block until `n` bytes may proceed. Splits into ≤ [`MAX_ACQUIRE_QUANTUM`] chunks.
    pub async fn acquire(&self, n: usize) {
        if n == 0 {
            return;
        }
        let mut remaining = n;
        while remaining > 0 {
            let want = remaining.min(Self::MAX_ACQUIRE_QUANTUM);
            self.acquire_quantum(want).await;
            remaining -= want;
        }
    }

    async fn acquire_quantum(&self, n: usize) {
        let need = n as f64;
        loop {
            let wait_for = {
                let mut state = self.state.lock().await;
                if state.rate.is_none() {
                    return;
                }
                let rate = state.rate.unwrap_or(0);
                if rate == 0 {
                    return;
                }

                refill(&mut state);

                if state.tokens >= need {
                    state.tokens -= need;
                    return;
                }

                let deficit = need - state.tokens;
                let secs = deficit / rate as f64;
                // Cap individual sleeps; re-check after notify or timeout.
                Duration::from_secs_f64(secs.clamp(0.001, 2.0))
            };

            tokio::select! {
                _ = self.notify.notified() => {}
                _ = sleep(wait_for) => {}
            }
        }
    }
}

fn normalize_rate(bytes_per_second: Option<u64>) -> Option<u64> {
    match bytes_per_second {
        None | Some(0) => None,
        Some(r) => Some(r),
    }
}

fn capacity_for(rate: Option<u64>) -> f64 {
    match rate {
        None => 0.0,
        Some(r) => (2.0 * r as f64).max(GlobalBandwidthLimiter::MAX_ACQUIRE_QUANTUM as f64),
    }
}

fn refill(state: &mut LimiterState) {
    let Some(rate) = state.rate else {
        return;
    };
    let now = Instant::now();
    let elapsed = now.duration_since(state.last_refill).as_secs_f64();
    if elapsed > 0.0 {
        state.tokens = (state.tokens + elapsed * rate as f64).min(state.capacity);
        state.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn rate_under_n_acquirers() {
        // 64 KiB/s shared across 4 concurrent acquirers.
        // Capacity is max(2*rate, 64KiB) = 128 KiB, so transfer well above burst.
        let rate = 64 * 1024u64;
        let limiter = GlobalBandwidthLimiter::new(Some(rate));
        let total = Arc::new(AtomicU64::new(0));
        // 4 × 64 KiB = 256 KiB total → ~2s after 128 KiB burst at 64 KiB/s.
        let per_task = 64 * 1024usize;
        let tasks = 4usize;

        let start = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..tasks {
            let limiter = limiter.clone();
            let total = total.clone();
            handles.push(tokio::spawn(async move {
                limiter.acquire(per_task).await;
                total.fetch_add(per_task as u64, Ordering::Relaxed);
            }));
        }
        for h in handles {
            h.await.expect("task");
        }
        let elapsed = start.elapsed().as_secs_f64().max(0.001);
        let bytes = total.load(Ordering::Relaxed);
        let observed = bytes as f64 / elapsed;

        // Allow generous slack for scheduler noise; must not free-run at wire speed.
        assert!(
            observed < rate as f64 * 3.0,
            "observed {observed:.0} B/s exceeds ~3× rate ({rate})"
        );
        // Minimum: (total - capacity) / rate ≈ (256-128)/64 = 2s, but allow 0.8s slack.
        assert!(
            elapsed >= 0.8,
            "elapsed {elapsed:.2}s too short for limited transfer of {bytes} bytes"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn set_limit_unblocks_waiters() {
        // Very low rate so acquire blocks, then open the limit.
        let limiter = GlobalBandwidthLimiter::new(Some(1)); // 1 byte/s
        let limiter2 = limiter.clone();

        let blocked = tokio::spawn(async move {
            limiter2.acquire(64 * 1024).await;
        });

        // Give the waiter time to park.
        sleep(Duration::from_millis(50)).await;
        limiter.set_limit(None).await;

        tokio::time::timeout(Duration::from_secs(2), blocked)
            .await
            .expect("acquire should unblock after set_limit(None)")
            .expect("task");
    }

    #[tokio::test]
    async fn unlimited_is_fast_path() {
        let limiter = GlobalBandwidthLimiter::new(None);
        let start = Instant::now();
        limiter.acquire(8 * 1024 * 1024).await;
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "unlimited acquire should not sleep"
        );
    }

    #[tokio::test]
    async fn zero_rate_is_unlimited() {
        let limiter = GlobalBandwidthLimiter::new(Some(0));
        let start = Instant::now();
        limiter.acquire(1024 * 1024).await;
        assert!(start.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn capacity_at_least_quantum() {
        let cap = capacity_for(Some(100));
        assert!(cap >= GlobalBandwidthLimiter::MAX_ACQUIRE_QUANTUM as f64);
        let cap_hi = capacity_for(Some(1024 * 1024));
        assert!((cap_hi - 2.0 * 1024.0 * 1024.0).abs() < 1.0);
    }
}
