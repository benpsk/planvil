//! `planvil start <feature> --here`: a run on the branch that is already checked out.

use super::*;
use crate::git::testing::{repo_with_setup, run as git};
use crate::state::{Stage, Status};
use std::fs;
use std::path::Path;

fn here(root: &Path, feature: Option<&str>) -> Result<String> {
    start(
        root,
        &StartArgs {
            setup_only: false,
            here: true,
            branch: feature,
            spec: None,
            base: None,
        },
    )
}

fn branch_mode(root: &Path, branch: Option<&str>) -> Result<String> {
    start(
        root,
        &StartArgs {
            setup_only: false,
            here: false,
            branch,
            spec: None,
            base: Some("main"),
        },
    )
}

fn head(root: &Path) -> String {
    git(root, &["symbolic-ref", "--short", "HEAD"])
}

fn peek(root: &Path) -> crate::state::RunState {
    Workspace::current(root)
        .unwrap()
        .unwrap()
        .peek()
        .unwrap()
        .unwrap()
}

#[test]
fn here_stays_on_the_current_branch_and_names_the_folder_after_the_feature() {
    let dir = repo_with_setup();
    let root = dir.path();
    fs::write(root.join("wip.txt"), "uncommitted user work").unwrap();
    let out = here(root, Some("payment-retry")).unwrap();
    assert!(out.contains("on the current branch `main`"), "{out}");

    assert_eq!(head(root), "main");
    assert!(!crate::git::branch_exists(root, "payment-retry").unwrap());
    assert!(root.join("planvil/payment-retry/FEATURE.md").exists());
    assert_eq!(
        fs::read_to_string(root.join("wip.txt")).unwrap(),
        "uncommitted user work"
    );
    let gitignore = fs::read_to_string(root.join(".gitignore")).unwrap();
    assert!(gitignore.contains("/planvil/"), "{gitignore}");

    let s = peek(root);
    assert!(s.on_current_branch);
    assert_eq!(
        (s.branch.as_str(), s.feature_id.as_str()),
        ("main", "payment-retry")
    );
    assert_eq!(s.stage, Stage::Feature);
    assert!(
        s.baseline_dirty.contains(&"wip.txt".to_string()),
        "{:?}",
        s.baseline_dirty
    );
}

#[test]
fn every_command_finds_the_run_through_the_current_branch() {
    let dir = repo_with_setup();
    let root = dir.path();
    here(root, Some("payment-retry")).unwrap();
    let ws = Workspace::current(root).unwrap().unwrap();
    assert!(
        ws.feature_dir.ends_with("planvil/payment-retry"),
        "{:?}",
        ws.feature_dir
    );
    assert!(status(root).unwrap().contains("stage: Feature"));
    // Plain `start` resumes it.
    let out = branch_mode(root, None).unwrap();
    assert!(out.contains("resumed"), "{out}");
    assert_eq!(head(root), "main");
}

#[test]
fn here_needs_a_feature_name_unless_resuming() {
    let dir = repo_with_setup();
    let err = here(dir.path(), None).unwrap_err().to_string();
    assert!(err.contains("NEEDS_FEATURE_NAME"), "{err}");
    here(dir.path(), Some("payment-retry")).unwrap();
    let out = here(dir.path(), None).unwrap();
    assert!(out.contains("resumed"), "{out}");
}

#[test]
fn one_unfinished_run_per_branch() {
    let dir = repo_with_setup();
    here(dir.path(), Some("payment-retry")).unwrap();
    let err = here(dir.path(), Some("other")).unwrap_err().to_string();
    assert!(err.contains("`payment-retry` is in progress"), "{err}");
    control(dir.path(), Control::Cancel).unwrap();
    here(dir.path(), Some("other")).unwrap();
    assert_eq!(peek(dir.path()).feature_id, "other");
}

#[test]
fn a_finished_here_run_does_not_capture_a_later_branch_run() {
    let dir = repo_with_setup();
    let root = dir.path();
    here(root, Some("payment-retry")).unwrap();
    control(root, Control::Cancel).unwrap();
    git(root, &["add", ".gitignore"]);
    git(root, &["commit", "--quiet", "-m", "ignore planvil docs"]);
    // A normal run on its own branch uses the branch's folder.
    branch_mode(root, Some("sp20/next")).unwrap();
    assert_eq!(head(root), "sp20/next");
    let s = peek(root);
    assert!(!s.on_current_branch);
    assert_eq!(s.feature_id, "sp20-next");
}

#[test]
fn a_feature_name_used_by_another_branch_is_refused() {
    let dir = repo_with_setup();
    let root = dir.path();
    branch_mode(root, Some("payment-retry")).unwrap();
    control(root, Control::Pause).unwrap();
    git(root, &["checkout", "--quiet", "main"]);
    let err = here(root, Some("payment-retry")).unwrap_err().to_string();
    assert!(err.contains("pick another feature name"), "{err}");
}

#[test]
fn rename_is_refused_for_a_run_on_the_current_branch() {
    let dir = repo_with_setup();
    here(dir.path(), Some("payment-retry")).unwrap();
    let err = rename(dir.path(), "feat/x").unwrap_err().to_string();
    assert!(err.contains("does not own its branch"), "{err}");
    assert_eq!(head(dir.path()), "main");
    assert_eq!(peek(dir.path()).status, Status::Active);
}

#[test]
fn here_runs_on_a_detached_head_are_refused() {
    let dir = repo_with_setup();
    let sha = git(dir.path(), &["rev-parse", "HEAD"]);
    git(dir.path(), &["checkout", "--quiet", &sha]);
    let err = here(dir.path(), Some("x")).unwrap_err().to_string();
    assert!(err.contains("detached"), "{err}");
}

#[test]
fn a_corrupt_branch_index_fails_closed() {
    let dir = repo_with_setup();
    here(dir.path(), Some("payment-retry")).unwrap();
    fs::write(
        dir.path().join("planvil/.planvil/branches.json"),
        "{not json",
    )
    .unwrap();
    let err = Workspace::current(dir.path()).unwrap_err().to_string();
    assert!(err.contains("branches.json"), "{err}");
    assert!(
        here(dir.path(), Some("other")).is_err(),
        "never overwritten"
    );
}

#[test]
fn a_branch_whose_folder_holds_a_here_run_is_refused_clearly() {
    let dir = repo_with_setup();
    let root = dir.path();
    here(root, Some("payment")).unwrap();
    control(root, Control::Cancel).unwrap();
    git(root, &["add", ".gitignore"]);
    git(root, &["commit", "--quiet", "-m", "ignore planvil docs"]);
    let err = branch_mode(root, Some("payment")).unwrap_err().to_string();
    assert!(err.contains("belongs to a run on branch `main`"), "{err}");
    assert_eq!(head(root), "main", "refused before switching");
    assert!(!crate::git::branch_exists(root, "payment").unwrap());
}
