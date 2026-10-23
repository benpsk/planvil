//! Which agent actions a run allows at its current stage. Host-independent.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::shell;
use crate::state::{Stage, Status};
use crate::store::RUN_DIR;

/// Tools that only read or talk to the user. Anything else is mutation-capable or unknown.
const READ_ONLY_TOOLS: [&str; 22] = [
    "Read",
    "Glob",
    "Grep",
    "LS",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    "TodoRead",
    "AskUserQuestion",
    "Skill",
    "ToolSearch",
    "Agent",
    "Task",
    "TaskOutput",
    "BashOutput",
    "TaskCreate",
    "TaskUpdate",
    "TaskList",
    "TaskGet",
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
    "LSP",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Feature, Audit, Plan, Decide: application source is read-only.
    Prepare,
    /// Implement, Validate, Repair: source edits allowed, protected paths still guarded.
    Build,
    /// Done or terminal: nothing enforced.
    Off,
}

pub fn phase(stage: Stage, status: Status) -> Phase {
    if status.is_terminal() {
        return Phase::Off;
    }
    match stage {
        Stage::Setup | Stage::Feature | Stage::Audit | Stage::Plan | Stage::Decide => {
            Phase::Prepare
        }
        Stage::Implement | Stage::Validate | Stage::Repair => Phase::Build,
        Stage::Done => Phase::Off,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// A file-writing tool (`Edit`, `Write`, `NotebookEdit`, ...) on `path`.
    Write {
        tool: String,
        path: PathBuf,
    },
    Bash {
        command: String,
    },
    /// Any other tool. `isolated` marks a subagent started in its own worktree.
    Other {
        tool: String,
        isolated: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
}

pub struct Context<'a> {
    pub stage: Stage,
    pub status: Status,
    /// Canonical repository root.
    pub repo_root: &'a Path,
    /// Canonical feature folder.
    pub feature_dir: &'a Path,
    /// Working directory for relative paths.
    pub cwd: &'a Path,
    pub config: &'a Config,
}

/// Tools that can queue prompts or work outside this session; never allowed during a run.
const DENIED_TOOLS: [&str; 4] = [
    "CronCreate",
    "ScheduleWakeup",
    "RemoteTrigger",
    "EnterWorktree",
];

pub fn decide(ctx: &Context, call: &Call) -> Decision {
    let phase = phase(ctx.stage, ctx.status);
    if phase == Phase::Off {
        return Decision::Allow;
    }
    match call {
        Call::Write { tool, path } => decide_write(ctx, phase, tool, path),
        Call::Bash { command } => decide_bash(ctx, phase, command),
        Call::Other { tool, isolated } => decide_other(ctx, phase, tool, *isolated),
    }
}

fn decide_other(ctx: &Context, phase: Phase, tool: &str, isolated: bool) -> Decision {
    if DENIED_TOOLS.contains(&tool) {
        return Decision::Deny(format!(
            "`{tool}` can schedule prompts or move work outside this session, so it is not allowed \
             during a planvil run"
        ));
    }
    if isolated {
        return Decision::Deny(
            "subagents in their own worktree are outside the run's enforcement; run them in this \
             worktree instead"
                .into(),
        );
    }
    let allowed =
        READ_ONLY_TOOLS.contains(&tool) || ctx.config.allowed_tools.iter().any(|t| t == tool);
    if phase == Phase::Build || allowed {
        Decision::Allow
    } else {
        Decision::Deny(format!(
            "`{tool}` is not classified as read-only, and application changes are not allowed before \
             the {} stage gate passes. If this tool only reads, the user can add it to `allowed_tools` \
             in .planvil.json.",
            stage_name(ctx.stage)
        ))
    }
}

fn decide_write(ctx: &Context, phase: Phase, tool: &str, path: &Path) -> Decision {
    let Some(target) = resolve(ctx.cwd, path) else {
        return Decision::Deny(format!(
            "`{tool}` on {} is not allowed: the path uses `..` through a folder that does not exist yet",
            path.display()
        ));
    };
    if let Some(why) = protected(ctx, &target) {
        return Decision::Deny(format!(
            "`{tool}` on {} is not allowed: {why}",
            target.display()
        ));
    }
    let inside_repo = within(&target, ctx.repo_root);
    match phase {
        Phase::Off => Decision::Allow,
        Phase::Prepare if within(&target, ctx.feature_dir) => Decision::Allow,
        Phase::Prepare => Decision::Deny(format!(
            "Stage {}: application files are read-only until the audit, plan and decision gates \
             pass. Only workflow docs under {} may be edited now.",
            stage_name(ctx.stage),
            ctx.feature_dir.display()
        )),
        Phase::Build if inside_repo || outside_allowed(&target) => Decision::Allow,
        Phase::Build => Decision::Deny(format!(
            "`{tool}` on {} is outside the repository; during a run only repository files, the \
             temp folder and Claude memory files may be written",
            target.display()
        )),
    }
}

fn decide_bash(ctx: &Context, phase: Phase, command: &str) -> Decision {
    if let Some(why) = shell::forbidden_planvil(command) {
        return Decision::Deny(why);
    }
    if let Some(why) = shell::destructive_git(command) {
        return Decision::Deny(why);
    }
    let read_only = shell::read_only(command, &ctx.config.readonly_commands);
    match (phase, read_only) {
        (_, Ok(())) => Decision::Allow,
        (Phase::Prepare, Err(why)) => Decision::Deny(format!(
            "Stage {}: only read-only commands may run before the plan gates pass ({why}).",
            stage_name(ctx.stage)
        )),
        (Phase::Build | Phase::Off, Err(_)) if touches_protected_text(command) => Decision::Deny(
            "this command references planvil's protected files (.planvil.json, .planvil run \
             state, or .claude settings and hooks). Only the planvil binary changes those."
                .into(),
        ),
        (Phase::Build | Phase::Off, Err(_)) => Decision::Allow,
    }
}

fn touches_protected_text(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [".planvil", ".claude/settings", ".claude/hooks", ".mcp.json"]
        .iter()
        .any(|p| lower.contains(p))
}

/// Case-folded on filesystems that are usually case-insensitive.
fn norm(path: &Path) -> PathBuf {
    if cfg!(any(target_os = "macos", windows)) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path.to_path_buf()
    }
}

fn within(path: &Path, dir: &Path) -> bool {
    norm(path).starts_with(norm(dir))
}

/// Paths outside the repository that a run may still write.
fn outside_allowed(target: &Path) -> bool {
    let temp = std::env::temp_dir();
    let temp = temp.canonicalize().unwrap_or(temp);
    let claude_memory = target
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with(".claude"))
        && target.components().any(|c| c.as_os_str() == "memory");
    within(target, &temp) || claude_memory
}

/// Why `target` is off-limits to the agent in any active stage.
fn protected(ctx: &Context, target: &Path) -> Option<&'static str> {
    let has_component = |name: &str| norm(target).components().any(|c| c.as_os_str() == name);
    if has_component(RUN_DIR) {
        return Some("planvil run state and evidence are written only by planvil");
    }
    if norm(target) == norm(&ctx.repo_root.join(crate::config::CONFIG_FILE)) {
        return Some(
            ".planvil.json holds the verification policy; changes need an explicit user action",
        );
    }
    let claude = ctx.repo_root.join(".claude");
    let settings = target
        .file_name()
        .is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with("settings"));
    if within(target, &claude) && (settings || within(target, &claude.join("hooks"))) {
        return Some("hook configuration is protected during a run");
    }
    if norm(target) == norm(&ctx.repo_root.join(".mcp.json")) {
        return Some("MCP server configuration is protected during a run");
    }
    if within(target, &ctx.repo_root.join(".git")) {
        return Some("git internals are protected");
    }
    let exe = std::env::current_exe()
        .ok()
        .and_then(|e| e.canonicalize().ok());
    if exe.is_some_and(|e| norm(&e) == norm(target)) {
        return Some("the planvil binary enforces the run");
    }
    if crate::install::data_dir().is_some_and(|d| within(target, &d.canonicalize().unwrap_or(d))) {
        return Some("planvil's installed plugin and hooks are protected");
    }
    None
}

/// Absolute, symlink-resolved form of `path` (relative to `cwd`). Components that do not
/// exist yet are appended as-is, so new files resolve correctly. `None` when `..` follows
/// a component that does not exist, because the physical result cannot be known.
pub fn resolve(cwd: &Path, path: &Path) -> Option<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut existing = joined.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            if tail.iter().any(|part| part == "..") {
                return None;
            }
            return Some(tail.iter().rev().fold(canonical, |acc, part| {
                if part == "." { acc } else { acc.join(part) }
            }));
        }
        let last = existing.components().next_back()?;
        tail.push(last.as_os_str().to_os_string());
        existing = existing.parent()?.to_path_buf();
    }
}

pub fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Setup => "Setup",
        Stage::Feature => "Feature",
        Stage::Audit => "Audit",
        Stage::Plan => "Plan",
        Stage::Decide => "Decide",
        Stage::Implement => "Implement",
        Stage::Validate => "Validate",
        Stage::Repair => "Repair",
        Stage::Done => "Done",
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
