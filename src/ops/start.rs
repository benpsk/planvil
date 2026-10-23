//! `planvil start`: branch, feature folder, templates, and run activation.

use std::path::{Path, PathBuf};

use super::guide::describe;
use super::{OpsError, Result, Workspace, refused};
use crate::config;
use crate::docs;
use crate::git;
use crate::layout;
use crate::state::{RunState, Stage, Status};
use crate::store;
use crate::util::now_secs;

pub struct StartArgs<'a> {
    /// Only (re)discover the project setup on the current branch; no feature.
    pub setup_only: bool,
    pub branch: Option<&'a str>,
    pub spec: Option<&'a Path>,
    pub base: Option<&'a str>,
}

pub fn start(cwd: &Path, args: &StartArgs) -> Result<String> {
    let root = git::toplevel(cwd)?.ok_or_else(|| refused("not inside a git repository"))?;
    let current = git::current_branch(&root)?;
    let current_run = current
        .as_deref()
        .and_then(|cur| unfinished_run(&root, cur));
    if args.setup_only {
        return start_setup_only(&root, current.as_deref(), current_run.as_ref());
    }
    let branch = choose_branch(args.branch, current.as_deref(), current_run.as_ref())?;
    layout::branch_to_folder(&branch)?;
    if let (Some(cur), Some(run)) = (&current, &current_run)
        && *cur != branch
        && run.status != Status::Paused
    {
        return Err(refused(format!(
            "a run on `{cur}` is in progress ({:?}). Finish it, or ask the user to type `planvil pause` \
             or `planvil cancel`, before starting another",
            run.stage
        )));
    }

    let base = choose_base(
        args.base,
        Workspace::for_branch(&root, &branch)?
            .config
            .base_branch
            .as_deref(),
    )?;
    let mut notes = Vec::new();
    // Switch first: planvil's own config and .gitignore edits must not count as user work.
    if current.as_deref() != Some(branch.as_str()) {
        notes.push(switch_branch(&root, &branch, &base)?);
    }
    // Reload: the target branch may carry its own .planvil.json.
    let mut ws = Workspace::for_branch(&root, &branch)?;
    let baseline = git::dirty_paths(&ws.repo_root, &ws.config.docs_root)?;
    if ws.config.base_branch.is_none() {
        save_base_branch(&ws.repo_root, &base)?;
        notes.push(format!(
            "saved base_branch `{base}` to .planvil.json; commit it with the project"
        ));
        ws = Workspace::for_branch(&root, &branch)?;
    }
    ws.feature_dir = create_feature_dir(&ws, &branch)?;
    notes.extend(prepare_folder(&ws, &branch, args.spec)?);

    let state = activate(&ws, &branch, &base, baseline, &mut notes)?;
    let list: Vec<String> = notes.iter().map(|n| format!("- {n}")).collect();
    Ok(format!(
        "planvil run on `{branch}` (base `{base}`), folder {}\n{}\n\n{}",
        ws.rel(&ws.feature_dir),
        list.join("\n"),
        describe(&ws, &state)?
    ))
}

/// A run on the current branch that only discovers and saves the project setup.
fn start_setup_only(
    root: &Path,
    current: Option<&str>,
    current_run: Option<&RunState>,
) -> Result<String> {
    let branch = current.ok_or_else(|| refused("check out a branch first (HEAD is detached)"))?;
    if let Some(run) = current_run {
        return Err(refused(format!(
            "a run on `{branch}` is in progress ({:?}); finish it or ask the user to type `planvil cancel` first",
            run.stage
        )));
    }
    let mut ws = Workspace::for_branch(root, branch)?;
    ws.feature_dir = create_feature_dir(&ws, branch)?;
    let mut notes = prepare_folder(&ws, branch, None)?;
    let base = ws
        .config
        .base_branch
        .clone()
        .unwrap_or_else(|| branch.to_string());
    let lock = ws.lock()?;
    if let Some(old) = lock.load()? {
        archive_run(&ws, &old)?;
        notes.push(format!("archived finished run {}", old.run_id));
    }
    let now = now_secs();
    let run_id = format!("{now:x}-{:x}", std::process::id());
    let state = RunState {
        stage: Stage::Setup,
        setup_only: true,
        ..RunState::new(
            run_id,
            layout::branch_to_folder(branch)?,
            branch,
            &base,
            ws.config.limits(),
            now,
        )
    };
    lock.save(&state)?;
    notes.push(if ws.config.verify.is_some() {
        "re-discovering the project setup; the approved result replaces the current one".to_string()
    } else {
        "discovering the project setup".to_string()
    });
    let list: Vec<String> = notes.iter().map(|n| format!("- {n}")).collect();
    Ok(format!(
        "planvil setup on `{branch}`\n{}\n\n{}",
        list.join("\n"),
        describe(&ws, &state)?
    ))
}

fn unfinished_run(root: &Path, branch: &str) -> Option<RunState> {
    let ws = Workspace::for_branch(root, branch).ok()?;
    ws.peek().ok().flatten().filter(|s| !s.status.is_terminal())
}

fn choose_branch(
    arg: Option<&str>,
    current: Option<&str>,
    current_run: Option<&RunState>,
) -> Result<String> {
    match (arg, current, current_run) {
        (Some(b), _, _) => Ok(b.to_string()),
        (None, Some(cur), Some(_)) => Ok(cur.to_string()),
        (None, _, _) => Err(OpsError::NeedsInput {
            code: "NEEDS_BRANCH",
            message: "ask the user for the feature branch name (for example `sp20/mcp`), then run \
                      `planvil start <branch>`"
                .into(),
        }),
    }
}

fn choose_base(arg: Option<&str>, configured: Option<&str>) -> Result<String> {
    let base = match (arg, configured) {
        (Some(b), _) | (None, Some(b)) => b.to_string(),
        (None, None) => {
            return Err(OpsError::NeedsInput {
                code: "NEEDS_BASE_BRANCH",
                message: "ask the user which branch feature branches are cut from (for example `main` or \
                          `production`), then rerun with `--base <branch>`; it is saved to .planvil.json"
                    .into(),
            });
        }
    };
    layout::branch_to_folder(&base)?;
    Ok(base)
}

fn switch_branch(root: &Path, branch: &str, base: &str) -> Result<String> {
    let docs_root = config::load(root)?.docs_root;
    let dirty = git::dirty_paths(root, &docs_root)?;
    if !dirty.is_empty() {
        return Err(OpsError::NeedsInput {
            code: "NEEDS_CLEAN_WORKTREE",
            message: format!(
                "the worktree has uncommitted changes ({}). planvil never stashes or resets user work: \
                 ask the user to commit or stash them, then rerun",
                dirty.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
            ),
        });
    }
    if git::branch_exists(root, branch)? {
        git::checkout(root, branch)?;
        return Ok(format!("checked out existing branch `{branch}`"));
    }
    if git::rev_exists(root, &format!("origin/{branch}"))? {
        git::track_remote(root, branch)?;
        return Ok(format!("created `{branch}` tracking `origin/{branch}`"));
    }
    let start_point = [base.to_string(), format!("origin/{base}")]
        .into_iter()
        .find(|rev| git::rev_exists(root, rev).unwrap_or(false))
        .ok_or_else(|| {
            refused(format!(
                "base branch `{base}` not found locally or on origin"
            ))
        })?;
    git::create_branch(root, branch, &start_point)?;
    Ok(format!("created branch `{branch}` from `{start_point}`"))
}

/// Create the feature folder one level inside the (possibly symlinked) docs root, refusing
/// a folder that resolves anywhere else. Checked before anything is written into it.
fn create_feature_dir(ws: &Workspace, branch: &str) -> Result<PathBuf> {
    let docs_root = ws.repo_root.join(&ws.config.docs_root);
    std::fs::create_dir_all(&docs_root)?;
    let docs_root = docs_root.canonicalize()?;
    let target = docs_root.join(layout::branch_to_folder(branch)?);
    if !target.exists() {
        std::fs::create_dir(&target)?;
    }
    let resolved = target.canonicalize()?;
    if resolved.parent() != Some(docs_root.as_path()) {
        return Err(refused(format!(
            "feature folder {} resolves to {}, outside the docs root",
            target.display(),
            resolved.display()
        )));
    }
    Ok(resolved)
}

/// Gitignore, templates and the optional spec copy. Returns notes for the user.
fn prepare_folder(ws: &Workspace, branch: &str, spec: Option<&Path>) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    if ws.config.docs_gitignored && ensure_gitignored(&ws.repo_root, &ws.config.docs_root)? {
        notes.push(format!("added `/{}/` to .gitignore", ws.config.docs_root));
    }
    let created = docs::write_missing_templates(&ws.feature_dir, branch)?;
    if !created.is_empty() {
        notes.push(format!("created {}", created.join(", ")));
    }
    if let Some(spec) = spec {
        let copy = ws.doc("SOURCE-SPEC.md");
        let same = spec
            .canonicalize()
            .ok()
            .is_some_and(|s| copy.canonicalize().ok() == Some(s));
        if !same {
            std::fs::copy(spec, &copy)?;
        }
        notes.push(format!(
            "copied {} to {}; rewrite it into FEATURE.md using the template headings without changing meaning",
            spec.display(),
            ws.rel(&copy)
        ));
    }
    Ok(notes)
}

/// Resume the branch's unfinished run, or archive a finished one and start fresh.
fn activate(
    ws: &Workspace,
    branch: &str,
    base: &str,
    baseline: Vec<String>,
    notes: &mut Vec<String>,
) -> Result<RunState> {
    let lock = ws.lock()?;
    let state = match lock.load()? {
        Some(existing) if !existing.status.is_terminal() => {
            notes.push(
                "resumed the existing run; re-check the code and docs before continuing".into(),
            );
            RunState {
                session_id: None,
                ..existing
            }
        }
        previous => {
            if let Some(old) = previous {
                archive_run(ws, &old)?;
                notes.push(format!(
                    "archived finished run {} and its evidence",
                    old.run_id
                ));
            }
            let now = now_secs();
            let run_id = format!("{now:x}-{:x}", std::process::id());
            let folder = layout::branch_to_folder(branch)?;
            RunState {
                baseline_dirty: baseline,
                // The first run in a repository discovers how to run and check it.
                stage: if ws.config.verify.is_none() {
                    Stage::Setup
                } else {
                    Stage::Feature
                },
                ..RunState::new(run_id, folder, branch, base, ws.config.limits(), now)
            }
        }
    };
    lock.save(&state)?;
    Ok(state)
}

fn save_base_branch(repo_root: &Path, base: &str) -> Result<()> {
    Ok(config::save_key(
        repo_root,
        "base_branch",
        serde_json::Value::String(base.into()),
    )?)
}

/// Add `/<docs_root>/` to .gitignore unless already ignored. Returns true when added.
/// Only a missing file counts as empty; any other read error is reported, never overwritten.
fn ensure_gitignored(repo_root: &Path, docs_root: &str) -> Result<bool> {
    let path = repo_root.join(".gitignore");
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(refused(format!(
                "cannot read .gitignore ({e}); leaving it untouched"
            )));
        }
    };
    let forms = [
        format!("/{docs_root}/"),
        format!("{docs_root}/"),
        format!("/{docs_root}"),
        docs_root.to_string(),
    ];
    if existing
        .lines()
        .any(|l| forms.contains(&l.trim().to_string()))
    {
        return Ok(false);
    }
    let sep = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    std::fs::write(
        &path,
        format!("{existing}{sep}# planvil feature docs\n/{docs_root}/\n"),
    )?;
    Ok(true)
}

/// Move a finished run's state and evidence to `.planvil/history/<run_id>/`.
fn archive_run(ws: &Workspace, old: &RunState) -> Result<()> {
    let run_dir = store::run_dir(&ws.feature_dir);
    let dir = run_dir.join("history").join(&old.run_id);
    std::fs::create_dir_all(&dir)?;
    let body = serde_json::to_vec_pretty(old).map_err(|e| refused(e.to_string()))?;
    std::fs::write(dir.join("state.json"), body)?;
    let evidence = run_dir.join("evidence");
    if evidence.exists() {
        std::fs::rename(&evidence, dir.join("evidence"))?;
    }
    Ok(())
}
