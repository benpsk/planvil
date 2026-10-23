//! Small shared helpers: time and hashing.

use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Milliseconds since the Unix epoch.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hash of a file's contents, or `None` when it does not exist.
pub fn file_hash(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(sha256_hex(&bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn file_hash_is_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(file_hash(&dir.path().join("nope")).unwrap(), None);
        std::fs::write(dir.path().join("f"), "abc").unwrap();
        assert_eq!(
            file_hash(&dir.path().join("f")).unwrap(),
            Some(sha256_hex(b"abc"))
        );
    }
}
