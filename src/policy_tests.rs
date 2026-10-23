//! Tests for `policy`.

use super::*;
use std::fs;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    feature: PathBuf,
    config: Config,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let feature = root.join("planvil/sp20-mcp");
    fs::create_dir_all(feature.join(RUN_DIR)).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    Fixture {
        _dir: dir,
        root,
        feature,
        config: Config::default(),
    }
}

fn ctx<'a>(f: &'a Fixture, stage: Stage) -> Context<'a> {
    Context {
        stage,
        status: Status::Active,
        repo_root: &f.root,
        feature_dir: &f.feature,
        cwd: &f.root,
        config: &f.config,
    }
}

fn write(path: &str) -> Call {
    Call::Write {
        tool: "Edit".into(),
        path: PathBuf::from(path),
    }
}

fn bash(cmd: &str) -> Call {
    Call::Bash {
        command: cmd.into(),
    }
}

fn denied(d: Decision) -> bool {
    matches!(d, Decision::Deny(_))
}

#[test]
fn source_edits_are_blocked_before_implement() {
    let f = fixture();
    for stage in [Stage::Feature, Stage::Audit, Stage::Plan, Stage::Decide] {
        assert!(
            denied(decide(&ctx(&f, stage), &write("src/main.go"))),
            "{stage:?}"
        );
        assert!(
            denied(decide(&ctx(&f, stage), &write("/etc/hosts"))),
            "{stage:?}"
        );
    }
}

#[test]
fn workflow_docs_are_writable_before_implement() {
    let f = fixture();
    let c = ctx(&f, Stage::Audit);
    assert_eq!(
        decide(&c, &write("planvil/sp20-mcp/AUDIT.md")),
        Decision::Allow
    );
    assert_eq!(
        decide(&c, &write("planvil/sp20-mcp/notes/new.md")),
        Decision::Allow
    );
}

#[test]
fn traversal_out_of_the_feature_folder_is_blocked() {
    let f = fixture();
    let c = ctx(&f, Stage::Plan);
    assert!(denied(decide(
        &c,
        &write("planvil/sp20-mcp/../../src/main.go")
    )));
    assert!(denied(decide(&c, &write("planvil/other-branch/PLAN.md"))));
}

#[cfg(unix)]
#[test]
fn symlink_inside_feature_folder_cannot_escape() {
    let f = fixture();
    std::os::unix::fs::symlink(f.root.join("src"), f.feature.join("link")).unwrap();
    assert!(denied(decide(
        &ctx(&f, Stage::Plan),
        &write("planvil/sp20-mcp/link/main.go")
    )));
}

#[test]
fn run_state_and_policy_files_are_protected_in_every_active_stage() {
    let f = fixture();
    for stage in [Stage::Audit, Stage::Implement, Stage::Repair] {
        let c = ctx(&f, stage);
        assert!(
            denied(decide(&c, &write("planvil/sp20-mcp/.planvil/state.json"))),
            "{stage:?}"
        );
        assert!(denied(decide(&c, &write(".planvil.json"))), "{stage:?}");
        assert!(
            denied(decide(&c, &write(".claude/settings.json"))),
            "{stage:?}"
        );
        assert!(denied(decide(&c, &write(".git/config"))), "{stage:?}");
    }
}

#[test]
fn source_edits_are_allowed_from_implement() {
    let f = fixture();
    for stage in [Stage::Implement, Stage::Validate, Stage::Repair] {
        assert_eq!(
            decide(&ctx(&f, stage), &write("src/main.go")),
            Decision::Allow
        );
    }
}

#[test]
fn shell_writes_are_blocked_before_implement() {
    let f = fixture();
    let c = ctx(&f, Stage::Decide);
    assert!(denied(decide(&c, &bash("echo x > src/main.go"))));
    assert!(denied(decide(&c, &bash("python3 gen.py"))));
    assert_eq!(decide(&c, &bash("grep -rn Login src")), Decision::Allow);
}

#[test]
fn build_stage_allows_shell_but_guards_protected_files() {
    let f = fixture();
    let c = ctx(&f, Stage::Implement);
    assert_eq!(decide(&c, &bash("go build ./...")), Decision::Allow);
    assert!(denied(decide(
        &c,
        &bash("echo '{}' > planvil/sp20-mcp/.planvil/state.json")
    )));
    assert!(denied(decide(&c, &bash("rm .planvil.json"))));
    assert_eq!(
        decide(&c, &bash("cat planvil/sp20-mcp/.planvil/state.json")),
        Decision::Allow
    );
}

#[test]
fn user_actions_and_destructive_git_are_always_blocked() {
    let f = fixture();
    for stage in [Stage::Audit, Stage::Implement] {
        let c = ctx(&f, stage);
        assert!(denied(decide(&c, &bash("planvil resume"))), "{stage:?}");
        assert!(denied(decide(&c, &bash("git reset --hard"))), "{stage:?}");
        assert!(denied(decide(&c, &bash("git push"))), "{stage:?}");
    }
}

#[test]
fn unknown_tools_are_denied_before_implement_unless_allowlisted() {
    let mut f = fixture();
    let mcp = Call::Other {
        tool: "mcp__db__query".into(),
        isolated: false,
    };
    assert!(denied(decide(&ctx(&f, Stage::Audit), &mcp)));
    assert_eq!(
        decide(
            &ctx(&f, Stage::Audit),
            &Call::Other {
                tool: "Read".into(),
                isolated: false
            }
        ),
        Decision::Allow
    );
    assert_eq!(decide(&ctx(&f, Stage::Implement), &mcp), Decision::Allow);
    f.config.allowed_tools = vec!["mcp__db__query".into()];
    assert_eq!(decide(&ctx(&f, Stage::Audit), &mcp), Decision::Allow);
}

#[test]
fn nothing_is_enforced_once_the_run_is_over() {
    let f = fixture();
    let c = Context {
        status: Status::Cancelled,
        ..ctx(&f, Stage::Audit)
    };
    assert_eq!(decide(&c, &write("src/main.go")), Decision::Allow);
    assert_eq!(
        decide(&ctx(&f, Stage::Done), &write("src/main.go")),
        Decision::Allow
    );
}

#[test]
fn paused_or_blocked_runs_keep_their_stage_policy() {
    let f = fixture();
    for status in [Status::Paused, Status::Blocked, Status::WaitingForUser] {
        let c = Context {
            status,
            ..ctx(&f, Stage::Plan)
        };
        assert!(denied(decide(&c, &write("src/main.go"))), "{status:?}");
    }
}

#[test]
fn build_stage_blocks_writes_outside_the_repo_except_temp() {
    let f = fixture();
    let c = ctx(&f, Stage::Implement);
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/x".into());
    assert!(denied(decide(
        &c,
        &write(&format!("{home}/.claude/settings.json"))
    )));
    assert!(denied(decide(&c, &write("/etc/hosts"))));
    let tmp = std::env::temp_dir().join("planvil-policy-test.txt");
    assert_eq!(decide(&c, &write(tmp.to_str().unwrap())), Decision::Allow);
}

#[test]
fn hook_and_mcp_config_and_the_binary_are_protected() {
    let f = fixture();
    let c = ctx(&f, Stage::Implement);
    assert!(denied(decide(&c, &write(".claude/hooks/guard.sh"))));
    assert!(denied(decide(&c, &write(".mcp.json"))));
    let exe = std::env::current_exe().unwrap();
    assert!(denied(decide(&c, &write(exe.to_str().unwrap()))));
}

#[test]
fn dotdot_through_missing_folders_is_refused() {
    let f = fixture();
    let c = ctx(&f, Stage::Plan);
    assert!(denied(decide(
        &c,
        &write("planvil/sp20-mcp/missing/../../../src/a.go")
    )));
    assert_eq!(
        resolve(&f.root, Path::new("planvil/sp20-mcp/missing/../x")),
        None
    );
    assert!(resolve(&f.root, Path::new("planvil/sp20-mcp/new/x.md")).is_some());
}

#[test]
fn scheduling_tools_and_isolated_subagents_are_denied() {
    let f = fixture();
    for stage in [Stage::Audit, Stage::Implement] {
        let c = ctx(&f, stage);
        assert!(denied(decide(
            &c,
            &Call::Other {
                tool: "CronCreate".into(),
                isolated: false
            }
        )));
        assert!(denied(decide(
            &c,
            &Call::Other {
                tool: "Agent".into(),
                isolated: true
            }
        )));
        assert_eq!(
            decide(
                &c,
                &Call::Other {
                    tool: "Agent".into(),
                    isolated: false
                }
            ),
            Decision::Allow
        );
    }
}
