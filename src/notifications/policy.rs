use super::types::{PendingOsTerminal, TerminalEdge, TerminalKind};
use crate::settings::OsNotifyMode;

/// Filter edges by user notify toggles (applies to both pipelines).
pub fn filter_notify_edges(
    edges: &[TerminalEdge],
    notify_on_complete: bool,
    notify_on_fail: bool,
) -> Vec<TerminalEdge> {
    edges
        .iter()
        .filter(|e| match e.kind {
            TerminalKind::Complete => notify_on_complete,
            TerminalKind::Fail => notify_on_fail,
        })
        .cloned()
        .collect()
}

/// Soft OS eligibility at enqueue (mode not Off). Hard check is at flush.
pub fn soft_os_eligible(mode: OsNotifyMode) -> bool {
    mode != OsNotifyMode::Off
}

/// Hard OS eligibility re-checked at flush time.
pub fn hard_os_eligible(mode: OsNotifyMode, window_hidden_to_tray: bool) -> bool {
    match mode {
        OsNotifyMode::Off => false,
        OsNotifyMode::WhenHiddenToTray => window_hidden_to_tray,
        OsNotifyMode::Always => true,
    }
}

/// Re-filter pending OS items by current notify toggles (at flush).
pub fn filter_pending_by_toggles(
    pending: Vec<PendingOsTerminal>,
    notify_on_complete: bool,
    notify_on_fail: bool,
) -> Vec<PendingOsTerminal> {
    pending
        .into_iter()
        .filter(|p| match p.kind {
            TerminalKind::Complete => notify_on_complete,
            TerminalKind::Fail => notify_on_fail,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::compose_balloon;
    use crate::notifications::BalloonOutcome;
    use std::path::PathBuf;

    #[test]
    fn filter_notify_prefs() {
        let edges = vec![
            TerminalEdge {
                job_id: "1".into(),
                kind: TerminalKind::Complete,
                filename: "a".into(),
                error: None,
                target_path: PathBuf::from("a"),
            },
            TerminalEdge {
                job_id: "2".into(),
                kind: TerminalKind::Fail,
                filename: "b".into(),
                error: Some("err".into()),
                target_path: PathBuf::from("b"),
            },
        ];
        assert_eq!(filter_notify_edges(&edges, true, true).len(), 2);
        assert_eq!(filter_notify_edges(&edges, true, false).len(), 1);
        assert_eq!(filter_notify_edges(&edges, false, true).len(), 1);
        assert!(filter_notify_edges(&edges, false, false).is_empty());
    }

    #[test]
    fn hard_eligibility() {
        assert!(!hard_os_eligible(OsNotifyMode::Off, true));
        assert!(!hard_os_eligible(OsNotifyMode::WhenHiddenToTray, false));
        assert!(hard_os_eligible(OsNotifyMode::WhenHiddenToTray, true));
        assert!(hard_os_eligible(OsNotifyMode::Always, false));
        assert!(hard_os_eligible(OsNotifyMode::Always, true));
    }

    #[test]
    fn filter_pending_by_toggles_at_flush() {
        let pending = vec![
            PendingOsTerminal {
                kind: TerminalKind::Complete,
                filename: "a.zip".into(),
                error: None,
                job_id: "1".into(),
                target_path: Some(PathBuf::from("a")),
            },
            PendingOsTerminal {
                kind: TerminalKind::Fail,
                filename: "b.zip".into(),
                error: Some("err".into()),
                job_id: "2".into(),
                target_path: None,
            },
        ];
        let only_complete = filter_pending_by_toggles(pending.clone(), true, false);
        assert_eq!(only_complete.len(), 1);
        assert_eq!(only_complete[0].kind, TerminalKind::Complete);
        let balloon = compose_balloon(&only_complete).unwrap();
        assert_eq!(balloon.kind, BalloonOutcome::SingleComplete);

        let only_fail = filter_pending_by_toggles(pending.clone(), false, true);
        assert_eq!(only_fail.len(), 1);
        assert_eq!(only_fail[0].kind, TerminalKind::Fail);

        assert!(filter_pending_by_toggles(pending, false, false).is_empty());
    }

    #[test]
    fn soft_os_mode() {
        assert!(soft_os_eligible(OsNotifyMode::Always));
        assert!(soft_os_eligible(OsNotifyMode::WhenHiddenToTray));
        assert!(!soft_os_eligible(OsNotifyMode::Off));
    }

    #[test]
    fn visible_window_drops_os_when_hidden_mode() {
        assert!(!hard_os_eligible(OsNotifyMode::WhenHiddenToTray, false));
    }
}
