use std::time::Instant;

use super::types::{PendingOsTerminal, OS_BURST_WINDOW, OS_HIGH_WATER};

/// OS-only coalesce buffer (Pipeline B). In-app toasts never touch this.
#[derive(Debug, Default)]
pub struct OsNotifyBuffer {
    pub pending: Vec<PendingOsTerminal>,
    pub coalesce_deadline: Option<Instant>,
    pub burst_open_until: Option<Instant>,
}

impl OsNotifyBuffer {
    fn is_burst_open(&self, now: Instant) -> bool {
        self.burst_open_until
            .map(|until| now < until)
            .unwrap_or(false)
    }

    /// Append soft-eligible edges. Returns `true` if the caller should flush now.
    pub fn enqueue(&mut self, edges: Vec<PendingOsTerminal>, now: Instant) -> bool {
        if edges.is_empty() {
            return false;
        }
        self.pending.extend(edges);

        if self.pending.len() >= OS_HIGH_WATER {
            return true;
        }

        let burst_open = self.is_burst_open(now);

        if self.pending.len() == 1 && !burst_open {
            return true;
        }

        if self.coalesce_deadline.is_none() {
            self.coalesce_deadline = Some(if burst_open {
                self.burst_open_until
                    .unwrap_or_else(|| now + OS_BURST_WINDOW)
            } else {
                now + OS_BURST_WINDOW
            });
        }
        false
    }

    /// True when a previously armed deadline has elapsed and items remain.
    pub fn deadline_elapsed(&self, now: Instant) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        if self.pending.len() >= OS_HIGH_WATER {
            return true;
        }
        match self.coalesce_deadline {
            Some(deadline) => now >= deadline,
            None => true, // pending without deadline → flush-safe
        }
    }

    /// Drain pending items (clear deadline only; does not arm the burst window).
    ///
    /// Call [`after_flush`] only after a balloon was actually shown.
    pub fn take_pending(&mut self) -> Vec<PendingOsTerminal> {
        self.coalesce_deadline = None;
        std::mem::take(&mut self.pending)
    }

    pub fn after_flush(&mut self, now: Instant) {
        self.coalesce_deadline = None;
        self.burst_open_until = Some(now + OS_BURST_WINDOW);
    }

    /// Fully reset pending, deadline, and burst window (e.g. mode turned Off).
    pub fn clear(&mut self) {
        self.pending.clear();
        self.coalesce_deadline = None;
        self.burst_open_until = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::{compose_balloon, BalloonOutcome, TerminalKind};
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn solitary_edge_flushes_immediately() {
        let mut buf = OsNotifyBuffer::default();
        let now = Instant::now();
        let edge = PendingOsTerminal {
            kind: TerminalKind::Complete,
            filename: "solo.zip".into(),
            error: None,
            job_id: "1".into(),
            target_path: Some(PathBuf::from("C:/dl/solo.zip")),
        };
        assert!(buf.enqueue(vec![edge], now));
    }

    #[test]
    fn multi_edge_same_apply_waits_then_coalesces() {
        let mut buf = OsNotifyBuffer::default();
        let now = Instant::now();
        let edges = vec![
            PendingOsTerminal {
                kind: TerminalKind::Complete,
                filename: "a.zip".into(),
                error: None,
                job_id: "1".into(),
                target_path: Some(PathBuf::from("a")),
            },
            PendingOsTerminal {
                kind: TerminalKind::Complete,
                filename: "b.zip".into(),
                error: None,
                job_id: "2".into(),
                target_path: Some(PathBuf::from("b")),
            },
        ];
        assert!(!buf.enqueue(edges, now));
        assert_eq!(buf.pending.len(), 2);
        assert!(buf.coalesce_deadline.is_some());

        let later = now + OS_BURST_WINDOW + Duration::from_millis(1);
        assert!(buf.deadline_elapsed(later));
        let taken = buf.take_pending();
        buf.after_flush(later);
        let balloon = compose_balloon(&taken).unwrap();
        assert_eq!(balloon.title, "Downloads complete");
        assert_eq!(balloon.body, "2 downloads finished");
        assert_eq!(balloon.kind, BalloonOutcome::Coalesced);
    }

    #[test]
    fn burst_window_holds_next_solitary() {
        let mut buf = OsNotifyBuffer::default();
        let t0 = Instant::now();
        let edge = PendingOsTerminal {
            kind: TerminalKind::Complete,
            filename: "a.zip".into(),
            error: None,
            job_id: "1".into(),
            target_path: Some(PathBuf::from("a")),
        };
        assert!(buf.enqueue(vec![edge], t0));
        let _ = buf.take_pending();
        buf.after_flush(t0);
        assert!(buf.is_burst_open(t0 + Duration::from_millis(100)));

        let edge2 = PendingOsTerminal {
            kind: TerminalKind::Fail,
            filename: "b.zip".into(),
            error: Some("x".into()),
            job_id: "2".into(),
            target_path: None,
        };
        let t1 = t0 + Duration::from_millis(200);
        assert!(!buf.enqueue(vec![edge2], t1));
    }

    #[test]
    fn high_water_flushes() {
        let mut buf = OsNotifyBuffer::default();
        let now = Instant::now();
        buf.burst_open_until = Some(now + OS_BURST_WINDOW);
        let mut edges = Vec::new();
        for i in 0..OS_HIGH_WATER {
            edges.push(PendingOsTerminal {
                kind: TerminalKind::Complete,
                filename: format!("{i}.bin"),
                error: None,
                job_id: format!("{i}"),
                target_path: Some(PathBuf::from(format!("{i}"))),
            });
        }
        assert!(buf.enqueue(edges, now));
    }

    #[test]
    fn take_pending_without_after_flush_does_not_arm_burst() {
        let mut buf = OsNotifyBuffer::default();
        let now = Instant::now();
        let edge = PendingOsTerminal {
            kind: TerminalKind::Complete,
            filename: "solo.zip".into(),
            error: None,
            job_id: "1".into(),
            target_path: Some(PathBuf::from("solo")),
        };
        assert!(buf.enqueue(vec![edge], now));
        let _ = buf.take_pending();
        assert!(!buf.is_burst_open(now + Duration::from_millis(50)));
        let next = PendingOsTerminal {
            kind: TerminalKind::Complete,
            filename: "two.zip".into(),
            error: None,
            job_id: "2".into(),
            target_path: Some(PathBuf::from("two")),
        };
        assert!(buf.enqueue(vec![next], now + Duration::from_millis(100)));
    }

    #[test]
    fn clear_resets_burst_window() {
        let mut buf = OsNotifyBuffer::default();
        let now = Instant::now();
        buf.after_flush(now);
        assert!(buf.is_burst_open(now + Duration::from_millis(10)));
        buf.pending.push(PendingOsTerminal {
            kind: TerminalKind::Complete,
            filename: "x".into(),
            error: None,
            job_id: "1".into(),
            target_path: None,
        });
        buf.coalesce_deadline = Some(now + OS_BURST_WINDOW);
        buf.clear();
        assert!(buf.pending.is_empty());
        assert!(buf.coalesce_deadline.is_none());
        assert!(!buf.is_burst_open(now + Duration::from_millis(10)));
    }
}
