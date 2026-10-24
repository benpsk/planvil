//! `planvil` command line.

use std::io::Read;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use planvil::hooks::{self, HookEvent};
use planvil::ops::{self, Control, OpsError, StartArgs};
use planvil::state::QuestionKind;
use planvil::{doctor, install, report};

/// Exit code when the agent must ask the user something and rerun.
const EXIT_NEEDS_INPUT: u8 = 3;

#[derive(Parser)]
#[command(
    name = "planvil",
    version,
    about = "Enforced feature workflow for Claude Code"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a run on a feature branch, or resume the current branch's run.
    Start {
        /// Feature branch, e.g. sp20/mcp. Omit to resume the current branch's run.
        branch: Option<String>,
        /// Existing requirements file to seed FEATURE.md from.
        #[arg(long)]
        spec: Option<PathBuf>,
        /// Branch to cut the feature branch from (saved to .planvil.json if unset).
        #[arg(long)]
        base: Option<String>,
        /// Only discover (or re-discover) the project setup on the current branch.
        #[arg(long, conflicts_with_all = ["branch", "spec"])]
        setup: bool,
    },
    /// Setup stage: check the proposed project verification, or apply the approved one.
    Setup {
        #[command(subcommand)]
        action: SetupAction,
    },
    /// Rename the current run's branch and move its feature folder to match.
    Rename {
        /// The new branch name, e.g. refactor/tailwind-ui-components.
        new_branch: String,
    },
    /// Show stage, status, unmet gate items, and the next action.
    Status,
    /// Check the current stage's gate and move to the next stage.
    Advance,
    /// Record a question for the user; the run waits for the answer.
    Ask {
        text: String,
        /// The user confirms FEATURE.md.
        #[arg(long, conflicts_with = "approve_plan")]
        confirm_feature: bool,
        /// The user approves PLAN.md and verify.json.
        #[arg(long, conflicts_with = "approve_setup")]
        approve_plan: bool,
        /// The user approves the proposed project setup.
        #[arg(long)]
        approve_setup: bool,
    },
    /// Record the user's answer to the open question.
    Answer {
        text: String,
        #[arg(long, conflicts_with = "rejected")]
        approved: bool,
        #[arg(long)]
        rejected: bool,
    },
    /// Run verification and record evidence (Validate stage).
    Verify,
    /// Write and print the completion or blocked report.
    Report,
    /// Pause the run (user action).
    Pause,
    /// Resume a paused or blocked run (user action).
    Resume,
    /// Cancel the run (user action).
    Cancel,
    /// Write a sample .planvil.json in this repository.
    Init,
    /// Check the installation and this project's configuration.
    Doctor,
    /// Install the Claude Code plugin (skill and hooks) for this binary.
    Install {
        #[arg(long, default_value = "user")]
        scope: String,
    },
    /// Remove the Claude Code plugin installed by `planvil install`.
    Uninstall {
        #[arg(long, default_value = "user")]
        scope: String,
    },
    /// Hook entry point called by Claude Code.
    #[command(hide = true)]
    Hook { host: String, event: String },
}

#[derive(Subcommand)]
enum SetupAction {
    /// Run setup.proposed.json once and report whether it is usable.
    Check,
    /// Save the approved proposal to .planvil.json and continue to the Feature stage.
    Apply,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match cli.command {
        Cmd::Hook { host, event } => run_hook(&host, &event, &cwd),
        Cmd::Doctor => {
            let (text, ok) = doctor::doctor(&cwd);
            println!("{text}");
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Cmd::Install { scope } => print_plain(install::install(&scope)),
        Cmd::Uninstall { scope } => print_plain(install::uninstall(&scope)),
        other => print_ops(run_ops(other, &cwd)),
    }
}

fn run_ops(cmd: Cmd, cwd: &Path) -> ops::Result<String> {
    match cmd {
        Cmd::Start {
            branch,
            spec,
            base,
            setup,
        } => ops::start(
            cwd,
            &StartArgs {
                setup_only: setup,
                branch: branch.as_deref(),
                spec: spec.as_deref(),
                base: base.as_deref(),
            },
        ),
        Cmd::Setup {
            action: SetupAction::Check,
        } => ops::setup_check(cwd),
        Cmd::Setup {
            action: SetupAction::Apply,
        } => ops::apply_setup(cwd),
        Cmd::Rename { new_branch } => ops::rename(cwd, &new_branch),
        Cmd::Status => ops::status(cwd),
        Cmd::Advance => ops::advance(cwd),
        Cmd::Ask {
            text,
            confirm_feature,
            approve_plan,
            approve_setup,
        } => {
            let kind = match (confirm_feature, approve_plan, approve_setup) {
                (true, _, _) => QuestionKind::ConfirmFeature,
                (false, true, _) => QuestionKind::ApprovePlan,
                (false, false, true) => QuestionKind::ApproveSetup,
                (false, false, false) => QuestionKind::General,
            };
            ops::ask(cwd, &text, kind)
        }
        Cmd::Answer {
            text,
            approved,
            rejected,
        } => {
            let verdict = match (approved, rejected) {
                (true, _) => Some(true),
                (false, true) => Some(false),
                (false, false) => None,
            };
            ops::answer(cwd, &text, verdict)
        }
        Cmd::Verify => ops::verify(cwd),
        Cmd::Report => report::report(cwd),
        Cmd::Pause => user_control(cwd, Control::Pause, "pause"),
        Cmd::Resume => user_control(cwd, Control::Resume, "resume"),
        Cmd::Cancel => user_control(cwd, Control::Cancel, "cancel"),
        Cmd::Init => ops::init(cwd),
        Cmd::Hook { .. } | Cmd::Doctor | Cmd::Install { .. } | Cmd::Uninstall { .. } => {
            unreachable!("handled in main")
        }
    }
}

/// Set by Claude Code in the environment of its Bash tool.
const CLAUDE_SHELL_MARKER: &str = "CLAUDECODE";

/// User controls refuse to run from Claude Code's shell, where the agent could run them.
/// Inside Claude Code the user types `planvil <action>` as a message instead.
fn user_control(cwd: &Path, action: Control, name: &str) -> ops::Result<String> {
    if std::env::var_os(CLAUDE_SHELL_MARKER).is_some() {
        return Err(OpsError::Refused(format!(
            "`planvil {name}` is a user action and does not run from Claude Code's shell. Type \
             `planvil {name}` as a message in Claude Code, or run it in a terminal outside Claude Code."
        )));
    }
    ops::control(cwd, action)
}

fn print_ops(result: ops::Result<String>) -> ExitCode {
    match result {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e @ OpsError::NeedsInput { .. }) => {
            eprintln!("planvil: {e}");
            ExitCode::from(EXIT_NEEDS_INPUT)
        }
        Err(e) => {
            eprintln!("planvil: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_plain(result: Result<String, String>) -> ExitCode {
    match result {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("planvil: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Hooks always exit 0 and answer in JSON; a panic still yields an explicit answer.
fn run_hook(host: &str, event: &str, cwd: &Path) -> ExitCode {
    let Some(event) = HookEvent::parse(event).filter(|_| host == "claude") else {
        eprintln!("planvil: unknown hook `{host} {event}` (supported host: claude)");
        return ExitCode::FAILURE;
    };
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    panic::set_hook(Box::new(|_| {}));
    let output = panic::catch_unwind(AssertUnwindSafe(|| hooks::run(event, &stdin, cwd)))
        .unwrap_or_else(|_| hooks::failure_output(event, "internal panic"));
    if let Some(json) = output {
        println!("{json}");
    }
    ExitCode::SUCCESS
}
