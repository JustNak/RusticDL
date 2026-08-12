//! Process-wide token-bucket bandwidth limiter shared by all transfer bodies.
//!
//! **Placement:** callers charge after a network read and before the disk write
//! so concurrent jobs share one budget and TCP eventually back-pressures. Short
//! bursts up to the bucket capacity (`max(2×rate, 64 KiB)`) are expected after
//! idle; that is intentional shaping, not a failed limit.

use std::sync::atomic::{AtomicU8, Ordering};
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
    /// Bytes per second. `None` means unlimited.
    rate: Option<u64>,
    tokens: f64,
    capacity: f64,
    last_refill: Instant,
}

/// Result of trying to take tokens under the lock.
enum TryTake {
    /// Unlimited or enough tokens charged.
    Done,
    /// Need to wait this long (or until notified) before retrying.
    Wait(Duration),
}

impl GlobalBandwidthLimiter {
    /// Max bytes a single wait loop iteration may charge (callers may pass larger `n`).
    pub const MAX_ACQUIRE_QUANTUM: usize = 64 * 1024;

    /// While waiting with a control signal, re-check at least this often.
    const CONTROL_POLL: Duration = Duration::from_millis(50);

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
            // Accrue under the old rate before changing capacity.
            refill(&mut state);
            let rate = normalize_rate(bytes_per_second);
            state.rate = rate;
            state.capacity = capacity_for(rate);
            if state.tokens > state.capacity {
                state.tokens = state.capacity;
            }
        }
        self.notify.notify_waiters();
    }

    /// Block until `n` bytes may proceed. Splits into ≤ [`MAX_ACQUIRE_QUANTUM`] chunks.
    ///
    /// When `control` is `Some` and becomes non-zero (pause/cancel), returns `false`
    /// without charging remaining quanta. Unlimited / zero-rate is a fast path.
    pub async fn acquire(&self, n: usize, control: Option<&AtomicU8>) -> bool {
        if n == 0 {
            return true;
        }
        let mut remaining = n;
        while remaining > 0 {
            if control.is_some_and(|c| c.load(Ordering::Relaxed) != 0) {
                return false;
            }
            let want = remaining.min(Self::MAX_ACQUIRE_QUANTUM);
            if !self.acquire_quantum(want, control).await {
                return false;
            }
            remaining -= want;
        }
        true
    }

    async fn acquire_quantum(&self, n: usize, control: Option<&AtomicU8>) -> bool {
        let need = n as f64;
        loop {
            if control.is_some_and(|c| c.load(Ordering::Relaxed) != 0) {
                return false;
            }

            // Create + pin the wait future, then enable it *under the mutex* after
            // re-checking the condition so notify_waiters cannot be lost between
            // unlock and first poll (Notify does not store waiter permits).
            let mut notified = std::pin::pin!(self.notify.notified());
            let wait_for = {
                let mut state = self.state.lock().await;
                match try_take(&mut state, need) {
                    TryTake::Done => return true,
                    TryTake::Wait(mut wait_for) => {
                        if control.is_some() {
                            wait_for = wait_for.min(Self::CONTROL_POLL);
                        }
                        // Register as a waiter while still holding the lock.
                        notified.as_mut().enable();
                        wait_for
                    }
                }
            }; // mutex dropped here; we are already on the waiter list

            tokio::select! {
                _ = notified.as_mut() => {}
                _ = sleep(wait_for) => {}
            }
        }
    }
}

fn try_take(state: &mut LimiterState, need: f64) -> TryTake {
    let Some(rate) = state.rate else {
        return TryTake::Done;
    };

    refill(state);

    if state.tokens >= need {
        state.tokens -= need;
        return TryTake::Done;
    }

    let deficit = need - state.tokens;
    let secs = deficit / rate as f64;
    TryTake::Wait(Duration::from_secs_f64(secs.clamp(0.001, 2.0)))
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
    use std::sync::atomic::AtomicU64;
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
                assert!(limiter.acquire(per_task, None).await);
                total.fetch_add(per_task as u64, Ordering::Relaxed);
            }));
        }
        for h in handles {
            h.await.expect("task");
        }
        let elapsed = start.elapsed().as_secs_f64().max(0.001);
        let bytes = total.load(Ordering::Relaxed);
        let observed = bytes as f64 / elapsed;

        assert!(
            observed < rate as f64 * 3.0,
            "observed {observed:.0} B/s exceeds ~3× rate ({rate})"
        );
        assert!(
            elapsed >= 0.8,
            "elapsed {elapsed:.2}s too short for limited transfer of {bytes} bytes"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn set_limit_unblocks_waiters() {
        let limiter = GlobalBandwidthLimiter::new(Some(1)); // 1 byte/s
        let limiter2 = limiter.clone();

        let blocked = tokio::spawn(async move {
            assert!(limiter2.acquire(64 * 1024, None).await);
        });

        sleep(Duration::from_millis(50)).await;
        let t0 = Instant::now();
        limiter.set_limit(None).await;

        tokio::time::timeout(Duration::from_secs(5), blocked)
            .await
            .expect("acquire should unblock after set_limit(None)")
            .expect("task");
        // Missed notify would sleep up to 2s; a correct wake is near-instant.
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "set_limit should wake promptly, elapsed {:?}",
            t0.elapsed()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn acquire_aborts_on_control() {
        // Capacity is max(2*rate, 64 KiB) = 64 KiB at 1 B/s — drain it first so
        // the next acquire must wait (and can observe pause).
        let limiter = GlobalBandwidthLimiter::new(Some(1));
        assert!(limiter.acquire(64 * 1024, None).await);

        let control = Arc::new(AtomicU8::new(0));
        let control2 = control.clone();
        let limiter2 = limiter.clone();

        let blocked = tokio::spawn(async move { limiter2.acquire(1024, Some(&control2)).await });

        sleep(Duration::from_millis(80)).await;
        control.store(1, Ordering::Relaxed); // paused

        let ok = tokio::time::timeout(Duration::from_secs(2), blocked)
            .await
            .expect("should not hang on pause")
            .expect("task");
        assert!(!ok, "acquire must return false when control is non-zero");
    }

    #[tokio::test]
    async fn unlimited_is_fast_path() {
        let limiter = GlobalBandwidthLimiter::new(None);
        let start = Instant::now();
        assert!(limiter.acquire(8 * 1024 * 1024, None).await);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "unlimited acquire should not sleep"
        );
    }

    #[tokio::test]
    async fn zero_rate_is_unlimited() {
        let limiter = GlobalBandwidthLimiter::new(Some(0));
        let start = Instant::now();
        assert!(limiter.acquire(1024 * 1024, None).await);
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
