
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::Mutex;

#[cfg(not(windows))]
use std::io::{Seek, SeekFrom, Write};
#[cfg(windows)]
use std::os::windows::fs::FileExt;

pub struct SegmentFileWriter {
    file: Mutex<File>,
}

impl SegmentFileWriter {
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

    #[must_use = "short count means the end-cap truncated; credit only the returned length"]
    pub fn write_at(&self, offset: u64, data: &[u8], end_inclusive: u64) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }

        let end_exclusive = end_inclusive
            .checked_add(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "segment end overflow"))?;

        if offset >= end_exclusive {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write offset past segment end",
            ));
        }

        let max_len = (end_exclusive - offset).min(data.len() as u64) as usize;
        let to_write = &data[..max_len];

        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "segment file lock poisoned"))?;

        write_all_at_locked(&mut file, offset, to_write)?;
        Ok(to_write.len())
    }

    pub fn flush_sync_data(&self) -> io::Result<()> {
        let file = self
            .file
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "segment file lock poisoned"))?;
        file.sync_data()
    }

    pub fn set_len(&self, len: u64) -> io::Result<()> {
        let file = self
            .file
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "segment file lock poisoned"))?;
        file.set_len(len)
    }
}

#[cfg(windows)]
fn write_all_at_locked(file: &mut File, offset: u64, data: &[u8]) -> io::Result<()> {
    let mut done = 0usize;
    while done < data.len() {
        let n = file.seek_write(&data[done..], offset + done as u64)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "failed to write whole buffer",
            ));
        }
        done += n;
    }
    Ok(())
}

#[cfg(not(windows))]
fn write_all_at_locked(file: &mut File, offset: u64, data: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)
}

pub fn preallocate_decision(
    free: Option<u64>,
    remaining: u64,
    total_bytes: u64,
) -> Result<bool, ()> {
    use super::filesystem::{free_space_allows_preallocate, free_space_allows_write};

    match free {
        None => Ok(false),
        Some(free) => {
            if !free_space_allows_write(free, remaining) {
                Err(())
            } else if !free_space_allows_preallocate(free, remaining, total_bytes) {
                Ok(false)
            } else {
                Ok(true)
            }
        }
    }
}

pub async fn try_preallocate(
    path: &Path,
    total_bytes: u64,
    remaining_to_write: u64,
) -> Result<bool, String> {
    use super::filesystem::free_space_bytes;

    let free = free_space_bytes(path).await;
    match preallocate_decision(free, remaining_to_write, total_bytes) {
        Err(()) => {
            let free = free.unwrap_or(0);
            Err(format!(
                "Not enough free disk space (need {remaining_to_write} bytes free, have {free})."
            ))
        }
        Ok(false) => Ok(false),
        Ok(true) => {
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
    fn write_at_full_buffer_commits_entire_capped_slice() {
        let path = temp_part();
        let writer = SegmentFileWriter::open(&path).unwrap();
        let buf = vec![0xCCu8; 4096];
        let n = writer.write_at(0, &buf, 4095).unwrap();
        assert_eq!(n, 4096, "full capped slice must be committed");
        writer.flush_sync_data().unwrap();
        drop(writer);
        let data = fs::read(&path).unwrap();
        assert_eq!(data, buf);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn two_threads_non_overlapping_writes() {
        let path = temp_part();
        let writer = Arc::new(SegmentFileWriter::open(&path).unwrap());
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

    #[test]
    fn preallocate_decision_tiers() {
        let total = 1000u64;
        let remaining = 100u64;
        assert_eq!(preallocate_decision(None, remaining, total), Ok(false));
        assert_eq!(
            preallocate_decision(Some(remaining), remaining, total),
            Err(())
        );
        assert_eq!(
            preallocate_decision(Some(remaining + 1), remaining, total),
            Ok(false),
            "fits write but not margin → soft skip"
        );
        let margin = super::super::filesystem::preallocate_margin(total);
        assert_eq!(
            preallocate_decision(Some(remaining + margin + 1), remaining, total),
            Ok(true)
        );
    }
}
