//! Tests for `machine`.

use super::*;
use crate::state::Limits;

const LIMITS: Limits = Limits {
    max_repair_cycles: 5,
    max_equivalent_failures: 3,
    max_stop_continuations: 3,
    max_run_minutes: None,
};

fn run() -> RunState {
    RunState::new("r1", "sp20-mcp", "sp20/mcp", "production", LIMITS, 0)
}

fn at(stage: Stage) -> RunState {
    RunState { stage, ..run() }
}

fn with_status(stage: Stage, status: Status) -> RunState {
    RunState {
        stage,
        status,
        ..run()
    }
}

fn fail(sig: &str) -> Event {
    Event::ValidationFailed(Failure {
        signature: sig.into(),
        summary: format!("{sig} failed"),
    })
}

fn step(s: &RunState, e: Event) -> RunState {
    apply(s, &e, 1).unwrap()
}

/// After a failure, advance back through every stage to Validate.
fn revalidate(s: &RunState) -> RunState {
    let mut s = s.clone();
    while s.stage != Stage::Validate {
        s = step(&s, Event::Advance);
    }
    s
}

#[test]
fn advances_through_stages_in_order() {
    let order = [
        Stage::Feature,
        Stage::Audit,
        Stage::Plan,
        Stage::Decide,
        Stage::Implement,
        Stage::Validate,
    ];
    let mut s = run();
    for pair in order.windows(2) {
        assert_eq!(s.stage, pair[0]);
        s = step(&s, Event::Advance);
        assert_eq!(s.stage, pair[1]);
    }
    assert_eq!(s.history.len(), order.len() - 1);
}

#[test]
fn apply_does_not_mutate_input() {
    let s = run();
    let before = s.clone();
    let _ = apply(&s, &Event::Advance, 5).unwrap();
    assert_eq!(s, before);
}

#[test]
fn records_transition_and_timestamp() {
    let s = apply(&run(), &Event::Advance, 42).unwrap();
    assert_eq!(s.updated_at, 42);
    let t = s.history.last().unwrap();
    assert_eq!(t.from, (Stage::Feature, Status::Active));
    assert_eq!(t.to, (Stage::Audit, Status::Active));
    assert_eq!(t.at, 42);
}

#[test]
fn validate_cannot_advance_without_a_result() {
    let err = apply(&at(Stage::Validate), &Event::Advance, 1).unwrap_err();
    assert!(matches!(err, TransitionError::NotAllowed { .. }));
}

#[test]
fn validation_pass_completes_the_run() {
    let s = step(&at(Stage::Validate), Event::ValidationPassed);
    assert_eq!((s.stage, s.status), (Stage::Done, Status::Completed));
}

#[test]
fn validation_result_only_accepted_in_validate() {
    for stage in [Stage::Audit, Stage::Implement, Stage::Repair] {
        assert!(apply(&at(stage), &Event::ValidationPassed, 1).is_err());
        assert!(apply(&at(stage), &fail("x"), 1).is_err());
    }
}

#[test]
fn validation_failure_loops_back_to_feature_and_counts() {
    let s = step(&at(Stage::Validate), fail("AC-01"));
    assert_eq!((s.stage, s.status), (Stage::Feature, Status::Active));
    assert_eq!(s.attempts.repair_cycles, 1);
    assert_eq!(s.attempts.equivalent_failures, 1);
    assert_eq!(s.last_failure.as_ref().unwrap().signature, "AC-01");
}

#[test]
fn a_failure_loop_passes_through_every_thinking_stage() {
    let mut s = step(&at(Stage::Validate), fail("AC-01"));
    let mut seen = vec![s.stage];
    while s.stage != Stage::Validate {
        s = step(&s, Event::Advance);
        seen.push(s.stage);
    }
    assert_eq!(
        seen,
        vec![
            Stage::Feature,
            Stage::Audit,
            Stage::Plan,
            Stage::Decide,
            Stage::Implement,
            Stage::Validate
        ]
    );
}

#[test]
fn legacy_repair_stage_still_advances_to_validate() {
    let s = step(&at(Stage::Repair), Event::Advance);
    assert_eq!(s.stage, Stage::Validate);
}

#[test]
fn different_failures_reset_equivalent_count() {
    let mut s = at(Stage::Validate);
    s = step(&s, fail("A"));
    s = step(&revalidate(&s), fail("A"));
    assert_eq!(s.attempts.equivalent_failures, 2);
    s = step(&revalidate(&s), fail("B"));
    assert_eq!(s.attempts.equivalent_failures, 1);
    assert_eq!(s.status, Status::Active);
}

#[test]
fn repeated_identical_failures_block() {
    let mut s = at(Stage::Validate);
    for _ in 0..2 {
        s = revalidate(&step(&s, fail("same")));
    }
    s = step(&s, fail("same"));
    assert_eq!(s.status, Status::Blocked);
    assert_eq!(s.attempts.equivalent_failures, 3);
    assert!(s.status_reason.unwrap().contains("3"));
}

#[test]
fn exceeding_repair_cycles_blocks() {
    let mut s = at(Stage::Validate);
    for i in 0..5 {
        s = step(&s, fail(&format!("f{i}")));
        assert_eq!(s.status, Status::Active, "cycle {i} should still be active");
        s = revalidate(&s);
    }
    s = step(&s, fail("f5"));
    assert_eq!(s.status, Status::Blocked);
    assert_eq!(s.attempts.repair_cycles, 6);
}

#[test]
fn ask_user_waits_and_answer_resumes() {
    let s = step(
        &at(Stage::Decide),
        Event::AskUser {
            question: "Q1?".into(),
        },
    );
    assert_eq!((s.stage, s.status), (Stage::Decide, Status::WaitingForUser));
    assert_eq!(s.status_reason.as_deref(), Some("Q1?"));
    let s = step(&s, Event::Answered);
    assert_eq!(s.status, Status::Active);
    assert_eq!(s.status_reason, None);
}

#[test]
fn cannot_advance_while_waiting_paused_or_blocked() {
    for status in [Status::WaitingForUser, Status::Paused, Status::Blocked] {
        let err = apply(&with_status(Stage::Plan, status), &Event::Advance, 1);
        assert!(
            matches!(err, Err(TransitionError::NotAllowed { .. })),
            "{status:?}"
        );
    }
}

#[test]
fn pause_and_resume() {
    let s = step(&at(Stage::Implement), Event::Pause);
    assert_eq!(s.status, Status::Paused);
    let s = step(&s, Event::Resume);
    assert_eq!((s.stage, s.status), (Stage::Implement, Status::Active));
}

#[test]
fn resume_from_blocked_resets_counters() {
    let mut s = with_status(Stage::Repair, Status::Blocked);
    s.attempts.repair_cycles = 6;
    s.attempts.equivalent_failures = 3;
    let s = step(&s, Event::Resume);
    assert_eq!(s.status, Status::Active);
    assert_eq!(s.attempts.repair_cycles, 0);
    assert_eq!(s.attempts.equivalent_failures, 0);
}

#[test]
fn resume_does_not_answer_a_question() {
    let s = with_status(Stage::Decide, Status::WaitingForUser);
    assert!(apply(&s, &Event::Resume, 1).is_err());
}

#[test]
fn cancel_from_any_non_terminal_state() {
    for status in [
        Status::Active,
        Status::WaitingForUser,
        Status::Paused,
        Status::Blocked,
    ] {
        let s = step(&with_status(Stage::Implement, status), Event::Cancel);
        assert_eq!(s.status, Status::Cancelled);
    }
}

#[test]
fn terminal_runs_reject_everything() {
    for status in [Status::Cancelled, Status::Completed] {
        for e in [Event::Advance, Event::Cancel, Event::Resume, Event::Pause] {
            let err = apply(&with_status(Stage::Done, status), &e, 1).unwrap_err();
            assert_eq!(err, TransitionError::Terminal(status));
        }
    }
}

#[test]
fn block_records_reason() {
    let s = step(
        &at(Stage::Validate),
        Event::Block {
            reason: "no browser".into(),
        },
    );
    assert_eq!(s.status, Status::Blocked);
    assert_eq!(s.status_reason.as_deref(), Some("no browser"));
}

#[test]
fn rewind_goes_back_to_an_earlier_stage() {
    let s = at(Stage::Validate);
    let s = step(
        &s,
        Event::Rewind {
            to: Stage::Audit,
            reason: "requirements changed".into(),
        },
    );
    assert_eq!((s.stage, s.status), (Stage::Audit, Status::Active));
    assert_eq!(s.history.last().unwrap().reason, "requirements changed");
}

#[test]
fn rewind_must_go_backwards() {
    for to in [Stage::Plan, Stage::Implement] {
        let err = apply(
            &at(Stage::Plan),
            &Event::Rewind {
                to,
                reason: "x".into(),
            },
            1,
        );
        assert!(matches!(err, Err(TransitionError::NotEarlier { .. })));
    }
}

#[test]
fn rewind_is_rejected_unless_active() {
    // H1: a rewind must not clear a block or skip a pending answer.
    let reasons = Some("why".to_string());
    for status in [Status::Blocked, Status::WaitingForUser, Status::Paused] {
        let s = RunState {
            status_reason: reasons.clone(),
            ..with_status(Stage::Validate, status)
        };
        let e = Event::Rewind {
            to: Stage::Plan,
            reason: "req changed".into(),
        };
        assert!(
            matches!(apply(&s, &e, 1), Err(TransitionError::NotAllowed { .. })),
            "{status:?}"
        );
    }
}

#[test]
fn block_is_rejected_unless_active() {
    // H2: blocking a waiting run would overwrite its question, then Resume would skip it.
    let s = step(
        &at(Stage::Decide),
        Event::AskUser {
            question: "Q1?".into(),
        },
    );
    let e = Event::Block {
        reason: "no browser".into(),
    };
    assert!(matches!(
        apply(&s, &e, 1),
        Err(TransitionError::NotAllowed { .. })
    ));
    let blocked = step(
        &at(Stage::Validate),
        Event::Block {
            reason: "first".into(),
        },
    );
    assert!(
        apply(
            &blocked,
            &Event::Block {
                reason: "second".into()
            },
            1
        )
        .is_err()
    );
}

#[test]
fn rewind_resets_repair_counters_and_last_failure() {
    // M1: failures from before a requirements change must not count against new work.
    let s = revalidate(&step(&at(Stage::Validate), fail("A")));
    assert_eq!(s.attempts.repair_cycles, 1);
    let s = step(
        &s,
        Event::Rewind {
            to: Stage::Plan,
            reason: "req changed".into(),
        },
    );
    assert_eq!(s.attempts, Attempts::default());
    assert_eq!(s.last_failure, None);
}

#[test]
fn uses_limits_stored_in_the_run() {
    // M8: limits come from the run snapshot, not from whatever config says now.
    let strict = Limits {
        max_repair_cycles: 1,
        max_equivalent_failures: 9,
        ..LIMITS
    };
    let s = RunState {
        limits: strict,
        ..at(Stage::Validate)
    };
    let s = revalidate(&step(&s, fail("a")));
    assert_eq!(s.status, Status::Active);
    let s = step(&s, fail("b"));
    assert_eq!(s.status, Status::Blocked);
}

#[test]
fn counters_saturate_instead_of_overflowing() {
    let mut s = at(Stage::Validate);
    s.limits = Limits {
        max_repair_cycles: u32::MAX,
        max_equivalent_failures: u32::MAX,
        ..LIMITS
    };
    s.attempts = Attempts {
        repair_cycles: u32::MAX,
        equivalent_failures: u32::MAX,
    };
    s.last_failure = Some(Failure {
        signature: "x".into(),
        summary: "x".into(),
    });
    let s = step(&s, fail("x"));
    assert_eq!(s.attempts.repair_cycles, u32::MAX);
}

#[test]
fn rewind_invalidates_evidence_and_approvals_it_goes_back_past() {
    let base = RunState {
        feature_confirmed_hash: Some("f".into()),
        plan_approved_hash: Some("p".into()),
        last_verify: Some(crate::state::VerifyRecord {
            run: 1,
            at: 0,
            fingerprint: "x".into(),
            passed: false,
            evidence_dir: "e".into(),
            failing: vec![],
        }),
        ..at(Stage::Repair)
    };
    let to_plan = step(
        &base,
        Event::Rewind {
            to: Stage::Plan,
            reason: "r".into(),
        },
    );
    assert_eq!(to_plan.last_verify, None);
    assert_eq!(to_plan.plan_approved_hash, None);
    assert_eq!(to_plan.feature_confirmed_hash.as_deref(), Some("f"));
    let to_impl = step(
        &base,
        Event::Rewind {
            to: Stage::Implement,
            reason: "r".into(),
        },
    );
    assert_eq!(to_impl.plan_approved_hash.as_deref(), Some("p"));
    let to_feature = step(
        &base,
        Event::Rewind {
            to: Stage::Feature,
            reason: "r".into(),
        },
    );
    assert_eq!(to_feature.feature_confirmed_hash, None);
}

#[test]
fn setup_applied_continues_a_feature_run_or_completes_a_setup_only_run() {
    let s = step(&at(Stage::Setup), Event::SetupApplied);
    assert_eq!((s.stage, s.status), (Stage::Feature, Status::Active));
    let only = RunState {
        setup_only: true,
        ..at(Stage::Setup)
    };
    let s = step(&only, Event::SetupApplied);
    assert_eq!((s.stage, s.status), (Stage::Done, Status::Completed));
    assert!(apply(&at(Stage::Feature), &Event::SetupApplied, 1).is_err());
}
