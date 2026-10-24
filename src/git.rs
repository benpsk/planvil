//! Thin wrappers over the `git` CLI. Every call uses an explicit argument list.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("cannot run git: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("git {args} failed: {stderr}")]
    Failed { args: String, stderr: String },
}

/// Longest a single git call may take; hooks must never hang on git.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(5);

fn git(root: &Path, args: &[&str]) -> Result<Output, GitError> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + GIT_TIMEOUT;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(GitError::Failed {
                args: args.join(" "),
                stderr: "timed out".into(),
            });
        }
        std::thread::sleep(POLL);
    }
    Ok(child.wait_with_output()?)
}

fn git_ok(root: &Path, args: &[&str]) -> Result<String, GitError> {
    let out = git(root, args)?;
    if !out.status.success() {
        return Err(GitError::Failed {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\n', '\r'])
        .to_string())
}

/// Repository (worktree) root containing `dir`. `Ok(None)` only when `dir` is genuinely
/// not inside a repository; any other git failure is an error.
pub fn toplevel(dir: &Path) -> Result<Option<PathBuf>, GitError> {
    let probe = if dir.is_dir() {
        dir.to_path_buf()
    } else {
        existing_ancestor(dir)
    };
    let out = git(&probe, &["rev-parse", "--show-toplevel"])?;
    if out.status.success() {
        return Ok(Some(PathBuf::from(
            String::from_utf8_lossy(&out.stdout).trim(),
        )));
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("not a git repository") {
        return Ok(None);
    }
    Err(GitError::Failed {
        args: "rev-parse --show-toplevel".into(),
        stderr: stderr.trim().to_string(),
    })
}

/// Nearest ancestor of `path` that exists (for paths of files not created yet).
pub fn existing_ancestor(path: &Path) -> PathBuf {
    path.ancestors()
        .find(|a| a.is_dir())
        .unwrap_or(Path::new("/"))
        .to_path_buf()
}

/// Current branch name, or `None` on a detached HEAD.
pub fn current_branch(root: &Path) -> Result<Option<String>, GitError> {
    let out = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )),
        // `--quiet` exits 1 with no output for a detached HEAD.
        Some(1) if out.stderr.is_empty() => Ok(None),
        _ => Err(GitError::Failed {
            args: "symbolic-ref HEAD".into(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }),
    }
}

pub fn branch_exists(root: &Path, branch: &str) -> Result<bool, GitError> {
    let reference = format!("refs/heads/{branch}");
    Ok(git(root, &["show-ref", "--verify", "--quiet", &reference])?
        .status
        .success())
}

pub fn rev_exists(root: &Path, rev: &str) -> Result<bool, GitError> {
    let spec = format!("{rev}^{{commit}}");
    Ok(git(root, &["rev-parse", "--verify", "--quiet", &spec])?
        .status
        .success())
}

/// Paths with uncommitted changes, excluding anything under `ignore_prefix`.
pub fn dirty_paths(root: &Path, ignore_prefix: &str) -> Result<Vec<String>, GitError> {
    let out = git_ok(
        root,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )?;
    Ok(parse_porcelain_z(&out)
        .into_iter()
        .filter(|path| !under(path, ignore_prefix))
        .collect())
}

fn under(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Paths from `git status --porcelain -z`. Rename and copy records (`R`/`C`) carry the
/// original path as an extra NUL-separated field, which is reported too.
fn parse_porcelain_z(out: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    while let Some(record) = fields.next() {
        let (Some(status), Some(path)) = (record.get(..2), record.get(3..)) else {
            continue;
        };
        paths.push(path.to_string());
        if (status.contains('R') || status.contains('C'))
            && let Some(original) = fields.next()
        {
            paths.push(original.to_string());
        }
    }
    paths
}

/// Rename a local branch.
pub fn rename_branch(root: &Path, from: &str, to: &str) -> Result<(), GitError> {
    git_ok(root, &["branch", "-m", from, to]).map(|_| ())
}

/// The branch's upstream (for example `origin/sp20/mcp`), if it has one.
pub fn upstream(root: &Path, branch: &str) -> Result<Option<String>, GitError> {
    let spec = format!("{branch}@{{upstream}}");
    let out = git(root, &["rev-parse", "--abbrev-ref", &spec])?;
    Ok(out
        .status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

pub fn checkout(root: &Path, branch: &str) -> Result<(), GitError> {
    git_ok(root, &["checkout", "--quiet", branch]).map(|_| ())
}

pub fn create_branch(root: &Path, branch: &str, base: &str) -> Result<(), GitError> {
    git_ok(root, &["checkout", "--quiet", "-b", branch, base]).map(|_| ())
}

/// Create a local branch tracking `origin/<branch>`.
pub fn track_remote(root: &Path, branch: &str) -> Result<(), GitError> {
    let remote = format!("origin/{branch}");
    git_ok(
        root,
        &["checkout", "--quiet", "-b", branch, "--track", &remote],
    )
    .map(|_| ())
}

/// Tracked and untracked-but-not-ignored files, relative to `root`.
pub fn listed_files(root: &Path) -> Result<Vec<String>, GitError> {
    let out = git_ok(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let mut files: Vec<String> = out
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// `git diff --stat` of the working tree against the merge base with `base`.
pub fn diff_stat_since(root: &Path, base: &str) -> Result<String, GitError> {
    let merge_base = git_ok(root, &["merge-base", base, "HEAD"])?;
    git_ok(root, &["diff", "--stat", &merge_base])
}

pub fn untracked_files(root: &Path) -> Result<Vec<String>, GitError> {
    let out = git_ok(root, &["ls-files", "-z", "--others", "--exclude-standard"])?;
    Ok(out
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A fresh repository with one commit on `main`.
    pub fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for args in [
            vec!["init", "--quiet", "--initial-branch=main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git_ok(root, &args).unwrap();
        }
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        git_ok(root, &["add", "."]).unwrap();
        git_ok(root, &["commit", "--quiet", "-m", "init"]).unwrap();
        dir
    }

    /// A repository whose `.planvil.json` already has (empty) project verification, so
    /// runs skip the Setup stage.
    pub fn repo_with_setup() -> tempfile::TempDir {
        let dir = repo();
        std::fs::write(dir.path().join(".planvil.json"), r#"{"verify": {}}"#).unwrap();
        git_ok(dir.path(), &["add", "."]).unwrap();
        git_ok(dir.path(), &["commit", "--quiet", "-m", "planvil config"]).unwrap();
        dir
    }

    pub fn run(root: &Path, args: &[&str]) -> String {
        git_ok(root, args).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::repo;
    use super::*;

    #[test]
    fn finds_toplevel_and_branch() {
        let dir = repo();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let top = toplevel(&sub).unwrap().unwrap();
        assert_eq!(
            top.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
        assert_eq!(current_branch(dir.path()).unwrap().as_deref(), Some("main"));
    }

    #[test]
    fn toplevel_is_none_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert!(toplevel(dir.path()).unwrap().is_none());
    }

    #[test]
    fn creates_and_detects_branches() {
        let dir = repo();
        assert!(!branch_exists(dir.path(), "sp20/mcp").unwrap());
        create_branch(dir.path(), "sp20/mcp", "main").unwrap();
        assert!(branch_exists(dir.path(), "sp20/mcp").unwrap());
        assert_eq!(
            current_branch(dir.path()).unwrap().as_deref(),
            Some("sp20/mcp")
        );
        checkout(dir.path(), "main").unwrap();
        assert!(rev_exists(dir.path(), "main").unwrap());
        assert!(!rev_exists(dir.path(), "nope").unwrap());
    }

    #[test]
    fn dirty_paths_ignore_the_docs_root() {
        let dir = repo();
        std::fs::create_dir_all(dir.path().join("planvil/x")).unwrap();
        std::fs::write(dir.path().join("planvil/x/FEATURE.md"), "x").unwrap();
        assert!(dirty_paths(dir.path(), "planvil").unwrap().is_empty());
        std::fs::write(dir.path().join("README.md"), "changed").unwrap();
        assert_eq!(
            dirty_paths(dir.path(), "planvil").unwrap(),
            vec!["README.md"]
        );
    }

    #[test]
    fn porcelain_parsing_handles_renames_and_multibyte_names() {
        let out = "R  new.rs\0ab日.rs\0 M src/x.rs\0?? planvil/f/a.md\0";
        assert_eq!(
            parse_porcelain_z(out),
            vec!["new.rs", "ab日.rs", "src/x.rs", "planvil/f/a.md"]
        );
        assert!(under("planvil/f/a.md", "planvil"));
        assert!(!under("planvilx/a.md", "planvil"));
    }

    #[test]
    fn rename_out_of_docs_root_counts_as_dirty() {
        let dir = repo();
        std::fs::create_dir_all(dir.path().join("planvil")).unwrap();
        git_ok(dir.path(), &["mv", "README.md", "planvil/README.md"]).unwrap();
        assert_eq!(
            dirty_paths(dir.path(), "planvil").unwrap(),
            vec!["README.md"]
        );
    }

    #[test]
    fn lists_tracked_and_untracked_but_not_ignored() {
        let dir = repo();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("new.rs"), "x").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "x").unwrap();
        let files = listed_files(dir.path()).unwrap();
        assert!(files.contains(&"README.md".to_string()));
        assert!(files.contains(&"new.rs".to_string()));
        assert!(!files.contains(&"ignored.txt".to_string()));
    }
}
