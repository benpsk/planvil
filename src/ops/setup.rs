//! Setup stage: the agent proposes how to run and check the project, planvil proves the
//! proposal runs, the user approves it once, and planvil writes it to `.planvil.json`.

use std::path::Path;

use super::guide::next_action;
use super::{Result, Workspace, apply, current_workspace, refused};
use crate::config;
use crate::machine::Event;
use crate::state::{RunState, SetupCheck, Stage, Status};
use crate::store;
use crate::util::{file_hash, now_secs};
use crate::verify::{self, ProjectVerify, SmokeOutcome};

/// The agent's proposal, inside the feature folder (writable during Setup).
pub const PROPOSAL: &str = "setup.proposed.json";

/// Hash of the proposal file, if it exists.
pub(super) fn proposal_hash(ws: &Workspace) -> Result<Option<String>> {
    Ok(file_hash(&ws.doc(PROPOSAL))?)
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
    let hash = file_hash(&path)?.ok_or_else(|| refused(format!("{PROPOSAL} disappeared")))?;
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
    // The lock is released while commands run, so hooks stay responsive.
    let outcome = verify::smoke(&proposal, &ws.repo_root, &evidence)?;

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
    let raw = std::fs::read_to_string(ws.doc(PROPOSAL))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(config::ConfigError::Parse)?;
    config::save_key(&ws.repo_root, "verify", value)?;
    config::load(&ws.repo_root)?;
    let next = apply(&state, &Event::SetupApplied)?;
    lock.save(&next)?;
    let moved = if next.stage == Stage::Done {
        "setup complete"
    } else {
        "Setup → Feature"
    };
    Ok(format!(
        "saved the approved setup to .planvil.json (commit it with the project).\n{moved}\nnext: {}",
        next_action(&ws, &next)
    ))
}
