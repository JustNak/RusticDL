use std::collections::HashMap;

use super::types::{TerminalEdge, TerminalKind};
use crate::download::{Job, JobState};

/// Diff previous vs next job snapshots for terminal Complete/Failed edges.
///
/// Canceled is intentionally excluded. Intermediate retry states are not terminal.
pub fn terminal_edges(previous: &[Job], next: &[Job]) -> Vec<TerminalEdge> {
    let prev: HashMap<&str, JobState> = previous.iter().map(|j| (j.id.as_str(), j.state)).collect();

    let mut edges = Vec::new();
    for job in next {
        let prev_state = prev.get(job.id.as_str()).copied();
        let was_non_terminal = match prev_state {
            None => true,
            Some(s) => !matches!(
                s,
                JobState::Completed | JobState::Failed | JobState::Canceled
            ),
        };
        if !was_non_terminal {
            continue;
        }
        match job.state {
            JobState::Completed => edges.push(TerminalEdge {
                job_id: job.id.clone(),
                kind: TerminalKind::Complete,
                filename: job.filename.clone(),
                error: None,
                target_path: job.target_path.clone(),
            }),
            JobState::Failed => edges.push(TerminalEdge {
                job_id: job.id.clone(),
                kind: TerminalKind::Fail,
                filename: job.filename.clone(),
                error: job.error.clone(),
                target_path: job.target_path.clone(),
            }),
            _ => {}
        }
    }
    edges
}

#[cfg(test)]
pub(crate) fn test_job(id: &str, state: JobState, name: &str) -> Job {
    use std::path::PathBuf;

    let mut j = Job::new(
        format!("https://example.com/{name}"),
        name.to_string(),
        PathBuf::from(format!("C:/dl/{name}")),
        PathBuf::from(format!("C:/dl/{name}.part")),
    );
    j.id = id.to_string();
    j.state = state;
    if state == JobState::Failed {
        j.error = Some("network error".into());
    }
    j
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::JobState;

    #[test]
    fn terminal_edges_complete_and_fail_only() {
        let prev = vec![
            test_job("a", JobState::Downloading, "a.zip"),
            test_job("b", JobState::Downloading, "b.zip"),
            test_job("c", JobState::Downloading, "c.zip"),
            test_job("d", JobState::Completed, "d.zip"),
        ];
        let next = vec![
            test_job("a", JobState::Completed, "a.zip"),
            test_job("b", JobState::Failed, "b.zip"),
            test_job("c", JobState::Canceled, "c.zip"),
            test_job("d", JobState::Completed, "d.zip"),
        ];
        let edges = terminal_edges(&prev, &next);
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].kind, TerminalKind::Complete);
        assert_eq!(edges[0].job_id, "a");
        assert_eq!(edges[1].kind, TerminalKind::Fail);
        assert_eq!(edges[1].job_id, "b");
    }

    #[test]
    fn canceled_never_in_edges() {
        let prev = vec![test_job("x", JobState::Downloading, "x.bin")];
        let next = vec![test_job("x", JobState::Canceled, "x.bin")];
        assert!(terminal_edges(&prev, &next).is_empty());
    }
}
