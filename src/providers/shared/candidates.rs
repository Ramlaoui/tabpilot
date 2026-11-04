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
    let answer = unfence(response.trim(), &request.document.language_id);
    if answer.is_empty() {
        return Vec::new();
    }
    vec![CompletionCandidate {
        insert_text: format!("\n{answer}"),
        range: Some(TextRange::empty_at(request.position)),
    }]
}

/// Models fence code even when told not to, and outside Markdown the fence
/// would land in the file. Unwraps an answer that is one fenced block.
fn unfence<'a>(answer: &'a str, language_id: &str) -> &'a str {
    if language_id == "markdown" {
        return answer;
    }
    let body = answer
        .strip_prefix("```")
        .and_then(|rest| rest.strip_suffix("```"))
        .and_then(|rest| rest.split_once('\n'))
        .map(|(_info, body)| body.trim_end());
    match body {
        Some(body) if !body.contains("```") => body,
        _ => answer,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::CompletionCancellation;
    use crate::context::ContextEngine;
    use crate::documents::DocumentSnapshot;
    use crate::protocol::CursorPosition;
    use crate::workspace::WorkspaceContext;

    fn answer_request(language_id: &str) -> AnswerRequest {
        let document = DocumentSnapshot {
            uri: "file:///tmp/notes".to_owned(),
            language_id: language_id.to_owned(),
            version: 1,
            text: "-- why --".to_owned(),
        };
        let position = CursorPosition::new(0, 9);
        AnswerRequest {
            completion_id: 1,
            question: "why".to_owned(),
            context: ContextEngine.build(&document, position),
            document,
            position,
            workspace: WorkspaceContext::default(),
            cancellation: CompletionCancellation::new(),
        }
    }

    #[test]
    fn answers_follow_the_question() {
        assert_eq!(
            answer_to_candidate(&answer_request("markdown"), " because \n"),
            vec![CompletionCandidate {
                insert_text: "\nbecause".to_owned(),
                range: Some(TextRange::empty_at(CursorPosition::new(0, 9))),
            }]
        );
    }

    #[test]
    fn unwraps_a_fenced_answer_outside_markdown() {
        let fenced = "```python\n# in place\nitems.reverse()\n```\n";
        let answer = |language_id| {
            answer_to_candidate(&answer_request(language_id), fenced)[0]
                .insert_text
                .clone()
        };
        assert_eq!(answer("python"), "\n# in place\nitems.reverse()");
        assert_eq!(answer("markdown"), format!("\n{}", fenced.trim()));

        let two_blocks = "```\na\n```\nor\n```\nb\n```";
        assert_eq!(unfence(two_blocks, "python"), two_blocks);
    }
}
