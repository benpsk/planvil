//! `planvil verify`: run the controlled verifier and turn its outcome into a transition.

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

pub fn verify(cwd: &Path) -> Result<String> {
    let ws = current_workspace(cwd)?;
    let (plan, criteria, before, run_no) = claim(&ws)?;

    // The lock is released while commands run, so hooks stay responsive.
    let evidence = verify::evidence_dir(&store::run_dir(&ws.feature_dir), run_no);
    let outcome = verify::run(&plan, &ws.repo_root, &evidence)?;
    let after = ws.snapshot()?;

    let (lock, state) = ws.locked_run()?;
    if state.verify_runs != run_no
        || state.stage != Stage::Validate
        || state.status != Status::Active
    {
        return Err(refused(
            "the run changed while verification was running; results discarded. Rerun `planvil verify`.",
        ));
    }
    let evidence_rel = evidence
        .strip_prefix(&ws.feature_dir)
        .unwrap_or(&evidence)
        .display()
        .to_string();
    let changed = fingerprint::changed(&before, &after);
    let (next, headline) = record(
        &state,
        &plan,
        &outcome,
        &changed,
        fingerprint::digest_of(&before),
        run_no,
        &evidence_rel,
    )?;
    lock.save(&next)?;
    Ok(format!(
        "{headline}\nevidence: {}/{evidence_rel}\n{}\nnext: {}",
        ws.rel(&ws.feature_dir),
        summarize(&outcome, &criteria),
        next_action(&ws, &next)
    ))
}

/// Check preconditions under the lock and claim a verification run number.
fn claim(ws: &Workspace) -> Result<(verify::Plan, Vec<String>, Snapshot, u32)> {
    let (lock, state) = ws.locked_run()?;
    let (state, rewound) = sync_requirements(ws, state)?;
    if let Some(msg) = rewound {
        lock.save(&state)?;
        return Err(refused(msg));
    }
    if state.status != Status::Active || state.stage != Stage::Validate {
        return Err(refused(format!(
            "verification runs in the Validate stage. next: {}",
            next_action(ws, &state)
        )));
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
    let criteria = ws.criteria()?;
    let plan = ws.verify_plan()?;
    let gaps = plan.coverage_problems(&criteria);
    if !gaps.is_empty() {
        return Err(refused(format!(
            "verify.json cannot prove the feature:\n  - {}",
            gaps.join("\n  - ")
        )));
    }
    if let Some(problem) = weakened_check(ws, &state, &plan)? {
        return Err(refused(problem));
    }
    let run_no = state.verify_runs.saturating_add(1);
    lock.save(&RunState {
        verify_runs: run_no,
        ..state
    })?;
    Ok((plan, criteria, ws.snapshot()?, run_no))
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

/// The next state and a headline for a finished verification.
fn record(
    state: &RunState,
    plan: &verify::Plan,
    outcome: &verify::Outcome,
    changed: &[String],
    fingerprint: String,
    run_no: u32,
    evidence_rel: &str,
) -> Result<(RunState, String)> {
    let stale = !changed.is_empty();
    let failing: Vec<String> = match (&outcome.verdict, stale) {
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
        run: run_no,
        at: now_secs(),
        fingerprint,
        passed: failing.is_empty(),
        evidence_dir: evidence_rel.to_string(),
        failing: failing.clone(),
    };
    let with_record = RunState {
        last_verify: Some(record),
        ..state.clone()
    };
    let listed = list_paths(changed);
    let result = match (&outcome.verdict, stale) {
        (_, true) if was_stale => {
            let reason = format!(
                "verification keeps changing files ({listed}); add generated files to .gitignore or fix the check"
            );
            (
                apply(
                    &with_record,
                    &Event::Block {
                        reason: reason.clone(),
                    },
                )?,
                format!("verification BLOCKED: {reason}"),
            )
        }
        (_, true) => (
            with_record,
            format!(
                "verification discarded: files changed while it ran ({listed}). Do not edit during verification. \
                 If a check generates these files, add them to .gitignore. Then rerun `planvil verify`."
            ),
        ),
        (Verdict::Passed, false) => (
            apply(
                &RunState {
                    failed_steps: BTreeMap::new(),
                    ..with_record
                },
                &Event::ValidationPassed,
            )?,
            "verification PASSED for every acceptance criterion.".to_string(),
        ),
        (Verdict::Failed { failing }, false) => {
            let mut sorted = failing.clone();
            sorted.sort();
            let failure = Failure {
                signature: sorted.join("|"),
                summary: format!("failing: {}", sorted.join(", ")),
            };
            let failed_steps = plan
                .steps
                .iter()
                .filter(|(_, s)| failing.contains(&s.name))
                .map(|(_, s)| (s.name.clone(), verify::step_hash(s)))
                .collect();
            let next = apply(
                &RunState {
                    failed_steps,
                    ..with_record
                },
                &Event::ValidationFailed(failure),
            )?;
            (next, format!("verification FAILED: {}", sorted.join(", ")))
        }
        (Verdict::Unavailable { reason }, false) => (
            apply(
                &with_record,
                &Event::Block {
                    reason: format!("verification unavailable: {reason}"),
                },
            )?,
            format!("verification UNAVAILABLE (run blocked): {reason}"),
        ),
    };
    Ok(result)
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
    format!(
        "{}\n  criteria proven: {proven}/{}{app}",
        rows.join("\n"),
        criteria.len()
    )
}

/// Reason the run's wall-clock budget is exhausted, if it is.
pub fn time_exceeded(state: &RunState, now: u64) -> Option<String> {
    let minutes = state.limits.max_run_minutes?;
    let elapsed = now.saturating_sub(state.created_at) / 60;
    (elapsed >= u64::from(minutes)).then(|| format!("run time limit of {minutes} minutes reached"))
}
