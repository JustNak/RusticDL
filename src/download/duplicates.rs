
use super::job::Job;

pub fn find_active_duplicate<'a>(jobs: &'a [Job], url: &str) -> Option<&'a Job> {
    jobs.iter()
        .find(|job| job.url == url && job.state.is_active())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::job::{Job, JobState};
    use std::path::PathBuf;

    fn job_with(url: &str, state: JobState) -> Job {
        let mut job = Job::new(
            url.to_string(),
            "file.bin".into(),
            PathBuf::from("C:\\dl\\file.bin"),
            PathBuf::from("C:\\dl\\file.bin.part"),
        );
        job.state = state;
        job
    }

    #[test]
    fn finds_active_duplicate_for_queued_starting_downloading_paused() {
        for state in [
            JobState::Queued,
            JobState::Starting,
            JobState::Downloading,
            JobState::Paused,
        ] {
            let jobs = vec![job_with("https://example.com/a", state)];
            let found = find_active_duplicate(&jobs, "https://example.com/a");
            assert!(
                found.is_some(),
                "expected active duplicate for state {state:?}"
            );
            assert_eq!(found.unwrap().state, state);
        }
    }

    #[test]
    fn paused_counts_as_active_and_blocks_same_request_url() {
        let jobs = vec![job_with("https://cdn.example/file.zip", JobState::Paused)];
        assert!(find_active_duplicate(&jobs, "https://cdn.example/file.zip").is_some());
    }

    #[test]
    fn completed_failed_canceled_allow_redownload_same_url() {
        for state in [JobState::Completed, JobState::Failed, JobState::Canceled] {
            let jobs = vec![job_with("https://example.com/a", state)];
            assert!(
                find_active_duplicate(&jobs, "https://example.com/a").is_none(),
                "terminal state {state:?} must not block re-add"
            );
        }
    }

    #[test]
    fn exact_string_match_only_different_query_or_scheme_not_dup() {
        let jobs = vec![job_with(
            "https://example.com/a?token=1",
            JobState::Downloading,
        )];
        assert!(find_active_duplicate(&jobs, "https://example.com/a?token=2").is_none());
        assert!(find_active_duplicate(&jobs, "http://example.com/a?token=1").is_none());
        assert!(find_active_duplicate(&jobs, "https://example.com/a?token=1").is_some());
    }

    #[test]
    fn request_url_only_redirect_final_url_is_not_compared() {
        let jobs = vec![job_with("https://short.example/abc", JobState::Downloading)];
        assert!(
            find_active_duplicate(&jobs, "https://cdn.example/real/file.bin").is_none(),
            "final/redirect target URL must not match against original request job.url"
        );
        assert!(
            find_active_duplicate(&jobs, "https://short.example/abc").is_some(),
            "exact original request URL still dedupes"
        );
    }

    #[test]
    fn returns_first_matching_active_job() {
        let mut a = job_with("https://example.com/x", JobState::Completed);
        a.id = "completed-old".into();
        let mut b = job_with("https://example.com/x", JobState::Queued);
        b.id = "active-first".into();
        let mut c = job_with("https://example.com/x", JobState::Paused);
        c.id = "active-second".into();
        let jobs = vec![a, b, c];
        let found = find_active_duplicate(&jobs, "https://example.com/x").unwrap();
        assert_eq!(found.id, "active-first");
    }

    #[test]
    fn no_match_when_url_absent() {
        let jobs = vec![job_with("https://example.com/a", JobState::Downloading)];
        assert!(find_active_duplicate(&jobs, "https://example.com/b").is_none());
        assert!(find_active_duplicate(&[], "https://example.com/a").is_none());
    }
}
