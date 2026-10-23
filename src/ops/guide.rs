use std::path::Path;

use super::gates::{failed_verification, gate_problems};
use super::{Result, Workspace, apply, current_workspace};
use crate::docs;
use crate::machine::Event;
use crate::policy::stage_name;
use crate::state::{RunState, Stage, Status};

// ---------------------------------------------------------------------------------------
// status and next action

pub fn status(cwd: &Path) -> Result<String> {
    let ws = current_workspace(cwd)?;
    match ws.peek()? {
        None => Ok(format!(
            "no planvil run on branch `{}`. Start one with /planvil.",
            ws.branch
        )),
        Some(state) => describe(&ws, &state),
    }
}

/// Human-readable state, gate problems, and the next action.
pub fn describe(ws: &Workspace, state: &RunState) -> Result<String> {
    let mut out = vec![format!(
        "stage: {} · status: {:?}{}",
        stage_name(state.stage),
        state.status,
        state
            .status_reason
            .as_ref()
            .map(|r| format!(" — {r}"))
            .unwrap_or_default()
    )];
    if state.attempts.repair_cycles > 0 {
        out.push(format!(
            "repair cycles: {}/{} · identical failures: {}/{}",
            state.attempts.repair_cycles,
            state.limits.max_repair_cycles,
            state.attempts.equivalent_failures,
            state.limits.max_equivalent_failures
        ));
    }
    if let Some(v) = &state.last_verify {
        out.push(format!(
            "last verification #{}: {} (evidence {})",
            v.run,
            if v.passed {
                "passed".to_string()
            } else {
                format!("failed: {}", v.failing.join(", "))
            },
            v.evidence_dir
        ));
    }
    if let Some(changed) = requirements_changed(ws, state)? {
        out.push(changed);
    }
    if state.status == Status::Active {
        let problems = gate_problems(ws, state)?;
        if !problems.is_empty() && state.stage != Stage::Validate {
            out.push("gate not yet met:".into());
            out.extend(problems.iter().map(|p| format!("  - {p}")));
        }
    }
    out.push(format!("next: {}", next_action(ws, state)));
    Ok(out.join("\n"))
}

pub fn next_action(ws: &Workspace, state: &RunState) -> String {
    let dir = ws.rel(&ws.feature_dir);
    match state.status {
        Status::WaitingForUser => {
            let q = state
                .open_question()
                .map(|q| q.text.as_str())
                .unwrap_or("(question)");
            return format!(
                "waiting for the user to answer: {q}. End your turn. When they reply, record it with \
                 `planvil answer \"<their answer>\"` (add --approved or --rejected for confirmations)."
            );
        }
        Status::Paused => {
            return "the user paused this run. Stop and wait; they resume it with `planvil resume`."
                .into();
        }
        Status::Blocked => {
            return format!(
                "blocked: {}. Report this to the user with the diagnostics. They can fix the cause and \
                 type `planvil resume`.",
                state.status_reason.as_deref().unwrap_or("see status")
            );
        }
        Status::Cancelled => return "the run was cancelled. Do not continue the workflow.".into(),
        Status::Completed => {
            if state.setup_only {
                return "the project setup is saved in .planvil.json. Tell the user to commit it; features start \
                        with /planvil:planvil <branch>."
                    .into();
            }
            return "the run is complete. Run `planvil report` and present the report once.".into();
        }
        Status::Active => {}
    }
    if let Some(step) = repair_step(ws, state) {
        return step;
    }
    match state.stage {
        Stage::Setup => format!(
            "set up how this project runs and is checked (once per repository). Discover it from CI \
             workflows, Makefile/justfile, package scripts, manifests, docker-compose and README; write \
             {dir}/setup.proposed.json; run `planvil setup check` until it is USABLE; then \
             `planvil ask --approve-setup \"<summary>\"`, end your turn, and after approval run `planvil setup apply`."
        ),
        Stage::Feature => {
            let confirmed = state.feature_confirmed_hash.is_some()
                && state.feature_confirmed_hash == ws.doc_hash(docs::FEATURE).ok().flatten();
            if confirmed {
                "FEATURE.md is confirmed. Run `planvil advance`.".into()
            } else {
                format!(
                    "write {dir}/FEATURE.md with the user (goal, scope, out of scope, behavior, acceptance \
                     criteria AC-01…). Then run `planvil ask --confirm-feature \"<summary; is this right?>\"` \
                     and end your turn."
                )
            }
        }
        Stage::Audit => format!(
            "audit the code and write {dir}/AUDIT.md grounded in real files and symbols, then run `planvil advance`."
        ),
        Stage::Plan => format!(
            "write {dir}/PLAN.md (steps, AC → verification map, risks) and {dir}/verify.json (a check or \
             runtime scenario for every AC), then run `planvil advance`."
        ),
        Stage::Decide => {
            let approval = if ws.config.require_plan_approval {
                " This project requires plan approval: `planvil ask --approve-plan \"...\"` once the plan is final."
            } else {
                ""
            };
            format!(
                "record routine decisions in {dir}/DECISIONS.md without asking. Ask the user only about \
                 business rules, destructive scope, or materially different outcomes, one at a time with \
                 `planvil ask \"...\"`.{approval} Then run `planvil advance`."
            )
        }
        Stage::Implement => format!(
            "implement the steps in {dir}/PLAN.md and keep its Status current. When done, run `planvil advance` \
             to enter validation."
        ),
        Stage::Validate => "run `planvil verify` and wait for its result.".into(),
        Stage::Repair => {
            let failing = state
                .last_verify
                .as_ref()
                .map(|v| v.failing.join(", "))
                .unwrap_or_default();
            let evidence = state
                .last_verify
                .as_ref()
                .map(|v| format!("{dir}/{}", v.evidence_dir))
                .unwrap_or_default();
            format!(
                "verification failed ({failing}). Read the logs in {evidence}, find the root cause, update \
                 PLAN.md if the approach changes, fix the code (never weaken checks or criteria), then run \
                 `planvil advance` and `planvil verify`."
            )
        }
        Stage::Done => "run `planvil report`.".into(),
    }
}

/// Message when FEATURE.md no longer matches what the user confirmed.
pub(super) fn requirements_changed(ws: &Workspace, state: &RunState) -> Result<Option<String>> {
    if state.stage <= Stage::Feature || state.status.is_terminal() {
        return Ok(None);
    }
    let current = ws.doc_hash(docs::FEATURE)?;
    Ok((current != state.feature_confirmed_hash).then(|| {
        "FEATURE.md changed since the user confirmed it; the run returns to the Feature stage on the next \
         planvil command"
            .to_string()
    }))
}

/// If requirements changed, rewind to Feature: audit, plan and evidence no longer apply.
pub(super) fn sync_requirements(
    ws: &Workspace,
    state: RunState,
) -> Result<(RunState, Option<String>)> {
    if state.status != Status::Active || requirements_changed(ws, &state)?.is_none() {
        return Ok((state, None));
    }
    let rewound = apply(
        &state,
        &Event::Rewind {
            to: Stage::Feature,
            reason: "FEATURE.md changed".into(),
        },
    )?;
    Ok((
        rewound,
        Some(
            "FEATURE.md changed after confirmation: back to the Feature stage. Audit, plan and evidence are \
             invalidated; review them against the new requirements and confirm FEATURE.md with the user again."
                .into(),
        ),
    ))
}

/// Guidance for each stage while looping after a failed verification.
fn repair_step(ws: &Workspace, state: &RunState) -> Option<String> {
    let failing = failed_verification(state)?;
    let dir = ws.rel(&ws.feature_dir);
    let evidence = state
        .last_verify
        .as_ref()
        .map(|v| format!("{dir}/{}", v.evidence_dir))
        .unwrap_or_default();
    let names: Vec<String> = failing.iter().map(|f| format!("`{f}`")).collect();
    let cycle = format!(
        "Repair cycle {}/{}: verification failed ({}). Evidence: {evidence}.",
        state.attempts.repair_cycles,
        state.limits.max_repair_cycles,
        names.join(", ")
    );
    let step = match state.stage {
        Stage::Feature => {
            let confirmed = state.feature_confirmed_hash.is_some()
                && state.feature_confirmed_hash == ws.doc_hash(docs::FEATURE).ok().flatten();
            if confirmed {
                "Re-read FEATURE.md against the failure. It is unchanged and still confirmed, so run \
                 `planvil advance`. If the failure shows a requirement must change, edit FEATURE.md \
                 first, then ask once with `planvil ask --confirm-feature \"<what changed and why>\"`."
                    .to_string()
            } else {
                "FEATURE.md changed since the user confirmed it. Ask once with `planvil ask --confirm-feature \
                 \"<what changed and why>\"`, end your turn, then record the reply."
                    .to_string()
            }
        }
        Stage::Audit => format!(
            "Read the failing logs and fill `## Failure analysis` in {dir}/AUDIT.md: the root cause of each \
             failing check, found in the logs and the code, not guessed. Then run `planvil advance`."
        ),
        Stage::Plan => format!(
            "Update {dir}/PLAN.md for the fix (steps and Status). Fix the code, not the checks: changing a \
             check that just failed needs the user's approval. Then run `planvil advance`."
        ),
        Stage::Decide => {
            "Record any new decision in DECISIONS.md. Ask the user only if the fix needs a \
                          business or scope decision; otherwise run `planvil advance`."
                .to_string()
        }
        Stage::Implement => {
            "Implement the fix from PLAN.md, then run `planvil advance`.".to_string()
        }
        Stage::Setup | Stage::Validate | Stage::Repair | Stage::Done => return None,
    };
    Some(format!("{cycle} {step}"))
}
