//! Stable remaining-time estimates from noisy 400ms throughput windows.
//!
//! Instantaneous `remaining / window_speed` jumps whenever TCP, disk, or a
//! multi-segment burst moves the last measurement. The UI should count down
//! smoothly and only revise upward when the new estimate is clearly worse.

/// Exponential moving average of throughput plus hysteresis on the shown ETA.
#[derive(Debug, Clone)]
pub struct EtaSmoother {
    ema_bps: Option<f64>,
    shown_eta: Option<u64>,
}

impl Default for EtaSmoother {
    fn default() -> Self {
        Self::new()
    }
}

impl EtaSmoother {
    /// Blend toward each new 400ms sample. 0.22 ≈ 1.5s time constant.
    const ALPHA: f64 = 0.22;
    /// Ease the displayed ETA toward the raw estimate (both directions).
    const DISPLAY_ALPHA: f64 = 0.28;
    /// Only jump the shown ETA up when the raw estimate is this much worse.
    const RAISE_RATIO: f64 = 1.35;
    const RAISE_FLOOR_SECS: u64 = 8;

    pub fn new() -> Self {
        Self {
            ema_bps: None,
            shown_eta: None,
        }
    }

    /// Feed one measurement window.
    ///
    /// Returns `(smoothed_speed_bps, stable_eta_secs)`. Speed `0` and unknown
    /// remaining (`remaining == 0` with no total) yield ETA `0` (UI "—").
    pub fn observe(&mut self, instant_bps: u64, remaining: u64) -> (u64, u64) {
        let speed = self.push_speed(instant_bps);
        if remaining == 0 || speed == 0 {
            if remaining == 0 {
                self.shown_eta = Some(0);
            }
            return (speed, self.shown_eta.unwrap_or(0));
        }
        let raw = remaining / speed;
        let eta = self.stabilize(raw);
        (speed, eta)
    }

    /// Last EMA throughput, if any sample has been accepted.
    pub fn last_speed(&self) -> Option<u64> {
        self.ema_bps.map(|s| s.round() as u64)
    }

    /// Last stabilized ETA, if any.
    pub fn last_eta(&self) -> Option<u64> {
        self.shown_eta
    }

    fn push_speed(&mut self, instant_bps: u64) -> u64 {
        if instant_bps == 0 {
            return self.ema_bps.map(|s| s.round() as u64).unwrap_or(0);
        }
        let sample = instant_bps as f64;
        let ema = match self.ema_bps {
            None => sample,
            Some(prev) => Self::ALPHA * sample + (1.0 - Self::ALPHA) * prev,
        };
        self.ema_bps = Some(ema);
        ema.round().max(1.0) as u64
    }

    fn stabilize(&mut self, raw: u64) -> u64 {
        let shown = match self.shown_eta {
            None | Some(0) => raw,
            Some(prev) => {
                if raw > prev
                    && raw <= prev.saturating_add(Self::RAISE_FLOOR_SECS)
                    && (raw as f64) <= (prev as f64) * Self::RAISE_RATIO
                {
                    prev
                } else {
                    let blended = (prev as f64) * (1.0 - Self::DISPLAY_ALPHA)
                        + (raw as f64) * Self::DISPLAY_ALPHA;
                    blended.round() as u64
                }
            }
        };
        self.shown_eta = Some(shown);
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_eta_secs(speed_bps: u64, remaining: u64) -> u64 {
        if speed_bps == 0 || remaining == 0 {
            0
        } else {
            remaining / speed_bps
        }
    }

    #[test]
    fn first_sample_uses_raw_estimate() {
        let mut s = EtaSmoother::new();
        let (speed, eta) = s.observe(1_000, 10_000);
        assert_eq!(speed, 1_000);
        assert_eq!(eta, 10);
    }

    #[test]
    fn zero_speed_without_history_is_unknown() {
        let mut s = EtaSmoother::new();
        assert_eq!(s.observe(0, 10_000), (0, 0));
    }

    #[test]
    fn remaining_zero_is_done() {
        let mut s = EtaSmoother::new();
        s.observe(1_000, 10_000);
        let (_, eta) = s.observe(1_000, 0);
        assert_eq!(eta, 0);
    }

    #[test]
    fn single_empty_window_does_not_collapse_ema() {
        let mut s = EtaSmoother::new();
        s.observe(2_000, 20_000);
        let (speed, eta) = s.observe(0, 18_000);
        assert!(speed > 0, "EMA should survive one empty window");
        assert!(eta > 0);
    }

    #[test]
    fn oscillating_speed_does_not_track_raw_spikes() {
        let mut s = EtaSmoother::new();
        let remaining = 100_000_000u64; // 100 MB
        let mut last = 0u64;
        for speed in [10_000_000u64, 2_000_000, 12_000_000, 1_500_000, 11_000_000] {
            let (_, eta) = s.observe(speed, remaining);
            last = eta;
        }
        let raw_low = raw_eta_secs(1_500_000, remaining);
        let raw_high = raw_eta_secs(12_000_000, remaining);
        // Smoothed ETA must sit strictly between the worst and best raw spikes.
        assert!(
            last > raw_high && last < raw_low,
            "smoothed {last} should be between raw high {raw_high} and raw low {raw_low}"
        );
    }

    #[test]
    fn small_upward_noise_does_not_raise_display() {
        let mut s = EtaSmoother::new();
        let (_, first) = s.observe(10_000_000, 100_000_000);
        assert_eq!(first, 10);
        // Instant 2_424_241 bps → EMA 8_333_333; 100_000_000 / 8_333_333 == 12.
        // 12 > 10 but inside RAISE_FLOOR_SECS (8) and RAISE_RATIO (1.35).
        let (_, second) = s.observe(2_424_241, 100_000_000);
        assert_eq!(
            second, first,
            "sub-threshold raise should hold the previous ETA"
        );
    }

    #[test]
    fn large_slowdown_does_raise_display() {
        let mut s = EtaSmoother::new();
        s.observe(10_000_000, 100_000_000); // 10s
                                            // Several ticks at 2 MB/s so EMA and raise hysteresis both move.
        let mut eta = 0;
        for _ in 0..8 {
            eta = s.observe(2_000_000, 90_000_000).1;
        }
        assert!(eta > 15, "sustained slowdown should lift ETA, got {eta}");
    }
}
