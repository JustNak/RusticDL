//! Single resume classifier for planner, single-stream, multi-start, and reconcile.
//!
//! Classification uses job identity only (`segment_map`, `transfer_format_version`,
//! `downloaded_bytes`). On-disk length is never a classifier.

use super::job::Job;
use super::segment::SegmentMap;

pub(crate) const FALLBACK_LEGACY_PARTIAL: &str = "legacy_contiguous_partial";
pub(crate) const FALLBACK_MAP_MISSING: &str = "map_missing";
pub(crate) const FALLBACK_MAP_INCONSISTENT: &str = "map_inconsistent";

/// How an existing job may be resumed. One function; every caller matches this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeOracle {
    /// v0, no map, no durable partial. Single-stream from offset 0.
    FreshSingle,
    /// v0 contiguous `.part` (`downloaded_bytes > 0`). Stay single until Restart.
    LegacySingle,
    /// Present consistent map (any segment count, including 1). Multi only.
    Multi { map: SegmentMap },
    /// v1 + missing map, or present map that fails `is_consistent`.
    RestartRequired,
}

/// Classify resume from job identity. Never looks at on-disk length.
pub fn resume_oracle(job: &Job) -> ResumeOracle {
    match job.segment_map.as_ref() {
        Some(map) if !map.is_consistent() => ResumeOracle::RestartRequired,
        Some(map) => ResumeOracle::Multi { map: map.clone() },
        None if job.transfer_format_version >= 1 => ResumeOracle::RestartRequired,
        None if job.downloaded_bytes > 0 => ResumeOracle::LegacySingle,
        None => ResumeOracle::FreshSingle,
    }
}

impl ResumeOracle {
    pub fn is_resume_error(&self) -> bool {
        matches!(self, Self::RestartRequired)
    }

    /// Visibility key. `RestartRequired` is split at the publish site via
    /// `job.segment_map.is_none()` (`map_missing` vs `map_inconsistent`).
    pub fn fallback_reason(&self) -> Option<&'static str> {
        match self {
            Self::LegacySingle => Some(FALLBACK_LEGACY_PARTIAL),
            Self::FreshSingle | Self::Multi { .. } | Self::RestartRequired => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::segment::{Segment, SegmentState};
    use std::path::PathBuf;

    fn sample_job() -> Job {
        Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        )
    }

    fn one_seg_map(written: u64) -> SegmentMap {
        SegmentMap {
            total_bytes: 1000,
            segment_count: 1,
            segments: vec![Segment {
                index: 0,
                start: 0,
                end: 999,
                written,
                state: SegmentState::Active,
            }],
            preallocated: true,
        }
    }

    fn two_seg_map() -> SegmentMap {
        SegmentMap {
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![
                Segment {
                    index: 0,
                    start: 0,
                    end: 499,
                    written: 100,
                    state: SegmentState::Active,
                },
                Segment {
                    index: 1,
                    start: 500,
                    end: 999,
                    written: 25,
                    state: SegmentState::Pending,
                },
            ],
            preallocated: true,
        }
    }

    fn inconsistent_map() -> SegmentMap {
        SegmentMap {
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![
                Segment {
                    index: 0,
                    start: 0,
                    end: 499,
                    written: 100,
                    state: SegmentState::Active,
                },
                Segment {
                    index: 1,
                    start: 0,
                    end: 999,
                    written: 25,
                    state: SegmentState::Pending,
                },
            ],
            preallocated: true,
        }
    }

    #[test]
    fn resume_oracle_classifies_fresh_legacy_multi_and_restart() {
        let cases: &[(&str, fn() -> Job, fn(&ResumeOracle) -> bool)] = &[
            (
                "fresh",
                || sample_job(),
                |o| matches!(o, ResumeOracle::FreshSingle),
            ),
            (
                "legacy",
                || {
                    let mut job = sample_job();
                    job.downloaded_bytes = 42;
                    job
                },
                |o| matches!(o, ResumeOracle::LegacySingle),
            ),
            (
                "1-seg map is Multi",
                || {
                    let mut job = sample_job();
                    job.downloaded_bytes = 250;
                    job.segment_map = Some(one_seg_map(250));
                    job
                },
                |o| matches!(o, ResumeOracle::Multi { map } if map.segments.len() == 1),
            ),
            (
                "n-seg Multi",
                || {
                    let mut job = sample_job();
                    job.downloaded_bytes = 125;
                    job.segment_map = Some(two_seg_map());
                    job
                },
                |o| matches!(o, ResumeOracle::Multi { map } if map.segments.len() == 2),
            ),
            (
                "v1 missing map",
                || {
                    let mut job = sample_job();
                    job.downloaded_bytes = 42;
                    job.transfer_format_version = 1;
                    job
                },
                |o| matches!(o, ResumeOracle::RestartRequired),
            ),
            (
                "inconsistent map",
                || {
                    let mut job = sample_job();
                    job.transfer_format_version = 1;
                    job.segment_map = Some(inconsistent_map());
                    job
                },
                |o| matches!(o, ResumeOracle::RestartRequired),
            ),
        ];

        for (name, make_job, check) in cases {
            let oracle = resume_oracle(&make_job());
            assert!(check(&oracle), "{name}: got {oracle:?}");
        }
    }

    #[test]
    fn resume_oracle_one_segment_map_is_multi() {
        let mut job = sample_job();
        job.downloaded_bytes = 42;
        job.segment_map = Some(one_seg_map(250));
        let ResumeOracle::Multi { map } = resume_oracle(&job) else {
            panic!("1-segment consistent map must be Multi, not single-stream");
        };
        assert_eq!(map.segments.len(), 1);
        assert_eq!(map.segments[0].written, 250);
        assert!(!resume_oracle(&job).is_resume_error());
        assert!(resume_oracle(&job).fallback_reason().is_none());
    }

    #[test]
    fn resume_oracle_legacy_fallback_reason() {
        let mut job = sample_job();
        job.downloaded_bytes = 10;
        let oracle = resume_oracle(&job);
        assert_eq!(oracle, ResumeOracle::LegacySingle);
        assert_eq!(oracle.fallback_reason(), Some(FALLBACK_LEGACY_PARTIAL));
        assert!(!oracle.is_resume_error());
    }

    #[test]
    fn resume_oracle_ignores_on_disk_for_fresh() {
        // leftover hole file is still FreshSingle; caller must not promote it.
        let job = sample_job();
        assert_eq!(job.downloaded_bytes, 0);
        assert_eq!(resume_oracle(&job), ResumeOracle::FreshSingle);
    }
}
