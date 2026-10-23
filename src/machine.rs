//! The only place run state changes. `apply` returns a new state or rejects the event.
//!
//! Gate checks (do the docs have their sections? does evidence exist?) happen before an
//! event is built. This module enforces which moves are legal and the repair limits.

use thiserror::Error;

use crate::state::{Attempts, Failure, RunState, Stage, Status, Transition};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The current stage's gate passed; move to the next stage.
    Advance,
    /// Validation passed with current evidence.
    ValidationPassed,
    /// Validation failed; loop back to Feature (or Blocked if a limit is hit).
    ValidationFailed(Failure),
    /// A material question needs the user.
    AskUser {
        question: String,
    },
    /// The user answered the open question(s).
    Answered,
    Pause,
    Resume,
    Cancel,
    /// Something required is missing (driver, service, credentials).
    Block {
        reason: String,
    },
    /// The approved project setup was saved. A setup-only run completes; a feature run
    /// continues to the Feature stage.
    SetupApplied,
    /// Requirements or other inputs changed; go back to an earlier stage.
    Rewind {
        to: Stage,
        reason: String,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error("run is {0:?}; no further changes are allowed")]
    Terminal(Status),
    #[error("cannot {event} while the run is {status:?} at stage {stage:?}")]
    NotAllowed {
        event: &'static str,
        stage: Stage,
        status: Status,
    },
    #[error("can only rewind to an earlier stage (at {from:?}, asked for {to:?})")]
    NotEarlier { from: Stage, to: Stage },
}

/// Apply `event` to `state` at time `now`. Never mutates `state`.
///
/// Pause, Resume, Cancel and Answered are user actions. This function cannot tell who
/// asked; the hook layer must stop the agent from issuing them itself.
pub fn apply(state: &RunState, event: &Event, now: u64) -> Result<RunState, TransitionError> {
    if state.status.is_terminal() {
        return Err(TransitionError::Terminal(state.status));
    }
    let not_allowed = |event| TransitionError::NotAllowed {
        event,
        stage: state.stage,
        status: state.status,
    };
    let active = state.status == Status::Active;

    let next = match event {
        Event::Advance => {
            let to = next_stage(state.stage)
                .filter(|_| active)
                .ok_or(not_allowed("advance"))?;
            moved(state, to, Status::Active, "gate passed", now)
        }
        Event::ValidationPassed => {
            if !(active && state.stage == Stage::Validate) {
                return Err(not_allowed("record a validation pass"));
            }
            moved(
                state,
                Stage::Done,
                Status::Completed,
                "validation passed",
                now,
            )
        }
        Event::ValidationFailed(failure) => {
            if !(active && state.stage == Stage::Validate) {
                return Err(not_allowed("record a validation failure"));
            }
            record_failure(state, failure, now)
        }
        Event::AskUser { question } => {
            if !active {
                return Err(not_allowed("ask the user"));
            }
            RunState {
                status_reason: Some(question.clone()),
                ..moved(
                    state,
                    state.stage,
                    Status::WaitingForUser,
                    "waiting for user",
                    now,
                )
            }
        }
        Event::Answered => {
            if state.status != Status::WaitingForUser {
                return Err(not_allowed("record an answer"));
            }
            moved(state, state.stage, Status::Active, "user answered", now)
        }
        Event::Pause => {
            if !active {
                return Err(not_allowed("pause"));
            }
            moved(state, state.stage, Status::Paused, "paused by user", now)
        }
        Event::Resume => match state.status {
            Status::Paused => moved(state, state.stage, Status::Active, "resumed by user", now),
            // An explicit user resume grants a fresh repair budget.
            Status::Blocked => RunState {
                attempts: Default::default(),
                ..moved(
                    state,
                    state.stage,
                    Status::Active,
                    "resumed from blocked by user",
                    now,
                )
            },
            Status::Active | Status::WaitingForUser | Status::Cancelled | Status::Completed => {
                return Err(not_allowed("resume"));
            }
        },
        Event::Cancel => moved(
            state,
            state.stage,
            Status::Cancelled,
            "cancelled by user",
            now,
        ),
        Event::Block { reason } => {
            // Only from Active: blocking a waiting run would hide its open question.
            if !active {
                return Err(not_allowed("block"));
            }
            RunState {
                status_reason: Some(reason.clone()),
                ..moved(state, state.stage, Status::Blocked, reason, now)
            }
        }
        Event::SetupApplied => {
            if !(active && state.stage == Stage::Setup) {
                return Err(not_allowed("apply the setup"));
            }
            if state.setup_only {
                moved(
                    state,
                    Stage::Done,
                    Status::Completed,
                    "project setup applied",
                    now,
                )
            } else {
                moved(
                    state,
                    Stage::Feature,
                    Status::Active,
                    "project setup applied",
                    now,
                )
            }
        }
        Event::Rewind { to, reason } => {
            // Only from Active: a rewind must not clear a block or skip an answer.
            if !active {
                return Err(not_allowed("rewind"));
            }
            if *to >= state.stage {
                return Err(TransitionError::NotEarlier {
                    from: state.stage,
                    to: *to,
                });
            }
            // Earlier failures, evidence and approvals belong to work that is being redone.
            RunState {
                attempts: Attempts::default(),
                last_failure: None,
                last_verify: None,
                plan_approved_hash: state
                    .plan_approved_hash
                    .clone()
                    .filter(|_| *to > Stage::Decide),
                feature_confirmed_hash: state
                    .feature_confirmed_hash
                    .clone()
                    .filter(|_| *to > Stage::Feature),
                ..moved(state, *to, Status::Active, reason, now)
            }
        }
    };
    Ok(next)
}

/// The stage `Advance` leads to. Validate and Done have no plain advance.
fn next_stage(stage: Stage) -> Option<Stage> {
    match stage {
        Stage::Setup => Some(Stage::Feature),
        Stage::Feature => Some(Stage::Audit),
        Stage::Audit => Some(Stage::Plan),
        Stage::Plan => Some(Stage::Decide),
        Stage::Decide => Some(Stage::Implement),
        Stage::Implement => Some(Stage::Validate),
        // Kept so state from older builds still advances.
        Stage::Repair => Some(Stage::Validate),
        Stage::Validate | Stage::Done => None,
    }
}

/// New state at `stage`/`status` with the transition appended. Clears `status_reason`.
fn moved(state: &RunState, stage: Stage, status: Status, reason: &str, now: u64) -> RunState {
    let transition = Transition {
        at: now,
        from: (state.stage, state.status),
        to: (stage, status),
        reason: reason.to_string(),
    };
    let history = state.history.iter().cloned().chain([transition]).collect();
    RunState {
        stage,
        status,
        status_reason: None,
        history,
        updated_at: now,
        ..state.clone()
    }
}

fn record_failure(state: &RunState, failure: &Failure, now: u64) -> RunState {
    let limits = state.limits;
    let repeat = state
        .last_failure
        .as_ref()
        .is_some_and(|f| f.signature == failure.signature);
    let attempts = Attempts {
        repair_cycles: state.attempts.repair_cycles.saturating_add(1),
        equivalent_failures: if repeat {
            state.attempts.equivalent_failures.saturating_add(1)
        } else {
            1
        },
    };
    let limit_hit = if attempts.equivalent_failures >= limits.max_equivalent_failures {
        Some(format!(
            "{} consecutive equivalent failures: {}",
            attempts.equivalent_failures, failure.summary
        ))
    } else if attempts.repair_cycles > limits.max_repair_cycles {
        Some(format!(
            "repair limit of {} cycles reached",
            limits.max_repair_cycles
        ))
    } else {
        None
    };
    let (status, reason) = match &limit_hit {
        Some(r) => (Status::Blocked, r.as_str()),
        None => (Status::Active, "validation failed"),
    };
    RunState {
        attempts,
        last_failure: Some(failure.clone()),
        status_reason: limit_hit.clone(),
        // A failure goes back through the thinking stages: re-read the requirements,
        // re-audit with the failure in hand, re-plan, then fix.
        ..moved(state, Stage::Feature, status, reason, now)
    }
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
