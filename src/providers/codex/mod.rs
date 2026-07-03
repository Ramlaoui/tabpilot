mod app_server;
mod config;
mod debug;
mod dry_run;
mod protocol;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::answer::AnswerRequest;
use crate::completion::{CompletionCandidate, CompletionRequest};
use crate::providers::CompletionProvider;
use crate::providers::shared::{
    CompletionPromptVariant, answer_context_to_text, answer_to_candidate, context_to_text,
    debug_answer_document_metadata, debug_candidates, debug_document_metadata,
    decorate_answer_prompt, decorate_completion_prompt, suggestions_to_candidates, workspace_cwd,
};

use self::app_server::CodexAppServerClient;
use self::config::CodexProviderConfig;
use self::debug::write_codex_debug_event;
use self::dry_run::{dry_run_answer_response, dry_run_response};

pub struct CodexProvider {
    config: CodexProviderConfig,
    client: Mutex<Option<CodexAppServerClient>>,
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self {
            config: CodexProviderConfig::default(),
            client: Mutex::new(None),
        }
    }
}

#[async_trait]
impl CompletionProvider for CodexProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionCandidate>> {
        if request.cancellation.is_cancelled() {
            info!(
                completion_id = request.completion_id,
                "skipping cancelled codex completion",
            );
            return Ok(Vec::new());
        }

        let context_text = context_to_text(&request);
        let response = match self
            .communicate_with_codex_thread(&request, &context_text)
            .await
        {
            Ok(Some(response)) => response,
            Ok(None) => return Ok(Vec::new()),
            Err(err) => {
                write_codex_debug_event(
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
                write_codex_debug_event(
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
            write_codex_debug_event(
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
            context_chars = context_text.len(),
            response_chars = response.len(),
            candidate_count = candidates.len(),
            "codex provider completed",
        );
        debug!(%context_text, %response, "codex provider context and response");
        write_codex_debug_event(
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
                "skipping cancelled codex answer",
            );
            return Ok(Vec::new());
        }

        let context_text = answer_context_to_text(&request);
        let prompt = decorate_answer_prompt(&context_text, &request.question);
        let response = match self.communicate_with_codex_answer(&request, &prompt).await {
            Ok(Some(response)) => response,
            Ok(None) => return Ok(Vec::new()),
            Err(err) => {
                write_codex_debug_event(
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
            "codex provider answered",
        );
        debug!(%prompt, %response, "codex answer prompt and response");
        write_codex_debug_event(
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
        if self.config.dry_run {
            write_codex_debug_event(
                &self.config,
                json!({
                    "event": "cancel_skipped_dry_run",
                    "completion_id": completion_id,
                }),
            );
            return Ok(());
        }

        self.cancel_completion(completion_id).await
    }
}

impl CodexProvider {
    async fn cancel_completion(&self, completion_id: u64) -> Result<()> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            write_codex_debug_event(
                &self.config,
                json!({
                    "event": "cancel_skipped_no_client",
                    "completion_id": completion_id,
                }),
            );
            return Ok(());
        };

        client.interrupt_completion(completion_id).await
    }

    async fn communicate_with_codex_thread(
        &self,
        request: &CompletionRequest,
        context_text: &str,
    ) -> Result<Option<String>> {
        if self.config.dry_run {
            let prompt =
                decorate_completion_prompt(context_text, CompletionPromptVariant::FullContract);
            return Ok(Some(dry_run_response(&prompt)));
        }

        let mut client = tokio::select! {
            client = self.client.lock() => client,
            _ = request.cancellation.cancelled() => {
                write_codex_debug_event(
                    &self.config,
                    json!({
                        "event": "completion_cancelled_before_client_lock",
                        "completion_id": request.completion_id,
                        "document": debug_document_metadata(request),
                    }),
                );
                return Ok(None);
            }
        };

        if request.cancellation.is_cancelled() {
            return Ok(None);
        }

        let client = match client.as_mut() {
            Some(client) => client,
            None => client.insert(CodexAppServerClient::spawn(self.config.clone()).await?),
        };

        client
            .complete(
                request.completion_id,
                request.document.uri.as_str(),
                workspace_cwd(&request.workspace),
                context_text,
                request.cancellation.clone(),
            )
            .await
    }

    async fn communicate_with_codex_answer(
        &self,
        request: &AnswerRequest,
        prompt: &str,
    ) -> Result<Option<String>> {
        if self.config.dry_run {
            return Ok(Some(dry_run_answer_response(&request.question)));
        }

        let mut client = tokio::select! {
            client = self.client.lock() => client,
            _ = request.cancellation.cancelled() => {
                write_codex_debug_event(
                    &self.config,
                    json!({
                        "event": "answer_cancelled_before_client_lock",
                        "completion_id": request.completion_id,
                        "document": debug_answer_document_metadata(request),
                        "question": request.question,
                    }),
                );
                return Ok(None);
            }
        };

        if request.cancellation.is_cancelled() {
            return Ok(None);
        }

        let client = match client.as_mut() {
            Some(client) => client,
            None => client.insert(CodexAppServerClient::spawn(self.config.clone()).await?),
        };

        client
            .answer(
                request.completion_id,
                request.document.uri.as_str(),
                workspace_cwd(&request.workspace),
                prompt,
                request.cancellation.clone(),
            )
            .await
    }
}
