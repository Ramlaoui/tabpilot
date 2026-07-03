use anyhow::Result;
use async_trait::async_trait;

use crate::answer::AnswerRequest;
use crate::completion::{CompletionCandidate, CompletionRequest};
use crate::protocol::TextRange;
use crate::providers::CompletionProvider;

#[derive(Debug, Default)]
pub struct MockProvider;

#[async_trait]
impl CompletionProvider for MockProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionCandidate>> {
        let _ = &request.workspace;

        let comment_prefix = comment_prefix_for_language(&request.document.language_id);
        let insert_text = if request.context.active_prefix.trim().is_empty() {
            "tabpilot".to_owned()
        } else if request
            .context
            .active_suffix
            .starts_with(char::is_alphanumeric)
        {
            String::new()
        } else {
            format!(" {comment_prefix} tabpilot")
        };

        if insert_text.is_empty() {
            Ok(vec![])
        } else {
            Ok(vec![CompletionCandidate {
                insert_text,
                range: Some(TextRange::empty_at(request.position)),
            }])
        }
    }

    async fn answer(&self, request: AnswerRequest) -> Result<Vec<CompletionCandidate>> {
        Ok(vec![CompletionCandidate {
            insert_text: format!("\nmock answer: {}", request.question),
            range: Some(TextRange::empty_at(request.position)),
        }])
    }
}

fn comment_prefix_for_language(language_id: &str) -> &'static str {
    match language_id {
        "lua" | "vim" => "--",
        "python" | "ruby" | "sh" | "bash" | "zsh" => "#",
        _ => "//",
    }
}
