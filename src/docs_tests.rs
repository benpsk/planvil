//! Tests for `docs`.

use super::*;

fn template(name: &str) -> String {
    TEMPLATES
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap()
        .1
        .replace("{{branch}}", "x")
}

const FEATURE_OK: &str = "\
## Goal
Users can reset passwords.
## Scope
Reset by email.
## Out of scope
None
## Behavior
A link valid for 1 hour.
## Acceptance criteria
| ID | Criterion |
|---|---|
| AC-01 | Link is emailed |
| AC-02 | Link expires after 1 hour |
";

#[test]
fn blank_templates_fail_their_gates() {
    let repo = tempfile::tempdir().unwrap();
    assert!(!check_feature(&template(FEATURE)).is_empty());
    assert!(!check_audit(&template(AUDIT), repo.path()).is_empty());
    assert!(!check_plan(&template(PLAN), &[]).is_empty());
    assert!(!check_decisions(&template(DECISIONS)).is_empty());
}

#[test]
fn complete_feature_passes() {
    assert_eq!(check_feature(FEATURE_OK), Vec::<String>::new());
    assert_eq!(feature_criteria(FEATURE_OK), vec!["AC-01", "AC-02"]);
}

#[test]
fn feature_without_criteria_ids_fails() {
    let md = FEATURE_OK.replace("AC-01", "one").replace("AC-02", "two");
    let problems = check_feature(&md);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("no acceptance criteria")),
        "{problems:?}"
    );
}

#[test]
fn missing_section_is_named() {
    let md = FEATURE_OK.replace("## Behavior\nA link valid for 1 hour.\n", "");
    assert_eq!(
        check_feature(&md),
        vec!["FEATURE.md: missing section `## Behavior`"]
    );
}

#[test]
fn placeholder_and_comment_only_sections_are_empty() {
    assert!(!has_content("<!-- guidance -->\n\n"));
    assert!(!has_content("TODO\n"));
    assert!(!has_content("| A | B |\n|---|---|\n"));
    assert!(!has_content("| A | B |\n|---|---|\n| TBD | ... |\n"));
    assert!(has_content("| A | B |\n|---|---|\n| x | y |\n"));
    assert!(has_content("None\n"));
}

#[test]
fn headings_in_code_fences_are_ignored() {
    let md = "## Goal\n```\n## Scope\n```\nreal\n";
    let secs = sections(md);
    assert_eq!(secs.len(), 1);
    assert!(secs[0].body.contains("real"));
}

#[test]
fn ac_ids_need_digits_and_a_word_boundary() {
    assert_eq!(
        ac_ids("AC-01, AC-2 and XAC-03, AC-x, AC-01"),
        vec!["AC-01", "AC-2"]
    );
}

#[test]
fn audit_must_reference_an_existing_path() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/login.go"), "").unwrap();
    let body = |path: &str| {
        format!(
            "## Affected code\n| Path | Symbol | Why |\n|---|---|---|\n| `{path}` | Login | entry |\n\
             ## Current behavior\nx\n## Reuse\nx\n## Constraints\nx\n## Gaps\nNone\n"
        )
    };
    assert_eq!(
        check_audit(&body("src/login.go:42"), repo.path()),
        Vec::<String>::new()
    );
    assert_eq!(
        check_audit(&body("src/login.go::Login"), repo.path()),
        Vec::<String>::new()
    );
    assert!(!check_audit(&body("src/missing.go"), repo.path()).is_empty());
    assert!(!check_audit(&body("../etc/passwd"), repo.path()).is_empty());
}

#[test]
fn plan_must_map_every_criterion() {
    let md = "## Status\nP0\n## Steps\n1. do\n## Verification\n| AC | Check | Kind |\n|---|---|---|\n\
              | AC-01 | unit | check |\n## Risks\nNone\n";
    let criteria = vec!["AC-01".to_string(), "AC-02".to_string()];
    assert_eq!(
        check_plan(md, &criteria),
        vec!["PLAN.md: AC-02 is not mapped under `## Verification`"]
    );
    assert_eq!(check_plan(md, &criteria[..1]), Vec::<String>::new());
}

#[test]
fn decisions_reject_unchecked_questions() {
    let md = "## Decided\n| # | D | W | A | B |\n|---|---|---|---|---|\n| 1 | use X | fits | Y | agent |\n\
              ## Open questions\n- [ ] Q1: who?\n- [x] Q2: done\n";
    assert_eq!(
        check_decisions(md),
        vec!["DECISIONS.md: 1 open question(s) still unchecked"]
    );
    assert_eq!(
        check_decisions(&md.replace("- [ ]", "- [x]")),
        Vec::<String>::new()
    );
}

#[test]
fn writes_templates_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(FEATURE), "mine").unwrap();
    let created = write_missing_templates(dir.path(), "sp20/mcp").unwrap();
    assert!(!created.contains(&FEATURE.to_string()));
    assert_eq!(
        std::fs::read_to_string(dir.path().join(FEATURE)).unwrap(),
        "mine"
    );
    let plan = std::fs::read_to_string(dir.path().join(PLAN)).unwrap();
    assert!(plan.starts_with("# Plan: sp20/mcp"));
}

#[test]
fn headings_inside_comments_and_tilde_fences_are_ignored() {
    let md = "## Goal\nreal\n<!--\n## Scope\n-->\n~~~\n## Behavior\n~~~\n";
    let secs = sections(md);
    assert_eq!(secs.len(), 1, "{secs:?}");
}

#[test]
fn criteria_come_from_rows_not_prose() {
    let md = "## Acceptance criteria\nThis replaces AC-99 from the old spec.\n| ID | C |\n|---|---|\n| AC-01 | x |\n- AC-02: y\n";
    assert_eq!(feature_criteria(md), vec!["AC-01", "AC-02"]);
}

#[test]
fn audit_grounding_needs_a_real_file() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    let body = |path: &str| {
        format!(
            "## Affected code\n`{path}`\n## Current behavior\nx\n## Reuse\nx\n## Constraints\nx\n## Gaps\nNone\n"
        )
    };
    assert!(!check_audit(&body("."), repo.path()).is_empty());
    assert!(!check_audit(&body("src"), repo.path()).is_empty());
}

#[test]
fn failure_analysis_must_name_every_failing_check() {
    let failing = vec!["unit".to_string(), "journey".to_string()];
    assert!(
        check_failure_analysis("## Goal\nx\n", &failing)[0].contains("add `## Failure analysis`")
    );
    let partial = "## Failure analysis\n`unit` fails because the parser drops the last field.\n";
    assert_eq!(
        check_failure_analysis(partial, &failing),
        vec!["AUDIT.md: `## Failure analysis` does not explain the failure of `journey`"]
    );
    let full = format!("{partial}`journey` fails because the button is hidden.\n");
    assert!(check_failure_analysis(&full, &failing).is_empty());
}
