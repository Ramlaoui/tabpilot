mod cli;
mod config;
mod debug;
mod dry_run;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use tracing::{debug, info};

use crate::answer::AnswerRequest;
use crate::completion::{CompletionCandidate, CompletionRequest};
use crate::providers::CompletionProvider;
use crate::providers::shared::{
    CompletionPromptVariant, answer_context_to_text, answer_to_candidate, completion_output_schema,
    context_to_text, debug_answer_document_metadata, debug_candidates, debug_document_metadata,
    decorate_answer_prompt, decorate_completion_prompt, suggestions_to_candidates, workspace_cwd,
};

use self::cli::ClaudeCliClient;
use self::config::ClaudeProviderConfig;
use self::debug::write_claude_debug_event;
use self::dry_run::{dry_run_answer_response, dry_run_response};

pub struct ClaudeProvider {
    config: ClaudeProviderConfig,
    client: ClaudeCliClient,
}

impl Default for ClaudeProvider {
    fn default() -> Self {
        let config = ClaudeProviderConfig::default();
        Self {
            client: ClaudeCliClient::new(config.clone()),
            config,
        }
    }
}

#[async_trait]
impl CompletionProvider for ClaudeProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionCandidate>> {
        if request.cancellation.is_cancelled() {
            info!(
                completion_id = request.completion_id,
                "skipping cancelled claude completion",
            );
            return Ok(Vec::new());
        }

        let context_text = context_to_text(&request);
        let prompt =
            decorate_completion_prompt(&context_text, CompletionPromptVariant::FullContract);
        let response = match self.complete_with_claude(&request, &prompt).await {
            Ok(Some(response)) => response,
            Ok(None) => return Ok(Vec::new()),
            Err(err) => {
                write_claude_debug_event(
                    &self.config,
                    json!({
                        "event": "transport_error",
                        "completion_id": request.completion_id,
                        "document": debug_document_metadata(&request),
                        "error": err.to_string(),
                    }),
                );
                return Err(err);
            }
        };

        let conversion = match suggestions_to_candidates(&request, &response) {
            Ok(conversion) => conversion,
            Err(err) => {
                write_claude_debug_event(
                    &self.config,
                    json!({
                        "event": "parse_error",
                        "completion_id": request.completion_id,
                        "document": debug_document_metadata(&request),
                        "response": response,
                        "error": err.to_string(),
                    }),
                );
                return Err(err);
            }
        };
        if !conversion.rejections.is_empty() {
            write_claude_debug_event(
                &self.config,
                json!({
                    "event": "candidate_rejections",
                    "completion_id": request.completion_id,
                    "document": debug_document_metadata(&request),
                    "rejection_count": conversion.rejections.len(),
                    "rejections": conversion
                        .rejections
                        .iter()
                        .map(|rejection| {
                            json!({
                                "insert_text": rejection.insert_text,
                                "score": rejection.score,
                                "reason": rejection.reason.as_str(),
                            })
                        })
                        .collect::<Vec<_>>(),
                }),
            );
        }
        let candidates = conversion.candidates;

        info!(
            dry_run = self.config.dry_run,
            prompt_chars = prompt.len(),
            response_chars = response.len(),
            candidate_count = candidates.len(),
            "claude provider completed",
        );
        debug!(%prompt, %response, "claude provider prompt and response");
        write_claude_debug_event(
            &self.config,
            json!({
                "event": "completion_candidates",
                "completion_id": request.completion_id,
                "document": debug_document_metadata(&request),
                "candidate_count": candidates.len(),
                "candidates": debug_candidates(&candidates),
            }),
        );

        Ok(candidates)
    }

    async fn answer(&self, request: AnswerRequest) -> Result<Vec<CompletionCandidate>> {
        if request.cancellation.is_cancelled() {
            info!(
                completion_id = request.completion_id,
                "skipping cancelled claude answer",
            );
            return Ok(Vec::new());
        }

        let context_text = answer_context_to_text(&request);
        let prompt = decorate_answer_prompt(&context_text, &request.question);
        let response = match self.answer_with_claude(&request, &prompt).await {
            Ok(Some(response)) => response,
            Ok(None) => return Ok(Vec::new()),
            Err(err) => {
                write_claude_debug_event(
                    &self.config,
                    json!({
                        "event": "answer_transport_error",
                        "completion_id": request.completion_id,
                        "document": debug_answer_document_metadata(&request),
                        "question": request.question,
                        "error": err.to_string(),
                    }),
                );
                return Err(err);
            }
        };

        let candidates = answer_to_candidate(&request, &response);
        info!(
            dry_run = self.config.dry_run,
            prompt_chars = prompt.len(),
            response_chars = response.len(),
            candidate_count = candidates.len(),
            "claude provider answered",
        );
        debug!(%prompt, %response, "claude answer prompt and response");
        write_claude_debug_event(
            &self.config,
            json!({
                "event": "answer_candidates",
                "completion_id": request.completion_id,
                "document": debug_answer_document_metadata(&request),
                "question": request.question,
                "response": response,
                "candidate_count": candidates.len(),
                "candidates": debug_candidates(&candidates),
            }),
        );

        Ok(candidates)
    }

    async fn cancel(&self, completion_id: u64) -> Result<()> {
        write_claude_debug_event(
            &self.config,
            json!({
                "event": "cancel_observed",
                "completion_id": completion_id,
                "note": "in-flight claude processes are cancelled through the request cancellation token",
            }),
        );
        Ok(())
    }
}

impl ClaudeProvider {
    async fn complete_with_claude(
        &self,
        request: &CompletionRequest,
        prompt: &str,
    ) -> Result<Option<String>> {
        if self.config.dry_run {
            return Ok(Some(dry_run_response(prompt)));
        }

        self.client
            .run(
                prompt,
                Some(completion_output_schema()),
                workspace_cwd(&request.workspace),
                request.cancellation.clone(),
            )
            .await
    }

    async fn answer_with_claude(
        &self,
        request: &AnswerRequest,
        prompt: &str,
    ) -> Result<Option<String>> {
        if self.config.dry_run {
            return Ok(Some(dry_run_answer_response(&request.question)));
        }

        self.client
            .run(
                prompt,
                None,
                workspace_cwd(&request.workspace),
                request.cancellation.clone(),
            )
            .await
    }
}
