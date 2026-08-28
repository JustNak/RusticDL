use super::types::{InAppToastKind, TerminalEdge, TerminalKind};
use crate::settings::OsNotifyMode;

fn in_app_for_kind(mode: OsNotifyMode, kind: TerminalKind) -> Option<InAppToastKind> {
    match (mode, kind) {
        (OsNotifyMode::Always, TerminalKind::Complete) => None,
        (_, TerminalKind::Complete) => Some(InAppToastKind::Info),
        (_, TerminalKind::Fail) => Some(InAppToastKind::Error),
    }
}

/// At most one toast per kind (aggregated when multiple edges share a kind).
pub fn in_app_summary_messages(
    edges: &[TerminalEdge],
    mode: OsNotifyMode,
) -> Vec<(InAppToastKind, String)> {
    let mut completes: Vec<&TerminalEdge> = Vec::new();
    let mut fails: Vec<&TerminalEdge> = Vec::new();
    for e in edges {
        match e.kind {
            TerminalKind::Complete => {
                if in_app_for_kind(mode, TerminalKind::Complete).is_some() {
                    completes.push(e);
                }
            }
            TerminalKind::Fail => {
                if in_app_for_kind(mode, TerminalKind::Fail).is_some() {
                    fails.push(e);
                }
            }
        }
    }

    let mut out = Vec::new();
    if !completes.is_empty() {
        let message = if completes.len() == 1 {
            format!("Download complete: {}", completes[0].filename)
        } else {
            format!("{} downloads finished", completes.len())
        };
        out.push((InAppToastKind::Info, message));
    }
    if !fails.is_empty() {
        let message = if fails.len() == 1 {
            match &fails[0].error {
                Some(err) if !err.is_empty() => {
                    format!("Download failed: {} — {}", fails[0].filename, err)
                }
                _ => format!("Download failed: {}", fails[0].filename),
            }
        } else {
            format!("{} downloads failed", fails.len())
        };
        out.push((InAppToastKind::Error, message));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn in_app_matrix_visible() {
        assert_eq!(
            in_app_for_kind(OsNotifyMode::WhenHiddenToTray, TerminalKind::Complete),
            Some(InAppToastKind::Info)
        );
        assert_eq!(
            in_app_for_kind(OsNotifyMode::WhenHiddenToTray, TerminalKind::Fail),
            Some(InAppToastKind::Error)
        );
        assert_eq!(
            in_app_for_kind(OsNotifyMode::Always, TerminalKind::Complete),
            None
        );
        assert_eq!(
            in_app_for_kind(OsNotifyMode::Always, TerminalKind::Fail),
            Some(InAppToastKind::Error)
        );
        assert_eq!(
            in_app_for_kind(OsNotifyMode::Off, TerminalKind::Complete),
            Some(InAppToastKind::Info)
        );
        assert_eq!(
            in_app_for_kind(OsNotifyMode::Off, TerminalKind::Fail),
            Some(InAppToastKind::Error)
        );
    }

    #[test]
    fn in_app_aggregates_multi() {
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
                kind: TerminalKind::Complete,
                filename: "b".into(),
                error: None,
                target_path: PathBuf::from("b"),
            },
            TerminalEdge {
                job_id: "3".into(),
                kind: TerminalKind::Fail,
                filename: "c".into(),
                error: Some("x".into()),
                target_path: PathBuf::from("c"),
            },
        ];
        let toasts = in_app_summary_messages(&edges, OsNotifyMode::WhenHiddenToTray);
        assert_eq!(toasts.len(), 2);
        assert_eq!(toasts[0].1, "2 downloads finished");
        assert_eq!(toasts[1].1, "Download failed: c — x");
    }

    #[test]
    fn always_mode_skips_success_in_app() {
        let edges = vec![TerminalEdge {
            job_id: "1".into(),
            kind: TerminalKind::Complete,
            filename: "a".into(),
            error: None,
            target_path: PathBuf::from("a"),
        }];
        let toasts = in_app_summary_messages(&edges, OsNotifyMode::Always);
        assert!(toasts.is_empty());
    }
}
