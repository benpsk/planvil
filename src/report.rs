//! Completion (or blocked) report built only from recorded state and evidence.

use std::path::Path;

use crate::docs;
use crate::git;
use crate::ops::{self, OpsError, Workspace};
use crate::policy::stage_name;
use crate::state::{RunState, Status};
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
    let outcome = load_outcome(ws, state);
    let results: &[StepResult] = outcome.as_ref().map_or(&[], |o| o.results.as_slice());

    let headline = match state.status {
        Status::Completed => completed_headline(ws, state, outcome.as_ref())?,
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
            "Base `{}` · run `{}` · stage {} · repair cycles {} · verification runs {}",
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

    out.extend(["".into(), "## Verification evidence".into(), String::new()]);
    match (&state.last_verify, &outcome) {
        (Some(v), Some(o)) => {
            out.push(format!(
                "Run #{} · evidence `{}/{}`",
                v.run,
                ws.doc("").display().to_string().trim_end_matches('/'),
                v.evidence_dir
            ));
            out.push(String::new());
            out.push("| Step | Kind | Command | Exit | Duration | Logs |".into());
            out.push("|---|---|---|---|---|---|".into());
            for r in &o.results {
                out.push(format!(
                    "| {} | {:?} | `{}` | {} | {} ms | `{}`, `{}` |",
                    r.name,
                    r.kind,
                    r.argv.join(" ").replace('|', "\\|"),
                    r.exit_code.map_or_else(
                        || if r.timed_out {
                            "timeout".to_string()
                        } else {
                            "—".to_string()
                        },
                        |c| c.to_string()
                    ),
                    r.ended_ms.saturating_sub(r.started_ms),
                    r.stdout,
                    r.stderr
                ));
            }
            if !o.app_notes.is_empty() {
                out.push(format!("\nApp: {}", o.app_notes.join("; ")));
            }
        }
        _ => out.push("No verification evidence recorded.".into()),
    }
    out.extend([
        String::new(),
        "## Limits of this evidence".into(),
        String::new(),
        "Checks prove only what they exercise. Behavior not covered by a listed check or scenario is not verified."
            .into(),
    ]);
    Ok(out.join("\n") + "\n")
}

/// "Completed" only when the recorded evidence exists and still matches the code and the
/// confirmed requirements; otherwise say exactly why the evidence no longer holds.
fn completed_headline(
    ws: &Workspace,
    state: &RunState,
    outcome: Option<&Outcome>,
) -> ops::Result<String> {
    let Some(record) = state.last_verify.as_ref().filter(|v| v.passed) else {
        return Ok(
            "**Completed, but no passing verification is recorded.** Treat this as unverified."
                .into(),
        );
    };
    if outcome.is_none() {
        return Ok(format!(
            "**Completed, but the evidence is missing.** `{}/result.json` cannot be read; treat this as unverified.",
            record.evidence_dir
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
    Ok("**Completed.** Every acceptance criterion passed against the current code.".into())
}

fn load_outcome(ws: &Workspace, state: &RunState) -> Option<Outcome> {
    let v = state.last_verify.as_ref()?;
    let raw = std::fs::read(ws.feature_dir.join(&v.evidence_dir).join("result.json")).ok()?;
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
