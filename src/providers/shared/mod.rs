mod candidates;
mod context;
mod debug;
mod prompt;
mod schema;

const MAX_PREFIX_CHARS: usize = 8_000;
const MAX_SUFFIX_CHARS: usize = 4_000;
const MAX_CURSOR_WINDOW_CHARS: usize = 10_000;
const MAX_SUGGESTIONS: usize = 3;

pub(super) use candidates::{answer_to_candidate, suggestions_to_candidates};
pub(super) use context::{answer_context_to_text, context_to_text, workspace_cwd};
pub(super) use debug::{
    debug_answer_document_metadata, debug_candidates, debug_document_metadata,
    provider_debug_transcript_path,
};
pub(super) use prompt::{
    CompletionPromptVariant, decorate_answer_prompt, decorate_completion_prompt,
};
pub(super) use schema::completion_output_schema;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::{CompletionCandidate, CompletionRequest};
    use crate::context::ContextPack;
    use crate::documents::DocumentSnapshot;
    use crate::protocol::{CursorPosition, TextRange};
    use crate::workspace::WorkspaceContext;

    #[test]
    fn full_prompt_keeps_static_instructions_first() {
        let prompt = decorate_completion_prompt(
            "dynamic cursor context",
            CompletionPromptVariant::FullContract,
        );

        assert!(prompt.starts_with(prompt::COMPLETION_SYSTEM_PROMPT));
        assert!(prompt.contains(prompt::COMPLETION_OUTPUT_PROMPT));
        assert!(prompt.ends_with("dynamic cursor context"));
    }

    #[test]
    fn continuation_prompt_keeps_only_the_short_reminder() {
        let prompt = decorate_completion_prompt(
            "dynamic cursor context",
            CompletionPromptVariant::ContinuationReminder,
        );

        assert!(prompt.starts_with(prompt::COMPLETION_CONTINUATION_PROMPT));
        assert!(!prompt.contains("Default behavior:"));
        assert!(!prompt.contains("Return a single JSON object with this exact shape"));
        assert!(prompt.ends_with("dynamic cursor context"));
    }

    #[test]
    fn parses_suggestions_into_cursor_insertions() {
        let request = completion_request();
        let response = r#"{"suggestions":[{"insert_text":" world","score":0.9}]}"#;

        let conversion = suggestions_to_candidates(&request, response).unwrap();

        assert_eq!(
            conversion.candidates,
            vec![CompletionCandidate {
                insert_text: " world".to_owned(),
                range: Some(TextRange::empty_at(CursorPosition::new(0, 5))),
            }]
        );
        assert!(conversion.rejections.is_empty());
    }

    #[test]
    fn accepts_low_confidence_suggestions_by_default() {
        let request = completion_request();
        let response = r#"{"suggestions":[{"insert_text":" speculative","score":0.1}]}"#;

        let conversion = suggestions_to_candidates(&request, response).unwrap();

        assert_eq!(
            conversion.candidates,
            vec![CompletionCandidate {
                insert_text: " speculative".to_owned(),
                range: Some(TextRange::empty_at(CursorPosition::new(0, 5))),
            }]
        );
        assert!(conversion.rejections.is_empty());
    }

    #[test]
    fn rejects_dangling_prefix_suggestions() {
        let request = completion_request();
        let response = r#"{"suggestions":[{"insert_text":"\"items\": {","score":0.99}]}"#;

        let conversion = suggestions_to_candidates(&request, response).unwrap();

        assert!(conversion.candidates.is_empty());
        assert_eq!(conversion.rejections.len(), 1);
        assert_eq!(conversion.rejections[0].reason.as_str(), "dangling_prefix");
    }

    #[test]
    fn keeps_completed_suggestions_that_end_with_commas() {
        let request = completion_request();
        let response =
            r#"{"suggestions":[{"insert_text":"Self::Code => \"code\",","score":0.99}]}"#;

        let conversion = suggestions_to_candidates(&request, response).unwrap();

        assert_eq!(
            conversion.candidates,
            vec![CompletionCandidate {
                insert_text: "Self::Code => \"code\",".to_owned(),
                range: Some(TextRange::empty_at(CursorPosition::new(0, 5))),
            }]
        );
        assert!(conversion.rejections.is_empty());
    }

    #[test]
    fn context_text_trims_large_cursor_context() {
        let mut request = completion_request();
        request.context.active_prefix = "a".repeat(MAX_PREFIX_CHARS + 10);
        request.context.active_suffix = "b".repeat(MAX_SUFFIX_CHARS + 10);
        request.context.cursor_window = "c".repeat(MAX_CURSOR_WINDOW_CHARS + 10);

        let context = context_to_text(&request);

        assert!(context.contains("Current cursor:"));
        assert!(context.contains("- position_kind: code"));
        assert!(context.contains(r#"- line_prefix: "hello""#));
        assert!(context.contains("Local cursor window:"));
        assert!(context.contains(&"a".repeat(MAX_PREFIX_CHARS)));
        assert!(context.contains(&"b".repeat(MAX_SUFFIX_CHARS)));
        assert!(context.contains(&"c".repeat(MAX_CURSOR_WINDOW_CHARS)));
        assert!(!context.contains(&"a".repeat(MAX_PREFIX_CHARS + 1)));
        assert!(!context.contains(&"b".repeat(MAX_SUFFIX_CHARS + 1)));
        assert!(!context.contains(&"c".repeat(MAX_CURSOR_WINDOW_CHARS + 1)));
    }

    fn completion_request() -> CompletionRequest {
        CompletionRequest {
            completion_id: 1,
            document: DocumentSnapshot {
                uri: "file:///tmp/main.rs".to_owned(),
                language_id: "rust".to_owned(),
                version: 1,
                text: "hello".to_owned(),
            },
            position: CursorPosition::new(0, 5),
            context: ContextPack {
                active_prefix: "hello".to_owned(),
                active_suffix: String::new(),
                current_line_prefix: "hello".to_owned(),
                current_line_suffix: String::new(),
                cursor_window: "   1: hello<|cursor|>".to_owned(),
                position_kind: crate::context::PositionKind::Code,
            },
            workspace: WorkspaceContext::default(),
            cancellation: Default::default(),
        }
    }
}
