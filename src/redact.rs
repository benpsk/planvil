//! Scrub secret values from evidence logs.

use std::collections::BTreeMap;
use std::path::Path;

const SECRET_MARKERS: [&str; 5] = ["TOKEN", "SECRET", "PASSWORD", "PASSWD", "KEY"];
/// Names that mention a marker but hold locations, not secrets (`SSH_KEY_PATH`).
const NON_SECRET_SUFFIXES: [&str; 4] = ["_PATH", "_FILE", "_DIR", "_HOME"];
const MIN_SECRET_LEN: usize = 6;
const MASK: &[u8] = b"[REDACTED]";

fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_MARKERS.iter().any(|m| upper.contains(m))
        && !NON_SECRET_SUFFIXES.iter().any(|s| upper.ends_with(s))
}

/// Secret values from the step environment and the inherited environment, longest first.
pub fn secrets(env: &BTreeMap<String, String>) -> Vec<Vec<u8>> {
    let inherited = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    let mut values: Vec<Vec<u8>> = env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .chain(inherited)
        .filter(|(k, v)| v.len() >= MIN_SECRET_LEN && is_secret_name(k))
        .map(|(_, v)| v.into_bytes())
        .collect();
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    values.dedup();
    values
}

fn replace_all(hay: &[u8], needle: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    let mut hit = false;
    while i < hay.len() {
        if hay[i..].starts_with(needle) {
            out.extend_from_slice(MASK);
            i += needle.len();
            hit = true;
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    hit.then_some(out)
}

/// Replace every secret in the file at `path`, working on raw bytes.
pub fn redact_file(path: &Path, secrets: &[Vec<u8>]) -> std::io::Result<()> {
    if secrets.is_empty() {
        return Ok(());
    }
    let original = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let redacted = secrets.iter().fold(None::<Vec<u8>>, |acc, s| {
        replace_all(acc.as_deref().unwrap_or(&original), s).or(acc)
    });
    match redacted {
        Some(bytes) => std::fs::write(path, bytes),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_names_exclude_location_variables() {
        assert!(is_secret_name("API_TOKEN"));
        assert!(is_secret_name("db_password"));
        assert!(!is_secret_name("SSH_KEY_PATH"));
        assert!(!is_secret_name("HOME"));
    }

    #[test]
    fn redacts_bytes_without_touching_other_content() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("log");
        std::fs::write(&f, b"\xff\xfe binary token=abcdef123 end").unwrap();
        redact_file(&f, &[b"abcdef123".to_vec()]).unwrap();
        assert_eq!(
            std::fs::read(&f).unwrap(),
            b"\xff\xfe binary token=[REDACTED] end"
        );
    }

    #[test]
    fn untouched_when_no_secret_matches() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("log");
        std::fs::write(&f, b"\xff plain").unwrap();
        redact_file(&f, &[b"zzzzzzzz".to_vec()]).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"\xff plain");
    }
}
