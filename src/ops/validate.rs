//! `planvil validate` and `planvil verify`: run the controlled runner and turn its outcome
//! into a transition.
//!
//! - Validate runs the feature's own checks (`validate.json`): does this feature work?
//! - Verify runs the project's verify script: does the whole app still work?
//!
//! Both feed the same repair loop and budget.

use std::collections::BTreeMap;
use std::path::Path;

use super::guide::{next_action, sync_requirements};
use super::{Result, Workspace, apply, current_workspace, refused};
use crate::fingerprint;
use crate::machine::Event;
use crate::state::{Failure, RunState, Stage, Status, VerifyRecord};
use crate::store;
use crate::util::now_secs;
use crate::verify::{self, Verdict};

const STALE: &str = "stale: files changed during verification";
const MAX_LISTED_PATHS: usize = 10;

type Snapshot = BTreeMap<String, String>;

/// Which of the two checking stages is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    Validate,
    Verify,
}

impl Pass {
    fn stage(self) -> Stage {
        match self {
            Pass::Validate => Stage::Validate,
            Pass::Verify => Stage::Verify,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Pass::Validate => "validation",
            Pass::Verify => "verification",
        }
    }
}

/// Run the feature's own checks (Validate stage).
pub fn validate(cwd: &Path) -> Result<String> {
    run_pass(cwd, Pass::Validate)
}

/// Run the project's verify script on the whole app (Verify stage).
pub fn verify(cwd: &Path) -> Result<String> {
    run_pass(cwd, Pass::Verify)
}

fn run_pass(cwd: &Path, pass: Pass) -> Result<String> {
    let ws = current_workspace(cwd)?;
    let (plan, criteria, before, run_no) = claim(&ws, pass)?;

    // The lock is released while commands run, so hooks stay responsive.
    let evidence = verify::evidence_dir(&store::run_dir(&ws.feature_dir), run_no);
    let outcome = verify::run(&plan, &ws.repo_root, &evidence)?;
    let after = ws.snapshot()?;

    let (lock, state) = ws.locked_run()?;
    if state.verify_runs != run_no || state.stage != pass.stage() || state.status != Status::Active
    {
        return Err(refused(format!(
            "the run changed while {} was running; results discarded. Rerun it.",
            pass.noun()
        )));
    }
    let evidence_rel = evidence
        .strip_prefix(&ws.feature_dir)
        .unwrap_or(&evidence)
        .display()
        .to_string();
    let changed = fingerprint::changed(&before, &after);
    let ran = Ran {
        pass,
        plan: &plan,
        outcome: &outcome,
        changed: &changed,
        fingerprint: fingerprint::digest_of(&before),
        run_no,
        evidence_rel: &evidence_rel,
    };
    let (next, headline) = record(&state, &ran)?;
    lock.save(&next)?;
    Ok(format!(
        "{headline}\nevidence: {}/{evidence_rel}\n{}\nnext: {}",
        ws.rel(&ws.feature_dir),
        summarize(&outcome, &criteria),
        next_action(&ws, &next)
    ))
}

/// Check preconditions under the lock and claim a run number.
fn claim(ws: &Workspace, pass: Pass) -> Result<(verify::Plan, Vec<String>, Snapshot, u32)> {
    let (lock, state) = ws.locked_run()?;
    let (state, rewound) = sync_requirements(ws, state)?;
    if let Some(msg) = rewound {
        lock.save(&state)?;
        return Err(refused(msg));
    }
    if state.status != Status::Active || state.stage != pass.stage() {
        return Err(refused(wrong_stage(ws, &state, pass)));
    }
    if let Some(why) = time_exceeded(&state, now_secs()) {
        lock.save(&apply(
            &state,
            &Event::Block {
                reason: why.clone(),
            },
        )?)?;
        return Err(refused(why));
    }
    let (plan, criteria) = match pass {
        Pass::Validate => {
            let criteria = ws.criteria()?;
            let plan = ws.validate_plan()?;
            let gaps = plan.coverage_problems(&criteria);
            if !gaps.is_empty() {
                return Err(refused(format!(
                    "{} cannot prove the feature:\n  - {}",
                    ws.validate_file(),
                    gaps.join("\n  - ")
                )));
            }
            if let Some(problem) = weakened_check(ws, &state, &plan)? {
                return Err(refused(problem));
            }
            (plan, criteria)
        }
        Pass::Verify => {
            if let Some(why) = script_changed(ws, &state)? {
                lock.save(&apply(
                    &state,
                    &Event::Block {
                        reason: why.clone(),
                    },
                )?)?;
                return Err(refused(why));
            }
            let validated = state.validated.as_ref().map(|v| v.fingerprint.clone());
            if validated != Some(ws.fingerprint()?) {
                let reason = "the code changed after validation passed".to_string();
                lock.save(&apply(&state, &Event::Revalidate { reason })?)?;
                return Err(refused(
                    "the code changed after validation passed, so the run is back in Validate. Run \
                     `planvil validate` again, then `planvil verify`.",
                ));
            }
            (ws.verify_plan()?, Vec::new())
        }
    };
    let run_no = state.verify_runs.saturating_add(1);
    lock.save(&RunState {
        verify_runs: run_no,
        ..state
    })?;
    Ok((plan, criteria, ws.snapshot()?, run_no))
}

/// The verify script differs from the one the run started with (or the user accepted).
/// The text checks on shell commands are best effort; this is the reliable guard.
fn script_changed(ws: &Workspace, state: &RunState) -> Result<Option<String>> {
    let Some(expected) = &state.verify_script_hash else {
        return Ok(None);
    };
    if ws.verify_script_hash()?.as_ref() == Some(expected) {
        return Ok(None);
    }
    Ok(Some(
        "the verify script changed during this run (only the user may change it). Tell the user; if \
         they made the change, they type `planvil resume` to accept it"
            .into(),
    ))
}

fn wrong_stage(ws: &Workspace, state: &RunState, pass: Pass) -> String {
    let next = next_action(ws, state);
    match (pass, state.stage, state.status) {
        (Pass::Verify, Stage::Validate, Status::Active) => format!(
            "this run is in Validate: `planvil validate` checks this feature first, then \
             `planvil verify` checks the whole app. next: {next}"
        ),
        (Pass::Validate, Stage::Verify, Status::Active) => {
            format!("validation already passed; the run is in Verify. next: {next}")
        }
        _ => format!(
            "{} runs in the {} stage. next: {next}",
            pass.noun(),
            crate::policy::stage_name(pass.stage())
        ),
    }
}

/// A step that failed last time and has since been changed or removed needs the user's
/// approval of the current plan and checks.
fn weakened_check(ws: &Workspace, state: &RunState, plan: &verify::Plan) -> Result<Option<String>> {
    let current: BTreeMap<&str, String> = plan
        .steps
        .iter()
        .map(|(_, s)| (s.name.as_str(), verify::step_hash(s)))
        .collect();
    let altered: Vec<&String> = state
        .failed_steps
        .iter()
        .filter(|(name, hash)| current.get(name.as_str()) != Some(*hash))
        .map(|(name, _)| name)
        .collect();
    if altered.is_empty()
        || (state.plan_approved_hash.is_some() && state.plan_approved_hash == ws.plan_hash()?)
    {
        return Ok(None);
    }
    let names: Vec<String> = altered.iter().map(|n| format!("`{n}`")).collect();
    Ok(Some(format!(
        "check {} failed last time and has since been changed or removed. Fix the code instead, or, if the \
         check itself was wrong, get the user's approval: `planvil ask --approve-plan \"<what changed and why>\"`",
        names.join(", ")
    )))
}

/// What one finished run produced.
struct Ran<'a> {
    pass: Pass,
    plan: &'a verify::Plan,
    outcome: &'a verify::Outcome,
    changed: &'a [String],
    fingerprint: String,
    run_no: u32,
    evidence_rel: &'a str,
}

/// The next state and a headline for a finished run.
fn record(state: &RunState, ran: &Ran) -> Result<(RunState, String)> {
    let stale = !ran.changed.is_empty();
    let failing: Vec<String> = match (&ran.outcome.verdict, stale) {
        (_, true) => vec![STALE.to_string()],
        (Verdict::Passed, false) => Vec::new(),
        (Verdict::Failed { failing }, false) => failing.clone(),
        (Verdict::Unavailable { reason }, false) => vec![format!("unavailable: {reason}")],
    };
    let was_stale = state
        .last_verify
        .as_ref()
        .is_some_and(|v| v.failing.first().is_some_and(|f| f == STALE));
    let record = VerifyRecord {
        stage: ran.pass.stage(),
        run: ran.run_no,
        at: now_secs(),
        fingerprint: ran.fingerprint.clone(),
        passed: failing.is_empty(),
        evidence_dir: ran.evidence_rel.to_string(),
        failing: failing.clone(),
    };
    let with_record = RunState {
        last_verify: Some(record.clone()),
        ..state.clone()
    };
    let noun = ran.pass.noun();
    let listed = list_paths(ran.changed);
    let result = match (&ran.outcome.verdict, stale) {
        (_, true) if was_stale => {
            let reason = format!(
                "{noun} keeps changing files ({listed}); add generated files to .gitignore or fix the check"
            );
            let blocked = apply(
                &with_record,
                &Event::Block {
                    reason: reason.clone(),
                },
            )?;
            (blocked, format!("{noun} BLOCKED: {reason}"))
        }
        (_, true) => (
            with_record,
            format!(
                "{noun} discarded: files changed while it ran ({listed}). Do not edit while it runs. \
                 If a check generates these files, add them to .gitignore. Then rerun it."
            ),
        ),
        (Verdict::Passed, false) => passed(with_record, record, ran)?,
        (Verdict::Failed { failing }, false) => failed(with_record, ran, failing)?,
        (Verdict::Unavailable { reason }, false) => (
            apply(
                &with_record,
                &Event::Block {
                    reason: format!("{noun} unavailable: {reason}"),
                },
            )?,
            format!("{noun} UNAVAILABLE (run blocked): {reason}"),
        ),
    };
    Ok(result)
}

fn passed(state: RunState, record: VerifyRecord, ran: &Ran) -> Result<(RunState, String)> {
    Ok(match ran.pass {
        Pass::Validate => (
            apply(
                &RunState {
                    failed_steps: BTreeMap::new(),
                    validated: Some(record),
                    ..state
                },
                &Event::ValidationPassed,
            )?,
            "validation PASSED for every acceptance criterion.".to_string(),
        ),
        Pass::Verify => {
            let checked = ran
                .plan
                .steps
                .iter()
                .any(|(k, _)| *k != verify::StepKind::Preflight);
            let headline = if checked {
                "verification PASSED: the whole app still works."
            } else {
                "verification PASSED, but there was nothing to verify: this project has no verify \
                 script or project checks. Tell the user; after this run, `/planvil:planvil init` sets one up."
            };
            (
                apply(&state, &Event::VerificationPassed)?,
                headline.to_string(),
            )
        }
    })
}

fn failed(state: RunState, ran: &Ran, failing: &[String]) -> Result<(RunState, String)> {
    let mut sorted = failing.to_vec();
    sorted.sort();
    let noun = ran.pass.noun();
    let failure = Failure {
        signature: format!("{noun}:{}", sorted.join("|")),
        summary: format!("{noun} failing: {}", sorted.join(", ")),
    };
    let (failed_steps, event) = match ran.pass {
        // The feature's checks are the agent's own; changing one that failed needs approval.
        Pass::Validate => (
            ran.plan
                .steps
                .iter()
                .filter(|(_, s)| failing.contains(&s.name))
                .map(|(_, s)| (s.name.clone(), verify::step_hash(s)))
                .collect(),
            Event::ValidationFailed(failure),
        ),
        // The project's checks are protected files the agent cannot change.
        Pass::Verify => (BTreeMap::new(), Event::VerificationFailed(failure)),
    };
    let next = apply(
        &RunState {
            failed_steps,
            ..state
        },
        &event,
    )?;
    let headline = match ran.pass {
        Pass::Validate => format!("validation FAILED: {}", sorted.join(", ")),
        Pass::Verify => format!(
            "verification FAILED: the feature works, but the whole-app check broke ({})",
            sorted.join(", ")
        ),
    };
    Ok((next, headline))
}

fn list_paths(paths: &[String]) -> String {
    let shown: Vec<String> = paths
        .iter()
        .take(MAX_LISTED_PATHS)
        .map(|p| format!("`{p}`"))
        .collect();
    let more = paths.len().saturating_sub(MAX_LISTED_PATHS);
    if more > 0 {
        format!("{} and {more} more", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

fn summarize(outcome: &verify::Outcome, criteria: &[String]) -> String {
    let rows: Vec<String> = outcome
        .results
        .iter()
        .map(|r| {
            let exit = r.exit_code.map_or_else(
                || {
                    if r.timed_out {
                        "timeout".into()
                    } else {
                        "none".into()
                    }
                },
                |c| c.to_string(),
            );
            let proves = if r.criteria.is_empty() {
                String::new()
            } else {
                format!("[{}]", r.criteria.join(", "))
            };
            format!(
                "  {} {:?} `{}` exit={exit} {}ms {proves}",
                if r.passed { "PASS" } else { "FAIL" },
                r.kind,
                r.name,
                r.ended_ms.saturating_sub(r.started_ms)
            )
        })
        .collect();
    let proven = criteria
        .iter()
        .filter(|id| {
            outcome
                .results
                .iter()
                .any(|r| r.passed && r.criteria.contains(id))
        })
        .count();
    let app = if outcome.app_notes.is_empty() {
        String::new()
    } else {
        format!("\n  app: {}", outcome.app_notes.join("; "))
    };
    let proven = if criteria.is_empty() {
        String::new()
    } else {
        format!("\n  criteria proven: {proven}/{}", criteria.len())
    };
    format!("{}{proven}{app}", rows.join("\n"))
}

/// Reason the run's wall-clock budget is exhausted, if it is.
pub fn time_exceeded(state: &RunState, now: u64) -> Option<String> {
    let minutes = state.limits.max_run_minutes?;
    let elapsed = now.saturating_sub(state.created_at) / 60;
    (elapsed >= u64::from(minutes)).then(|| format!("run time limit of {minutes} minutes reached"))
}
