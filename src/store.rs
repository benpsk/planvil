//! Run state on disk: `<feature_dir>/.planvil/state.json`, guarded by a per-run lock.
//!
//! Writes go to a temp file in the same directory, are synced, then renamed over the
//! old file, so a crash never leaves a half-written state.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::state::{RunState, SCHEMA_VERSION};

pub const RUN_DIR: &str = ".planvil";
const STATE_FILE: &str = "state.json";
const LOCK_FILE: &str = "lock";
const LOCK_RETRY: Duration = Duration::from_millis(25);

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("state I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("state file {path} is corrupt: {reason}")]
    Corrupt { path: PathBuf, reason: String },
    #[error("state file {path} has schema version {found}, expected {SCHEMA_VERSION}")]
    SchemaVersion { path: PathBuf, found: u32 },
    #[error("another session holds the run lock at {0}")]
    Locked(PathBuf),
    #[error("state file {path} belongs to branch `{found}`, not `{expected}`")]
    BranchMismatch {
        path: PathBuf,
        expected: String,
        found: String,
    },
    #[error("cannot serialize state: {0}")]
    Serialize(String),
}

/// Exclusive lock on one run. Released when dropped.
///
/// Reading for a change and writing it back both go through the lock, so
/// load → apply → save is one critical section and cannot lose another session's update.
#[derive(Debug)]
pub struct RunLock {
    _file: File,
    feature_dir: PathBuf,
    branch: String,
}

pub fn run_dir(feature_dir: &Path) -> PathBuf {
    feature_dir.join(RUN_DIR)
}

/// Take the lock for `branch`'s run in `feature_dir`, retrying until `timeout`.
pub fn lock(feature_dir: &Path, branch: &str, timeout: Duration) -> Result<RunLock, StoreError> {
    let dir = run_dir(feature_dir);
    fs::create_dir_all(&dir).map_err(io_at(&dir))?;
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(io_at(&path))?;
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => {
                return Ok(RunLock {
                    _file: file,
                    feature_dir: feature_dir.to_path_buf(),
                    branch: branch.to_string(),
                });
            }
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(LOCK_RETRY);
            }
            Err(fs::TryLockError::WouldBlock) => return Err(StoreError::Locked(path)),
            Err(fs::TryLockError::Error(e)) => return Err(io_at(&path)(e)),
        }
    }
}

impl RunLock {
    /// The same lock after the run's folder was moved and its branch renamed. The open
    /// lock file moves with the folder, so the lock is never released in between.
    pub fn relocated(self, feature_dir: &Path, branch: &str) -> RunLock {
        RunLock {
            _file: self._file,
            feature_dir: feature_dir.to_path_buf(),
            branch: branch.to_string(),
        }
    }

    /// Read state for a change. `Ok(None)` when no run exists yet.
    pub fn load(&self) -> Result<Option<RunState>, StoreError> {
        read_state(&self.feature_dir, &self.branch)
    }

    /// Atomically write state for the locked run.
    pub fn save(&self, state: &RunState) -> Result<(), StoreError> {
        let path = run_dir(&self.feature_dir).join(STATE_FILE);
        if state.branch != self.branch {
            return Err(StoreError::BranchMismatch {
                path,
                expected: self.branch.clone(),
                found: state.branch.clone(),
            });
        }
        let body =
            serde_json::to_vec_pretty(state).map_err(|e| StoreError::Serialize(e.to_string()))?;
        write_atomic(&path, &body)
    }
}

/// Read-only look at whatever run a feature folder holds, without a branch check.
pub fn peek_any(feature_dir: &Path) -> Result<Option<RunState>, StoreError> {
    read_raw(feature_dir)
}

/// Read-only look at state (status, hooks). Never use the result to write back.
pub fn peek(feature_dir: &Path, branch: &str) -> Result<Option<RunState>, StoreError> {
    read_state(feature_dir, branch)
}

fn read_state(feature_dir: &Path, branch: &str) -> Result<Option<RunState>, StoreError> {
    let Some(state) = read_raw(feature_dir)? else {
        return Ok(None);
    };
    // Different branches can share a folder name (`a/b-c`, `a-b/c`); never mix their runs.
    if state.branch != branch {
        let path = run_dir(feature_dir).join(STATE_FILE);
        return Err(StoreError::BranchMismatch {
            path,
            expected: branch.to_string(),
            found: state.branch,
        });
    }
    Ok(Some(state))
}

fn read_raw(feature_dir: &Path) -> Result<Option<RunState>, StoreError> {
    let path = run_dir(feature_dir).join(STATE_FILE);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_at(&path)(e)),
    };
    let corrupt = |reason: String| StoreError::Corrupt {
        path: path.clone(),
        reason,
    };
    // Check the version before the shape, so a newer schema reports as such, not as corrupt.
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| corrupt(e.to_string()))?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(v) if v == u64::from(SCHEMA_VERSION) => {}
        Some(v) => {
            let found = u32::try_from(v).unwrap_or(u32::MAX);
            return Err(StoreError::SchemaVersion { path, found });
        }
        None => return Err(corrupt("missing schema_version".into())),
    }
    let state: RunState = serde_json::from_value(value).map_err(|e| corrupt(e.to_string()))?;
    state.validate().map_err(corrupt)?;
    Ok(Some(state))
}

/// Write to a fresh temp file, sync, rename over `path`, sync the directory.
fn write_atomic(path: &Path, body: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let tmp = dir.join(format!("{STATE_FILE}.tmp-{}-{nanos}", std::process::id()));
    // create_new refuses an existing path, so a planted symlink is never followed.
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| file.write_all(body).and_then(|()| file.sync_all()))
        .map_err(io_at(&tmp))
        .and_then(|()| fs::rename(&tmp, path).map_err(io_at(path)));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.and_then(|()| sync_dir(dir))
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_at(dir))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), StoreError> {
    Ok(())
}

fn io_at(path: &Path) -> impl Fn(std::io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Limits, Stage, Status};

    const T: Duration = Duration::from_millis(200);
    const BRANCH: &str = "sp20/mcp";
    const LIMITS: Limits = Limits {
        max_repair_cycles: 5,
        max_equivalent_failures: 3,
        max_stop_continuations: 3,
        max_run_minutes: None,
    };

    fn sample() -> RunState {
        RunState::new("r1", "sp20-mcp", BRANCH, "production", LIMITS, 10)
    }

    fn state_path(dir: &Path) -> PathBuf {
        run_dir(dir).join(STATE_FILE)
    }

    #[test]
    fn load_returns_none_when_no_run() {
        let dir = tempfile::tempdir().unwrap();
        assert!(peek(dir.path(), BRANCH).unwrap().is_none());
        assert!(
            lock(dir.path(), BRANCH, T)
                .unwrap()
                .load()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let l = lock(dir.path(), BRANCH, T).unwrap();
        l.save(&sample()).unwrap();
        assert_eq!(l.load().unwrap(), Some(sample()));
        assert_eq!(peek(dir.path(), BRANCH).unwrap(), Some(sample()));
    }

    #[test]
    fn save_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let l = lock(dir.path(), BRANCH, T).unwrap();
        l.save(&sample()).unwrap();
        l.save(&sample()).unwrap();
        let mut names: Vec<_> = fs::read_dir(run_dir(dir.path()))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, vec!["lock", "state.json"]);
    }

    #[test]
    fn corrupt_state_is_an_error_not_a_fresh_run() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(run_dir(dir.path())).unwrap();
        fs::write(state_path(dir.path()), "{garbage").unwrap();
        assert!(matches!(
            peek(dir.path(), BRANCH),
            Err(StoreError::Corrupt { .. })
        ));
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let l = lock(dir.path(), BRANCH, T).unwrap();
        l.save(&RunState {
            schema_version: 99,
            ..sample()
        })
        .unwrap();
        assert!(matches!(
            l.load(),
            Err(StoreError::SchemaVersion { found: 99, .. })
        ));
    }

    #[test]
    fn inconsistent_state_is_rejected_on_load() {
        // L6: deserializes fine, but Completed at Implement is impossible.
        let dir = tempfile::tempdir().unwrap();
        let l = lock(dir.path(), BRANCH, T).unwrap();
        l.save(&RunState {
            status: Status::Completed,
            stage: Stage::Implement,
            ..sample()
        })
        .unwrap();
        assert!(matches!(l.load(), Err(StoreError::Corrupt { .. })));
    }

    #[test]
    fn another_branchs_run_in_the_same_folder_is_rejected() {
        // M3: `sp20/mcp` and `sp20-mcp` share a folder name.
        let dir = tempfile::tempdir().unwrap();
        lock(dir.path(), BRANCH, T)
            .unwrap()
            .save(&sample())
            .unwrap();
        assert!(matches!(
            peek(dir.path(), "sp20-mcp"),
            Err(StoreError::BranchMismatch { .. })
        ));
    }

    #[test]
    fn lock_refuses_to_save_another_branchs_state() {
        // M6: a lock for one run does not authorize writing another.
        let dir = tempfile::tempdir().unwrap();
        let l = lock(dir.path(), "other/branch", T).unwrap();
        assert!(matches!(
            l.save(&sample()),
            Err(StoreError::BranchMismatch { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn planted_temp_symlink_is_never_followed() {
        // M5: the temp name is unpredictable and opened with create_new.
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "keep me").unwrap();
        fs::create_dir_all(run_dir(dir.path())).unwrap();
        let planted = run_dir(dir.path()).join(format!("{STATE_FILE}.tmp-{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        lock(dir.path(), BRANCH, T)
            .unwrap()
            .save(&sample())
            .unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
    }

    #[test]
    fn second_lock_times_out_while_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let _held = lock(dir.path(), BRANCH, T).unwrap();
        let start = Instant::now();
        let second = lock(dir.path(), BRANCH, Duration::from_millis(100));
        assert!(matches!(second, Err(StoreError::Locked(_))));
        assert!(start.elapsed() >= Duration::from_millis(100));
    }

    #[test]
    fn lock_is_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        drop(lock(dir.path(), BRANCH, T).unwrap());
        assert!(lock(dir.path(), BRANCH, T).is_ok());
    }
}
