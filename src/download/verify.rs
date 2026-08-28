use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::job::{download_error, DownloadError, FailureCategory};

/// FIPS 180-2 empty-string SHA-256 (known vector).
#[cfg(test)]
pub const SHA256_EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// FIPS 180-2 `"abc"` SHA-256 (known vector).
#[cfg(test)]
pub const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[cfg(test)]
pub fn sha256_hex(data: &[u8]) -> String {
    to_hex(&Sha256::digest(data))
}

pub fn sha256_file_hex(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

pub fn normalize_sha256_hex(input: &str) -> Option<String> {
    let s = input.trim();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(s.to_ascii_lowercase())
}

pub async fn verify_sha256_if_expected(
    temp_path: &Path,
    expected: Option<&str>,
) -> Result<(), DownloadError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let expected_norm = normalize_sha256_hex(expected).ok_or_else(|| {
        download_error(
            FailureCategory::Internal,
            format!("Invalid expected SHA-256 (need 64 hex digits): {expected}"),
            false,
        )
    })?;

    let path = temp_path.to_path_buf();
    let actual = tokio::task::spawn_blocking(move || sha256_file_hex(&path))
        .await
        .map_err(|error| {
            download_error(
                FailureCategory::Internal,
                format!("SHA-256 verify task failed: {error}"),
                false,
            )
        })?
        .map_err(|error| {
            download_error(
                FailureCategory::Disk,
                format!("Could not hash download for SHA-256 verify: {error}"),
                false,
            )
        })?;

    if actual != expected_norm {
        return Err(download_error(
            FailureCategory::Internal,
            format!("SHA-256 mismatch: expected {expected_norm}, got {actual}. Partial file kept."),
            false,
        ));
    }
    Ok(())
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::job::Job;
    use std::path::PathBuf;

    #[test]
    fn known_vector_empty() {
        assert_eq!(sha256_hex(b""), SHA256_EMPTY);
    }

    #[test]
    fn known_vector_abc() {
        assert_eq!(sha256_hex(b"abc"), SHA256_ABC);
    }

    #[test]
    fn known_vector_fips_448_bit() {
        // FIPS 180-2 / NIST CAVP: 448-bit (56-byte) ASCII message.
        let msg = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(
            sha256_hex(msg),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn known_vector_hello() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn normalize_accepts_upper_and_trims() {
        let upper = SHA256_ABC.to_ascii_uppercase();
        assert_eq!(
            normalize_sha256_hex(&format!("  {upper}  ")).as_deref(),
            Some(SHA256_ABC)
        );
    }

    #[test]
    fn normalize_rejects_wrong_length_and_non_hex() {
        assert!(normalize_sha256_hex("abc").is_none());
        assert!(normalize_sha256_hex("").is_none());
        assert!(normalize_sha256_hex(&"g".repeat(64)).is_none());
        assert!(normalize_sha256_hex(&format!("{SHA256_ABC}00")).is_none());
    }

    #[test]
    fn file_hash_matches_in_memory() {
        let dir = std::env::temp_dir().join(format!("rusticdl-sha-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc.bin");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(sha256_file_hex(&path).unwrap(), SHA256_ABC);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn skip_when_expected_none() {
        let path = PathBuf::from("C:\\missing.bin.part");
        verify_sha256_if_expected(&path, None)
            .await
            .expect("None skips verify (file need not exist)");
    }

    #[tokio::test]
    async fn match_and_mismatch_on_part_file() {
        let dir = std::env::temp_dir().join(format!("rusticdl-sha-v-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello.bin.part");
        std::fs::write(&path, b"hello").unwrap();

        verify_sha256_if_expected(
            &path,
            Some("2CF24DBA5FB0A30E26E83B2AC5B9E29E1B161E5C1FA7425E73043362938B9824"),
        )
        .await
        .expect("uppercase expected matches");

        let err = verify_sha256_if_expected(&path, Some(SHA256_ABC))
            .await
            .expect_err("wrong digest");
        assert_eq!(err.category, FailureCategory::Internal);
        assert!(!err.retryable);
        assert!(err.message.contains("SHA-256 mismatch"));
        assert!(path.exists(), "mismatch must keep .part");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn invalid_expected_hex_fails_without_hashing_need() {
        let err = verify_sha256_if_expected(Path::new("C:\\nope.part"), Some("not-a-hash"))
            .await
            .expect_err("invalid expected");
        assert_eq!(err.category, FailureCategory::Internal);
        assert!(err.message.contains("Invalid expected SHA-256"));
    }

    #[test]
    fn job_field_can_be_set_directly() {
        let mut job = Job::new(
            "https://example.com/f.bin".into(),
            "f.bin".into(),
            PathBuf::from("C:\\dl\\f.bin"),
            PathBuf::from("C:\\dl\\f.bin.part"),
        );
        assert!(job.expected_sha256.is_none());
        job.expected_sha256 = Some(SHA256_ABC.into());
        assert_eq!(job.expected_sha256.as_deref(), Some(SHA256_ABC));
    }
}
