//! `planvil setup check`: try a proposed project setup once.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{
    AppStart, FeatureVerify, Plan, ProjectVerify, RunCtx, StepKind, StepResult, Verdict,
    leftover_problem, run_step, start_app, unavailable,
};
use crate::{process, redact};

/// Result of trying a proposed project setup once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmokeOutcome {
    /// Why the setup cannot be used as is (missing tools, services down, app not ready).
    pub problems: Vec<String>,
    /// Checks that ran but exited non-zero on the current code.
    pub failing_checks: Vec<String>,
    pub results: Vec<StepResult>,
    pub app_notes: Vec<String>,
}

/// Run preflight and every check, then start the app, wait for readiness and stop it.
/// Writes logs and `smoke.json` into `evidence_dir`.
pub fn smoke(
    project: &ProjectVerify,
    repo_root: &Path,
    evidence_dir: &Path,
) -> std::io::Result<SmokeOutcome> {
    fs::create_dir_all(evidence_dir)?;
    let plan = match Plan::new(project, &FeatureVerify::default()) {
        Ok(plan) => plan,
        Err(e) => {
            return Ok(SmokeOutcome {
                problems: vec![e],
                failing_checks: vec![],
                results: vec![],
                app_notes: vec![],
            });
        }
    };
    let ctx = RunCtx {
        repo_root,
        evidence_dir,
        env: plan.env.clone(),
        secrets: redact::secrets(&plan.env),
        registry: process::registry_for(evidence_dir.parent().unwrap_or(evidence_dir)),
    };
    let mut problems: Vec<String> = leftover_problem(&ctx.registry)?.into_iter().collect();
    let mut results = Vec::new();
    for (i, (kind, step)) in plan.steps.iter().enumerate() {
        if !problems.is_empty() {
            break;
        }
        let r = run_step(i, *kind, step, &ctx)?;
        let unusable = r.spawn_error.is_some() || (*kind == StepKind::Preflight && !r.passed);
        if let (true, Verdict::Unavailable { reason }) = (unusable, unavailable(&r, evidence_dir)) {
            problems.push(reason);
        }
        if r.timed_out {
            problems.push(format!(
                "`{}` timed out after {}s; raise timeout_secs or narrow the check",
                r.name, step.timeout_secs
            ));
        }
        results.push(r);
    }
    let mut app_notes = Vec::new();
    if let (true, Some(app)) = (problems.is_empty(), plan.app.as_ref()) {
        match start_app(app, &ctx)? {
            AppStart::Ready(mut proc, note) => {
                app_notes.push(note);
                app_notes.push(proc.stop());
            }
            AppStart::NotReady(note) => {
                problems.push(format!("the app did not become ready: {note}"))
            }
            AppStart::Unavailable(reason) => problems.push(reason),
        }
        for log in ["app.stdout.log", "app.stderr.log"] {
            redact::redact_file(&evidence_dir.join(log), &ctx.secrets)?;
        }
    }
    let failing_checks = results
        .iter()
        .filter(|r| {
            r.kind == StepKind::Check && !r.passed && r.spawn_error.is_none() && !r.timed_out
        })
        .map(|r| r.name.clone())
        .collect();
    let outcome = SmokeOutcome {
        problems,
        failing_checks,
        results,
        app_notes,
    };
    fs::write(
        evidence_dir.join("smoke.json"),
        serde_json::to_vec_pretty(&outcome).map_err(std::io::Error::other)?,
    )?;
    Ok(outcome)
}
