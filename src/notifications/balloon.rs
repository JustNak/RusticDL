use std::collections::VecDeque;

use super::types::{
    BalloonClickContext, BalloonOutcome, BalloonPayload, PendingOsTerminal, TerminalKind,
    BALLOON_CONTEXT_CAP,
};
use crate::tray::NotifyLevel;

/// Ring buffer of recent balloon click contexts.
#[derive(Debug, Default)]
pub struct BalloonContextMap {
    next_id: u64,
    pub contexts: VecDeque<BalloonClickContext>,
}

impl BalloonContextMap {
    /// Allocate a context id, store payload fields, return the id for the tray.
    pub fn allocate(&mut self, payload: &BalloonPayload) -> u64 {
        let context_id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.contexts.push_back(BalloonClickContext {
            context_id,
            kind: payload.kind,
            job_id: payload.job_id.clone(),
            target_path: payload.target_path.clone(),
        });
        while self.contexts.len() > BALLOON_CONTEXT_CAP {
            self.contexts.pop_front();
        }
        context_id
    }

    pub fn lookup(&self, context_id: u64) -> Option<&BalloonClickContext> {
        self.contexts.iter().find(|c| c.context_id == context_id)
    }
}

/// Compose a single OS balloon from a non-empty pending buffer.
pub fn compose_balloon(pending: &[PendingOsTerminal]) -> Option<BalloonPayload> {
    if pending.is_empty() {
        return None;
    }

    let completes: Vec<&PendingOsTerminal> = pending
        .iter()
        .filter(|p| p.kind == TerminalKind::Complete)
        .collect();
    let fails: Vec<&PendingOsTerminal> = pending
        .iter()
        .filter(|p| p.kind == TerminalKind::Fail)
        .collect();
    let c = completes.len();
    let f = fails.len();

    let (title, body, level, kind, job_id, target_path) = if c == 1 && f == 0 {
        let item = completes[0];
        (
            "Download complete".to_string(),
            item.filename.clone(),
            NotifyLevel::Info,
            BalloonOutcome::SingleComplete,
            Some(item.job_id.clone()),
            item.target_path.clone(),
        )
    } else if c == 0 && f == 1 {
        let item = fails[0];
        let body = match &item.error {
            Some(err) if !err.is_empty() => format!("{} — {}", item.filename, err),
            _ => item.filename.clone(),
        };
        (
            "Download failed".to_string(),
            body,
            NotifyLevel::Error,
            BalloonOutcome::SingleFail,
            Some(item.job_id.clone()),
            None,
        )
    } else if c >= 1 && f == 0 {
        (
            "Downloads complete".to_string(),
            format!("{c} downloads finished"),
            NotifyLevel::Info,
            BalloonOutcome::Coalesced,
            None,
            None,
        )
    } else if c == 0 && f >= 1 {
        (
            "Downloads failed".to_string(),
            format!("{f} downloads failed"),
            NotifyLevel::Error,
            BalloonOutcome::Coalesced,
            None,
            None,
        )
    } else {
        // Mixed completes + fails → single combined balloon.
        (
            "Downloads finished".to_string(),
            format!("{c} finished, {f} failed"),
            NotifyLevel::Info,
            BalloonOutcome::Coalesced,
            None,
            None,
        )
    };

    Some(BalloonPayload {
        title,
        body,
        level,
        kind,
        job_id,
        target_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn compose_mixed_balloon() {
        let pending = vec![
            PendingOsTerminal {
                kind: TerminalKind::Complete,
                filename: "a".into(),
                error: None,
                job_id: "1".into(),
                target_path: Some(PathBuf::from("a")),
            },
            PendingOsTerminal {
                kind: TerminalKind::Fail,
                filename: "b".into(),
                error: Some("e".into()),
                job_id: "2".into(),
                target_path: None,
            },
        ];
        let b = compose_balloon(&pending).unwrap();
        assert_eq!(b.title, "Downloads finished");
        assert_eq!(b.body, "1 finished, 1 failed");
        assert_eq!(b.level, NotifyLevel::Info);
        assert_eq!(b.kind, BalloonOutcome::Coalesced);
    }

    #[test]
    fn compose_single_complete_open_path() {
        let pending = vec![PendingOsTerminal {
            kind: TerminalKind::Complete,
            filename: "file.zip".into(),
            error: None,
            job_id: "j1".into(),
            target_path: Some(PathBuf::from("C:/dl/file.zip")),
        }];
        let b = compose_balloon(&pending).unwrap();
        assert_eq!(b.kind, BalloonOutcome::SingleComplete);
        assert_eq!(
            b.target_path.as_deref(),
            Some(std::path::Path::new("C:/dl/file.zip"))
        );
        assert_eq!(b.job_id.as_deref(), Some("j1"));
    }

    #[test]
    fn balloon_context_map_caps_at_8() {
        let mut map = BalloonContextMap::default();
        for i in 0..12 {
            let payload = BalloonPayload {
                title: "t".into(),
                body: "b".into(),
                level: NotifyLevel::Info,
                kind: BalloonOutcome::Coalesced,
                job_id: None,
                target_path: None,
            };
            let id = map.allocate(&payload);
            assert_eq!(id, i);
        }
        assert_eq!(map.contexts.len(), BALLOON_CONTEXT_CAP);
        assert!(map.lookup(11).is_some());
        assert!(map.lookup(0).is_none());
    }
}
