//! Workflow operations behind the CLI and hooks. Every state change goes through
//! `machine::apply` under the run lock.

use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::config::{self, Config};
use crate::docs;
use crate::fingerprint;
use crate::git;
use crate::layout;
use crate::machine::{self, Event};
use crate::state::{RunState, Status};
use crate::store::{self, RunLock, StoreError};
use crate::util::{file_hash, now_secs};
use crate::verify::{self, FeatureVerify};

const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum OpsError {
    #[error("{0}")]
    Config(#[from] config::ConfigError),
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Git(#[from] git::GitError),
    #[error("{0}")]
    Layout(#[from] layout::LayoutError),
    #[error("{0}")]
    Transition(#[from] machine::TransitionError),
    #[error("{0}")]
    Fingerprint(#[from] fingerprint::FingerprintError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The agent must ask the user something before retrying. `code` is stable for the skill.
    #[error("{code}: {message}")]
    NeedsInput { code: &'static str, message: String },
    /// A gate or precondition failed; the message says what to do.
    #[error("{0}")]
    Refused(String),
}

pub type Result<T> = std::result::Result<T, OpsError>;

pub(crate) fn refused(msg: impl Into<String>) -> OpsError {
    OpsError::Refused(msg.into())
}

/// A repository, its config, and the branch's feature folder.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub repo_root: PathBuf,
    pub config: Config,
    pub branch: String,
    pub feature_dir: PathBuf,
}

impl Workspace {
    /// Workspace for the current branch at `cwd`. `Ok(None)` outside a repository or on a
    /// detached HEAD.
    pub fn current(cwd: &Path) -> Result<Option<Self>> {
        let Some(root) = git::toplevel(cwd)? else {
            return Ok(None);
        };
        let Some(branch) = git::current_branch(&root)? else {
            return Ok(None);
        };
        Self::for_branch(&root, &branch).map(Some)
    }

    pub fn for_branch(root: &Path, branch: &str) -> Result<Self> {
        let repo_root = root.canonicalize()?;
        let config = config::load(&repo_root)?;
        let feature_dir = layout::feature_dir(&repo_root, &config.docs_root, branch)?;
        let feature_dir = feature_dir.canonicalize().unwrap_or(feature_dir);
        Ok(Self {
            repo_root,
            config,
            branch: branch.to_string(),
            feature_dir,
        })
    }

    pub fn doc(&self, name: &str) -> PathBuf {
        self.feature_dir.join(name)
    }

    fn read_doc(&self, name: &str) -> Result<String> {
        match std::fs::read_to_string(self.doc(name)) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn doc_hash(&self, name: &str) -> Result<Option<String>> {
        Ok(file_hash(&self.doc(name))?)
    }

    /// Hash over PLAN.md and verify.json together: approving the plan approves its checks.
    pub fn plan_hash(&self) -> Result<Option<String>> {
        let plan = std::fs::read(self.doc(docs::PLAN)).ok();
        let checks = std::fs::read(self.doc(docs::VERIFY)).unwrap_or_default();
        Ok(plan.map(|p| crate::util::sha256_hex(&[p, vec![0], checks].concat())))
    }

    pub fn peek(&self) -> Result<Option<RunState>> {
        Ok(store::peek(&self.feature_dir, &self.branch)?)
    }

    pub fn lock(&self) -> Result<RunLock> {
        Ok(store::lock(&self.feature_dir, &self.branch, LOCK_TIMEOUT)?)
    }

    /// Lock and load an existing run.
    fn locked_run(&self) -> Result<(RunLock, RunState)> {
        let lock = self.lock()?;
        let state = lock.load()?.ok_or_else(|| {
            refused(format!(
                "no planvil run for branch `{}`; start one with /planvil",
                self.branch
            ))
        })?;
        Ok((lock, state))
    }

    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.repo_root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// Per-file digests of code and verification inputs for this feature.
    pub fn snapshot(&self) -> Result<std::collections::BTreeMap<String, String>> {
        let docs_rel = self.rel(&self.feature_dir);
        let extras = [
            format!("{docs_rel}/{}", docs::FEATURE),
            format!("{docs_rel}/{}", docs::VERIFY),
            config::CONFIG_FILE.to_string(),
        ];
        Ok(fingerprint::snapshot(
            &self.repo_root,
            &self.config.docs_root,
            &extras,
        )?)
    }

    /// Fingerprint of code and verification inputs for this feature.
    pub fn fingerprint(&self) -> Result<String> {
        Ok(fingerprint::digest_of(&self.snapshot()?))
    }

    pub(crate) fn criteria(&self) -> Result<Vec<String>> {
        Ok(docs::feature_criteria(&self.read_doc(docs::FEATURE)?))
    }

    pub(crate) fn verify_plan(&self) -> Result<verify::Plan> {
        let raw = self.read_doc(docs::VERIFY)?;
        let feature: FeatureVerify =
            serde_json::from_str(if raw.trim().is_empty() { "{}" } else { &raw })
                .map_err(|e| refused(format!("{} is not valid: {e}", docs::VERIFY)))?;
        let project = self.config.verify.clone().unwrap_or_default();
        verify::Plan::new(&project, &feature)
            .map_err(|e| refused(format!("verification config: {e}")))
    }
}

pub(crate) fn apply(state: &RunState, event: &Event) -> Result<RunState> {
    Ok(machine::apply(state, event, now_secs())?)
}

// ---------------------------------------------------------------------------------------
// user controls

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Pause,
    Resume,
    Cancel,
}

pub fn control(cwd: &Path, action: Control) -> Result<String> {
    let ws = current_workspace(cwd)?;
    let (lock, state) = ws.locked_run()?;
    let event = match action {
        Control::Pause => Event::Pause,
        Control::Resume => Event::Resume,
        Control::Cancel => Event::Cancel,
    };
    let next = apply(&state, &event)?;
    lock.save(&next)?;
    Ok(format!(
        "{:?} → {:?}. {}",
        state.status,
        next.status,
        next_action(&ws, &next)
    ))
}

/// Write a sample .planvil.json if the repository has none.
pub fn init(cwd: &Path) -> Result<String> {
    let root = git::toplevel(cwd)?.ok_or_else(|| refused("not inside a git repository"))?;
    let path = root.join(config::CONFIG_FILE);
    if path.exists() {
        return Err(refused(format!("{} already exists", path.display())));
    }
    std::fs::write(&path, include_str!("../../examples/planvil.sample.json"))?;
    Ok(format!(
        "wrote {}. Edit base_branch and verify for this project.",
        path.display()
    ))
}

mod gates;
mod guide;
mod questions;
mod setup;
mod start;
mod validate;

pub use gates::advance;
pub use guide::{describe, next_action, status};
pub use questions::{answer, ask};
pub use setup::{apply_setup, check as setup_check};
pub use start::{StartArgs, start};
pub use validate::{time_exceeded, verify};

/// An unfinished, unpaused run in this checkout on a branch other than HEAD's. Returned
/// so hooks keep enforcing when HEAD is moved off the run (switch, detach).
pub fn foreign_run(cwd: &Path) -> Result<Option<RunState>> {
    let Some(root) = git::toplevel(cwd)? else {
        return Ok(None);
    };
    let current = git::current_branch(&root)?;
    let docs_root = root.join(config::load(&root)?.docs_root);
    let Ok(entries) = std::fs::read_dir(&docs_root) else {
        return Ok(None);
    };
    for entry in entries.flatten() {
        let Ok(Some(state)) = store::peek_any(&entry.path()) else {
            continue;
        };
        let live = matches!(
            state.status,
            Status::Active | Status::WaitingForUser | Status::Blocked
        );
        if live && current.as_deref() != Some(state.branch.as_str()) {
            return Ok(Some(state));
        }
    }
    Ok(None)
}

pub(crate) fn current_workspace(cwd: &Path) -> Result<Workspace> {
    Workspace::current(cwd)?.ok_or_else(|| refused("not on a branch inside a git repository"))
}

#[cfg(test)]
#[path = "../ops_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "setup_tests.rs"]
mod setup_tests;
