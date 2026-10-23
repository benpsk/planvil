//! `planvil doctor`: installation checks, project checks, and the enforcement matrix.

use std::path::Path;
use std::process::Command;

use crate::install;
use crate::ops::Workspace;

/// What planvil enforces in Claude Code, and how. Shown so no gap is silent.
const CAPABILITIES: [(&str, &str); 12] = [
    (
        "Edit/Write/MultiEdit/NotebookEdit before Implement",
        "enforced: denied outside the feature folder (PreToolUse)",
    ),
    (
        "Writes outside the repository during a run",
        "denied, except the temp folder and Claude memory files",
    ),
    (
        "Shell commands before Implement",
        "best effort: one-line recognized read-only commands with per-command option rules; anything unparseable is denied",
    ),
    (
        "Unknown and MCP tools before Implement",
        "denied unless listed in allowed_tools",
    ),
    (
        "Scheduling tools, worktree-isolated subagents",
        "denied during a run",
    ),
    (
        "Branch switching or detaching HEAD mid-run",
        "denied; a live run on another branch still blocks mutations",
    ),
    (
        "Finishing before Done",
        "Stop hook continues the session; bounded by max_stop_continuations and Claude Code's cap of 8",
    ),
    (
        "pause / resume / cancel",
        "user only: applied when the user types them; refused from Claude Code's shell",
    ),
    (
        "Answers and confirmations",
        "accepted only after a real user prompt or AskUserQuestion answer; verbatim reply recorded",
    ),
    (
        "Hook crash, bad payload, git failure",
        "planvil denies mutation-capable calls; PreToolUse uses onFailure=block",
    ),
    (
        "Build-stage shell",
        "open by design: text checks only guard protected paths, git and planvil controls",
    ),
    (
        "Tamper resistance",
        "not a security boundary: code running as the same OS user can bypass hooks; state is unsigned",
    ),
];

struct Check {
    ok: bool,
    required: bool,
    label: String,
}

fn check(ok: bool, required: bool, label: impl Into<String>) -> Check {
    Check {
        ok,
        required,
        label: label.into(),
    }
}

fn version_of(bin: &str) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Returns the report and whether every required check passed.
pub fn doctor(cwd: &Path) -> (String, bool) {
    let mut checks = vec![check(
        true,
        true,
        format!("planvil {}", env!("CARGO_PKG_VERSION")),
    )];
    checks.push(match version_of("git") {
        Some(v) => check(true, true, v),
        None => check(false, true, "git not found on PATH"),
    });
    let claude_bin = std::env::var("PLANVIL_CLAUDE_BIN").unwrap_or_else(|_| "claude".into());
    let claude = version_of(&claude_bin);
    checks.push(match &claude {
        Some(v) => check(true, true, format!("Claude Code {v}")),
        None => check(false, true, "Claude Code (`claude`) not found on PATH"),
    });
    if claude.is_some() {
        let installed = Command::new(&claude_bin)
            .args(["plugin", "list", "--json"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(install::PLUGIN_ID));
        checks.push(check(
            installed,
            true,
            if installed {
                "plugin planvil@planvil installed".into()
            } else {
                "plugin not installed: run `planvil install`".to_string()
            },
        ));
    }

    match Workspace::current(cwd) {
        Ok(None) => checks.push(check(
            true,
            false,
            "not inside a git repository on a branch (project checks skipped)",
        )),
        Err(e) => checks.push(check(false, true, format!("project config: {e}"))),
        Ok(Some(ws)) => {
            let cfg = &ws.config;
            checks.push(check(
                true,
                true,
                format!(".planvil.json valid (docs_root `{}`)", cfg.docs_root),
            ));
            checks.push(check(
                cfg.base_branch.is_some(),
                false,
                match &cfg.base_branch {
                    Some(b) => format!("base_branch `{b}`"),
                    None => "base_branch not set; /planvil will ask once".into(),
                },
            ));
            let verify = cfg.verify.clone().unwrap_or_default();
            checks.push(check(
                !verify.checks.is_empty() || verify.app.is_some(),
                false,
                format!(
                    "verify: {} preflight, {} checks, app {}",
                    verify.preflight.len(),
                    verify.checks.len(),
                    if verify.app.is_some() {
                        "configured"
                    } else {
                        "not configured (no runtime scenarios possible)"
                    }
                ),
            ));
            checks.push(match ws.peek() {
                Ok(Some(s)) => check(
                    true,
                    false,
                    format!(
                        "run on `{}`: stage {:?}, status {:?}",
                        ws.branch, s.stage, s.status
                    ),
                ),
                Ok(None) => check(true, false, format!("no run on `{}`", ws.branch)),
                Err(e) => check(false, true, format!("run state for `{}`: {e}", ws.branch)),
            });
        }
    }

    let ok = checks.iter().all(|c| c.ok || !c.required);
    let mut out: Vec<String> = checks
        .iter()
        .map(|c| {
            format!(
                "{} {}",
                if c.ok {
                    "✓"
                } else if c.required {
                    "✗"
                } else {
                    "!"
                },
                c.label
            )
        })
        .collect();
    out.push(String::new());
    out.push("Enforcement in Claude Code:".into());
    out.extend(
        CAPABILITIES
            .iter()
            .map(|(what, how)| format!("  - {what}: {how}")),
    );
    (out.join("\n"), ok)
}
