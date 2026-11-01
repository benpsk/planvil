//! Completion (or blocked) report built only from recorded state and evidence.

use std::path::Path;

use crate::docs;
use crate::git;
use crate::ops::{self, OpsError, Workspace};
use crate::policy::stage_name;
use crate::state::{RunState, Stage, Status, VerifyRecord};
use crate::verify::{Outcome, StepResult};

/// Write REPORT.md for a finished or blocked run and return its text.
pub fn report(cwd: &Path) -> ops::Result<String> {
    let ws = Workspace::current(cwd)?
        .ok_or_else(|| OpsError::Refused("not on a branch inside a git repository".into()))?;
    let state = ws
        .peek()?
        .ok_or_else(|| OpsError::Refused(format!("no planvil run on `{}`", ws.branch)))?;
    if matches!(
        state.status,
        Status::Active | Status::WaitingForUser | Status::Paused
    ) {
        return Err(OpsError::Refused(format!(
            "the run is not finished (stage {}, {:?}); a completion report needs passing, current evidence. next: {}",
            stage_name(state.stage),
            state.status,
            ops::next_action(&ws, &state)
        )));
    }
    let text = render(&ws, &state)?;
    std::fs::write(ws.doc(docs::REPORT), &text)?;
    Ok(text)
}

fn render(ws: &Workspace, state: &RunState) -> ops::Result<String> {
    let feature = std::fs::read_to_string(ws.doc(docs::FEATURE)).unwrap_or_default();
    let decisions = std::fs::read_to_string(ws.doc(docs::DECISIONS)).unwrap_or_default();
    let validated = validate_record(state);
    let verified = state
        .last_verify
        .as_ref()
        .filter(|v| v.stage == Stage::Verify);
    let validate_outcome = validated.and_then(|r| load_outcome(ws, r));
    let verify_outcome = verified.and_then(|r| load_outcome(ws, r));
    let results: &[StepResult] = validate_outcome
        .as_ref()
        .map_or(&[], |o| o.results.as_slice());

    let headline = match state.status {
        Status::Completed => completed_headline(
            ws,
            state,
            validate_outcome.as_ref(),
            verify_outcome.as_ref(),
        )?,
        Status::Blocked => format!(
            "**Blocked — not verified.** {}",
            state.status_reason.as_deref().unwrap_or("see run status")
        ),
        Status::Cancelled => "**Cancelled by the user.** Nothing here is verified.".to_string(),
        Status::Active | Status::WaitingForUser | Status::Paused => "**In progress.**".to_string(),
    };

    let mut out = vec![
        format!("# Report: {}", ws.branch),
        String::new(),
        headline,
        String::new(),
        format!(
            "Base `{}` · run `{}` · stage {} · repair cycles {} · validate/verify runs {}",
            state.base_branch,
            state.run_id,
            stage_name(state.stage),
            state.attempts.repair_cycles,
            state.verify_runs
        ),
        String::new(),
        "## Acceptance criteria".into(),
        String::new(),
        "| AC | Criterion | Evidence | Result |".into(),
        "|---|---|---|---|".into(),
    ];
    for (id, text) in criteria_rows(&feature) {
        let proving: Vec<&StepResult> = results
            .iter()
            .filter(|r| r.criteria.contains(&id))
            .collect();
        let evidence = proving
            .iter()
            .map(|r| format!("`{}`", r.name))
            .collect::<Vec<_>>()
            .join(", ");
        let result = match (proving.is_empty(), proving.iter().all(|r| r.passed)) {
            (true, _) => "no evidence",
            (false, true) => "pass",
            (false, false) => "FAIL",
        };
        out.push(format!(
            "| {id} | {text} | {} | {result} |",
            if evidence.is_empty() {
                "—".into()
            } else {
                evidence
            }
        ));
    }

    out.extend(["".into(), "## Changes".into(), String::new()]);
    match git::diff_stat_since(&ws.repo_root, &state.base_branch) {
        Ok(stat) if !stat.is_empty() => out.extend(["```".into(), stat, "```".into()]),
        Ok(_) => out.push("No tracked-file changes against the base branch.".into()),
        Err(e) => out.push(format!("Could not compute the diff: {e}")),
    }
    if let Ok(untracked) = git::untracked_files(&ws.repo_root)
        && !untracked.is_empty()
    {
        out.push(format!(
            "New untracked files: {}",
            untracked
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !state.baseline_dirty.is_empty() {
        out.push(format!(
            "Already modified before the run (user work, left untouched): {}",
            state
                .baseline_dirty
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    out.extend(["".into(), "## Decisions".into(), String::new()]);
    out.push(section(&decisions, "Decided").unwrap_or_else(|| "None recorded.".into()));
    let answered: Vec<String> = state
        .questions
        .iter()
        .filter_map(|q| {
            q.answer.as_ref().map(|a| {
                let said = a
                    .user_reply
                    .as_deref()
                    .map(|r| format!(" (user's words: \"{}\")", r.replace('\n', " ")))
                    .unwrap_or_default();
                format!("- Q{}: {} → {}{said}", q.id, q.text, a.text)
            })
        })
        .collect();
    if !answered.is_empty() {
        out.extend(["".into(), "Questions answered by the user:".into()]);
        out.extend(answered);
    }

    out.extend(["".into(), "## Evidence".into()]);
    out.extend(evidence_section(
        ws,
        "Validate: this feature's checks",
        validated,
        validate_outcome.as_ref(),
    ));
    out.extend(evidence_section(
        ws,
        "Verify: the whole app",
        verified,
        verify_outcome.as_ref(),
    ));
    out.extend([
        String::new(),
        "## Limits of this evidence".into(),
        String::new(),
        "Checks prove only what they exercise. Behavior not covered by a listed check or scenario is not verified."
            .into(),
    ]);
    Ok(out.join("\n") + "\n")
}

/// The Validate record behind the acceptance-criteria table: the pass Verify built on, or
/// the latest validation result.
fn validate_record(state: &RunState) -> Option<&VerifyRecord> {
    state.validated.as_ref().or(state
        .last_verify
        .as_ref()
        .filter(|v| v.stage == Stage::Validate))
}

fn evidence_section(
    ws: &Workspace,
    title: &str,
    record: Option<&VerifyRecord>,
    outcome: Option<&Outcome>,
) -> Vec<String> {
    let mut out = vec![String::new(), format!("### {title}"), String::new()];
    let (Some(v), Some(o)) = (record, outcome) else {
        out.push("No evidence recorded.".into());
        return out;
    };
    out.push(format!(
        "Run #{} · {} · evidence `{}/{}`",
        v.run,
        if v.passed { "passed" } else { "FAILED" },
        ws.doc("").display().to_string().trim_end_matches('/'),
        v.evidence_dir
    ));
    out.push(String::new());
    out.push("| Step | Kind | Command | Exit | Duration | Logs |".into());
    out.push("|---|---|---|---|---|---|".into());
    for r in &o.results {
        let exit = r.exit_code.map_or_else(
            || {
                if r.timed_out {
                    "timeout".to_string()
                } else {
                    "—".to_string()
                }
            },
            |c| c.to_string(),
        );
        out.push(format!(
            "| {} | {:?} | `{}` | {exit} | {} ms | `{}`, `{}` |",
            r.name,
            r.kind,
            r.argv.join(" ").replace('|', "\\|"),
            r.ended_ms.saturating_sub(r.started_ms),
            r.stdout,
            r.stderr
        ));
    }
    if !o.app_notes.is_empty() {
        out.push(format!("\nApp: {}", o.app_notes.join("; ")));
    }
    out
}

/// "Completed" only when the recorded evidence exists and still matches the code and the
/// confirmed requirements; otherwise say exactly why the evidence no longer holds.
fn completed_headline(
    ws: &Workspace,
    state: &RunState,
    validate_outcome: Option<&Outcome>,
    verify_outcome: Option<&Outcome>,
) -> ops::Result<String> {
    let Some(record) = state.last_verify.as_ref().filter(|v| v.passed) else {
        return Ok(
            "**Completed, but no passing verification is recorded.** Treat this as unverified."
                .into(),
        );
    };
    // Runs finished before Validate and Verify were split end on a validation pass.
    let split = record.stage == Stage::Verify;
    if validate_outcome.is_none() {
        let path = validate_record(state).map_or("?", |v| v.evidence_dir.as_str());
        return Ok(format!(
            "**Completed, but the evidence is missing.** `{path}/result.json` cannot be read; treat this as unverified."
        ));
    }
    let requirements_same = ws.doc_hash(docs::FEATURE)? == state.feature_confirmed_hash;
    if ws.fingerprint()? != record.fingerprint || !requirements_same {
        return Ok(
            "**Completed earlier, but the code or requirements changed after verification.** The evidence \
             below is stale; start a new run to re-verify."
                .into(),
        );
    }
    let whole_app_checked = verify_outcome.is_some_and(|o| {
        o.results
            .iter()
            .any(|r| r.kind != crate::verify::StepKind::Preflight)
    });
    Ok(if split && !whole_app_checked {
        "**Completed.** Every acceptance criterion passed against the current code, but no whole-app \
         check is set up (no verify script); run `/planvil:planvil init` to add one."
            .into()
    } else if split {
        "**Completed.** Every acceptance criterion passed, and the whole app passed its verify script, \
         against the current code."
            .into()
    } else {
        "**Completed.** Every acceptance criterion passed against the current code.".into()
    })
}

fn load_outcome(ws: &Workspace, record: &VerifyRecord) -> Option<Outcome> {
    let raw = std::fs::read(
        ws.feature_dir
            .join(&record.evidence_dir)
            .join("result.json"),
    )
    .ok()?;
    serde_json::from_slice(&raw).ok()
}

/// `(id, criterion text)` from FEATURE.md's criteria table.
fn criteria_rows(feature: &str) -> Vec<(String, String)> {
    let body = docs::sections(feature)
        .into_iter()
        .find(|s| s.heading.eq_ignore_ascii_case("Acceptance criteria"))
        .map(|s| docs::strip_comments(&s.body))
        .unwrap_or_default();
    docs::ac_ids(&body)
        .into_iter()
        .map(|id| {
            let text = body
                .lines()
                .find(|l| l.contains(&id))
                .map(|l| {
                    l.split('|')
                        .map(str::trim)
                        .filter(|c| !c.is_empty() && *c != id)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            (id, text)
        })
        .collect()
}

fn section(md: &str, heading: &str) -> Option<String> {
    docs::sections(md)
        .into_iter()
        .find(|s| s.heading.eq_ignore_ascii_case(heading))
        .map(|s| docs::strip_comments(&s.body).trim().to_string())
        .filter(|b| !b.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_criteria_rows() {
        let md = "## Acceptance criteria\n| ID | Criterion |\n|---|---|\n| AC-01 | Link is emailed |\n| AC-02 | Expires |\n";
        assert_eq!(
            criteria_rows(md),
            vec![
                ("AC-01".to_string(), "Link is emailed".to_string()),
                ("AC-02".to_string(), "Expires".to_string())
            ]
        );
    }
}
