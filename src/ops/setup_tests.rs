//! Setup stage: propose, prove, approve once, apply.

use std::fs;
use std::path::Path;

use crate::git::testing::repo;
use crate::ops::{self, StartArgs, Workspace};
use crate::state::{QuestionKind, RunState, Stage, Status};

fn fresh() -> tempfile::TempDir {
    let dir = repo();
    ops::start(
        dir.path(),
        &StartArgs {
            setup_only: false,
            branch: Some("sp20/x"),
            spec: None,
            base: Some("main"),
        },
    )
    .unwrap();
    dir
}

fn state(root: &Path) -> RunState {
    Workspace::current(root)
        .unwrap()
        .unwrap()
        .peek()
        .unwrap()
        .unwrap()
}

fn propose(root: &Path, body: &str) {
    fs::write(root.join("planvil/sp20-x/setup.proposed.json"), body).unwrap();
}

fn user_replied(root: &Path) {
    let ws = Workspace::current(root).unwrap().unwrap();
    let lock = ws.lock().unwrap();
    let s = lock.load().unwrap().unwrap();
    lock.save(&RunState {
        user_prompts: s.user_prompts + 1,
        last_user_reply: Some("ok".into()),
        ..s
    })
    .unwrap();
}

const USABLE: &str = r#"{"checks":[{"name":"readme","run":["test","-f","README.md"]}]}"#;

#[test]
fn a_repo_without_verification_starts_in_setup_with_source_read_only() {
    let dir = fresh();
    assert_eq!(state(dir.path()).stage, Stage::Setup);
    let err = ops::advance(dir.path()).unwrap_err().to_string();
    assert!(err.contains("not set up yet"), "{err}");
}

#[test]
fn usable_proposal_approved_once_is_applied_and_moves_to_feature() {
    let dir = fresh();
    propose(dir.path(), USABLE);
    let out = ops::setup_check(dir.path()).unwrap();
    assert!(out.contains("USABLE"), "{out}");
    ops::ask(
        dir.path(),
        "Setup: checks README exists. OK?",
        QuestionKind::ApproveSetup,
    )
    .unwrap();
    user_replied(dir.path());
    ops::answer(dir.path(), "ok", Some(true)).unwrap();
    let out = ops::apply_setup(dir.path()).unwrap();
    assert!(out.contains("Setup → Feature"), "{out}");
    assert_eq!(state(dir.path()).stage, Stage::Feature);
    let cfg = fs::read_to_string(dir.path().join(".planvil.json")).unwrap();
    assert!(
        cfg.contains("\"readme\"") && cfg.contains("\"base_branch\""),
        "{cfg}"
    );
}

#[test]
fn a_missing_tool_makes_the_proposal_unusable_and_unapprovable() {
    let dir = fresh();
    propose(
        dir.path(),
        r#"{"checks":[{"name":"lint","run":["planvil-no-such-linter"]}]}"#,
    );
    let out = ops::setup_check(dir.path()).unwrap();
    assert!(out.contains("NOT USABLE"), "{out}");
    let err = ops::ask(dir.path(), "ok?", QuestionKind::ApproveSetup)
        .unwrap_err()
        .to_string();
    assert!(err.contains("not usable"), "{err}");
}

#[test]
fn failing_checks_must_be_listed_as_known_failures() {
    let dir = fresh();
    propose(
        dir.path(),
        r#"{"checks":[{"name":"legacy","run":["test","-f","missing.txt"]}]}"#,
    );
    let out = ops::setup_check(dir.path()).unwrap();
    assert!(out.contains("failing on the current code"), "{out}");
    assert!(ops::ask(dir.path(), "ok?", QuestionKind::ApproveSetup).is_err());
    propose(
        dir.path(),
        r#"{"checks":[{"name":"legacy","run":["test","-f","missing.txt"]}],"known_failures":["legacy"]}"#,
    );
    assert!(ops::setup_check(dir.path()).unwrap().contains("USABLE"));
    assert!(ops::ask(dir.path(), "ok?", QuestionKind::ApproveSetup).is_ok());
}

#[test]
fn apply_needs_approval_of_the_current_proposal() {
    let dir = fresh();
    propose(dir.path(), USABLE);
    ops::setup_check(dir.path()).unwrap();
    let err = ops::apply_setup(dir.path()).unwrap_err().to_string();
    assert!(err.contains("not approved"), "{err}");
    ops::ask(dir.path(), "ok?", QuestionKind::ApproveSetup).unwrap();
    user_replied(dir.path());
    ops::answer(dir.path(), "ok", Some(true)).unwrap();
    // Editing the proposal after approval voids it.
    propose(
        dir.path(),
        r#"{"checks":[{"name":"readme","run":["test","-e","README.md"]}]}"#,
    );
    let err = ops::apply_setup(dir.path()).unwrap_err().to_string();
    assert!(err.contains("has not been checked"), "{err}");
    assert_eq!(state(dir.path()).status, Status::Active);
}

#[test]
fn known_failures_do_not_fail_a_feature() {
    let project: crate::verify::ProjectVerify = serde_json::from_str(
        r#"{"checks":[{"name":"legacy","run":["test","-f","missing.txt"]},{"name":"ok","run":["test","-d","."]}],"known_failures":["legacy"]}"#,
    )
    .unwrap();
    let plan = crate::verify::Plan::verify(&project, None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = crate::verify::run(&plan, dir.path(), &dir.path().join("ev")).unwrap();
    assert_eq!(out.verdict, crate::verify::Verdict::Passed);
    assert!(
        out.results.iter().any(|r| r.name == "legacy" && !r.passed),
        "still run and reported"
    );
}

#[test]
fn known_failures_must_name_project_checks() {
    let bad: crate::verify::ProjectVerify =
        serde_json::from_str(r#"{"known_failures":["ghost"]}"#).unwrap();
    assert!(bad.validate().is_err());
}

#[test]
fn init_runs_setup_only_and_completes_without_a_feature() {
    let dir = crate::git::testing::repo_with_setup();
    let out = ops::start(
        dir.path(),
        &StartArgs {
            setup_only: true,
            branch: None,
            spec: None,
            base: None,
        },
    )
    .unwrap();
    assert!(out.contains("re-discovering"), "{out}");
    assert_eq!(
        crate::git::testing::run(dir.path(), &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
    fs::write(dir.path().join("planvil/main/setup.proposed.json"), USABLE).unwrap();
    ops::setup_check(dir.path()).unwrap();
    ops::ask(dir.path(), "ok?", QuestionKind::ApproveSetup).unwrap();
    user_replied(dir.path());
    ops::answer(dir.path(), "ok", Some(true)).unwrap();
    let out = ops::apply_setup(dir.path()).unwrap();
    assert!(out.contains("setup complete"), "{out}");
    let s = state(dir.path());
    assert_eq!((s.stage, s.status), (Stage::Done, Status::Completed));
    assert!(
        fs::read_to_string(dir.path().join(".planvil.json"))
            .unwrap()
            .contains("\"readme\"")
    );
}

#[test]
fn init_refuses_while_a_run_is_in_progress() {
    let dir = crate::git::testing::repo_with_setup();
    ops::start(
        dir.path(),
        &StartArgs {
            setup_only: false,
            branch: Some("sp20/x"),
            spec: None,
            base: Some("main"),
        },
    )
    .unwrap();
    let err = ops::start(
        dir.path(),
        &StartArgs {
            setup_only: true,
            branch: None,
            spec: None,
            base: None,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("in progress"), "{err}");
}

const SCRIPTED: &str = r#"{"script":"scripts/planvil-verify.sh"}"#;

fn propose_script(root: &Path, body: &str) {
    fs::write(
        root.join("planvil/sp20-x/verify.proposed.sh"),
        format!("#!/bin/sh\nset -e\n{body}\n"),
    )
    .unwrap();
}

fn approve(root: &Path) {
    ops::ask(root, "Setup ok?", QuestionKind::ApproveSetup).unwrap();
    user_replied(root);
    ops::answer(root, "ok", Some(true)).unwrap();
}

#[test]
fn setup_proves_the_verify_script_and_installs_it_where_configured() {
    let dir = fresh();
    propose(dir.path(), SCRIPTED);
    propose_script(dir.path(), "test -f README.md");
    let out = ops::setup_check(dir.path()).unwrap();
    assert!(
        out.contains("USABLE") && out.contains("verify-script"),
        "{out}"
    );
    approve(dir.path());
    ops::apply_setup(dir.path()).unwrap();

    let installed = dir.path().join("scripts/planvil-verify.sh");
    assert!(
        fs::read_to_string(&installed)
            .unwrap()
            .contains("README.md")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&installed).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "executable");
    }
    let cfg = fs::read_to_string(dir.path().join(".planvil.json")).unwrap();
    assert!(cfg.contains("scripts/planvil-verify.sh"), "{cfg}");
    assert_eq!(state(dir.path()).stage, Stage::Feature);
}

#[test]
fn a_script_proposal_needs_the_proposed_script() {
    let dir = fresh();
    propose(dir.path(), SCRIPTED);
    let err = ops::setup_check(dir.path()).unwrap_err().to_string();
    assert!(err.contains("verify.proposed.sh"), "{err}");
}

#[test]
fn a_proposal_must_check_something() {
    let dir = fresh();
    propose(dir.path(), "{}");
    let err = ops::setup_check(dir.path()).unwrap_err().to_string();
    assert!(err.contains("script"), "{err}");
}

#[test]
fn a_verify_script_that_fails_today_cannot_be_approved() {
    let dir = fresh();
    propose(dir.path(), SCRIPTED);
    propose_script(dir.path(), "test -f missing.txt");
    let out = ops::setup_check(dir.path()).unwrap();
    assert!(out.contains("verify script fails"), "{out}");
    assert!(ops::ask(dir.path(), "ok?", QuestionKind::ApproveSetup).is_err());
}

#[test]
fn editing_the_proposed_script_after_approval_voids_the_approval() {
    let dir = fresh();
    propose(dir.path(), SCRIPTED);
    propose_script(dir.path(), "test -f README.md");
    ops::setup_check(dir.path()).unwrap();
    approve(dir.path());
    propose_script(dir.path(), "true");
    let err = ops::apply_setup(dir.path()).unwrap_err().to_string();
    assert!(err.contains("has not been checked"), "{err}");
    assert!(!dir.path().join("scripts/planvil-verify.sh").exists());
}

#[test]
fn the_verify_script_may_not_replace_an_existing_file() {
    for target in ["README.md", ".planvil.json", ".gitignore"] {
        let dir = fresh();
        propose(dir.path(), &format!(r#"{{"script":"{target}"}}"#));
        propose_script(dir.path(), "true");
        let err = ops::setup_check(dir.path()).unwrap_err().to_string();
        let refused = err.contains("choose another path") || err.contains("relative path inside");
        assert!(refused, "{target}: {err}");
    }
}

#[cfg(unix)]
#[test]
fn a_failed_apply_leaves_no_half_installed_script() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fresh();
    propose(dir.path(), SCRIPTED);
    propose_script(dir.path(), "test -f README.md");
    ops::setup_check(dir.path()).unwrap();
    approve(dir.path());
    let config = dir.path().join(".planvil.json");
    let before = fs::read_to_string(&config).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o444)).unwrap();
    if fs::OpenOptions::new().write(true).open(&config).is_ok() {
        return; // running as root: the file cannot be made unwritable
    }
    assert!(ops::apply_setup(dir.path()).is_err());
    assert!(!dir.path().join("scripts/planvil-verify.sh").exists());
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
    assert_eq!(state(dir.path()).stage, Stage::Setup);
}
