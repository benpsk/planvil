//! Setup stage: the agent proposes how to run and check the project, planvil proves the
//! proposal runs, the user approves it once, and planvil writes it to `.planvil.json`.
//!
//! The proposal's centrepiece is the project's verify script. The agent writes it into the
//! feature folder (`verify.proposed.sh`); `setup apply` installs it at the path named by
//! `script`, where it is committed with the project and protected during runs.

use std::path::{Path, PathBuf};

use super::guide::next_action;
use super::{Result, Workspace, apply, current_workspace, refused};
use crate::config;
use crate::machine::Event;
use crate::state::{RunState, SetupCheck, Stage, Status};
use crate::store;
use crate::util::{now_secs, sha256_hex};
use crate::verify::{self, ProjectVerify, SCRIPT_STEP, SmokeOutcome};

/// The agent's proposal, inside the feature folder (writable during Setup).
pub const PROPOSAL: &str = "setup.proposed.json";
/// The proposed verify script, next to the proposal.
pub const PROPOSED_SCRIPT: &str = "verify.proposed.sh";

/// Hash over the proposal and the proposed script, if the proposal exists. Approving the
/// setup approves both; changing either voids the approval.
pub(super) fn proposal_hash(ws: &Workspace) -> Result<Option<String>> {
    let Ok(proposal) = std::fs::read(ws.doc(PROPOSAL)) else {
        return Ok(None);
    };
    let script = std::fs::read(ws.doc(PROPOSED_SCRIPT)).unwrap_or_default();
    Ok(Some(sha256_hex(&[proposal, vec![0], script].concat())))
}

/// The proposed script, when the proposal names one. It must exist.
fn proposed_script(ws: &Workspace, proposal: &ProjectVerify) -> Result<Option<PathBuf>> {
    if proposal.script.is_none() {
        return Ok(None);
    }
    let path = ws.doc(PROPOSED_SCRIPT);
    if !path.is_file() {
        return Err(refused(format!(
            "{PROPOSAL} names a verify script, but {} does not exist; write the script there first",
            ws.rel(&path)
        )));
    }
    Ok(Some(path))
}

fn read_proposal(ws: &Workspace) -> Result<(ProjectVerify, String)> {
    let path = ws.doc(PROPOSAL);
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        refused(format!(
            "cannot read {} ({e}); write the proposal there first",
            ws.rel(&path)
        ))
    })?;
    let proposal: ProjectVerify =
        serde_json::from_str(&raw).map_err(|e| refused(format!("{PROPOSAL} is not valid: {e}")))?;
    proposal
        .validate()
        .map_err(|e| refused(format!("{PROPOSAL}: {e}")))?;
    if proposal.script.is_none() && proposal.checks.is_empty() {
        return Err(refused(format!(
            "{PROPOSAL} checks nothing: set `script` (for example \"scripts/planvil-verify.sh\") and \
             write the script to {PROPOSED_SCRIPT}"
        )));
    }
    proposed_script(ws, &proposal)?;
    if let Some(rel) = &proposal.script {
        check_script_target(ws, rel)?;
    }
    let hash = proposal_hash(ws)?.ok_or_else(|| refused(format!("{PROPOSAL} disappeared")))?;
    Ok((proposal, hash))
}

fn in_setup(ws: &Workspace, state: &RunState) -> Result<()> {
    if state.stage != Stage::Setup || state.status != Status::Active {
        return Err(refused(format!(
            "this runs in the Setup stage. next: {}",
            next_action(ws, state)
        )));
    }
    Ok(())
}

/// Run the proposal once and record whether it is usable.
pub fn check(cwd: &Path) -> Result<String> {
    let ws = current_workspace(cwd)?;
    {
        let (_lock, state) = ws.locked_run()?;
        in_setup(&ws, &state)?;
    }
    let (proposal, hash) = read_proposal(&ws)?;
    let evidence = store::run_dir(&ws.feature_dir)
        .join("evidence")
        .join(format!("setup-{}", now_secs()));
    let script = proposed_script(&ws, &proposal)?;
    if let Some(path) = &script {
        make_executable(path)?;
    }
    // The lock is released while commands run, so hooks stay responsive.
    let outcome = verify::smoke(&proposal, script.as_deref(), &ws.repo_root, &evidence)?;

    let (lock, state) = ws.locked_run()?;
    in_setup(&ws, &state)?;
    let record = SetupCheck {
        proposal_hash: hash,
        at: now_secs(),
        runnable: outcome.problems.is_empty(),
        failing_checks: outcome.failing_checks.clone(),
        evidence_dir: evidence
            .strip_prefix(&ws.feature_dir)
            .unwrap_or(&evidence)
            .display()
            .to_string(),
    };
    let next = RunState {
        setup_check: Some(record.clone()),
        setup_approved_hash: None,
        ..state
    };
    lock.save(&next)?;
    Ok(summarize(&ws, &proposal, &outcome, &record))
}

fn summarize(
    ws: &Workspace,
    proposal: &ProjectVerify,
    outcome: &SmokeOutcome,
    record: &SetupCheck,
) -> String {
    let mut out = vec![format!(
        "setup check (evidence {}/{}):",
        ws.rel(&ws.feature_dir),
        record.evidence_dir
    )];
    out.extend(outcome.results.iter().map(|r| {
        let status = match (&r.spawn_error, r.timed_out, r.passed) {
            (Some(_), _, _) => "CANNOT RUN".to_string(),
            (None, true, _) => "TIMEOUT".to_string(),
            (None, false, true) => "pass".to_string(),
            (None, false, false) => format!("fails (exit {:?})", r.exit_code),
        };
        format!("  {:?} `{}`: {status}", r.kind, r.name)
    }));
    out.extend(outcome.app_notes.iter().map(|n| format!("  app: {n}")));
    if !outcome.problems.is_empty() {
        out.push("NOT USABLE:".into());
        out.extend(outcome.problems.iter().map(|p| format!("  - {p}")));
        out.push(format!("next: fix {PROPOSAL} (or ask the user to start a missing service), then rerun `planvil setup check`."));
        return out.join("\n");
    }
    let unlisted: Vec<&String> = outcome
        .failing_checks
        .iter()
        .filter(|c| !proposal.known_failures.contains(c))
        .collect();
    if unlisted.iter().any(|c| *c == SCRIPT_STEP) {
        out.push(format!(
            "the verify script fails on the current code. If the script is wrong, fix {PROPOSED_SCRIPT}. If \
             the failures are real and pre-existing, leave only those parts out of the script, name them \
             in the summary for the user, and rerun `planvil setup check`."
        ));
        return out.join("\n");
    }
    if !unlisted.is_empty() {
        out.push(format!(
            "failing on the current code: {}. If the command is wrong, fix it. If these failures are real \
             and pre-existing, split the check so only the failing part is isolated, list that part in \
             `known_failures`, and rerun `planvil setup check`.",
            unlisted.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ")
        ));
        return out.join("\n");
    }
    out.push(
        "USABLE. next: summarize the setup for the user (how the app starts, what is checked, known \
         failures) and run `planvil ask --approve-setup \"<summary>\"`, then ask with AskUserQuestion. After approval, \
         run `planvil setup apply`."
            .into(),
    );
    out.join("\n")
}

/// Problems that stop the current proposal from being approved or applied.
pub(super) fn proposal_problems(ws: &Workspace, state: &RunState) -> Result<Vec<String>> {
    let (proposal, hash) = read_proposal(ws)?;
    let Some(check) = state
        .setup_check
        .as_ref()
        .filter(|c| c.proposal_hash == hash)
    else {
        return Ok(vec![format!(
            "{PROPOSAL} has not been checked in its current form; run `planvil setup check`"
        )]);
    };
    let mut problems = Vec::new();
    if !check.runnable {
        problems.push("the last setup check found the setup not usable".to_string());
    }
    problems.extend(
        check
            .failing_checks
            .iter()
            .filter(|c| !proposal.known_failures.contains(c))
            .map(|c| format!("check `{c}` failed during setup but is not in known_failures")),
    );
    Ok(problems)
}

/// Write the approved proposal into `.planvil.json` and move on to the Feature stage.
pub fn apply_setup(cwd: &Path) -> Result<String> {
    let ws = current_workspace(cwd)?;
    let (lock, state) = ws.locked_run()?;
    in_setup(&ws, &state)?;
    let problems = proposal_problems(&ws, &state)?;
    if !problems.is_empty() {
        return Err(refused(format!(
            "cannot apply the setup:\n  - {}",
            problems.join("\n  - ")
        )));
    }
    let (_, hash) = read_proposal(&ws)?;
    if state.setup_approved_hash.as_deref() != Some(hash.as_str()) {
        return Err(refused(
            "the user has not approved this setup in its current form: `planvil ask --approve-setup \"<summary>\"`",
        ));
    }
    let (proposal, _) = read_proposal(&ws)?;
    let raw = std::fs::read_to_string(ws.doc(PROPOSAL))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(config::ConfigError::Parse)?;
    let installed = install_and_save(&ws, &proposal, value)?;
    let reloaded = Workspace::for_branch(&ws.repo_root, &ws.branch)?;
    let next = apply(
        &RunState {
            verify_script_hash: reloaded.verify_script_hash()?,
            ..state
        },
        &Event::SetupApplied,
    )?;
    lock.save(&next)?;
    let moved = if next.stage == Stage::Done {
        "setup complete"
    } else {
        "Setup → Feature"
    };
    let script_note = installed
        .map(|p| format!(" and installed the verify script at {}", ws.rel(&p)))
        .unwrap_or_default();
    Ok(format!(
        "saved the approved setup to .planvil.json{script_note} (commit both with the project).\n{moved}\nnext: {}",
        next_action(&ws, &next)
    ))
}

/// The verify script gets a file of its own: never an existing file (other than the current
/// verify script, when setup is re-run), never a symlink, never the gitignored docs folder.
fn check_script_target(ws: &Workspace, rel: &str) -> Result<()> {
    let choose = "choose another path, for example `scripts/planvil-verify.sh`";
    let in_docs = Path::new(rel).starts_with(&ws.config.docs_root);
    if in_docs {
        return Err(refused(format!(
            "`script` {rel} is inside the gitignored docs folder, so it would not be committed; {choose}"
        )));
    }
    let current = ws.config.verify.as_ref().and_then(|v| v.script.as_deref());
    match std::fs::symlink_metadata(ws.repo_root.join(rel)) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(refused(format!("`script` {rel} is a symlink; {choose}")))
        }
        Ok(meta) if meta.is_file() && current == Some(rel) => Ok(()),
        Ok(_) => Err(refused(format!(
            "`script` {rel} already exists and is not the current verify script; {choose}"
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Install the script and save the config as one step: if saving fails, the script and
/// `.planvil.json` are put back as they were, so setup never ends half applied.
fn install_and_save(
    ws: &Workspace,
    proposal: &ProjectVerify,
    value: serde_json::Value,
) -> Result<Option<PathBuf>> {
    let config_path = ws.repo_root.join(config::CONFIG_FILE);
    let old_config = std::fs::read(&config_path).ok();
    let target = proposal.script.as_ref().map(|s| ws.repo_root.join(s));
    let old_script = target.as_ref().and_then(|t| std::fs::read(t).ok());
    let installed = install_script(ws, proposal)?;
    let saved = config::save_key(&ws.repo_root, "verify", value)
        .and_then(|()| config::load(&ws.repo_root).map(|_| ()));
    if let Err(e) = saved {
        if let Some(t) = &target {
            let _ = match &old_script {
                Some(bytes) => std::fs::write(t, bytes),
                None => std::fs::remove_file(t),
            };
        }
        if let Some(bytes) = &old_config {
            let _ = std::fs::write(&config_path, bytes);
        }
        return Err(e.into());
    }
    Ok(installed)
}

/// Copy the approved proposed script to its configured path. Returns where it went.
fn install_script(ws: &Workspace, proposal: &ProjectVerify) -> Result<Option<PathBuf>> {
    let (Some(rel), Some(source)) = (&proposal.script, proposed_script(ws, proposal)?) else {
        return Ok(None);
    };
    let target = ws.repo_root.join(rel);
    if target.is_dir() {
        return Err(refused(format!(
            "{} is a folder; choose another `script` path",
            ws.rel(&target)
        )));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&source, &target)?;
    make_executable(&target)?;
    Ok(Some(target))
}

/// Mark a script executable for its owner, group and others (no-op off Unix).
fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
