//! Integration tests that run the real `planvil` binary, as Claude Code and users do.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn planvil(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_planvil"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn hook(dir: &Path, event: &str, payload: serde_json::Value) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_planvil"))
        .args(["hook", "claude", event])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "hooks always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join(".planvil.json"), r#"{"verify": {}}"#).unwrap();
    git(dir.path(), &["add", "."]);
    git(
        dir.path(),
        &[
            "-c",
            "user.email=t@e",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    dir
}

#[test]
fn needs_input_uses_exit_code_3_with_a_stable_code() {
    let dir = repo();
    let out = planvil(dir.path(), &["start", "sp20/x"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("NEEDS_BASE_BRANCH"));
}

#[test]
fn start_then_hooks_enforce_the_feature_stage() {
    let dir = repo();
    let root = dir.path().canonicalize().unwrap();
    let out = planvil(&root, &["start", "sp20/x", "--base", "main"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let deny = hook(
        &root,
        "pre-tool",
        serde_json::json!({ "session_id": "s", "cwd": root, "tool_name": "Write", "tool_input": { "file_path": root.join("main.go") } }),
    );
    let v: serde_json::Value = serde_json::from_str(&deny).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");

    let allow = hook(
        &root,
        "pre-tool",
        serde_json::json!({ "session_id": "s", "cwd": root, "tool_name": "Write", "tool_input": { "file_path": root.join("planvil/sp20-x/FEATURE.md") } }),
    );
    assert_eq!(allow.trim(), "");

    let advance = planvil(&root, &["advance"]);
    assert_eq!(advance.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&advance.stderr).contains("gate for Feature not met"));
}

#[test]
fn unknown_hook_event_fails_loudly() {
    let dir = repo();
    let out = planvil(dir.path(), &["hook", "claude", "bogus"]);
    assert!(!out.status.success());
}

#[test]
fn report_refuses_an_unfinished_run() {
    let dir = repo();
    planvil(dir.path(), &["start", "sp20/x", "--base", "main"]);
    let out = planvil(dir.path(), &["report"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not finished"));
}

#[test]
fn user_controls_refuse_to_run_inside_claude_codes_shell() {
    let dir = repo();
    planvil(dir.path(), &["start", "sp20/x", "--base", "main"]);
    let out = Command::new(env!("CARGO_BIN_EXE_planvil"))
        .args(["cancel"])
        .env("CLAUDECODE", "1")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("user action"));
    let out = Command::new(env!("CARGO_BIN_EXE_planvil"))
        .args(["cancel"])
        .env_remove("CLAUDECODE")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn launcher(
    dir: &Path,
    event: &str,
    payload: &str,
    planvil_bin: Option<&str>,
    home: &Path,
) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin/bin/planvil-hook");
    let mut cmd = Command::new("sh");
    cmd.arg(script)
        .arg(event)
        .current_dir(dir)
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env_remove("PLANVIL_BIN")
        .env_remove("CLAUDE_PLUGIN_DATA")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if let Some(bin) = planvil_bin {
        cmd.env("PLANVIL_BIN", bin);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn launcher_without_the_binary_never_blocks_and_explains_at_session_start() {
    let dir = repo();
    let home = tempfile::tempdir().unwrap();
    let payload = r#"{"session_id":"s","tool_name":"Write","tool_input":{"file_path":"/tmp/x"}}"#;
    let out = launcher(dir.path(), "pre-tool", payload, None, home.path());
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let out = launcher(dir.path(), "session-start", "{}", None, home.path());
    assert!(String::from_utf8_lossy(&out.stdout).contains("binary is not"));
}

#[test]
fn launcher_with_the_binary_enforces() {
    let dir = repo();
    let root = dir.path().canonicalize().unwrap();
    planvil(&root, &["start", "sp20/x", "--base", "main"]);
    let home = tempfile::tempdir().unwrap();
    let payload = serde_json::json!({ "session_id": "s", "cwd": root, "tool_name": "Write", "tool_input": { "file_path": root.join("main.go") } }).to_string();
    let out = launcher(
        &root,
        "pre-tool",
        &payload,
        Some(env!("CARGO_BIN_EXE_planvil")),
        home.path(),
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
}
