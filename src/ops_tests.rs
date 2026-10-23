//! End-to-end tests of the workflow operations against real temporary git repositories.

use super::*;
use crate::docs;
use crate::git::testing::{repo, run as git};
use crate::state::{QuestionKind, RunState, Stage, Status};
use std::fs;
use std::path::{Path, PathBuf};

const FEATURE_MD: &str = "\
# Feature
## Goal
Greet users.
## Scope
A greeting file.
## Out of scope
None
## Behavior
`hello.txt` contains `hello`.
## Acceptance criteria
| ID | Criterion |
|---|---|
| AC-01 | hello.txt says hello |
";

const AUDIT_MD: &str = "\
## Affected code
| Path | Symbol | Why |
|---|---|---|
| `README.md` | - | project entry |
## Current behavior
No greeting exists.
## Reuse
None
## Constraints
None
## Gaps
None
";

const PLAN_MD: &str = "\
## Status
P0 planned
## Steps
1. Create hello.txt
## Verification
| AC | Check | Kind |
|---|---|---|
| AC-01 | greeting | check |
## Risks
None
";

const DECISIONS_MD: &str = "\
## Decided
| # | Decision | Why | Alternatives | By |
|---|---|---|---|---|
| 1 | Plain text file | simplest | JSON | agent |
## Open questions
None
";

const VERIFY_JSON: &str = r#"{"checks":[{"name":"greeting","run":["grep","-q","hello","hello.txt"],"criteria":["AC-01"]}]}"#;

struct Run {
    dir: tempfile::TempDir,
}

impl Run {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn feature(&self) -> PathBuf {
        self.root().join("planvil/sp20-greet")
    }

    fn write_doc(&self, name: &str, body: &str) {
        fs::write(self.feature().join(name), body).unwrap();
    }

    fn state(&self) -> RunState {
        Workspace::current(self.root())
            .unwrap()
            .unwrap()
            .peek()
            .unwrap()
            .unwrap()
    }

    /// Simulate the prompt hook seeing a user message now.
    fn user_replied(&self) {
        let ws = Workspace::current(self.root()).unwrap().unwrap();
        let lock = ws.lock().unwrap();
        let s = lock.load().unwrap().unwrap();
        lock.save(&RunState {
            user_prompts: s.user_prompts + 1,
            last_user_reply: Some("yes".into()),
            ..s
        })
        .unwrap();
    }
}

fn started() -> Run {
    let dir = crate::git::testing::repo_with_setup();
    let out = start(
        dir.path(),
        &StartArgs {
            setup_only: false,
            branch: Some("sp20/greet"),
            spec: None,
            base: Some("main"),
        },
    )
    .unwrap();
    assert!(out.contains("planvil run on `sp20/greet`"), "{out}");
    Run { dir }
}

fn confirm_feature(r: &Run) {
    r.write_doc(docs::FEATURE, FEATURE_MD);
    ask(
        r.root(),
        "Is FEATURE.md right?",
        QuestionKind::ConfirmFeature,
    )
    .unwrap();
    r.user_replied();
    answer(r.root(), "yes", Some(true)).unwrap();
}

/// Drive a run from start to the Validate stage.
fn to_validate(r: &Run) {
    confirm_feature(r);
    advance(r.root()).unwrap();
    r.write_doc(docs::AUDIT, AUDIT_MD);
    advance(r.root()).unwrap();
    r.write_doc(docs::PLAN, PLAN_MD);
    r.write_doc(docs::VERIFY, VERIFY_JSON);
    advance(r.root()).unwrap();
    r.write_doc(docs::DECISIONS, DECISIONS_MD);
    advance(r.root()).unwrap();
    assert_eq!(r.state().stage, Stage::Implement);
    advance(r.root()).unwrap();
    assert_eq!(r.state().stage, Stage::Validate);
}

/// After a failed verification: walk the loop Feature → Audit → Plan → Decide → Implement →
/// Validate, explaining the failure of `greeting` in AUDIT.md on the way.
fn repair_loop(r: &Run) {
    assert_eq!(r.state().stage, Stage::Feature);
    advance(r.root()).unwrap();
    r.write_doc(
        docs::AUDIT,
        &format!("{AUDIT_MD}## Failure analysis\n`greeting` failed: hello.txt is missing.\n"),
    );
    for _ in 0..4 {
        advance(r.root()).unwrap();
    }
    assert_eq!(r.state().stage, Stage::Validate);
}

#[test]
fn start_creates_branch_folder_templates_and_gitignore() {
    let r = started();
    assert_eq!(
        git(r.root(), &["symbolic-ref", "--short", "HEAD"]),
        "sp20/greet"
    );
    for name in [
        docs::FEATURE,
        docs::AUDIT,
        docs::PLAN,
        docs::DECISIONS,
        docs::HANDOFF,
        docs::VERIFY,
    ] {
        assert!(r.feature().join(name).exists(), "{name}");
    }
    assert!(
        fs::read_to_string(r.root().join(".gitignore"))
            .unwrap()
            .contains("/planvil/")
    );
    let cfg = fs::read_to_string(r.root().join(".planvil.json")).unwrap();
    assert!(cfg.contains("\"base_branch\": \"main\""));
    let s = r.state();
    assert_eq!((s.stage, s.status), (Stage::Feature, Status::Active));
}

#[test]
fn start_asks_for_missing_base_and_branch() {
    let dir = repo();
    let err = start(
        dir.path(),
        &StartArgs {
            setup_only: false,
            branch: Some("x/y"),
            spec: None,
            base: None,
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        OpsError::NeedsInput {
            code: "NEEDS_BASE_BRANCH",
            ..
        }
    ));
    let err = start(
        dir.path(),
        &StartArgs {
            setup_only: false,
            branch: None,
            spec: None,
            base: Some("main"),
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        OpsError::NeedsInput {
            code: "NEEDS_BRANCH",
            ..
        }
    ));
}

#[test]
fn start_refuses_to_switch_branches_over_uncommitted_work() {
    let dir = repo();
    fs::write(dir.path().join("README.md"), "user edit").unwrap();
    let err = start(
        dir.path(),
        &StartArgs {
            setup_only: false,
            branch: Some("sp20/x"),
            spec: None,
            base: Some("main"),
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        OpsError::NeedsInput {
            code: "NEEDS_CLEAN_WORKTREE",
            ..
        }
    ));
    assert_eq!(
        fs::read_to_string(dir.path().join("README.md")).unwrap(),
        "user edit"
    );
}

#[test]
fn start_preserves_unrelated_gitignore_and_config_keys() {
    let dir = repo();
    fs::write(dir.path().join(".gitignore"), "node_modules/").unwrap();
    fs::write(
        dir.path().join(".planvil.json"),
        r#"{"max_repair_cycles": 4}"#,
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "cfg"]);
    start(
        dir.path(),
        &StartArgs {
            setup_only: false,
            branch: Some("sp20/x"),
            spec: None,
            base: Some("main"),
        },
    )
    .unwrap();
    let gi = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(gi.starts_with("node_modules/\n"), "{gi}");
    let cfg: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".planvil.json")).unwrap())
            .unwrap();
    assert_eq!(cfg["max_repair_cycles"], 4);
    assert_eq!(cfg["base_branch"], "main");
}

#[test]
fn start_on_an_active_run_resumes_it_and_detaches_the_session() {
    let r = started();
    let ws = Workspace::current(r.root()).unwrap().unwrap();
    let lock = ws.lock().unwrap();
    let s = lock.load().unwrap().unwrap();
    lock.save(&RunState {
        session_id: Some("old".into()),
        ..s
    })
    .unwrap();
    drop(lock);
    let out = start(
        r.root(),
        &StartArgs {
            setup_only: false,
            branch: None,
            spec: None,
            base: None,
        },
    )
    .unwrap();
    assert!(out.contains("resumed the existing run"), "{out}");
    assert_eq!(r.state().session_id, None);
}

#[test]
fn feature_gate_needs_content_and_user_confirmation() {
    let r = started();
    let err = advance(r.root()).unwrap_err().to_string();
    assert!(err.contains("section `## Goal` is empty"), "{err}");
    r.write_doc(docs::FEATURE, FEATURE_MD);
    let err = advance(r.root()).unwrap_err().to_string();
    assert!(err.contains("not confirmed"), "{err}");
}

#[test]
fn answer_requires_a_real_user_message_after_the_question() {
    let r = started();
    r.write_doc(docs::FEATURE, FEATURE_MD);
    ask(r.root(), "Right?", QuestionKind::ConfirmFeature).unwrap();
    assert_eq!(r.state().status, Status::WaitingForUser);
    let err = answer(r.root(), "yes", Some(true)).unwrap_err().to_string();
    assert!(err.contains("no user message"), "{err}");
    r.user_replied();
    answer(r.root(), "yes", Some(true)).unwrap();
    assert_eq!(r.state().status, Status::Active);
    advance(r.root()).unwrap();
    assert_eq!(r.state().stage, Stage::Audit);
}

#[test]
fn rejected_confirmation_does_not_confirm() {
    let r = started();
    r.write_doc(docs::FEATURE, FEATURE_MD);
    ask(r.root(), "Right?", QuestionKind::ConfirmFeature).unwrap();
    r.user_replied();
    answer(r.root(), "no, change scope", Some(false)).unwrap();
    assert!(
        advance(r.root())
            .unwrap_err()
            .to_string()
            .contains("not confirmed")
    );
}

#[test]
fn audit_gate_rejects_missing_content() {
    let r = started();
    confirm_feature(&r);
    advance(r.root()).unwrap();
    let err = advance(r.root()).unwrap_err().to_string();
    assert!(err.contains("AUDIT.md"), "{err}");
    assert_eq!(r.state().stage, Stage::Audit);
}

#[test]
fn plan_gate_requires_verification_for_every_criterion() {
    let r = started();
    confirm_feature(&r);
    advance(r.root()).unwrap();
    r.write_doc(docs::AUDIT, AUDIT_MD);
    advance(r.root()).unwrap();
    r.write_doc(docs::PLAN, PLAN_MD);
    let err = advance(r.root()).unwrap_err().to_string();
    assert!(err.contains("AC-01 has no check or scenario"), "{err}");
}

#[test]
fn decide_gate_blocks_on_open_questions() {
    let r = started();
    confirm_feature(&r);
    advance(r.root()).unwrap();
    r.write_doc(docs::AUDIT, AUDIT_MD);
    advance(r.root()).unwrap();
    r.write_doc(docs::PLAN, PLAN_MD);
    r.write_doc(docs::VERIFY, VERIFY_JSON);
    advance(r.root()).unwrap();
    r.write_doc(
        docs::DECISIONS,
        &DECISIONS_MD.replace("None\n", "- [ ] Q1: which locale?\n"),
    );
    assert!(
        advance(r.root())
            .unwrap_err()
            .to_string()
            .contains("open question")
    );
}

#[test]
fn full_run_fails_repairs_and_completes() {
    let r = started();
    to_validate(&r);

    let out = verify(r.root()).unwrap();
    assert!(out.contains("verification FAILED"), "{out}");
    let s = r.state();
    assert_eq!(
        (s.stage, s.status),
        (Stage::Feature, Status::Active),
        "a failure loops back to Feature"
    );
    assert_eq!(s.attempts.repair_cycles, 1);

    // Feature is still confirmed, so it advances without asking anyone.
    advance(r.root()).unwrap();
    let err = advance(r.root()).unwrap_err().to_string();
    assert!(err.contains("Failure analysis"), "{err}");
    r.write_doc(
        docs::AUDIT,
        &format!("{AUDIT_MD}## Failure analysis\n`greeting` failed: hello.txt is missing.\n"),
    );
    for _ in 0..3 {
        advance(r.root()).unwrap();
    }
    assert_eq!(r.state().stage, Stage::Implement);
    let err = advance(r.root()).unwrap_err().to_string();
    assert!(err.contains("nothing changed"), "{err}");

    fs::write(r.root().join("hello.txt"), "hello\n").unwrap();
    advance(r.root()).unwrap();
    let out = verify(r.root()).unwrap();
    assert!(out.contains("PASSED"), "{out}");
    let s = r.state();
    assert_eq!((s.stage, s.status), (Stage::Done, Status::Completed));
    assert!(s.last_verify.unwrap().passed);
}

#[test]
fn repair_guidance_names_the_cycle_and_failing_checks() {
    let r = started();
    to_validate(&r);
    verify(r.root()).unwrap();
    let out = status(r.root()).unwrap();
    assert!(
        out.contains("Repair cycle 1/5") && out.contains("`greeting`"),
        "{out}"
    );
}

#[test]
fn repeated_identical_failures_block_the_run() {
    let r = started();
    to_validate(&r);
    for i in 0..3 {
        verify(r.root()).unwrap();
        if i < 2 {
            fs::write(r.root().join(format!("noise{i}.txt")), "x").unwrap();
            repair_loop(&r);
        }
    }
    let s = r.state();
    assert_eq!(s.status, Status::Blocked);
    assert!(
        s.status_reason
            .unwrap()
            .contains("consecutive equivalent failures")
    );
}

#[test]
fn missing_tool_blocks_instead_of_passing() {
    let r = started();
    to_validate(&r);
    r.write_doc(
        docs::VERIFY,
        r#"{"checks":[{"name":"browser","run":["planvil-no-such-driver"],"criteria":["AC-01"]}]}"#,
    );
    let out = verify(r.root()).unwrap();
    assert!(out.contains("UNAVAILABLE"), "{out}");
    let s = r.state();
    assert_eq!(s.status, Status::Blocked);
    assert!(!s.last_verify.unwrap().passed);
}

#[test]
fn changing_requirements_mid_run_rewinds_to_feature() {
    let r = started();
    to_validate(&r);
    r.write_doc(
        docs::FEATURE,
        &FEATURE_MD.replace("Greet users.", "Greet users politely."),
    );
    let err = verify(r.root()).unwrap_err().to_string();
    assert!(err.contains("back to the Feature stage"), "{err}");
    let s = r.state();
    assert_eq!(s.stage, Stage::Feature);
    assert_eq!(s.feature_confirmed_hash, None);
    assert_eq!(s.last_verify, None);
}

#[test]
fn user_controls_pause_resume_and_cancel() {
    let r = started();
    control(r.root(), Control::Pause).unwrap();
    assert_eq!(r.state().status, Status::Paused);
    assert!(advance(r.root()).is_err());
    control(r.root(), Control::Resume).unwrap();
    control(r.root(), Control::Cancel).unwrap();
    assert_eq!(r.state().status, Status::Cancelled);
    assert!(control(r.root(), Control::Resume).is_err());
}

#[test]
fn status_reports_gate_problems_and_next_action() {
    let r = started();
    let out = status(r.root()).unwrap();
    assert!(out.contains("stage: Feature"), "{out}");
    assert!(out.contains("gate not yet met"), "{out}");
    assert!(out.contains("next: write"), "{out}");
}

#[test]
fn weakening_a_failing_check_needs_user_approval() {
    let r = started();
    to_validate(&r);
    verify(r.root()).unwrap();
    // Make the failing check unable to fail instead of fixing the code.
    r.write_doc(
        docs::VERIFY,
        r#"{"checks":[{"name":"greeting","run":["test","-d","."],"criteria":["AC-01"]}]}"#,
    );
    repair_loop(&r);
    let err = verify(r.root()).unwrap_err().to_string();
    assert!(
        err.contains("failed last time and has since been changed"),
        "{err}"
    );
    assert_eq!(r.state().stage, Stage::Validate);
}

#[test]
fn checks_that_keep_changing_files_block_instead_of_looping() {
    let r = started();
    to_validate(&r);
    r.write_doc(
        docs::VERIFY,
        r#"{"checks":[{"name":"greeting","shell":"date +%N > generated-$$.txt; test -n x","criteria":["AC-01"]}]}"#,
    );
    let first = verify(r.root()).unwrap();
    assert!(
        first.contains("discarded") && first.contains("generated-"),
        "{first}"
    );
    assert_eq!(r.state().status, Status::Active);
    let second = verify(r.root()).unwrap();
    assert!(second.contains("BLOCKED"), "{second}");
    assert_eq!(r.state().status, Status::Blocked);
}

#[test]
fn report_is_honest_about_stale_evidence() {
    let r = started();
    to_validate(&r);
    fs::write(r.root().join("hello.txt"), "hello\n").unwrap();
    verify(r.root()).unwrap();
    let report = crate::report::report(r.root()).unwrap();
    assert!(report.contains("**Completed.**"), "{report}");
    assert!(
        report.contains("| AC-01 | hello.txt says hello | `greeting` | pass |"),
        "{report}"
    );
    fs::write(r.root().join("hello.txt"), "changed\n").unwrap();
    let report = crate::report::report(r.root()).unwrap();
    assert!(report.contains("changed after verification"), "{report}");
}

#[test]
fn repair_hint_for_feature_reflects_whether_it_is_still_confirmed() {
    let r = started();
    to_validate(&r);
    verify(r.root()).unwrap();
    assert!(status(r.root()).unwrap().contains("still confirmed"));
    r.write_doc(
        docs::FEATURE,
        &FEATURE_MD.replace("Greet users.", "Greet users warmly."),
    );
    let out = status(r.root()).unwrap();
    assert!(out.contains("changed since the user confirmed"), "{out}");
}
