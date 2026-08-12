//! Positioned concurrent writes into a single multi-segment `.part` file.
//!
//! Network work is parallel; disk writes are intentionally serialized under a
//! short `std::sync::Mutex` around `seek_write` (Windows) / seek+write (other).
//!
//! Consumed by multi-segment transfer (later PR); keep public API live.

#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::Mutex;

#[cfg(not(windows))]
use std::io::{Seek, SeekFrom, Write};
#[cfg(windows)]
use std::os::windows::fs::FileExt;

/// Shared handle for multi-segment positioned writes into one `.part` file.
pub struct SegmentFileWriter {
    file: Mutex<File>,
}

impl SegmentFileWriter {
    /// Open (or create) `path` for read/write positioned IO.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    /// Write `data` starting at `offset`, hard-capped so no byte past
    /// `end_inclusive` is written.
    ///
    /// Returns the number of bytes written (may be less than `data.len()` when
    /// the end-cap truncates). Zero-length `data` is a no-op. Writing with
    /// `offset` past the end-cap returns `InvalidInput`.
    pub fn write_at(&self, offset: u64, data: &[u8], end_inclusive: u64) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }

        // Inclusive end → exclusive upper bound (checked add for u64::MAX).
        let end_exclusive = end_inclusive
            .checked_add(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "segment end overflow"))?;

        if offset >= end_exclusive {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write offset past segment end",
            ));
        }

        let allowed = (end_exclusive - offset) as usize;
        let to_write = &data[..data.len().min(allowed)];

        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "segment file lock poisoned"))?;

        write_at_locked(&mut file, offset, to_write)
    }

    /// Flush OS buffers for this file (data only; metadata may lag).
    pub fn flush_sync_data(&self) -> io::Result<()> {
        let file = self
            .file
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "segment file lock poisoned"))?;
        file.sync_data()
    }

    /// Extend or truncate the file to `len` (used after free-space preallocate gate).
    pub fn set_len(&self, len: u64) -> io::Result<()> {
        let file = self
            .file
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "segment file lock poisoned"))?;
        file.set_len(len)
    }
}

#[cfg(windows)]
fn write_at_locked(file: &mut File, offset: u64, data: &[u8]) -> io::Result<usize> {
    // FileExt::seek_write takes &self.
    file.seek_write(data, offset)
}

#[cfg(not(windows))]
fn write_at_locked(file: &mut File, offset: u64, data: &[u8]) -> io::Result<usize> {
    file.seek(SeekFrom::Start(offset))?;
    file.write(data)
}

/// Preallocate `path` to `total_bytes` only when free space is known and
/// sufficient for `remaining_to_write` plus margin.
///
/// Returns:
/// - `Ok(true)` — `set_len` applied
/// - `Ok(false)` — free space unknown or below preallocate margin; caller may
///   extend-on-write (`preallocated = false`)
/// - `Err` — free space known and insufficient for remaining bytes (Disk)
///
/// Used by multi-segment start (PR 11); kept exported for early integration.
pub async fn try_preallocate(
    path: &Path,
    total_bytes: u64,
    remaining_to_write: u64,
) -> Result<bool, String> {
    use super::filesystem::{
        free_space_allows_preallocate, free_space_allows_write, free_space_bytes,
    };

    let free = free_space_bytes(path).await;
    match free {
        None => Ok(false),
        Some(free) => {
            if !free_space_allows_write(free, remaining_to_write) {
                return Err(format!(
                    "Not enough free disk space (need {remaining_to_write} bytes free, have {free})."
                ));
            }
            if !free_space_allows_preallocate(free, remaining_to_write, total_bytes) {
                return Ok(false);
            }

            let path = path.to_path_buf();
            tokio::task::spawn_blocking(move || {
                let writer = SegmentFileWriter::open(&path)
                    .map_err(|e| format!("Could not open file for preallocate: {e}"))?;
                writer
                    .set_len(total_bytes)
                    .map_err(|e| format!("Could not preallocate file: {e}"))?;
                Ok(true)
            })
            .await
            .map_err(|e| format!("Preallocate task failed: {e}"))?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use std::thread;

    fn temp_part() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "rusticdl-seg-io-{}-{}.part",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn write_at_end_cap_truncates() {
        let path = temp_part();
        let writer = SegmentFileWriter::open(&path).unwrap();
        // Segment owns bytes 0..=9 (10 bytes).
        let n = writer.write_at(8, b"ABCDEF", 9).unwrap();
        assert_eq!(n, 2, "only bytes 8 and 9 may be written");
        writer.flush_sync_data().unwrap();
        drop(writer);

        let data = fs::read(&path).unwrap();
        assert!(data.len() >= 10);
        assert_eq!(&data[8..10], b"AB");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn write_at_offset_past_end_errors() {
        let path = temp_part();
        let writer = SegmentFileWriter::open(&path).unwrap();
        let err = writer.write_at(10, b"x", 9).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn two_threads_non_overlapping_writes() {
        let path = temp_part();
        let writer = Arc::new(SegmentFileWriter::open(&path).unwrap());
        // 64 KiB file, two halves.
        let half = 32 * 1024usize;
        let end0 = (half as u64) - 1;
        let end1 = (2 * half as u64) - 1;

        let w0 = writer.clone();
        let t0 = thread::spawn(move || {
            let buf = vec![0xAAu8; half];
            let n = w0.write_at(0, &buf, end0).unwrap();
            assert_eq!(n, half);
        });

        let w1 = writer.clone();
        let t1 = thread::spawn(move || {
            let buf = vec![0xBBu8; half];
            let n = w1.write_at(half as u64, &buf, end1).unwrap();
            assert_eq!(n, half);
        });

        t0.join().unwrap();
        t1.join().unwrap();
        writer.flush_sync_data().unwrap();
        drop(writer);

        let data = fs::read(&path).unwrap();
        assert_eq!(data.len(), 2 * half);
        assert!(data[..half].iter().all(|&b| b == 0xAA));
        assert!(data[half..].iter().all(|&b| b == 0xBB));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn set_len_preallocates() {
        let path = temp_part();
        let writer = SegmentFileWriter::open(&path).unwrap();
        writer.set_len(1024).unwrap();
        drop(writer);
        assert_eq!(fs::metadata(&path).unwrap().len(), 1024);
        let _ = fs::remove_file(&path);
    }
}
