//! Code fingerprint: one hash over every relevant input to verification.
//!
//! Covers tracked files (including uncommitted edits and deletions), untracked files git
//! does not ignore, and explicitly listed extras (requirements, verify config). The docs
//! root is excluded so evidence and logs never change the fingerprint. Git HEAD alone is
//! not enough: uncommitted edits must change it too.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::git;
use crate::util::hex;

#[derive(Debug, thiserror::Error)]
pub enum FingerprintError {
    #[error(transparent)]
    Git(#[from] git::GitError),
    #[error("cannot read {path}: {source}")]
    Io { path: String, source: io::Error },
}

/// Per-path content digests (path → hex digest) for `repo_root`, skipping
/// `exclude_prefix` and adding `extras` (paths relative to `repo_root`).
pub fn snapshot(
    repo_root: &Path,
    exclude_prefix: &str,
    extras: &[String],
) -> Result<BTreeMap<String, String>, FingerprintError> {
    let prefix = format!("{}/", exclude_prefix.trim_end_matches('/'));
    git::listed_files(repo_root)?
        .into_iter()
        .filter(|p| !p.starts_with(&prefix))
        .chain(extras.iter().cloned())
        .map(|rel| {
            let digest = content_digest(&repo_root.join(&rel), &rel)?;
            Ok((rel, hex(&digest)))
        })
        .collect()
}

/// One hash over a snapshot.
pub fn digest_of(snapshot: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (rel, digest) in snapshot {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(digest.as_bytes());
        hasher.update([0]);
    }
    hex(&hasher.finalize())
}

/// Fingerprint of `repo_root`; see `snapshot`.
pub fn compute(
    repo_root: &Path,
    exclude_prefix: &str,
    extras: &[String],
) -> Result<String, FingerprintError> {
    Ok(digest_of(&snapshot(repo_root, exclude_prefix, extras)?))
}

/// Paths whose digest differs between two snapshots (added, removed or changed).
pub fn changed(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<String> {
    let mut paths: Vec<String> = before
        .iter()
        .filter(|(k, v)| after.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .chain(after.keys().filter(|k| !before.contains_key(*k)).cloned())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Digest of one path's content and type. Streams regular files; never opens FIFOs or
/// sockets; distinguishes missing files, symlink targets and the executable bit.
fn content_digest(path: &Path, rel: &str) -> Result<Vec<u8>, FingerprintError> {
    let io_err = |source| FingerprintError::Io {
        path: rel.to_string(),
        source,
    };
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Sha256::digest(b"<missing>").to_vec());
        }
        Err(e) => return Err(io_err(e)),
    };
    let kind = meta.file_type();
    let mut hasher = Sha256::new();
    if kind.is_symlink() {
        let target = std::fs::read_link(path).map_err(io_err)?;
        hasher.update(format!("<symlink>{}", target.display()).as_bytes());
    } else if kind.is_dir() {
        // Submodules and nested repositories: their content is not tracked here.
        hasher.update(b"<dir>");
    } else if kind.is_file() {
        hasher.update(if executable(&meta) { b"x:" } else { b"f:" });
        let mut file = std::fs::File::open(path).map_err(io_err)?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = io::Read::read(&mut file, &mut buf).map_err(io_err)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    } else {
        hasher.update(b"<special>");
    }
    Ok(hasher.finalize().to_vec())
}

#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::repo;
    use std::fs;

    fn fp(root: &Path) -> String {
        compute(root, "planvil", &["planvil/x/FEATURE.md".into()]).unwrap()
    }

    #[test]
    fn is_stable_without_changes() {
        let dir = repo();
        assert_eq!(fp(dir.path()), fp(dir.path()));
    }

    #[test]
    fn changes_on_uncommitted_edit() {
        let dir = repo();
        let before = fp(dir.path());
        fs::write(dir.path().join("README.md"), "edited").unwrap();
        assert_ne!(before, fp(dir.path()));
    }

    #[test]
    fn changes_on_new_untracked_source_file() {
        let dir = repo();
        let before = fp(dir.path());
        fs::write(dir.path().join("new.go"), "package x").unwrap();
        assert_ne!(before, fp(dir.path()));
    }

    #[test]
    fn changes_on_deleted_tracked_file() {
        let dir = repo();
        let before = fp(dir.path());
        fs::remove_file(dir.path().join("README.md")).unwrap();
        assert_ne!(before, fp(dir.path()));
    }

    #[test]
    fn ignores_evidence_under_docs_root_but_tracks_extras() {
        let dir = repo();
        fs::create_dir_all(dir.path().join("planvil/x/.planvil/evidence")).unwrap();
        let before = fp(dir.path());
        fs::write(
            dir.path().join("planvil/x/.planvil/evidence/log.txt"),
            "noise",
        )
        .unwrap();
        assert_eq!(before, fp(dir.path()));
        fs::write(dir.path().join("planvil/x/FEATURE.md"), "requirements").unwrap();
        assert_ne!(before, fp(dir.path()));
    }

    #[test]
    fn ignores_gitignored_build_output() {
        let dir = repo();
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        let before = fp(dir.path());
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target/bin"), "x").unwrap();
        assert_eq!(before, fp(dir.path()));
    }

    #[test]
    fn changed_lists_the_differing_paths() {
        let dir = repo();
        let before = snapshot(dir.path(), "planvil", &[]).unwrap();
        fs::write(dir.path().join("README.md"), "x").unwrap();
        fs::write(dir.path().join("gen.txt"), "x").unwrap();
        let after = snapshot(dir.path(), "planvil", &[]).unwrap();
        assert_eq!(changed(&before, &after), vec!["README.md", "gen.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_changes_the_fingerprint() {
        use std::os::unix::fs::PermissionsExt;
        let dir = repo();
        let before = fp(dir.path());
        let f = dir.path().join("README.md");
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
        assert_ne!(before, fp(dir.path()));
    }
}
