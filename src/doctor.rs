//! `planvil doctor`: installation checks, project checks, and the enforcement matrix.

use std::path::Path;
use std::process::Command;

use crate::install;
use crate::ops::Workspace;

/// What planvil enforces in Claude Code, and how. Shown so no gap is silent.
const CODEX_DIFFERENCES: [(&str, &str); 4] = [
    (
        "Codex file edits",
        "apply_patch: every added, updated, deleted or moved file is checked; an unreadable patch is denied",
    ),
    (
        "Codex hook trust",
        "hooks run only after the user trusts them, again after each plugin update; until then `planvil start` refuses (HOOKS_NOT_ACTIVE)",
    ),
    (
        "Codex hook failure",
        "fails open: Codex has no onFailure=block; planvil still denies on its own errors",
    ),
    (
        "Codex questions",
        "request_user_input exists only in some modes; otherwise questions are asked in chat",
    ),
];

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

/// Version and plugin checks for one host; the bool says whether the plugin is installed.
fn host_check(name: &str, env: &str, default_bin: &str, fix: &str) -> (Vec<Check>, bool) {
    let bin = std::env::var(env).unwrap_or_else(|_| default_bin.into());
    let Some(version) = version_of(&bin) else {
        return (
            vec![check(
                false,
                false,
                format!("{name} (`{bin}`) not found on PATH"),
            )],
            false,
        );
    };
    let installed = Command::new(&bin)
        .args(["plugin", "list", "--json"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(install::PLUGIN_ID));
    let plugin = if installed {
        check(
            true,
            false,
            format!("{name}: plugin {} installed", install::PLUGIN_ID),
        )
    } else {
        check(
            false,
            false,
            format!("{name}: plugin not installed (`{fix}`)"),
        )
    };
    (
        vec![check(true, false, format!("{name} {version}")), plugin],
        installed,
    )
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
    let hosts = [
        host_check(
            "Claude Code",
            "PLANVIL_CLAUDE_BIN",
            "claude",
            "planvil install",
        ),
        host_check(
            "Codex",
            "PLANVIL_CODEX_BIN",
            "codex",
            "planvil install --host codex",
        ),
    ];
    let usable = hosts.iter().any(|(_, installed)| *installed);
    for (lines, _) in hosts {
        checks.extend(lines);
    }
    checks.push(check(
        usable,
        true,
        if usable {
            "planvil is installed in at least one host".to_string()
        } else {
            "planvil is not installed in Claude Code or Codex".to_string()
        },
    ));

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
    out.push("Enforcement (Claude Code; Codex is the same except as noted below):".into());
    out.extend(
        CAPABILITIES
            .iter()
            .map(|(what, how)| format!("  - {what}: {how}")),
    );
    out.push("Codex differences:".into());
    out.extend(
        CODEX_DIFFERENCES
            .iter()
            .map(|(what, how)| format!("  - {what}: {how}")),
    );
    (out.join("\n"), ok)
}
