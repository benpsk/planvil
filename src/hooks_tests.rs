//! Hook adapter tests using Claude Code-shaped payload fixtures.

use super::*;
use crate::git::testing::repo;
use crate::ops::{StartArgs, start};
use crate::state::Stage;
use std::fs;

struct Fx {
    dir: tempfile::TempDir,
}

impl Fx {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn hook(&self, event: HookEvent, payload: Value) -> Output {
        let mut payload = payload;
        payload["cwd"] = json!(self.root().display().to_string());
        payload["hook_event_name"] = json!(event.claude_name());
        payload
            .as_object_mut()
            .unwrap()
            .entry("session_id")
            .or_insert(json!("s1"));
        run(event, &payload.to_string(), self.root())
    }

    fn pre(&self, tool: &str, tool_input: Value) -> Output {
        self.hook(
            HookEvent::PreTool,
            json!({ "tool_name": tool, "tool_input": tool_input }),
        )
    }

    fn state(&self) -> RunState {
        Workspace::current(self.root())
            .unwrap()
            .unwrap()
            .peek()
            .unwrap()
            .unwrap()
    }

    fn feature(&self) -> PathBuf {
        self.root().join("planvil/sp20-x")
    }
}

fn started() -> Fx {
    let dir = crate::git::testing::repo_with_setup();
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
    Fx { dir }
}

fn is_deny(out: &Output) -> bool {
    out.as_ref()
        .is_some_and(|v| v["hookSpecificOutput"]["permissionDecision"] == "deny")
}

#[test]
fn no_run_means_no_output() {
    let dir = repo();
    let fx = Fx { dir };
    assert_eq!(fx.pre("Edit", json!({ "file_path": "src/a.go" })), None);
    assert_eq!(
        fx.hook(HookEvent::Stop, json!({ "stop_hook_active": false })),
        None
    );
}

#[test]
fn outside_a_repo_means_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        HookEvent::PreTool,
        &json!({ "cwd": dir.path(), "tool_name": "Write", "tool_input": { "file_path": "x" } })
            .to_string(),
        dir.path(),
    );
    assert_eq!(out, None);
}

#[test]
fn source_edit_before_plan_gate_is_denied_with_next_action() {
    let fx = started();
    let out = fx.pre("Edit", json!({ "file_path": fx.root().join("README.md") }));
    assert!(is_deny(&out), "{out:?}");
    let reason = out.unwrap()["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        reason.contains("stage Feature") && reason.contains("Next:"),
        "{reason}"
    );
}

#[test]
fn workflow_doc_edits_are_allowed() {
    let fx = started();
    assert_eq!(
        fx.pre(
            "Write",
            json!({ "file_path": fx.feature().join("FEATURE.md") })
        ),
        None
    );
    assert_eq!(
        fx.pre("Read", json!({ "file_path": fx.root().join("README.md") })),
        None
    );
}

#[test]
fn shell_writes_and_user_actions_are_denied() {
    let fx = started();
    assert!(is_deny(
        &fx.pre("Bash", json!({ "command": "echo x > README.md" }))
    ));
    assert!(is_deny(
        &fx.pre("Bash", json!({ "command": "planvil cancel" }))
    ));
    assert_eq!(fx.pre("Bash", json!({ "command": "git status" })), None);
    assert!(is_deny(&fx.pre(
        "NotebookEdit",
        json!({ "notebook_path": fx.root().join("n.ipynb") })
    )));
}

#[test]
fn missing_tool_input_is_denied() {
    let fx = started();
    assert!(is_deny(&fx.pre("Edit", json!({}))));
}

#[test]
fn malformed_payload_with_an_active_run_is_denied() {
    let fx = started();
    assert!(is_deny(&run(HookEvent::PreTool, "{not json", fx.root())));
}

#[test]
fn corrupt_state_denies_mutation_but_allows_reads() {
    let fx = started();
    fs::write(fx.feature().join(".planvil/state.json"), "{garbage").unwrap();
    assert!(is_deny(
        &fx.pre("Write", json!({ "file_path": fx.root().join("a.go") }))
    ));
    assert_eq!(
        fx.pre("Read", json!({ "file_path": fx.root().join("a.go") })),
        None
    );
    assert_eq!(fx.pre("Bash", json!({ "command": "ls" })), None);
}

#[test]
fn post_tool_bash_attaches_the_session_once() {
    let fx = started();
    assert_eq!(fx.state().session_id, None);
    fx.hook(
        HookEvent::PostTool,
        json!({ "tool_name": "Bash", "session_id": "s1" }),
    );
    assert_eq!(fx.state().session_id.as_deref(), Some("s1"));
    fx.hook(
        HookEvent::PostTool,
        json!({ "tool_name": "Bash", "session_id": "s2" }),
    );
    assert_eq!(fx.state().session_id.as_deref(), Some("s1"));
}

#[test]
fn answered_ask_user_question_counts_as_user_input() {
    let fx = started();
    assert_eq!(fx.state().user_prompts, 0);
    fx.hook(
        HookEvent::PostTool,
        json!({ "tool_name": "AskUserQuestion", "tool_response": { "answers": { "Right?": "Yes" } } }),
    );
    assert_eq!(fx.state().user_prompts, 1);
    assert!(fx.state().last_user_reply.unwrap().contains("Yes"));
}

#[test]
fn stop_continues_an_unfinished_attached_run() {
    let fx = started();
    fx.hook(HookEvent::PostTool, json!({ "tool_name": "Bash" }));
    let out = fx
        .hook(HookEvent::Stop, json!({ "stop_hook_active": false }))
        .unwrap();
    assert_eq!(out["decision"], "block");
    assert!(out["reason"].as_str().unwrap().contains("Next:"));
}

#[test]
fn stop_is_bounded_when_no_progress_is_made() {
    let fx = started();
    fx.hook(HookEvent::PostTool, json!({ "tool_name": "Bash" }));
    for _ in 0..3 {
        let out = fx
            .hook(HookEvent::Stop, json!({ "stop_hook_active": true }))
            .unwrap();
        assert_eq!(out["decision"], "block");
    }
    let out = fx
        .hook(HookEvent::Stop, json!({ "stop_hook_active": true }))
        .unwrap();
    assert!(out.get("decision").is_none(), "{out}");
    assert!(out["systemMessage"].as_str().unwrap().contains("blocked"));
    assert_eq!(fx.state().status, Status::Blocked);
    assert_eq!(fx.hook(HookEvent::Stop, json!({})), None);
}

#[test]
fn stop_does_not_trap_other_sessions() {
    let fx = started();
    fx.hook(
        HookEvent::PostTool,
        json!({ "tool_name": "Bash", "session_id": "owner" }),
    );
    assert_eq!(
        fx.hook(HookEvent::Stop, json!({ "session_id": "other" })),
        None
    );
}

#[test]
fn stop_yields_while_waiting_for_the_user() {
    let fx = started();
    fx.hook(HookEvent::PostTool, json!({ "tool_name": "Bash" }));
    fs::write(
        fx.feature().join("FEATURE.md"),
        "## Goal\nx\n## Scope\nx\n## Out of scope\nNone\n## Behavior\nx\n## Acceptance criteria\n| ID | C |\n|---|---|\n| AC-01 | x |\n",
    )
    .unwrap();
    ops::ask(
        fx.root(),
        "Right?",
        crate::state::QuestionKind::ConfirmFeature,
    )
    .unwrap();
    assert_eq!(fx.hook(HookEvent::Stop, json!({})), None);
}

#[test]
fn typed_cancel_is_applied_and_not_sent_to_the_model() {
    let fx = started();
    let out = fx
        .hook(HookEvent::Prompt, json!({ "prompt": "planvil cancel" }))
        .unwrap();
    assert_eq!(out["decision"], "block");
    assert_eq!(fx.state().status, Status::Cancelled);
}

#[test]
fn slash_form_pause_and_resume_work() {
    let fx = started();
    fx.hook(
        HookEvent::Prompt,
        json!({ "prompt": "/planvil:planvil pause" }),
    );
    assert_eq!(fx.state().status, Status::Paused);
    fx.hook(HookEvent::Prompt, json!({ "prompt": "/planvil resume" }));
    assert_eq!(fx.state().status, Status::Active);
}

#[test]
fn ordinary_prompt_records_time_and_adds_context() {
    let fx = started();
    let out = fx
        .hook(HookEvent::Prompt, json!({ "prompt": "yes, that is right" }))
        .unwrap();
    let ctx = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("stage Feature"), "{ctx}");
    assert_eq!(fx.state().user_prompts, 1);
    assert_eq!(
        fx.state().last_user_reply.as_deref(),
        Some("yes, that is right")
    );
    assert_eq!(fx.state().stage, Stage::Feature);
}

#[test]
fn session_start_points_an_unattached_session_at_planvil() {
    let fx = started();
    fx.hook(
        HookEvent::PostTool,
        json!({ "tool_name": "Bash", "session_id": "old" }),
    );
    let out = fx
        .hook(
            HookEvent::SessionStart,
            json!({ "session_id": "new", "source": "startup" }),
        )
        .unwrap();
    let ctx = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        ctx.contains("not attached") && ctx.contains("/planvil"),
        "{ctx}"
    );
}

#[test]
fn failure_output_denies_for_pre_tool_only() {
    assert!(is_deny(&failure_output(HookEvent::PreTool, "boom")));
    assert!(
        failure_output(HookEvent::Stop, "boom")
            .unwrap()
            .get("decision")
            .is_none()
    );
}

#[test]
fn switching_branches_away_from_the_run_does_not_disable_enforcement() {
    let fx = started();
    crate::git::testing::run(fx.root(), &["checkout", "-q", "-b", "escape"]);
    let out = fx.pre("Write", json!({ "file_path": fx.root().join("README.md") }));
    assert!(is_deny(&out), "{out:?}");
    crate::git::testing::run(fx.root(), &["checkout", "-q", "--detach"]);
    assert!(is_deny(
        &fx.pre("Bash", json!({ "command": "rm README.md" }))
    ));
    assert_eq!(
        fx.pre("Read", json!({ "file_path": fx.root().join("README.md") })),
        None
    );
}

#[test]
fn paused_runs_do_not_block_other_branches() {
    let fx = started();
    ops::control(fx.root(), ops::Control::Pause).unwrap();
    crate::git::testing::run(fx.root(), &["checkout", "-q", "-b", "hotfix"]);
    assert_eq!(
        fx.pre("Write", json!({ "file_path": fx.root().join("README.md") })),
        None
    );
}

#[test]
fn writes_into_a_run_repo_from_another_cwd_are_enforced() {
    let fx = started();
    let elsewhere = tempfile::tempdir().unwrap();
    let payload = json!({
        "session_id": "s1",
        "cwd": elsewhere.path(),
        "tool_name": "Write",
        "tool_input": { "file_path": fx.root().join("README.md") },
    });
    let out = run(HookEvent::PreTool, &payload.to_string(), elsewhere.path());
    assert!(is_deny(&out), "{out:?}");
}

#[test]
fn enforcement_follows_a_renamed_run() {
    let fx = started();
    ops::rename(fx.root(), "refactor/x").unwrap();
    assert!(is_deny(&fx.pre(
        "Write",
        json!({ "file_path": fx.root().join("README.md") })
    )));
    assert_eq!(
        fx.pre(
            "Write",
            json!({ "file_path": fx.root().join("planvil/refactor-x/FEATURE.md") })
        ),
        None
    );
    assert_eq!(
        fx.pre("Bash", json!({ "command": "planvil rename refactor/y" })),
        None
    );
}
