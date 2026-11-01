//! Runs on the branch that is already checked out (`planvil start <feature> --here`).
//!
//! A normal run owns its branch, so its folder is named after the branch. A run started
//! with `--here` stays on the current branch, so its folder is named after the feature and
//! an index maps the branch to that folder. The index lives in `<docs_root>/.planvil/`,
//! which only planvil may write.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::guide::describe;
use super::start::{activate, create_feature_dir, prepare_folder};
use super::{OpsError, Result, Workspace, refused};
use crate::git;
use crate::layout;
use crate::state::RunState;
use crate::store::{self, RUN_DIR};

const INDEX_FILE: &str = "branches.json";

type Index = BTreeMap<String, String>;

fn index_path(repo_root: &Path, docs_root: &str) -> PathBuf {
    repo_root.join(docs_root).join(RUN_DIR).join(INDEX_FILE)
}

/// The index, empty when missing. A corrupt index is an error, never "no runs": treating it
/// as empty would let hooks miss a live `--here` run.
fn read_index(repo_root: &Path, docs_root: &str) -> Result<Index> {
    let path = index_path(repo_root, docs_root);
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Index::new()),
        Err(e) => return Err(e.into()),
    };
    serde_json::from_slice(&raw).map_err(|e| {
        refused(format!(
            "{} is corrupt ({e}). planvil writes it; ask the user to delete it and rerun \
             `planvil start <feature> --here`",
            path.display()
        ))
    })
}

/// The feature folder a `--here` run on `branch` uses, if the index names a valid one.
pub(super) fn indexed_folder(
    repo_root: &Path,
    docs_root: &str,
    branch: &str,
) -> Result<Option<String>> {
    let Some(folder) = read_index(repo_root, docs_root)?.remove(branch) else {
        return Ok(None);
    };
    let valid = layout::branch_to_folder(&folder).is_ok_and(|f| f == folder);
    Ok(valid.then_some(folder))
}

/// The branch of a run already in `dir`, when it is not `branch`'s.
pub(super) fn other_owner(dir: &Path, branch: &str) -> Result<Option<String>> {
    Ok(store::peek_any(dir)?
        .map(|s| s.branch)
        .filter(|b| b != branch))
}

/// Point `branch` at `folder`, or drop its entry with `None`.
fn set_index(repo_root: &Path, docs_root: &str, branch: &str, folder: Option<&str>) -> Result<()> {
    let mut index = read_index(repo_root, docs_root)?;
    match folder {
        Some(f) => index.insert(branch.to_string(), f.to_string()),
        None => index.remove(branch),
    };
    let path = index_path(repo_root, docs_root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_vec_pretty(&index).map_err(|e| refused(e.to_string()))?;
    store::write_atomic(&path, &body)?;
    Ok(())
}

/// Before a branch-owned run starts on `branch`, forget a finished `--here` run there so
/// the new run gets the branch's own folder. An unfinished one is left to be resumed.
pub(super) fn release_finished(root: &Path, branch: &str) -> Result<()> {
    let ws = Workspace::for_branch(root, branch)?;
    if indexed_folder(&ws.repo_root, &ws.config.docs_root, branch)?.is_none() {
        return Ok(());
    }
    let finished = match ws.peek() {
        Ok(Some(state)) => state.status.is_terminal(),
        Ok(None) | Err(_) => true,
    };
    if finished {
        set_index(&ws.repo_root, &ws.config.docs_root, branch, None)?;
    }
    Ok(())
}

/// Start or resume a run on the checked-out branch, named `feature`.
pub(super) fn start_here(
    root: &Path,
    current: Option<&str>,
    current_run: Option<&RunState>,
    feature: Option<&str>,
    spec: Option<&Path>,
) -> Result<String> {
    let branch = current.ok_or_else(|| {
        refused("check out a branch first (HEAD is detached); `--here` runs on the current branch")
    })?;
    let feature = match (feature, current_run) {
        (Some(name), _) => layout::branch_to_folder(name)?,
        (None, Some(run)) => run.feature_id.clone(),
        (None, None) => {
            return Err(OpsError::NeedsInput {
                code: "NEEDS_FEATURE_NAME",
                message:
                    "ask the user for a short feature name (for example `payment-retry`), then run \
                          `planvil start <name> --here`"
                        .into(),
            });
        }
    };
    if let Some(run) = current_run
        && run.feature_id != feature
    {
        return Err(refused(format!(
            "the run `{}` is in progress on `{branch}` ({:?}). Finish it, or ask the user to type \
             `planvil cancel`, before starting another on this branch",
            run.feature_id, run.stage
        )));
    }
    let base_ws = Workspace::for_branch(root, branch)?;
    let docs_root = base_ws.config.docs_root.clone();
    let dir = layout::feature_dir(&base_ws.repo_root, &docs_root, &feature)?;
    if let Some(other) = other_owner(&dir, branch)? {
        return Err(refused(format!(
            "{docs_root}/{feature} belongs to a run on branch `{other}`; pick another feature name"
        )));
    }
    let baseline = git::dirty_paths(&base_ws.repo_root, &docs_root)?;
    let mut ws = Workspace {
        feature_dir: dir,
        ..base_ws
    };
    ws.feature_dir = create_feature_dir(&ws, &feature)?;
    let mut notes = vec![format!(
        "stayed on `{branch}`; no branch was created or switched"
    )];
    notes.extend(prepare_folder(&ws, &feature, spec)?);
    // The report's diff starts from where the run began.
    let base = git::head_commit(&ws.repo_root)?.unwrap_or_else(|| branch.to_string());
    let state = activate(&ws, &feature, &base, baseline, &mut notes, true)?;
    // Only once the run exists, so a failed start never points the branch at nothing.
    set_index(&ws.repo_root, &docs_root, branch, Some(&feature))?;
    let list: Vec<String> = notes.iter().map(|n| format!("- {n}")).collect();
    Ok(format!(
        "planvil run `{feature}` on the current branch `{branch}`, folder {}\n{}\n\n{}",
        ws.rel(&ws.feature_dir),
        list.join("\n"),
        describe(&ws, &state)?
    ))
}
