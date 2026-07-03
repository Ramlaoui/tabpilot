use anyhow::{Context, Result};
use serde::Deserialize;

use crate::answer::AnswerRequest;
use crate::completion::{CompletionCandidate, CompletionRequest};
use crate::protocol::TextRange;

use super::MAX_SUGGESTIONS;

pub(in crate::providers) fn answer_to_candidate(
    request: &AnswerRequest,
    response: &str,
) -> Vec<CompletionCandidate> {
    let answer = response.trim();

    if answer.is_empty() {
        Vec::new()
    } else {
        vec![CompletionCandidate {
            insert_text: format!("\n{answer}"),
            range: Some(TextRange::empty_at(request.position)),
        }]
    }
}

pub(in crate::providers) fn suggestions_to_candidates(
    request: &CompletionRequest,
    response: &str,
) -> Result<CandidateConversion> {
    let parsed: CompletionResponse =
        serde_json::from_str(response).context("failed to parse completion response")?;

    let mut candidates = Vec::new();
    let mut rejections = Vec::new();

    for suggestion in parsed.suggestions.into_iter().take(MAX_SUGGESTIONS) {
        let insert_text = suggestion.insert_text;
        let score = suggestion.score;

        if insert_text.is_empty() {
            rejections.push(CandidateRejection::new(
                insert_text,
                score,
                CandidateRejectionReason::EmptyInsertText,
            ));
            continue;
        }

        if is_dangling_prefix(&insert_text) {
            rejections.push(CandidateRejection::new(
                insert_text,
                score,
                CandidateRejectionReason::DanglingPrefix,
            ));
            continue;
        }

        candidates.push(CompletionCandidate {
            insert_text,
            range: Some(TextRange::empty_at(request.position)),
        });
    }

    Ok(CandidateConversion {
        candidates,
        rejections,
    })
}

fn is_dangling_prefix(insert_text: &str) -> bool {
    let trimmed = insert_text.trim_end();

    matches!(trimmed.chars().last(), Some('{' | '[' | '(' | ':'))
}

pub(in crate::providers) struct CandidateConversion {
    pub candidates: Vec<CompletionCandidate>,
    pub rejections: Vec<CandidateRejection>,
}

pub(in crate::providers) struct CandidateRejection {
    pub insert_text: String,
    pub score: Option<f32>,
    pub reason: CandidateRejectionReason,
}

impl CandidateRejection {
    fn new(insert_text: String, score: Option<f32>, reason: CandidateRejectionReason) -> Self {
        Self {
            insert_text,
            score,
            reason,
        }
    }
}

pub(in crate::providers) enum CandidateRejectionReason {
    EmptyInsertText,
    DanglingPrefix,
}

impl CandidateRejectionReason {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::EmptyInsertText => "empty_insert_text",
            Self::DanglingPrefix => "dangling_prefix",
        }
    }
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    suggestions: Vec<CompletionSuggestion>,
}

#[derive(Debug, Deserialize)]
struct CompletionSuggestion {
    insert_text: String,
    score: Option<f32>,
}
