use std::path::Path;

use super::guide::{next_action, sync_requirements};
use super::{Result, apply, current_workspace, refused};
use crate::docs;
use crate::machine::Event;
use crate::state::{Answer, Question, QuestionKind, RunState};
use crate::util::now_secs;

// ---------------------------------------------------------------------------------------
// questions

pub fn ask(cwd: &Path, text: &str, kind: QuestionKind) -> Result<String> {
    if text.trim().is_empty() {
        return Err(refused("the question text is empty"));
    }
    let ws = current_workspace(cwd)?;
    let (lock, state) = ws.locked_run()?;
    let (state, rewound) = sync_requirements(&ws, state)?;
    if rewound.is_some() {
        lock.save(&state)?;
    }
    let doc_hash = match kind {
        QuestionKind::General => None,
        QuestionKind::ConfirmFeature => {
            let problems = docs::check_feature(&ws.read_doc(docs::FEATURE)?);
            if !problems.is_empty() {
                return Err(refused(format!(
                    "complete FEATURE.md before asking for confirmation:\n  - {}",
                    problems.join("\n  - ")
                )));
            }
            ws.doc_hash(docs::FEATURE)?
        }
        QuestionKind::ApprovePlan => ws.plan_hash()?,
        QuestionKind::ApproveSetup => {
            let problems = super::setup::proposal_problems(&ws, &state)?;
            if !problems.is_empty() {
                return Err(refused(format!(
                    "the setup is not ready for approval:\n  - {}",
                    problems.join("\n  - ")
                )));
            }
            super::setup::proposal_hash(&ws)?
        }
    };
    let id = u32::try_from(state.questions.len())
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let question = Question {
        id,
        text: text.trim().to_string(),
        kind,
        asked_at: now_secs(),
        asked_after_prompt: state.user_prompts,
        doc_hash,
        answer: None,
    };
    let asked = RunState {
        questions: state
            .questions
            .iter()
            .cloned()
            .chain([question.clone()])
            .collect(),
        ..apply(
            &state,
            &Event::AskUser {
                question: format!("Q{id}: {}", question.text),
            },
        )?
    };
    lock.save(&asked)?;
    Ok(format!(
        "Q{id} recorded. Show the question to the user and end your turn; planvil allows stopping while it \
         waits. Record their reply with `planvil answer \"...\"`{}.",
        if kind == QuestionKind::General {
            ""
        } else {
            " plus --approved or --rejected"
        }
    ))
}

pub fn answer(cwd: &Path, text: &str, approved: Option<bool>) -> Result<String> {
    let ws = current_workspace(cwd)?;
    let (lock, state) = ws.locked_run()?;
    let question = state
        .open_question()
        .cloned()
        .ok_or_else(|| refused("there is no open question"))?;
    // The answer must come after a real user prompt, counted by the prompt hook.
    if state.user_prompts <= question.asked_after_prompt {
        return Err(refused(format!(
            "no user message since Q{} was asked. End your turn and wait for the user's reply.",
            question.id
        )));
    }
    if question.kind != QuestionKind::General && approved.is_none() {
        return Err(refused(
            "this is a confirmation question: pass --approved or --rejected",
        ));
    }
    if text.trim().is_empty() {
        return Err(refused(
            "the answer text is empty; summarize what the user said",
        ));
    }
    let answer = Answer {
        text: text.trim().to_string(),
        at: now_secs(),
        approved,
        user_reply: state.last_user_reply.clone(),
    };
    let questions = state
        .questions
        .iter()
        .map(|q| {
            if q.id == question.id {
                Question {
                    answer: Some(answer.clone()),
                    ..q.clone()
                }
            } else {
                q.clone()
            }
        })
        .collect();
    let mut note = String::new();
    let (feature_confirmed_hash, plan_approved_hash) = match (question.kind, approved) {
        (QuestionKind::ConfirmFeature, Some(true))
            if ws.doc_hash(docs::FEATURE)? == question.doc_hash =>
        {
            (question.doc_hash.clone(), state.plan_approved_hash.clone())
        }
        (QuestionKind::ApprovePlan, Some(true)) if ws.plan_hash()? == question.doc_hash => (
            state.feature_confirmed_hash.clone(),
            question.doc_hash.clone(),
        ),
        (QuestionKind::General | QuestionKind::ApproveSetup, _) | (_, Some(false)) => (
            state.feature_confirmed_hash.clone(),
            state.plan_approved_hash.clone(),
        ),
        _ => {
            note = " The document changed after the question was asked, so it is not confirmed; ask again.".into();
            (
                state.feature_confirmed_hash.clone(),
                state.plan_approved_hash.clone(),
            )
        }
    };
    let setup_approved_hash = match (question.kind, approved) {
        (QuestionKind::ApproveSetup, Some(true))
            if super::setup::proposal_hash(&ws)? == question.doc_hash =>
        {
            question.doc_hash.clone()
        }
        (QuestionKind::ApproveSetup, Some(true)) => {
            note = " The setup proposal changed after the question was asked, so it is not approved; check and ask again.".into();
            state.setup_approved_hash.clone()
        }
        _ => state.setup_approved_hash.clone(),
    };
    let next = RunState {
        questions,
        setup_approved_hash,
        feature_confirmed_hash,
        plan_approved_hash,
        ..apply(&state, &Event::Answered)?
    };
    lock.save(&next)?;
    Ok(format!(
        "recorded the answer to Q{}.{note} Update DECISIONS.md (and PLAN.md if affected).\nnext: {}",
        question.id,
        next_action(&ws, &next)
    ))
}
