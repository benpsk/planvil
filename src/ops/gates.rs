use std::path::Path;

use super::guide::{next_action, sync_requirements};
use super::{Result, Workspace, apply, current_workspace, refused};
use crate::docs;
use crate::machine::Event;
use crate::policy::stage_name;
use crate::state::{RunState, Stage, Status};

// ---------------------------------------------------------------------------------------
// gates

pub(super) fn gate_problems(ws: &Workspace, state: &RunState) -> Result<Vec<String>> {
    let problems = match state.stage {
        Stage::Setup if ws.config.verify.is_some() => Vec::new(),
        Stage::Setup => vec![
            "project verification is not set up yet: write setup.proposed.json, run `planvil setup check`, \
             get approval, then `planvil setup apply`"
                .to_string(),
        ],
        Stage::Feature => {
            let mut p = docs::check_feature(&ws.read_doc(docs::FEATURE)?);
            let current = ws.doc_hash(docs::FEATURE)?;
            if p.is_empty() && (current.is_none() || current != state.feature_confirmed_hash) {
                p.push(
                    "FEATURE.md is not confirmed by the user at its current content \
                     (`planvil ask --confirm-feature \"...\"`)"
                        .into(),
                );
            }
            p
        }
        Stage::Audit => {
            let audit = ws.read_doc(docs::AUDIT)?;
            let mut p = docs::check_audit(&audit, &ws.repo_root);
            if let Some(failing) = failed_verification(state) {
                p.extend(docs::check_failure_analysis(&audit, failing));
            }
            p
        }
        Stage::Plan => {
            let criteria = ws.criteria()?;
            let mut p = docs::check_plan(&ws.read_doc(docs::PLAN)?, &criteria);
            match ws.verify_plan() {
                Ok(plan) => p.extend(plan.coverage_problems(&criteria)),
                Err(e) => p.push(e.to_string()),
            }
            p
        }
        Stage::Decide => {
            let mut p = docs::check_decisions(&ws.read_doc(docs::DECISIONS)?);
            if let Some(q) = state.open_question() {
                p.push(format!("question Q{} is unanswered", q.id));
            }
            if ws.config.require_plan_approval {
                let plan = ws.plan_hash()?;
                if plan.is_none() || plan != state.plan_approved_hash {
                    p.push("PLAN.md and verify.json are not approved by the user at their current content (`planvil ask --approve-plan`)".into());
                }
            }
            p
        }
        Stage::Implement => {
            let mut p = match ws.verify_plan() {
                Ok(plan) => plan.coverage_problems(&ws.criteria()?),
                Err(e) => vec![e.to_string()],
            };
            p.extend(unchanged_since_failure(ws, state)?);
            p
        }
        Stage::Repair => unchanged_since_failure(ws, state)?,
        Stage::Validate => vec!["validation advances only through `planvil verify`".into()],
        Stage::Done => vec!["the run is done".into()],
    };
    Ok(problems)
}

/// Names of the checks that failed, when the run is looping after a failed verification.
pub(super) fn failed_verification(state: &RunState) -> Option<&[String]> {
    state
        .last_verify
        .as_ref()
        .filter(|v| !v.passed)
        .map(|v| v.failing.as_slice())
}

/// In a repair loop, the code must actually change before it is verified again.
fn unchanged_since_failure(ws: &Workspace, state: &RunState) -> Result<Vec<String>> {
    let Some(v) = state.last_verify.as_ref().filter(|v| !v.passed) else {
        return Ok(Vec::new());
    };
    if ws.fingerprint()? == v.fingerprint {
        return Ok(vec![
            "nothing changed since the failed verification; fix the cause before revalidating"
                .into(),
        ]);
    }
    Ok(Vec::new())
}

pub fn advance(cwd: &Path) -> Result<String> {
    let ws = current_workspace(cwd)?;
    let (lock, state) = ws.locked_run()?;
    let (state, rewound) = sync_requirements(&ws, state)?;
    if let Some(msg) = rewound {
        lock.save(&state)?;
        return Err(refused(msg));
    }
    if state.status != Status::Active {
        return Err(refused(next_action(&ws, &state)));
    }
    let problems = gate_problems(&ws, &state)?;
    if !problems.is_empty() {
        return Err(refused(format!(
            "gate for {} not met:\n{}\nnext: {}",
            stage_name(state.stage),
            problems
                .iter()
                .map(|p| format!("  - {p}"))
                .collect::<Vec<_>>()
                .join("\n"),
            next_action(&ws, &state)
        )));
    }
    let from = state.stage;
    let next = apply(&state, &Event::Advance)?;
    lock.save(&next)?;
    Ok(format!(
        "{} → {}\nnext: {}",
        stage_name(from),
        stage_name(next.stage),
        next_action(&ws, &next)
    ))
}
