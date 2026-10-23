//! Verification: configuration, the controlled runner, and evidence.
//!
//! Project-wide settings live in `.planvil.json` (`verify`); feature-specific checks and
//! runtime scenarios live in `<feature_dir>/verify.json`. Every step runs from an explicit
//! argument list unless the config deliberately chooses `shell`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::process::{self, Proc};
use crate::redact;
use crate::util::now_millis;

const DEFAULT_STEP_TIMEOUT_SECS: u64 = 600;
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 60;
const READY_POLL: Duration = Duration::from_millis(500);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const STATUS_LINE_MAX: usize = 256;

fn default_timeout() -> u64 {
    DEFAULT_STEP_TIMEOUT_SECS
}

fn default_startup() -> u64 {
    DEFAULT_STARTUP_TIMEOUT_SECS
}

/// One command to run. Exactly one of `run` (argument list) or `shell` is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    #[serde(default)]
    pub run: Option<Vec<String>>,
    /// Run through `sh -c`. A deliberate choice; prefer `run`.
    #[serde(default)]
    pub shell: Option<String>,
    /// Working directory relative to the repository root.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Acceptance criteria this step proves.
    #[serde(default)]
    pub criteria: Vec<String>,
    /// Whether the app must be running. Defaults: false for checks, true for scenarios.
    #[serde(default)]
    pub needs_app: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ready {
    /// Poll this `http://` URL until it answers 2xx/3xx.
    #[serde(default)]
    pub http: Option<String>,
    /// Or poll this command until it exits 0.
    #[serde(default)]
    pub run: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    #[serde(default)]
    pub run: Option<Vec<String>>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub ready: Ready,
    #[serde(default = "default_startup")]
    pub startup_timeout_secs: u64,
}

/// `.planvil.json` → `verify`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProjectVerify {
    /// Extra environment for every step (inherits the parent environment).
    pub env: BTreeMap<String, String>,
    /// Environment and dependency checks. A failure blocks the run (not repairable code).
    pub preflight: Vec<Step>,
    pub app: Option<App>,
    /// Checks required for every feature (lint, type, unit, ...).
    pub checks: Vec<Step>,
    /// Checks that already failed when setup was approved. They still run and are
    /// reported, but do not fail a feature. Keep them narrow so they hide nothing else.
    pub known_failures: Vec<String>,
}

/// `<feature_dir>/verify.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FeatureVerify {
    pub checks: Vec<Step>,
    /// Runtime acceptance scenarios against the running app.
    pub scenarios: Vec<Step>,
}

fn validate_command(
    what: &str,
    run: Option<&Vec<String>>,
    shell: Option<&String>,
) -> Result<(), String> {
    match (run, shell) {
        (Some(argv), None) if !argv.is_empty() && !argv[0].is_empty() => Ok(()),
        (Some(_), None) => Err(format!("{what}: `run` must be a non-empty argument list")),
        (None, Some(cmd)) if !cmd.trim().is_empty() => Ok(()),
        (None, Some(_)) => Err(format!("{what}: `shell` must not be empty")),
        (Some(_), Some(_)) => Err(format!("{what}: set `run` or `shell`, not both")),
        (None, None) => Err(format!("{what}: set `run` (argument list) or `shell`")),
    }
}

fn validate_cwd(what: &str, cwd: Option<&String>) -> Result<(), String> {
    match cwd {
        Some(c) if crate::layout::validate_docs_root(c).is_err() && c != "." => Err(format!(
            "{what}: cwd `{c}` must be a relative path inside the repository"
        )),
        _ => Ok(()),
    }
}

impl Step {
    fn validate(&self) -> Result<(), String> {
        let what = format!("step `{}`", self.name);
        if self.name.trim().is_empty() {
            return Err("every step needs a non-empty `name`".into());
        }
        validate_command(&what, self.run.as_ref(), self.shell.as_ref())?;
        validate_cwd(&what, self.cwd.as_ref())?;
        if self.timeout_secs == 0 {
            return Err(format!("{what}: timeout_secs must be at least 1"));
        }
        Ok(())
    }
}

/// Commands that cannot fail in a meaningful way.
const NO_OPS: [&str; 5] = ["true", ":", "echo", "printf", "exit"];

/// Agent-written steps may not be no-ops or reach into planvil's own state and binary.
fn validate_feature_step(step: &Step) -> Result<(), String> {
    let argv = argv_of(step.run.as_ref(), step.shell.as_ref());
    let text = argv.join(" ");
    if text.contains(".planvil")
        || text
            .split_whitespace()
            .any(|w| w.rsplit('/').next() == Some("planvil"))
    {
        return Err(format!(
            "step `{}` may not reference planvil's run state, config or binary",
            step.name
        ));
    }
    let script = step.shell.as_deref().unwrap_or("");
    let first = match &step.run {
        Some(argv) => argv
            .first()
            .map(|a| a.rsplit('/').next().unwrap_or(a).to_string()),
        None => script.split_whitespace().next().map(str::to_string),
    };
    let compound = ['|', ';', '&'].iter().any(|c| script.contains(*c));
    if first.as_deref().is_some_and(|f| NO_OPS.contains(&f)) && !compound {
        return Err(format!(
            "step `{}` is a no-op (`{text}`) and cannot prove anything",
            step.name
        ));
    }
    Ok(())
}

/// Stable hash of what a step runs and proves; a change means the check itself changed.
pub fn step_hash(step: &Step) -> String {
    let key = serde_json::json!([
        step.run,
        step.shell,
        step.cwd,
        step.criteria,
        step.needs_app,
        step.timeout_secs
    ]);
    crate::util::sha256_hex(key.to_string().as_bytes())
}

fn validate_unique(steps: &[&Step]) -> Result<(), String> {
    let mut names = BTreeSet::new();
    for s in steps {
        s.validate()?;
        if !names.insert(s.name.as_str()) {
            return Err(format!("duplicate step name `{}`", s.name));
        }
    }
    Ok(())
}

impl ProjectVerify {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(unknown) = self
            .known_failures
            .iter()
            .find(|n| !self.checks.iter().any(|c| &c.name == *n))
        {
            return Err(format!(
                "known_failures names `{unknown}`, which is not a project check"
            ));
        }
        validate_unique(
            &self
                .preflight
                .iter()
                .chain(&self.checks)
                .collect::<Vec<_>>(),
        )?;
        if let Some(app) = &self.app {
            validate_command("app", app.run.as_ref(), app.shell.as_ref())?;
            validate_cwd("app", app.cwd.as_ref())?;
            match (&app.ready.http, &app.ready.run) {
                (Some(url), None) if url.starts_with("http://") => {}
                (Some(_), None) => return Err("app.ready.http must be an http:// URL".into()),
                (None, Some(argv)) if argv.first().is_some_and(|p| !p.is_empty()) => {}
                _ => return Err("app.ready needs exactly one of `http` or `run`".into()),
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Preflight,
    Check,
    Scenario,
}

/// The full, validated set of steps for one verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Project checks whose failure is pre-existing and does not fail the run.
    pub known_failures: BTreeSet<String>,
    pub env: BTreeMap<String, String>,
    pub app: Option<App>,
    pub steps: Vec<(StepKind, Step)>,
}

impl Plan {
    pub fn new(project: &ProjectVerify, feature: &FeatureVerify) -> Result<Self, String> {
        project.validate()?;
        for step in &feature.checks {
            validate_feature_step(step)?;
        }
        for step in &feature.scenarios {
            validate_feature_step(step)?;
            if step.needs_app == Some(false) && project.app.is_some() {
                return Err(format!(
                    "scenario `{}` sets needs_app: false; scenarios run against the app (use a check instead)",
                    step.name
                ));
            }
        }
        let steps: Vec<(StepKind, Step)> = project
            .preflight
            .iter()
            .map(|s| (StepKind::Preflight, s.clone()))
            .chain(project.checks.iter().map(|s| (StepKind::Check, s.clone())))
            .chain(feature.checks.iter().map(|s| (StepKind::Check, s.clone())))
            .chain(
                feature
                    .scenarios
                    .iter()
                    .map(|s| (StepKind::Scenario, s.clone())),
            )
            .collect();
        validate_unique(&steps.iter().map(|(_, s)| s).collect::<Vec<_>>())?;
        let plan = Self {
            known_failures: project.known_failures.iter().cloned().collect(),
            env: project.env.clone(),
            app: project.app.clone(),
            steps,
        };
        if plan.steps.iter().any(|(k, s)| plan.needs_app(*k, s)) && plan.app.is_none() {
            return Err("a step needs the app, but `.planvil.json` has no `verify.app`".into());
        }
        Ok(plan)
    }

    fn needs_app(&self, kind: StepKind, step: &Step) -> bool {
        step.needs_app.unwrap_or(kind == StepKind::Scenario)
    }

    /// Problems that make the plan unable to prove the feature, before running anything.
    pub fn coverage_problems(&self, criteria: &[String]) -> Vec<String> {
        let covered: BTreeSet<&str> = self
            .steps
            .iter()
            .flat_map(|(_, s)| s.criteria.iter().map(String::as_str))
            .collect();
        let known: BTreeSet<&str> = criteria.iter().map(String::as_str).collect();
        let mut problems: Vec<String> = criteria
            .iter()
            .filter(|id| !covered.contains(id.as_str()))
            .map(|id| format!("{id} has no check or scenario in verify.json"))
            .collect();
        problems.extend(
            covered
                .iter()
                .filter(|id| !known.contains(*id))
                .map(|id| format!("verify.json references unknown criterion {id}")),
        );
        let has_scenario = self.steps.iter().any(|(k, _)| *k == StepKind::Scenario);
        if self.app.is_some() && !has_scenario {
            problems.push(
                "verify.json has no runtime scenario; the app is configured, so at least one \
                 scenario must exercise it"
                    .into(),
            );
        }
        problems
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepResult {
    pub name: String,
    pub kind: StepKind,
    pub argv: Vec<String>,
    pub cwd: String,
    pub started_ms: u64,
    pub ended_ms: u64,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// Set when the command could not be started at all.
    pub spawn_error: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub criteria: Vec<String>,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verdict {
    Passed,
    /// The code failed: repairable.
    Failed {
        failing: Vec<String>,
    },
    /// The environment is missing something: not repairable by code changes.
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub verdict: Verdict,
    pub results: Vec<StepResult>,
    /// Notes about the app process (start, readiness, cleanup).
    pub app_notes: Vec<String>,
}

/// Run `plan` from `repo_root`, writing logs and `result.json` into `evidence_dir`.
/// Live process groups are tracked in `running.pids` next to `evidence_dir`.
pub fn run(plan: &Plan, repo_root: &Path, evidence_dir: &Path) -> std::io::Result<Outcome> {
    fs::create_dir_all(evidence_dir)?;
    let registry = process::registry_for(evidence_dir.parent().unwrap_or(evidence_dir));
    let ctx = RunCtx {
        repo_root,
        evidence_dir,
        env: plan.env.clone(),
        secrets: redact::secrets(&plan.env),
        registry,
    };
    let (verdict, results, app_notes) = match leftover_problem(&ctx.registry)? {
        Some(reason) => (
            Some(Verdict::Unavailable { reason }),
            Vec::new(),
            Vec::new(),
        ),
        None => run_steps(plan, &ctx)?,
    };
    let verdict = verdict.unwrap_or_else(|| {
        let failing: Vec<String> = results
            .iter()
            .filter(|r| !r.passed && !plan.known_failures.contains(&r.name))
            .map(|r| r.name.clone())
            .collect();
        if failing.is_empty() {
            Verdict::Passed
        } else {
            Verdict::Failed { failing }
        }
    });
    let outcome = Outcome {
        verdict,
        results,
        app_notes,
    };
    let json = serde_json::to_vec_pretty(&outcome).map_err(std::io::Error::other)?;
    fs::write(evidence_dir.join("result.json"), json)?;
    Ok(outcome)
}

struct RunCtx<'a> {
    repo_root: &'a Path,
    evidence_dir: &'a Path,
    env: BTreeMap<String, String>,
    secrets: Vec<Vec<u8>>,
    registry: process::Registry,
}

type StepsOutcome = (Option<Verdict>, Vec<StepResult>, Vec<String>);

fn leftover_problem(registry: &process::Registry) -> std::io::Result<Option<String>> {
    let leftovers = registry.leftovers()?;
    Ok((!leftovers.is_empty()).then(|| {
        let list: Vec<String> = leftovers.iter().map(|(pid, cmd)| format!("group {pid} (`{cmd}`)")).collect();
        format!(
            "processes from an interrupted verification are still running: {}. Stop them (for example \
             `kill -- -<group>`) and rerun",
            list.join(", ")
        )
    }))
}

fn run_steps(plan: &Plan, ctx: &RunCtx) -> std::io::Result<StepsOutcome> {
    let mut results = Vec::new();
    let mut app_notes = Vec::new();
    // Steps that do not need the app run first, so a broken build fails fast.
    let (without_app, with_app): (Vec<_>, Vec<_>) = plan
        .steps
        .iter()
        .enumerate()
        .partition(|(_, (k, s))| !plan.needs_app(*k, s));

    for (i, (kind, step)) in &without_app {
        let r = run_step(*i, *kind, step, ctx)?;
        let stop = r.spawn_error.is_some() || (*kind == StepKind::Preflight && !r.passed);
        let verdict = stop.then(|| unavailable(&r, ctx.evidence_dir));
        results.push(r);
        if verdict.is_some() {
            return Ok((verdict, results, app_notes));
        }
    }
    if with_app.is_empty() {
        return Ok((None, results, app_notes));
    }
    let Some(app) = plan.app.as_ref() else {
        let reason = "a step needs the app, but no app is configured".to_string();
        return Ok((Some(Verdict::Unavailable { reason }), results, app_notes));
    };
    let mut app_proc = match start_app(app, ctx)? {
        AppStart::Unavailable(reason) => {
            return Ok((Some(Verdict::Unavailable { reason }), results, app_notes));
        }
        AppStart::NotReady(note) => {
            app_notes.push(note);
            let verdict = Verdict::Failed {
                failing: vec!["app:not-ready".into()],
            };
            return Ok((Some(verdict), results, app_notes));
        }
        AppStart::Ready(proc, note) => {
            app_notes.push(note);
            proc
        }
    };
    let mut verdict = None;
    for (i, (kind, step)) in &with_app {
        // `?` here drops `app_proc`, which stops the app group.
        let r = run_step(*i, *kind, step, ctx)?;
        let missing_tool = r.spawn_error.is_some();
        if missing_tool {
            verdict = Some(unavailable(&r, ctx.evidence_dir));
        }
        results.push(r);
        if missing_tool {
            break;
        }
    }
    app_notes.push(app_proc.stop());
    for log in ["app.stdout.log", "app.stderr.log"] {
        redact::redact_file(&ctx.evidence_dir.join(log), &ctx.secrets)?;
    }
    Ok((verdict, results, app_notes))
}

fn unavailable(r: &StepResult, evidence_dir: &Path) -> Verdict {
    let reason = match &r.spawn_error {
        Some(e) => format!("`{}` could not start ({}): {e}", r.name, r.argv.join(" ")),
        None => format!(
            "preflight `{}` failed (exit {:?}); see {}",
            r.name,
            r.exit_code,
            evidence_dir.join(&r.stderr).display()
        ),
    };
    Verdict::Unavailable { reason }
}

fn argv_of(run: Option<&Vec<String>>, shell: Option<&String>) -> Vec<String> {
    match (run, shell) {
        (Some(argv), _) => argv.clone(),
        (None, Some(cmd)) => vec!["sh".into(), "-c".into(), cmd.clone()],
        (None, None) => Vec::new(),
    }
}

fn command(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> std::io::Result<Command> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .envs(env)
        .stdin(Stdio::null());
    Ok(cmd)
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn run_step(
    index: usize,
    kind: StepKind,
    step: &Step,
    ctx: &RunCtx,
) -> std::io::Result<StepResult> {
    let argv = argv_of(step.run.as_ref(), step.shell.as_ref());
    let cwd = ctx.repo_root.join(step.cwd.as_deref().unwrap_or("."));
    let base = format!("{index:02}-{}", safe_name(&step.name));
    let stdout_path = ctx.evidence_dir.join(format!("{base}.stdout.log"));
    let stderr_path = ctx.evidence_dir.join(format!("{base}.stderr.log"));
    let mut env = ctx.env.clone();
    env.insert(
        "PLANVIL_EVIDENCE_DIR".into(),
        ctx.evidence_dir.display().to_string(),
    );

    let started_ms = now_millis();
    let mut cmd = command(&argv, &cwd, &env)?;
    cmd.stdout(File::create(&stdout_path)?)
        .stderr(File::create(&stderr_path)?);
    let (exit_code, timed_out, spawn_error) =
        match Proc::spawn(&mut cmd, &argv.join(" "), &ctx.registry) {
            Err(e) => (None, false, Some(e.to_string())),
            Ok(mut proc) => {
                let (code, timed_out) = proc.wait_timeout(Duration::from_secs(step.timeout_secs));
                (code, timed_out, None)
            }
        };
    let ended_ms = now_millis();
    redact::redact_file(&stdout_path, &ctx.secrets)?;
    redact::redact_file(&stderr_path, &ctx.secrets)?;
    Ok(StepResult {
        name: step.name.clone(),
        kind,
        argv,
        cwd: step.cwd.clone().unwrap_or_else(|| ".".into()),
        started_ms,
        ended_ms,
        exit_code,
        timed_out,
        passed: exit_code == Some(0) && !timed_out,
        spawn_error,
        stdout: file_name(&stdout_path),
        stderr: file_name(&stderr_path),
        criteria: step.criteria.clone(),
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

enum AppStart {
    Ready(Proc, String),
    NotReady(String),
    Unavailable(String),
}

fn start_app(app: &App, ctx: &RunCtx) -> std::io::Result<AppStart> {
    // Something already answering would make readiness meaningless.
    if probe(&app.ready, ctx) {
        return Ok(AppStart::Unavailable(
            "the readiness check already succeeds before the app starts; another process is serving \
             there. Stop it and rerun"
                .into(),
        ));
    }
    let argv = argv_of(app.run.as_ref(), app.shell.as_ref());
    let cwd = ctx.repo_root.join(app.cwd.as_deref().unwrap_or("."));
    let mut cmd = command(&argv, &cwd, &ctx.env)?;
    cmd.stdout(File::create(ctx.evidence_dir.join("app.stdout.log"))?)
        .stderr(File::create(ctx.evidence_dir.join("app.stderr.log"))?);
    let mut proc = match Proc::spawn(&mut cmd, &format!("app: {}", argv.join(" ")), &ctx.registry) {
        Ok(proc) => proc,
        Err(e) => {
            return Ok(AppStart::Unavailable(format!(
                "app `{}` could not start: {e}",
                argv.join(" ")
            )));
        }
    };
    let started = Instant::now();
    let deadline = started + Duration::from_secs(app.startup_timeout_secs);
    while Instant::now() < deadline {
        if let Some(status) = proc.exited() {
            proc.stop();
            return Ok(AppStart::NotReady(format!(
                "app exited before becoming ready ({status}); see app.stderr.log"
            )));
        }
        if probe(&app.ready, ctx) {
            let note = format!("app ready after {} ms", started.elapsed().as_millis());
            return Ok(AppStart::Ready(proc, note));
        }
        std::thread::sleep(READY_POLL);
    }
    let cleanup = proc.stop();
    Ok(AppStart::NotReady(format!(
        "app not ready within {}s; {cleanup}; see app.stderr.log",
        app.startup_timeout_secs
    )))
}

fn probe(ready: &Ready, ctx: &RunCtx) -> bool {
    match (&ready.http, &ready.run) {
        (Some(url), _) => http_ok(url),
        (None, Some(argv)) => {
            let Ok(mut cmd) = command(argv, ctx.repo_root, &ctx.env) else {
                return false;
            };
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
            match Proc::spawn(
                &mut cmd,
                &format!("ready: {}", argv.join(" ")),
                &ctx.registry,
            ) {
                Ok(mut proc) => proc.wait_timeout(PROBE_TIMEOUT) == (Some(0), false),
                Err(_) => false,
            }
        }
        (None, None) => false,
    }
}

/// Minimal HTTP/1.0 GET; true on a 2xx or 3xx status.
fn http_ok(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, "/".to_string()), |(a, p)| (a, format!("/{p}")));
    let host_port = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };
    let Some(addr) = host_port.to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let request = format!("GET {path} HTTP/1.0\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    // Read until the status line is complete; a single read may return part of it.
    let mut head = Vec::new();
    let mut buf = [0u8; 64];
    while head.len() < STATUS_LINE_MAX && !head.contains(&b'\n') {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let line = String::from_utf8_lossy(&head);
    let code = line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok());
    code.is_some_and(|c| (200..400).contains(&c))
}

#[path = "verify_smoke.rs"]
mod smoke;
pub use smoke::{SmokeOutcome, smoke};

/// Path of evidence run `n` inside a feature's run directory.
pub fn evidence_dir(run_dir: &Path, n: u32) -> PathBuf {
    run_dir.join("evidence").join(format!("{n:03}"))
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
