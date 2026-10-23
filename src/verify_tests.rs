//! Tests for `verify`.

use super::*;

fn step(name: &str, shell: &str, criteria: &[&str]) -> Step {
    Step {
        name: name.into(),
        run: None,
        shell: Some(shell.into()),
        cwd: None,
        timeout_secs: 10,
        criteria: criteria.iter().map(|s| s.to_string()).collect(),
        needs_app: None,
    }
}

fn plan(project: ProjectVerify, feature: FeatureVerify) -> Plan {
    Plan::new(&project, &feature).unwrap()
}

fn checks_only(steps: Vec<Step>) -> Plan {
    plan(
        ProjectVerify::default(),
        FeatureVerify {
            checks: steps,
            scenarios: vec![],
        },
    )
}

#[test]
fn step_needs_exactly_one_command_form() {
    let mut s = step("x", "test -d .", &[]);
    s.run = Some(vec!["true".into()]);
    assert!(s.validate().is_err());
    s.shell = None;
    assert!(s.validate().is_ok());
    s.run = Some(vec![]);
    assert!(s.validate().is_err());
}

#[test]
fn step_cwd_must_stay_in_repo() {
    let mut s = step("x", "test -d .", &[]);
    s.cwd = Some("../elsewhere".into());
    assert!(s.validate().is_err());
    s.cwd = Some("backend".into());
    assert!(s.validate().is_ok());
}

#[test]
fn duplicate_names_are_rejected() {
    let feature = FeatureVerify {
        checks: vec![step("a", "test -d .", &[]), step("a", "test -d .", &[])],
        scenarios: vec![],
    };
    assert!(Plan::new(&ProjectVerify::default(), &feature).is_err());
}

#[test]
fn scenarios_require_a_configured_app() {
    let feature = FeatureVerify {
        checks: vec![],
        scenarios: vec![step("s", "test -d .", &["AC-01"])],
    };
    let err = Plan::new(&ProjectVerify::default(), &feature).unwrap_err();
    assert!(err.contains("no `verify.app`"));
}

#[test]
fn coverage_flags_unmapped_and_unknown_criteria() {
    let p = checks_only(vec![step("a", "test -d .", &["AC-01", "AC-09"])]);
    let problems = p.coverage_problems(&["AC-01".into(), "AC-02".into()]);
    assert_eq!(
        problems,
        vec![
            "AC-02 has no check or scenario in verify.json",
            "verify.json references unknown criterion AC-09"
        ]
    );
}

#[test]
fn passing_steps_pass_and_write_evidence() {
    let repo = tempfile::tempdir().unwrap();
    let ev = repo.path().join("ev");
    let p = checks_only(vec![step("unit", "printf 'ok\\n' | cat", &["AC-01"])]);
    let out = run(&p, repo.path(), &ev).unwrap();
    assert_eq!(out.verdict, Verdict::Passed);
    assert_eq!(
        fs::read_to_string(ev.join(&out.results[0].stdout)).unwrap(),
        "ok\n"
    );
    assert!(ev.join("result.json").exists());
    assert!(out.results[0].ended_ms >= out.results[0].started_ms);
}

#[test]
fn failing_step_is_a_repairable_failure() {
    let repo = tempfile::tempdir().unwrap();
    let p = checks_only(vec![
        step("ok", "test -d .", &[]),
        step("bad", "sh -c 'exit 3'", &[]),
    ]);
    let out = run(&p, repo.path(), &repo.path().join("ev")).unwrap();
    assert_eq!(
        out.verdict,
        Verdict::Failed {
            failing: vec!["bad".into()]
        }
    );
    assert_eq!(out.results[1].exit_code, Some(3));
}

#[test]
fn missing_tool_makes_the_run_unavailable() {
    let repo = tempfile::tempdir().unwrap();
    let mut s = step("browser", "", &[]);
    s.shell = None;
    s.run = Some(vec!["planvil-no-such-binary-xyz".into()]);
    let out = run(&checks_only(vec![s]), repo.path(), &repo.path().join("ev")).unwrap();
    assert!(
        matches!(out.verdict, Verdict::Unavailable { .. }),
        "{:?}",
        out.verdict
    );
}

#[test]
fn failing_preflight_makes_the_run_unavailable_and_stops() {
    let repo = tempfile::tempdir().unwrap();
    let project = ProjectVerify {
        preflight: vec![step("db", "test 1 -eq 2", &[])],
        checks: vec![step("unit", "test -d .", &[])],
        ..Default::default()
    };
    let out = run(
        &plan(project, FeatureVerify::default()),
        repo.path(),
        &repo.path().join("ev"),
    )
    .unwrap();
    assert!(matches!(out.verdict, Verdict::Unavailable { .. }));
    assert_eq!(
        out.results.len(),
        1,
        "checks must not run after a failed preflight"
    );
}

#[test]
fn timeout_kills_the_step() {
    let repo = tempfile::tempdir().unwrap();
    let mut s = step("slow", "sleep 30", &[]);
    s.timeout_secs = 1;
    let start = Instant::now();
    let out = run(&checks_only(vec![s]), repo.path(), &repo.path().join("ev")).unwrap();
    assert!(out.results[0].timed_out);
    assert!(!out.results[0].passed);
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn secrets_are_redacted_from_logs() {
    let repo = tempfile::tempdir().unwrap();
    let project = ProjectVerify {
        env: BTreeMap::from([("API_TOKEN".to_string(), "s3cr3t-value".to_string())]),
        checks: vec![step("leak", "echo token=$API_TOKEN", &[])],
        ..Default::default()
    };
    let ev = repo.path().join("ev");
    let out = run(&plan(project, FeatureVerify::default()), repo.path(), &ev).unwrap();
    let log = fs::read_to_string(ev.join(&out.results[0].stdout)).unwrap();
    assert_eq!(log, "token=[REDACTED]\n");
}

#[test]
fn scenario_runs_against_a_ready_app_and_app_is_stopped() {
    let repo = tempfile::tempdir().unwrap();
    let marker = repo.path().join("ready");
    let project = ProjectVerify {
        app: Some(App {
            run: None,
            shell: Some(format!("touch {} && sleep 60", marker.display())),
            cwd: None,
            ready: Ready {
                http: None,
                run: Some(vec![
                    "test".into(),
                    "-f".into(),
                    marker.display().to_string(),
                ]),
            },
            startup_timeout_secs: 10,
        }),
        ..Default::default()
    };
    let feature = FeatureVerify {
        checks: vec![],
        scenarios: vec![step("journey", "test -d .", &["AC-01"])],
    };
    let out = run(
        &plan(project, feature),
        repo.path(),
        &repo.path().join("ev"),
    )
    .unwrap();
    assert_eq!(out.verdict, Verdict::Passed, "{:?}", out.app_notes);
    assert!(out.app_notes.iter().any(|n| n.starts_with("app ready")));
    assert!(
        out.app_notes
            .iter()
            .any(|n| n.contains("stopped") || n.contains("killed"))
    );
}

#[test]
fn app_that_never_becomes_ready_fails_scenarios() {
    let repo = tempfile::tempdir().unwrap();
    let project = ProjectVerify {
        app: Some(App {
            run: None,
            shell: Some("test 1 -eq 2".into()),
            cwd: None,
            ready: Ready {
                http: Some("http://127.0.0.1:9/".into()),
                run: None,
            },
            startup_timeout_secs: 2,
        }),
        ..Default::default()
    };
    let feature = FeatureVerify {
        checks: vec![],
        scenarios: vec![step("journey", "test -d .", &["AC-01"])],
    };
    let out = run(
        &plan(project, feature),
        repo.path(),
        &repo.path().join("ev"),
    )
    .unwrap();
    assert_eq!(
        out.verdict,
        Verdict::Failed {
            failing: vec!["app:not-ready".into()]
        }
    );
}

#[test]
fn http_probe_accepts_2xx() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = [0u8; 256];
        let _ = conn.read(&mut buf);
        conn.write_all(b"HTTP/1.0 200 OK\r\n\r\n").unwrap();
    });
    assert!(http_ok(&format!("http://127.0.0.1:{port}/health")));
    server.join().unwrap();
    assert!(!http_ok("http://127.0.0.1:9/"));
}

#[test]
fn a_server_already_answering_makes_the_run_unavailable() {
    let repo = tempfile::tempdir().unwrap();
    let project = ProjectVerify {
        app: Some(App {
            run: None,
            shell: Some("sleep 60".into()),
            cwd: None,
            ready: Ready {
                http: None,
                run: Some(vec!["true".into()]),
            },
            startup_timeout_secs: 5,
        }),
        ..Default::default()
    };
    let feature = FeatureVerify {
        checks: vec![],
        scenarios: vec![step("journey", "test -d .", &["AC-01"])],
    };
    let out = run(
        &plan(project, feature),
        repo.path(),
        &repo.path().join("ev"),
    )
    .unwrap();
    assert!(
        matches!(&out.verdict, Verdict::Unavailable { reason } if reason.contains("already succeeds")),
        "{:?}",
        out.verdict
    );
}

#[cfg(unix)]
#[test]
fn leftover_processes_from_an_interrupted_run_block_verification() {
    use std::os::unix::process::CommandExt;
    let repo = tempfile::tempdir().unwrap();
    let mut orphan = Command::new("sleep");
    orphan.arg("60").process_group(0);
    let mut child = orphan.spawn().unwrap();
    fs::write(
        repo.path().join("running.pids"),
        format!("{}\tapp: old server\n", child.id()),
    )
    .unwrap();
    let p = checks_only(vec![step("unit", "test -d .", &["AC-01"])]);
    let out = run(&p, repo.path(), &repo.path().join("ev")).unwrap();
    assert!(
        matches!(&out.verdict, Verdict::Unavailable { reason } if reason.contains("still running")),
        "{:?}",
        out.verdict
    );
    assert!(
        out.results.is_empty(),
        "nothing may run while leftovers exist"
    );
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn feature_steps_reject_no_ops_and_planvil_access() {
    for bad in ["true", "echo passed", "exit 0", ":"] {
        let f = FeatureVerify {
            checks: vec![step("x", bad, &["AC-01"])],
            scenarios: vec![],
        };
        assert!(Plan::new(&ProjectVerify::default(), &f).is_err(), "{bad}");
    }
    for bad in ["cat planvil/x/.planvil/state.json", "planvil resume"] {
        let f = FeatureVerify {
            checks: vec![step("x", bad, &["AC-01"])],
            scenarios: vec![],
        };
        assert!(Plan::new(&ProjectVerify::default(), &f).is_err(), "{bad}");
    }
    let ok = FeatureVerify {
        checks: vec![step("x", "echo hi | grep -q hi", &["AC-01"])],
        scenarios: vec![],
    };
    assert!(Plan::new(&ProjectVerify::default(), &ok).is_ok());
}

#[test]
fn scenarios_cannot_opt_out_of_the_app() {
    let project = ProjectVerify {
        app: Some(App {
            run: Some(vec!["sleep".into(), "1".into()]),
            shell: None,
            cwd: None,
            ready: Ready {
                http: None,
                run: Some(vec!["test".into(), "-d".into(), ".".into()]),
            },
            startup_timeout_secs: 5,
        }),
        ..Default::default()
    };
    let mut s = step("journey", "test -d .", &["AC-01"]);
    s.needs_app = Some(false);
    let f = FeatureVerify {
        checks: vec![],
        scenarios: vec![s],
    };
    assert!(Plan::new(&project, &f).unwrap_err().contains("needs_app"));
}

#[test]
fn step_hash_changes_with_the_command() {
    let a = step("x", "grep -q a f", &["AC-01"]);
    let b = Step {
        shell: Some("grep -q b f".into()),
        ..a.clone()
    };
    assert_eq!(step_hash(&a), step_hash(&a.clone()));
    assert_ne!(step_hash(&a), step_hash(&b));
}
