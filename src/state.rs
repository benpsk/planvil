//! Persistent run state. Written only by the `planvil` binary.

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// Fields added after a schema version shipped must have a serde default, so state written
/// by an older planvil still loads. Bump `SCHEMA_VERSION` only for incompatible changes.
fn default_stop_continuations() -> u32 {
    3
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// One-time per repository: discover and prove how to run and check the project.
    Setup,
    Feature,
    Audit,
    Plan,
    Decide,
    Implement,
    Validate,
    /// Legacy: failures now loop back to Feature. Kept so older state files load.
    Repair,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    WaitingForUser,
    Paused,
    Blocked,
    Cancelled,
    Completed,
}

impl Status {
    /// No further transitions are accepted from a terminal status.
    pub fn is_terminal(self) -> bool {
        match self {
            Status::Cancelled | Status::Completed => true,
            Status::Active | Status::WaitingForUser | Status::Paused | Status::Blocked => false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempts {
    pub repair_cycles: u32,
    pub equivalent_failures: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    /// Stable fingerprint of the failure, used to detect repeats.
    pub signature: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub at: u64,
    pub from: (Stage, Status),
    pub to: (Stage, Status),
    pub reason: String,
}

/// Repair limits, copied from config when the run starts so later config edits
/// cannot loosen a run in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_repair_cycles: u32,
    pub max_equivalent_failures: u32,
    /// Times the stop hook may send the agent back without any run progress.
    #[serde(default = "default_stop_continuations")]
    pub max_stop_continuations: u32,
    /// Wall-clock budget for the whole run, if any.
    #[serde(default)]
    pub max_run_minutes: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    General,
    /// The user confirms FEATURE.md at a specific content hash.
    ConfirmFeature,
    /// The user approves PLAN.md at a specific content hash.
    ApprovePlan,
    /// The user approves the proposed project verification at a specific content hash.
    ApproveSetup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub text: String,
    pub at: u64,
    /// For confirm/approve questions: did the user agree?
    #[serde(default)]
    pub approved: Option<bool>,
    /// The user's own words, as recorded by the prompt hook (not the agent's summary).
    #[serde(default)]
    pub user_reply: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub id: u32,
    pub text: String,
    pub kind: QuestionKind,
    pub asked_at: u64,
    /// Number of user prompts seen when the question was asked.
    #[serde(default)]
    pub asked_after_prompt: u64,
    /// Hash of the document being confirmed, when `kind` is not General.
    pub doc_hash: Option<String>,
    pub answer: Option<Answer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyRecord {
    pub run: u32,
    pub at: u64,
    /// Code fingerprint the evidence was collected against.
    pub fingerprint: String,
    pub passed: bool,
    /// Evidence folder relative to the feature folder.
    pub evidence_dir: String,
    pub failing: Vec<String>,
}

/// A setup proposal that was run and found runnable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupCheck {
    /// Hash of the proposal file that was checked.
    pub proposal_hash: String,
    pub at: u64,
    /// Every tool, service and the app worked; checks ran (pass or fail).
    pub runnable: bool,
    /// Checks that ran but failed on the current code (pre-existing failures).
    pub failing_checks: Vec<String>,
    pub evidence_dir: String,
}

/// Tracks stop-hook continuations so a stuck agent cannot loop forever.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopGuard {
    pub continuations: u32,
    pub progress_marker: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunState {
    pub schema_version: u32,
    pub run_id: String,
    /// Feature folder name (branch with `/` → `-`).
    pub feature_id: String,
    pub branch: String,
    pub base_branch: String,
    pub stage: Stage,
    pub status: Status,
    pub created_at: u64,
    pub updated_at: u64,
    pub limits: Limits,
    pub attempts: Attempts,
    pub last_failure: Option<Failure>,
    /// Why the run is blocked or waiting, shown to the user.
    pub status_reason: Option<String>,
    pub history: Vec<Transition>,
    /// Claude session the run is attached to. `None` until a hook claims it.
    #[serde(default)]
    pub session_id: Option<String>,
    /// User prompts (and AskUserQuestion answers) seen by the hooks, counted.
    #[serde(default)]
    pub user_prompts: u64,
    /// The latest of those, verbatim (truncated).
    #[serde(default)]
    pub last_user_reply: Option<String>,
    #[serde(default)]
    pub questions: Vec<Question>,
    #[serde(default)]
    pub feature_confirmed_hash: Option<String>,
    #[serde(default)]
    pub plan_approved_hash: Option<String>,
    #[serde(default)]
    pub verify_runs: u32,
    /// Result of the last `planvil setup check`.
    #[serde(default)]
    pub setup_check: Option<SetupCheck>,
    #[serde(default)]
    pub setup_approved_hash: Option<String>,
    /// A run started only to (re)discover the project setup; it ends after Setup.
    #[serde(default)]
    pub setup_only: bool,
    #[serde(default)]
    pub last_verify: Option<VerifyRecord>,
    #[serde(default)]
    pub stop_guard: StopGuard,
    /// Steps that failed in the last verification, with their command hash. Changing or
    /// removing one of them requires the user's plan approval.
    #[serde(default)]
    pub failed_steps: std::collections::BTreeMap<String, String>,
    /// Paths that already had uncommitted changes when the run started (user work).
    #[serde(default)]
    pub baseline_dirty: Vec<String>,
}

impl RunState {
    pub fn new(
        run_id: impl Into<String>,
        feature_id: impl Into<String>,
        branch: impl Into<String>,
        base_branch: impl Into<String>,
        limits: Limits,
        now: u64,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            run_id: run_id.into(),
            feature_id: feature_id.into(),
            branch: branch.into(),
            base_branch: base_branch.into(),
            stage: Stage::Feature,
            status: Status::Active,
            created_at: now,
            updated_at: now,
            limits,
            attempts: Attempts::default(),
            last_failure: None,
            status_reason: None,
            history: Vec::new(),
            session_id: None,
            user_prompts: 0,
            last_user_reply: None,
            questions: Vec::new(),
            feature_confirmed_hash: None,
            plan_approved_hash: None,
            verify_runs: 0,
            setup_check: None,
            setup_approved_hash: None,
            setup_only: false,
            last_verify: None,
            stop_guard: StopGuard::default(),
            failed_steps: std::collections::BTreeMap::new(),
            baseline_dirty: Vec::new(),
        }
    }

    /// The question the run is waiting on, if any.
    pub fn open_question(&self) -> Option<&Question> {
        self.questions.iter().find(|q| q.answer.is_none())
    }

    /// Changes whenever the run makes observable progress.
    pub fn progress_marker(&self) -> String {
        let answered = self.questions.iter().filter(|q| q.answer.is_some()).count();
        format!(
            "{}:{}:{}:{}",
            self.history.len(),
            self.verify_runs,
            self.questions.len(),
            answered
        )
    }
}

impl RunState {
    /// Invariants a loaded state must satisfy. A hand-edited file can break them even
    /// when it deserializes.
    pub fn validate(&self) -> Result<(), String> {
        let folder = crate::layout::branch_to_folder(&self.branch).map_err(|e| e.to_string())?;
        if folder != self.feature_id {
            return Err(format!(
                "feature_id `{}` does not match branch `{}`",
                self.feature_id, self.branch
            ));
        }
        if (self.status == Status::Completed) != (self.stage == Stage::Done) {
            return Err(format!(
                "status {:?} is inconsistent with stage {:?}",
                self.status, self.stage
            ));
        }
        let needs_reason = matches!(self.status, Status::Blocked | Status::WaitingForUser);
        if needs_reason && self.status_reason.is_none() {
            return Err(format!("status {:?} requires a status_reason", self.status));
        }
        let l = &self.limits;
        if l.max_repair_cycles == 0
            || l.max_equivalent_failures == 0
            || l.max_stop_continuations == 0
        {
            return Err("limits must be at least 1".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_repair_cycles: 5,
        max_equivalent_failures: 3,
        max_stop_continuations: 3,
        max_run_minutes: None,
    };

    fn sample() -> RunState {
        RunState::new("r1", "sp20-mcp", "sp20/mcp", "production", LIMITS, 100)
    }

    #[test]
    fn new_run_is_valid() {
        assert_eq!(sample().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_feature_id_branch_mismatch() {
        let s = RunState {
            feature_id: "other".into(),
            ..sample()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_completed_outside_done() {
        let s = RunState {
            status: Status::Completed,
            stage: Stage::Implement,
            ..sample()
        };
        assert!(s.validate().is_err());
        let s = RunState {
            status: Status::Active,
            stage: Stage::Done,
            ..sample()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_requires_reason_when_blocked_or_waiting() {
        let s = RunState {
            status: Status::Blocked,
            ..sample()
        };
        assert!(s.validate().is_err());
        let s = RunState {
            status: Status::Blocked,
            status_reason: Some("x".into()),
            ..sample()
        };
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn new_run_starts_at_feature_and_active() {
        let s = sample();
        assert_eq!(s.stage, Stage::Feature);
        assert_eq!(s.status, Status::Active);
        assert_eq!(s.schema_version, SCHEMA_VERSION);
        assert!(s.history.is_empty());
    }

    #[test]
    fn serializes_enums_as_snake_case() {
        let json = serde_json::to_string(&Status::WaitingForUser).unwrap();
        assert_eq!(json, "\"waiting_for_user\"");
        let stage: Stage = serde_json::from_str("\"implement\"").unwrap();
        assert_eq!(stage, Stage::Implement);
    }

    #[test]
    fn round_trips_through_json() {
        let s = sample();
        let back: RunState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn rejects_unknown_fields() {
        let mut v = serde_json::to_value(sample()).unwrap();
        v["sneaky"] = serde_json::json!(true);
        assert!(serde_json::from_value::<RunState>(v).is_err());
    }

    #[test]
    fn state_from_an_older_build_without_newer_fields_still_loads() {
        let mut v = serde_json::to_value(sample()).unwrap();
        let obj = v.as_object_mut().unwrap();
        for f in [
            "setup_only",
            "setup_check",
            "setup_approved_hash",
            "failed_steps",
            "user_prompts",
            "last_user_reply",
            "baseline_dirty",
        ] {
            obj.remove(f);
        }
        obj["limits"]
            .as_object_mut()
            .unwrap()
            .remove("max_stop_continuations");
        let back: RunState = serde_json::from_value(v).unwrap();
        assert!(!back.setup_only);
        assert_eq!(back.limits.max_stop_continuations, 3);
    }
}
