//! Multi-segment map types and even-split partition (orchestrator lands in PR 11).

use serde::{Deserialize, Serialize};

/// Settings default when `multi_max_segments` is not specified.
pub const DEFAULT_SEGMENT_COUNT: u32 = 8;
/// Partition floor: never emit a segment smaller than 1 MiB unless the file itself is.
pub const MIN_SEGMENT_SIZE: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentState {
    #[default]
    Pending,
    Active,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub index: u32,
    /// Inclusive byte offset.
    pub start: u64,
    /// Inclusive byte offset.
    pub end: u64,
    /// Bytes successfully written inside `[start, end]`.
    #[serde(default)]
    pub written: u64,
    #[serde(default)]
    pub state: SegmentState,
}

impl Segment {
    pub fn length(&self) -> u64 {
        self.end.saturating_sub(self.start).saturating_add(1)
    }

    /// Next Range start for this segment (`start + written`).
    pub fn remaining_start(&self) -> u64 {
        self.start.saturating_add(self.written)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentMap {
    pub total_bytes: u64,
    pub segment_count: u32,
    pub segments: Vec<Segment>,
    /// True after a successful `set_len(total)` preallocate (progress must ignore file len).
    #[serde(default)]
    pub preallocated: bool,
}

impl SegmentMap {
    pub fn written_sum(&self) -> u64 {
        self.segments.iter().map(|segment| segment.written).sum()
    }

    /// Bounds / segment `state` / `preallocated` — ignores `written` (§2.8 accepted lag).
    pub fn structure_eq(&self, other: &Self) -> bool {
        self.total_bytes == other.total_bytes
            && self.segment_count == other.segment_count
            && self.preallocated == other.preallocated
            && self.segments.len() == other.segments.len()
            && self.segments.iter().zip(&other.segments).all(|(a, b)| {
                a.index == b.index && a.start == b.start && a.end == b.end && a.state == b.state
            })
    }

    /// Contiguous coverage of `total_bytes`, no gaps/overlaps, written within bounds.
    pub fn is_consistent(&self) -> bool {
        if self.segment_count as usize != self.segments.len() {
            return false;
        }
        if self.total_bytes == 0 {
            return self.segments.is_empty();
        }
        if self.segments.is_empty() {
            return false;
        }
        let mut next = 0u64;
        for (i, segment) in self.segments.iter().enumerate() {
            if segment.index != i as u32 || segment.start != next || segment.end < segment.start {
                return false;
            }
            if segment.written > segment.length() {
                return false;
            }
            next = segment.end.saturating_add(1);
        }
        next == self.total_bytes
    }
}

/// Even-split `total_bytes` into at most `n` segments (default 8).
///
/// Lengths differ by at most 1 byte; no gaps or overlaps. Files smaller than
/// `n * MIN_SEGMENT_SIZE` get fewer segments so each is ≥ 1 MiB (or the whole file).
pub fn partition(total_bytes: u64, n: u32) -> SegmentMap {
    if total_bytes == 0 {
        return SegmentMap {
            total_bytes: 0,
            segment_count: 0,
            segments: Vec::new(),
            preallocated: false,
        };
    }

    let requested = n.max(1) as u64;
    let max_by_size = (total_bytes / MIN_SEGMENT_SIZE).max(1);
    let count = requested.min(max_by_size);

    let base = total_bytes / count;
    let remainder = total_bytes % count;
    let mut offset = 0u64;
    let mut segments = Vec::with_capacity(count as usize);
    for index in 0..count {
        let len = if index < remainder { base + 1 } else { base };
        let start = offset;
        let end = offset + len - 1;
        segments.push(Segment {
            index: index as u32,
            start,
            end,
            written: 0,
            state: SegmentState::Pending,
        });
        offset += len;
    }

    SegmentMap {
        total_bytes,
        segment_count: count as u32,
        segments,
        preallocated: false,
    }
}

/// Partition with the settings default of 8 segments.
#[allow(dead_code)]
pub fn partition_default(total_bytes: u64) -> SegmentMap {
    partition(total_bytes, DEFAULT_SEGMENT_COUNT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_contiguous(map: &SegmentMap) {
        assert!(map.is_consistent(), "map must be consistent: {map:?}");
        assert!(!map.preallocated);
        if map.total_bytes == 0 {
            assert!(map.segments.is_empty());
            return;
        }
        let mut lengths = Vec::new();
        for segment in &map.segments {
            assert_eq!(segment.written, 0);
            assert_eq!(segment.state, SegmentState::Pending);
            lengths.push(segment.length());
        }
        if let (Some(&min), Some(&max)) = (lengths.iter().min(), lengths.iter().max()) {
            assert!(max - min <= 1, "lengths differ by more than 1: {lengths:?}");
        }
        let covered: u64 = lengths.iter().sum();
        assert_eq!(covered, map.total_bytes);
    }

    #[test]
    fn partition_default_is_eight() {
        let total = 8 * MIN_SEGMENT_SIZE;
        let map = partition_default(total);
        assert_eq!(map.segment_count, DEFAULT_SEGMENT_COUNT);
        assert_eq!(map.segments.len(), 8);
        assert_contiguous(&map);
        for segment in &map.segments {
            assert_eq!(segment.length(), MIN_SEGMENT_SIZE);
        }
    }

    #[test]
    fn partition_even_split_no_gaps() {
        let total = 8 * MIN_SEGMENT_SIZE + 3;
        let map = partition(total, 8);
        assert_eq!(map.segment_count, 8);
        assert_contiguous(&map);
        // First 3 get the extra byte.
        assert_eq!(map.segments[0].length(), MIN_SEGMENT_SIZE + 1);
        assert_eq!(map.segments[2].length(), MIN_SEGMENT_SIZE + 1);
        assert_eq!(map.segments[3].length(), MIN_SEGMENT_SIZE);
        assert_eq!(map.segments[0].start, 0);
        assert_eq!(map.segments[7].end, total - 1);
    }

    #[test]
    fn partition_clamps_to_min_segment_size() {
        let map = partition(4 * MIN_SEGMENT_SIZE, 8);
        assert_eq!(map.segment_count, 4);
        assert_contiguous(&map);
        for segment in &map.segments {
            assert_eq!(segment.length(), MIN_SEGMENT_SIZE);
        }
    }

    #[test]
    fn partition_small_file_is_single_segment() {
        let map = partition(10, 8);
        assert_eq!(map.segment_count, 1);
        assert_eq!(map.segments[0].start, 0);
        assert_eq!(map.segments[0].end, 9);
        assert_contiguous(&map);
    }

    #[test]
    fn partition_zero_bytes_empty_map() {
        let map = partition(0, 8);
        assert_eq!(map.segment_count, 0);
        assert!(map.segments.is_empty());
        assert!(map.is_consistent());
    }

    #[test]
    fn partition_n_zero_treated_as_one() {
        let map = partition(MIN_SEGMENT_SIZE * 2, 0);
        assert_eq!(map.segment_count, 1);
        assert_contiguous(&map);
    }

    #[test]
    fn written_sum_and_inconsistent_bounds() {
        let mut map = partition(2 * MIN_SEGMENT_SIZE, 2);
        map.segments[0].written = 10;
        map.segments[1].written = 20;
        assert_eq!(map.written_sum(), 30);
        assert!(map.is_consistent());

        map.segments[1].start = 0;
        assert!(!map.is_consistent());
    }

    #[test]
    fn structure_eq_ignores_written() {
        let mut a = partition(2 * MIN_SEGMENT_SIZE, 2);
        let mut b = a.clone();
        a.segments[0].written = 10;
        b.segments[0].written = 99;
        assert!(a.structure_eq(&b));
        b.segments[0].state = SegmentState::Completed;
        assert!(!a.structure_eq(&b));
        b.segments[0].state = a.segments[0].state;
        b.preallocated = true;
        assert!(!a.structure_eq(&b));
    }

    #[test]
    fn segment_serde_defaults_written_and_state() {
        let json = r#"{
            "index": 0,
            "start": 0,
            "end": 99
        }"#;
        let segment: Segment = serde_json::from_str(json).expect("defaults");
        assert_eq!(segment.written, 0);
        assert_eq!(segment.state, SegmentState::Pending);
        assert_eq!(segment.remaining_start(), 0);
    }
}
