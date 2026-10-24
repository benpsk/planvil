//! Codex-shaped hook payloads: `Bash` and `apply_patch` tool calls, `request_user_input`.

use super::*;
use crate::git::testing::repo_with_setup;
use crate::ops::{StartArgs, start};

fn started() -> tempfile::TempDir {
    let dir = repo_with_setup();
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
    dir
}

fn codex(dir: &Path, event: HookEvent, mut payload: Value) -> Output {
    payload["cwd"] = json!(dir.display().to_string());
    payload
        .as_object_mut()
        .unwrap()
        .entry("session_id")
        .or_insert(json!("thread-1"));
    run(Host::Codex, event, &payload.to_string(), dir)
}

fn patch(dir: &Path, body: &str) -> Output {
    codex(
        dir,
        HookEvent::PreTool,
        json!({ "tool_name": "apply_patch", "tool_input": { "command": body } }),
    )
}

fn is_deny(out: &Output) -> bool {
    out.as_ref()
        .is_some_and(|v| v["hookSpecificOutput"]["permissionDecision"] == "deny")
}

#[test]
fn patch_editing_source_before_the_plan_is_denied() {
    let dir = started();
    let out = patch(
        dir.path(),
        "*** Begin Patch\n*** Update File: README.md\n@@\n-hello\n+hi\n*** End Patch\n",
    );
    assert!(is_deny(&out), "{out:?}");
}

#[test]
fn patch_touching_only_workflow_docs_is_allowed() {
    let dir = started();
    let out = patch(
        dir.path(),
        "*** Begin Patch\n*** Add File: planvil/sp20-x/notes.md\n+x\n*** Update File: planvil/sp20-x/FEATURE.md\n@@\n+y\n*** End Patch\n",
    );
    assert_eq!(out, None);
}

#[test]
fn one_forbidden_file_in_a_patch_denies_the_whole_patch() {
    let dir = started();
    let out = patch(
        dir.path(),
        "*** Begin Patch\n*** Add File: planvil/sp20-x/notes.md\n+x\n*** Delete File: src/main.go\n*** End Patch\n",
    );
    assert!(is_deny(&out), "{out:?}");
}

#[test]
fn move_target_is_checked_too() {
    let dir = started();
    let out = patch(
        dir.path(),
        "*** Begin Patch\n*** Update File: planvil/sp20-x/notes.md\n*** Move to: src/escape.go\n*** End Patch\n",
    );
    assert!(is_deny(&out), "{out:?}");
}

#[test]
fn unreadable_patch_is_denied_during_a_run() {
    let dir = started();
    assert!(is_deny(&patch(dir.path(), "not a patch")));
}

#[test]
fn codex_bash_uses_the_same_shell_policy() {
    let dir = started();
    let deny = codex(
        dir.path(),
        HookEvent::PreTool,
        json!({ "tool_name": "Bash", "tool_input": { "command": "echo x > README.md" } }),
    );
    assert!(is_deny(&deny));
    let allow = codex(
        dir.path(),
        HookEvent::PreTool,
        json!({ "tool_name": "Bash", "tool_input": { "command": "git status" } }),
    );
    assert_eq!(allow, None);
}

#[test]
fn request_user_input_answers_count_as_user_replies() {
    let dir = started();
    codex(
        dir.path(),
        HookEvent::PostTool,
        json!({ "tool_name": "request_user_input", "tool_response": { "answers": { "q": "Approve" } } }),
    );
    let s = Workspace::current(dir.path())
        .unwrap()
        .unwrap()
        .peek()
        .unwrap()
        .unwrap();
    assert_eq!(s.user_prompts, 1);
    assert!(s.last_user_reply.unwrap().contains("Approve"));
}

#[test]
fn codex_stop_continues_an_attached_run() {
    let dir = started();
    codex(
        dir.path(),
        HookEvent::PostTool,
        json!({ "tool_name": "Bash", "tool_input": { "command": "planvil start" } }),
    );
    let out = codex(
        dir.path(),
        HookEvent::Stop,
        json!({ "stop_hook_active": false }),
    )
    .unwrap();
    assert_eq!(out["decision"], "block");
}

#[test]
fn running_planvil_through_codex_leaves_the_hook_heartbeat() {
    let dir = started();
    codex(
        dir.path(),
        HookEvent::PreTool,
        json!({ "tool_name": "Bash", "tool_input": { "command": "planvil status" } }),
    );
    assert!(crate::heartbeat::fresh(crate::heartbeat::FRESH));
}
