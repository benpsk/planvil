//! `planvil rename`: give the run's branch a better name without losing the run.

use std::path::Path;

use super::guide::describe;
use super::{Result, Workspace, current_workspace, refused};
use crate::git;
use crate::layout;
use crate::state::RunState;

/// Rename the current run's branch and move its feature folder to match. Stage,
/// confirmations, approvals and evidence carry over; the documents are not edited.
pub fn rename(cwd: &Path, new_branch: &str) -> Result<String> {
    let ws = current_workspace(cwd)?;
    let new_folder = layout::branch_to_folder(new_branch)?;
    let (lock, state) = ws.locked_run()?;
    if state.status.is_terminal() {
        return Err(refused(
            "this run is finished; start a new one on the branch you want instead",
        ));
    }
    if new_branch == ws.branch {
        return Err(refused(format!(
            "the branch is already called `{new_branch}`"
        )));
    }
    if git::branch_exists(&ws.repo_root, new_branch)? {
        return Err(refused(format!(
            "a branch called `{new_branch}` already exists; pick another name"
        )));
    }
    let docs_root = ws
        .feature_dir
        .parent()
        .ok_or_else(|| refused("the feature folder has no parent"))?;
    let new_dir = docs_root.join(&new_folder);
    if new_dir.exists() {
        return Err(refused(format!(
            "{} already exists; pick another name",
            ws.rel(&new_dir)
        )));
    }

    let upstream = git::upstream(&ws.repo_root, &ws.branch)?;
    git::rename_branch(&ws.repo_root, &ws.branch, new_branch)?;
    if let Err(e) = std::fs::rename(&ws.feature_dir, &new_dir) {
        // Put the branch name back so the run and its folder stay consistent.
        let _ = git::rename_branch(&ws.repo_root, new_branch, &ws.branch);
        return Err(e.into());
    }
    let lock = lock.relocated(&new_dir, new_branch);
    let next = RunState {
        branch: new_branch.to_string(),
        feature_id: new_folder,
        ..state
    };
    lock.save(&next)?;

    let moved = Workspace {
        branch: new_branch.to_string(),
        feature_dir: new_dir.clone(),
        ..ws.clone()
    };
    let remote_note = upstream
        .map(|u| format!("\n- the old branch's upstream `{u}` was not renamed; the user pushes the new name when ready"))
        .unwrap_or_default();
    Ok(format!(
        "renamed `{}` → `{new_branch}`\n- moved {} → {}{remote_note}\n\n{}",
        ws.branch,
        ws.rel(&ws.feature_dir),
        moved.rel(&new_dir),
        describe(&moved, &next)?
    ))
}
