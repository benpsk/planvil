//! Claude Code adapter: translates hook payloads into core calls and core decisions into
//! Claude Code's hook output format. No workflow rules live here.
//!
//! Claude Code treats a crashed or failing hook as non-blocking, so every path here must
//! produce an explicit answer; errors in `pre-tool` deny mutation-capable calls.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::git;
use crate::heartbeat;
use crate::machine::{self, Event};
use crate::ops::{self, Control, Workspace};
use crate::patch;
use crate::policy::{self, Call, Context, Decision, stage_name};
use crate::shell;
use crate::state::{RunState, Status, StopGuard};
use crate::store;
use crate::util::now_secs;

const HOOK_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TOOLS: [&str; 3] = ["Edit", "Write", "MultiEdit"];
const NOTEBOOK_TOOLS: [&str; 1] = ["NotebookEdit"];
const BASH_TOOLS: [&str; 1] = ["Bash"];
/// Tools whose result is the user's own answer to a structured question.
const ASK_TOOLS: [&str; 2] = ["AskUserQuestion", "request_user_input"];
/// Codex edits files through one patch that can touch several files.
const PATCH_TOOL: &str = "apply_patch";

/// The agent host whose hooks are calling. Payloads and outputs share one format; tool
/// names and edit shapes differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Claude,
    Codex,
}

impl Host {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreTool,
    PostTool,
    Stop,
    Prompt,
    SessionStart,
}

impl HookEvent {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "pre-tool" => Some(Self::PreTool),
            "post-tool" => Some(Self::PostTool),
            "stop" => Some(Self::Stop),
            "prompt" => Some(Self::Prompt),
            "session-start" => Some(Self::SessionStart),
            _ => None,
        }
    }

    fn claude_name(self) -> &'static str {
        match self {
            Self::PreTool => "PreToolUse",
            Self::PostTool => "PostToolUse",
            Self::Stop => "Stop",
            Self::Prompt => "UserPromptSubmit",
            Self::SessionStart => "SessionStart",
        }
    }
}

/// Fields planvil reads. Unknown fields are ignored: the host adds new ones over time.
#[derive(Debug, Default, Deserialize)]
pub struct Input {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub prompt: Option<String>,
    pub tool_response: Option<Value>,
}

/// Longest user reply kept in run state.
const MAX_REPLY_CHARS: usize = 2000;

fn truncate(text: &str) -> String {
    text.chars().take(MAX_REPLY_CHARS).collect()
}

/// The user's answer from an AskUserQuestion result, as text.
fn reply_text(response: Option<&Value>) -> Option<String> {
    let value = response?;
    let text = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    Some(truncate(&text))
}

/// JSON to print on stdout (if any). Hooks always exit 0; decisions travel in the JSON.
pub type Output = Option<Value>;

pub fn run(host: Host, event: HookEvent, stdin: &str, process_cwd: &Path) -> Output {
    let parsed: Result<Input, _> = serde_json::from_str(stdin);
    let malformed = parsed.is_err();
    let input = parsed.unwrap_or_default();
    let cwd = input
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| process_cwd.to_path_buf());
    let located = locate(&cwd);
    match event {
        HookEvent::PreTool => pre_tool(host, &input, &cwd, located, malformed),
        HookEvent::PostTool => post_tool(&input, located),
        HookEvent::Stop => stop(&input, located),
        HookEvent::Prompt => prompt(&input, &cwd, located),
        HookEvent::SessionStart => session_start(&input, located),
    }
}

/// Output for a hook that failed internally (used for panics too).
pub fn failure_output(event: HookEvent, message: &str) -> Output {
    match event {
        HookEvent::PreTool => Some(deny(&format!(
            "planvil hook failed ({message}). Mutation-capable actions are denied until this is fixed. \
             The user can run `planvil doctor`, or `planvil cancel` to end the run."
        ))),
        _ => Some(json!({ "systemMessage": format!("planvil hook error: {message}") })),
    }
}

type Located = ops::Result<Option<(Workspace, RunState)>>;

/// The current branch's unfinished-or-finished run, if any.
fn locate(cwd: &Path) -> Located {
    let Some(ws) = Workspace::current(cwd)? else {
        return Ok(None);
    };
    Ok(ws.peek()?.map(|state| (ws, state)))
}

fn deny(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

fn context(event: HookEvent, text: &str) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": event.claude_name(), "additionalContext": text } })
}

fn str_field<'a>(input: &'a Option<Value>, key: &str) -> Option<&'a str> {
    input.as_ref()?.get(key)?.as_str()
}

fn to_call(input: &Input) -> Option<Call> {
    let tool = input.tool_name.clone().unwrap_or_default();
    let path_key = if NOTEBOOK_TOOLS.contains(&tool.as_str()) {
        "notebook_path"
    } else {
        "file_path"
    };
    if WRITE_TOOLS.contains(&tool.as_str()) || NOTEBOOK_TOOLS.contains(&tool.as_str()) {
        let path = str_field(&input.tool_input, path_key)
            .or_else(|| str_field(&input.tool_input, "file_path"))?;
        return Some(Call::Write {
            tool,
            path: PathBuf::from(path),
        });
    }
    if BASH_TOOLS.contains(&tool.as_str()) {
        let command = str_field(&input.tool_input, "command")?;
        return Some(Call::Bash {
            command: command.to_string(),
        });
    }
    let isolated = str_field(&input.tool_input, "isolation") == Some("worktree");
    Some(Call::Other { tool, isolated })
}

/// The calls to check for one tool use. A Codex patch becomes one write per file it
/// touches; `None` means the target cannot be determined (and is denied during a run).
fn to_calls(host: Host, input: &Input) -> Option<Vec<Call>> {
    let tool = input.tool_name.as_deref().unwrap_or("");
    if host == Host::Codex && tool == PATCH_TOOL {
        let paths = patch::touched_paths(str_field(&input.tool_input, "command")?);
        if paths.is_empty() {
            return None;
        }
        let writes = paths.into_iter().map(|p| Call::Write {
            tool: PATCH_TOOL.into(),
            path: PathBuf::from(p),
        });
        return Some(writes.collect());
    }
    to_call(input).map(|c| vec![c])
}

/// Whether a call is safe to allow even when planvil cannot read its own state.
fn harmless_without_state(call: Option<&Call>) -> bool {
    match call {
        Some(Call::Bash { command }) => {
            shell::read_only(command, &[]).is_ok() && shell::forbidden_planvil(command).is_none()
        }
        Some(Call::Other { tool, .. }) => matches!(tool.as_str(), "Read" | "Glob" | "Grep" | "LS"),
        Some(Call::Write { .. }) | None => false,
    }
}

fn pre_tool(host: Host, input: &Input, cwd: &Path, located: Located, malformed: bool) -> Output {
    let calls = to_calls(host, input);
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let runs_planvil = calls
        .iter()
        .flatten()
        .any(|c| matches!(c, Call::Bash { command } if command.contains("planvil")));
    if runs_planvil {
        // Lets `planvil start` prove that hooks run in this host (see `heartbeat`).
        heartbeat::touch();
    }
    let Some(calls) = calls else {
        return enforce(None, &cwd, located, malformed);
    };
    for call in &calls {
        // A write can target a different repository than the session's working directory.
        if let Call::Write { path, .. } = call
            && let Some(out) = enforce_target_repo(Some(call), path, &cwd)
        {
            return Some(out);
        }
        if let Some(out) = enforce(Some(call), &cwd, clone_located(&located), malformed) {
            return Some(out);
        }
    }
    None
}

/// `Located` holds a non-clonable error; re-derive it for each call of a multi-file patch.
fn clone_located(located: &Located) -> Located {
    match located {
        Ok(found) => Ok(found.clone()),
        Err(e) => Err(ops::OpsError::Refused(e.to_string())),
    }
}

/// Evaluate a write against the repository that contains its target, when that differs
/// from the working directory's repository.
fn enforce_target_repo(call: Option<&Call>, path: &Path, cwd: &Path) -> Output {
    let target = policy::resolve(cwd, path).unwrap_or_else(|| cwd.join(path));
    let dir = git::existing_ancestor(&target);
    let here = git::toplevel(cwd).ok().flatten();
    let there = git::toplevel(&dir);
    match there {
        Ok(Some(root)) if Some(&root) != here.as_ref() => enforce(call, cwd, locate(&dir), false),
        Ok(_) => None,
        Err(e) => Some(deny(&format!(
            "planvil cannot check the repository of {} ({e}); denying.",
            target.display()
        ))),
    }
}

fn enforce(call: Option<&Call>, cwd: &Path, located: Located, malformed: bool) -> Output {
    let (ws, state) = match located {
        Ok(Some(found)) => found,
        Ok(None) => return foreign_run_check(call, cwd),
        Err(_) if harmless_without_state(call) => return None,
        Err(e) => {
            return Some(deny(&format!(
                "planvil cannot read this branch's run ({e}). Mutation-capable actions are denied. The user \
                 can fix the file, or type `planvil cancel`."
            )));
        }
    };
    if malformed {
        return Some(deny(
            "planvil received a malformed hook payload; denying to be safe.",
        ));
    }
    let Some(call) = call else {
        return Some(deny(
            "planvil could not read the target path or command of this tool call; denying to be safe.",
        ));
    };
    let ctx = Context {
        stage: state.stage,
        status: state.status,
        repo_root: &ws.repo_root,
        feature_dir: &ws.feature_dir,
        cwd,
        config: &ws.config,
    };
    match policy::decide(&ctx, call) {
        Decision::Allow => None,
        Decision::Deny(why) => Some(deny(&format!(
            "planvil (branch `{}`, stage {}): {why}\nNext: {}",
            ws.branch,
            stage_name(state.stage),
            ops::next_action(&ws, &state)
        ))),
    }
}

/// No run on HEAD's branch: still refuse mutations if a live run exists on another branch
/// of this checkout (HEAD was switched or detached away from it).
fn foreign_run_check(call: Option<&Call>, cwd: &Path) -> Output {
    if harmless_without_state(call) {
        return None;
    }
    match ops::foreign_run(cwd) {
        Ok(None) => None,
        Ok(Some(run)) => Some(deny(&format!(
            "a planvil run on `{}` is in progress ({:?}, {:?}), but this checkout is no longer on that \
             branch. Ask the user to switch back to `{}`, or to type `planvil pause` or `planvil cancel`.",
            run.branch, run.stage, run.status, run.branch
        ))),
        Err(e) => Some(deny(&format!(
            "planvil cannot check for an active run ({e}); denying to be safe."
        ))),
    }
}

/// Run `update` on the locked state; best effort, since hooks must not hang.
fn update_state(
    ws: &Workspace,
    update: impl FnOnce(RunState) -> RunState,
) -> Result<RunState, String> {
    let lock =
        store::lock(&ws.feature_dir, &ws.branch, HOOK_LOCK_TIMEOUT).map_err(|e| e.to_string())?;
    let state = lock
        .load()
        .map_err(|e| e.to_string())?
        .ok_or("run disappeared")?;
    let next = update(state);
    lock.save(&next).map_err(|e| e.to_string())?;
    Ok(next)
}

fn post_tool(input: &Input, located: Located) -> Output {
    let Ok(Some((ws, state))) = located else {
        return None;
    };
    if state.status.is_terminal() {
        return None;
    }
    let tool = input.tool_name.as_deref().unwrap_or("");
    let session = input.session_id.clone();
    let claim = state.session_id.is_none() && !ASK_TOOLS.contains(&tool) && session.is_some();
    // An answered AskUserQuestion is genuine user input, like a typed prompt.
    let user_answered = ASK_TOOLS.contains(&tool);
    if !claim && !user_answered {
        return None;
    }
    let result = update_state(&ws, |s| RunState {
        session_id: if claim && s.session_id.is_none() {
            session
        } else {
            s.session_id.clone()
        },
        user_prompts: if user_answered {
            s.user_prompts.saturating_add(1)
        } else {
            s.user_prompts
        },
        last_user_reply: if user_answered {
            reply_text(input.tool_response.as_ref())
        } else {
            s.last_user_reply.clone()
        },
        ..s
    });
    result
        .err()
        .map(|e| json!({ "systemMessage": format!("planvil could not update run metadata: {e}") }))
}

fn stop(input: &Input, located: Located) -> Output {
    let (ws, state) = match located {
        Ok(Some(found)) => found,
        Ok(None) => return None,
        Err(e) => {
            return Some(json!({
                "systemMessage": format!("planvil could not read its run state ({e}); not continuing the workflow.")
            }));
        }
    };
    let attached = state.session_id.is_some() && state.session_id == input.session_id;
    if state.status != Status::Active || !attached {
        return None;
    }
    let now = now_secs();
    if let Some(why) = ops::time_exceeded(&state, now) {
        return block_and_release(&ws, &why);
    }
    let marker = state.progress_marker();
    let continuations = if state.stop_guard.progress_marker == marker {
        state.stop_guard.continuations.saturating_add(1)
    } else {
        1
    };
    if continuations > state.limits.max_stop_continuations {
        let why = format!(
            "the agent tried to stop {} times at stage {} without progress",
            continuations - 1,
            stage_name(state.stage)
        );
        return block_and_release(&ws, &why);
    }
    let guard = StopGuard {
        continuations,
        progress_marker: marker,
    };
    if let Err(e) = update_state(&ws, |s| RunState {
        stop_guard: guard,
        ..s
    }) {
        return Some(
            json!({ "systemMessage": format!("planvil could not record the stop check ({e}); not continuing.") }),
        );
    }
    Some(json!({
        "decision": "block",
        "reason": format!(
            "planvil: the run on `{}` is not finished (stage {}). Next: {}\nDo not stop until planvil reports \
             Done, Blocked, or a question for the user (`planvil ask`). Run `planvil status` if unsure.",
            ws.branch,
            stage_name(state.stage),
            ops::next_action(&ws, &state)
        ),
    }))
}

/// Mark the run blocked and let the agent stop, telling the user why.
fn block_and_release(ws: &Workspace, why: &str) -> Output {
    let mut applied = Ok(());
    let saved = update_state(ws, |s| {
        match machine::apply(
            &s,
            &Event::Block {
                reason: why.to_string(),
            },
            now_secs(),
        ) {
            Ok(next) => next,
            Err(e) => {
                applied = Err(e.to_string());
                s
            }
        }
    });
    let msg = match (saved, applied) {
        (Ok(_), Ok(())) => format!(
            "planvil blocked the run: {why}. Review it with `planvil status`; type `planvil resume` to continue."
        ),
        (Ok(_), Err(e)) => {
            format!("planvil could not block the run ({why}): {e}. The run is unchanged.")
        }
        (Err(e), _) => {
            format!("planvil wanted to block the run ({why}) but could not save state: {e}")
        }
    };
    Some(json!({ "systemMessage": msg }))
}

/// `planvil pause|resume|cancel` (or the `/planvil[:planvil] <action>` form) typed by the user.
fn control_command(prompt: &str) -> Option<Control> {
    let p = prompt.trim();
    let rest = ["/planvil:planvil ", "/planvil ", "planvil "]
        .iter()
        .find_map(|prefix| p.strip_prefix(prefix))?;
    match rest.trim() {
        "pause" => Some(Control::Pause),
        "resume" => Some(Control::Resume),
        "cancel" => Some(Control::Cancel),
        _ => None,
    }
}

fn prompt(input: &Input, cwd: &Path, located: Located) -> Output {
    let (ws, state) = match located {
        Ok(Some(found)) => found,
        Ok(None) => return None,
        Err(e) => {
            return Some(context(
                HookEvent::Prompt,
                &format!("planvil: cannot read the run state ({e})."),
            ));
        }
    };
    let text = input.prompt.as_deref().unwrap_or("");
    if let Some(action) = control_command(text) {
        // The user typed this, so it is a genuine user action. Do not pass it to the model.
        let reason = match ops::control(cwd, action) {
            Ok(msg) => format!("planvil: {msg}"),
            Err(e) => format!("planvil: {e}"),
        };
        return Some(json!({ "decision": "block", "reason": reason }));
    }
    if state.status.is_terminal() {
        return None;
    }
    let recorded = update_state(&ws, |s| RunState {
        user_prompts: s.user_prompts.saturating_add(1),
        last_user_reply: Some(truncate(text)),
        ..s
    });
    let attached = state.session_id.is_none() || state.session_id == input.session_id;
    let mut note = if attached {
        format!(
            "planvil run on `{}`: stage {}, status {:?}. Next: {}",
            ws.branch,
            stage_name(state.stage),
            state.status,
            ops::next_action(&ws, &state)
        )
    } else {
        format!(
            "A planvil run on `{}` is attached to another session. Run /planvil to attach it here.",
            ws.branch
        )
    };
    if let Err(e) = recorded {
        note.push_str(&format!(
            " (could not record this message for answers: {e})"
        ));
    }
    Some(context(HookEvent::Prompt, &note))
}

fn session_start(input: &Input, located: Located) -> Output {
    let Ok(Some((ws, state))) = located else {
        return None;
    };
    if state.status.is_terminal() {
        return None;
    }
    let attached = state.session_id.is_some() && state.session_id == input.session_id;
    let text = if attached {
        format!(
            "planvil run on `{}` continues in this session: stage {}, status {:?}. Re-check the files before \
             acting. Next: {}",
            ws.branch,
            stage_name(state.stage),
            state.status,
            ops::next_action(&ws, &state)
        )
    } else {
        format!(
            "An unfinished planvil run exists on branch `{}` (stage {}, status {:?}). It is not attached to \
             this session. To continue it, run /planvil; it reloads state and rechecks the code.",
            ws.branch,
            stage_name(state.stage),
            state.status
        )
    };
    Some(context(HookEvent::SessionStart, &text))
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "hooks_codex_tests.rs"]
mod codex_tests;
