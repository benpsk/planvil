//! End-to-end tests of the two checking stages: Validate (this feature's checks) then
//! Verify (the project's verify script on the whole app), and the repair loop between them.

use super::tests::{AUDIT_MD, Run, to_validate};
use super::*;
use crate::docs;
use crate::git::testing::{repo, run as git};
use crate::state::{Stage, Status};
use std::fs;
use std::path::Path;

/// The whole-app check: fails while `broken.txt` exists.
const SCRIPT: &str = "#!/bin/sh\ntest ! -f broken.txt\n";

/// A repository whose committed setup is a verify script, with a run started on it.
fn started_with_script() -> Run {
    let dir = repo();
    let root = dir.path();
    fs::write(
        root.join(".planvil.json"),
        r#"{"base_branch":"main","verify":{"script":"scripts/check.sh"}}"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("scripts")).unwrap();
    write_script(&root.join("scripts/check.sh"), SCRIPT);
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "setup"]);
    start(
        root,
        &StartArgs {
            setup_only: false,
            here: false,
            branch: Some("sp20/greet"),
            spec: None,
            base: None,
        },
    )
    .unwrap();
    Run { dir }
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Validate with the feature implemented, landing in Verify.
fn to_verify(r: &Run) {
    to_validate(r);
    fs::write(r.root().join("hello.txt"), "hello\n").unwrap();
    let out = validate(r.root()).unwrap();
    assert!(out.contains("validation PASSED"), "{out}");
    assert_eq!(r.state().stage, Stage::Verify);
}

#[test]
fn verify_runs_only_after_validate() {
    let r = started_with_script();
    to_validate(&r);
    let err = verify(r.root()).unwrap_err().to_string();
    assert!(
        err.contains("`planvil validate` checks this feature first"),
        "{err}"
    );
    assert_eq!(r.state().stage, Stage::Validate);
}

#[test]
fn validate_runs_only_the_feature_checks() {
    let r = started_with_script();
    to_validate(&r);
    // The whole-app check would fail, but Validate does not run it.
    fs::write(r.root().join("broken.txt"), "x").unwrap();
    fs::write(r.root().join("hello.txt"), "hello\n").unwrap();
    let out = validate(r.root()).unwrap();
    assert!(out.contains("validation PASSED"), "{out}");
    assert!(!out.contains("verify-script"), "{out}");
    assert!(out.contains("criteria proven: 1/1"), "{out}");
}

#[test]
fn a_whole_app_failure_loops_back_and_needs_validate_then_verify_again() {
    let r = started_with_script();
    fs::write(r.root().join("broken.txt"), "x").unwrap();
    to_verify(&r);

    let out = verify(r.root()).unwrap();
    assert!(
        out.contains("verification FAILED") && out.contains("verify-script"),
        "{out}"
    );
    let s = r.state();
    assert_eq!((s.stage, s.status), (Stage::Feature, Status::Active));
    assert_eq!(s.attempts.repair_cycles, 1, "same repair budget");
    assert!(
        s.validated.is_none(),
        "the validation pass no longer counts"
    );
    let guide = status(r.root()).unwrap();
    assert!(guide.contains("broke the whole-app check"), "{guide}");

    // The loop goes back through the thinking stages, explaining the failure.
    advance(r.root()).unwrap();
    r.write_doc(
        docs::AUDIT,
        &format!("{AUDIT_MD}## Failure analysis\n`verify-script` failed: broken.txt exists.\n"),
    );
    for _ in 0..3 {
        advance(r.root()).unwrap();
    }
    fs::remove_file(r.root().join("broken.txt")).unwrap();
    advance(r.root()).unwrap();
    assert_eq!(r.state().stage, Stage::Validate);
    validate(r.root()).unwrap();
    let out = verify(r.root()).unwrap();
    assert!(out.contains("verification PASSED"), "{out}");
    let s = r.state();
    assert_eq!((s.stage, s.status), (Stage::Done, Status::Completed));
}

#[test]
fn changing_code_after_validate_sends_the_run_back_to_validate() {
    let r = started_with_script();
    to_verify(&r);
    fs::write(r.root().join("extra.txt"), "late edit").unwrap();
    let err = verify(r.root()).unwrap_err().to_string();
    assert!(err.contains("changed after validation passed"), "{err}");
    let s = r.state();
    assert_eq!((s.stage, s.status), (Stage::Validate, Status::Active));
    validate(r.root()).unwrap();
    assert!(verify(r.root()).unwrap().contains("verification PASSED"));
}

#[test]
fn a_missing_verify_script_is_reported_not_passed() {
    let r = started_with_script();
    to_verify(&r);
    fs::remove_file(r.root().join("scripts/check.sh")).unwrap();
    // Losing the script during a run blocks it, like any change to the script.
    let err = verify(r.root()).unwrap_err().to_string();
    assert!(err.contains("verify script changed"), "{err}");
    control(r.root(), Control::Resume).unwrap();
    // The removal changed the code, so the run re-validates first.
    assert!(verify(r.root()).is_err());
    validate(r.root()).unwrap();
    let err = verify(r.root()).unwrap_err().to_string();
    assert!(
        err.contains("verify script scripts/check.sh is missing"),
        "{err}"
    );
    assert_eq!(r.state().stage, Stage::Verify);
}

#[test]
fn report_shows_validate_and_verify_evidence() {
    let r = started_with_script();
    to_verify(&r);
    verify(r.root()).unwrap();
    let report = crate::report::report(r.root()).unwrap();
    assert!(
        report.contains("whole app passed its verify script"),
        "{report}"
    );
    assert!(
        report.contains("| AC-01 | hello.txt says hello | `greeting` | pass |"),
        "{report}"
    );
    assert!(
        report.contains("### Validate: this feature's checks"),
        "{report}"
    );
    assert!(
        report.contains("### Verify: the whole app") && report.contains("verify-script"),
        "{report}"
    );
}

#[test]
fn a_folder_from_before_the_split_keeps_using_verify_json() {
    let r = started_with_script();
    let ws = Workspace::current(r.root()).unwrap().unwrap();
    fs::remove_file(r.feature().join(docs::VALIDATE)).unwrap();
    fs::write(r.feature().join(docs::LEGACY_VALIDATE), "{}").unwrap();
    assert_eq!(ws.validate_file(), docs::LEGACY_VALIDATE);
    // Restarting must not add an empty validate.json that would hide it.
    docs::write_missing_templates(&r.feature(), "sp20/greet").unwrap();
    assert!(!r.feature().join(docs::VALIDATE).exists());
}

#[test]
fn repeated_whole_app_failures_block_the_run() {
    let r = started_with_script();
    fs::write(r.root().join("broken.txt"), "x").unwrap();
    to_verify(&r);
    for i in 0..3 {
        verify(r.root()).unwrap();
        if r.state().status == Status::Blocked {
            break;
        }
        fs::write(r.root().join(format!("noise{i}.txt")), "x").unwrap();
        repair_loop_named(&r, "verify-script");
        validate(r.root()).unwrap();
    }
    let s = r.state();
    assert_eq!(s.status, Status::Blocked);
    assert!(
        s.status_reason
            .unwrap()
            .contains("consecutive equivalent failures"),
    );
}

/// Like `repair_loop`, but the failure analysis names `check`.
fn repair_loop_named(r: &Run, check: &str) {
    assert_eq!(r.state().stage, Stage::Feature);
    advance(r.root()).unwrap();
    r.write_doc(
        docs::AUDIT,
        &format!("{AUDIT_MD}## Failure analysis\n`{check}` failed: broken.txt exists.\n"),
    );
    for _ in 0..4 {
        advance(r.root()).unwrap();
    }
    assert_eq!(r.state().stage, Stage::Validate);
}

#[test]
fn a_verify_script_changed_during_the_run_blocks_verify() {
    let r = started_with_script();
    to_verify(&r);
    // A shell trick the text checks miss: weaken the script from inside its folder.
    write_script(&r.root().join("scripts/check.sh"), "#!/bin/sh\nexit 0\n");
    let err = verify(r.root()).unwrap_err().to_string();
    assert!(
        err.contains("verify script changed during this run"),
        "{err}"
    );
    let s = r.state();
    assert_eq!(s.status, Status::Blocked);
    // The user accepts the change by resuming; the new code is validated again first.
    control(r.root(), Control::Resume).unwrap();
    assert!(verify(r.root()).is_err(), "code changed: back to Validate");
    validate(r.root()).unwrap();
    assert!(verify(r.root()).unwrap().contains("verification PASSED"));
}

#[test]
fn verify_with_nothing_configured_says_so() {
    let r = super::tests::started_plain();
    to_validate(&r);
    fs::write(r.root().join("hello.txt"), "hello\n").unwrap();
    validate(r.root()).unwrap();
    let out = verify(r.root()).unwrap();
    assert!(out.contains("nothing to verify"), "{out}");
    let report = crate::report::report(r.root()).unwrap();
    assert!(report.contains("no whole-app check is set up"), "{report}");
}
