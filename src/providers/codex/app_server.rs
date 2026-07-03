use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::providers::shared::{
    CompletionPromptVariant, completion_output_schema, decorate_completion_prompt,
};

use super::config::{
    CodexProviderConfig, MAX_CANCELLED_TURNS_PER_THREAD, MAX_COMPLETED_TURNS_PER_THREAD,
    MAX_THREAD_INPUT_TOKENS, configured_codex_mcp_server_names,
};
use super::debug::write_codex_debug_event;
use super::protocol::{
    ClientMessage, ServerMessage, completed_turn_agent_message, completed_turn_error,
    error_message, item_completed_agent_message, notification_turn_id, same_known_turn,
    same_thread,
};

pub(super) struct CodexAppServerClient {
    config: CodexProviderConfig,
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_request_id: u64,
    thread_id: Option<String>,
    thread_document_uri: Option<String>,
    completed_turns_in_thread: u32,
    cancelled_turns_in_thread: u32,
    last_thread_input_tokens: Option<u64>,
    active_turn: Option<ActiveCodexTurn>,
}

#[derive(Debug, Clone)]
struct ActiveCodexTurn {
    completion_id: u64,
    thread_id: String,
    turn_id: String,
    counts_toward_completion_thread: bool,
}

#[derive(Debug, Clone, Copy)]
enum CodexTurnKind {
    Completion(CompletionPromptVariant),
    Answer,
}

impl CodexTurnKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Completion(_) => "completion",
            Self::Answer => "answer",
        }
    }

    const fn completion_prompt_variant(self) -> Option<CompletionPromptVariant> {
        match self {
            Self::Completion(prompt_variant) => Some(prompt_variant),
            Self::Answer => None,
        }
    }

    const fn counts_toward_completion_thread(self) -> bool {
        matches!(self, Self::Completion(_))
    }
}

impl CodexAppServerClient {
    pub(super) async fn spawn(config: CodexProviderConfig) -> Result<Self> {
        let mut command = Command::new("codex");
        command.args(["app-server", "--disable", "hooks", "--disable", "apps"]);

        for server_name in configured_codex_mcp_server_names() {
            command
                .arg("-c")
                .arg(format!("mcp_servers.{server_name}.enabled=false"));
        }

        let mut child = command
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("failed to spawn codex app-server")?;

        let stdin = child
            .stdin
            .take()
            .context("codex app-server stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .context("codex app-server stdout was not piped")?;

        let mut client = Self {
            config,
            _child: child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            next_request_id: 0,
            thread_id: None,
            thread_document_uri: None,
            completed_turns_in_thread: 0,
            cancelled_turns_in_thread: 0,
            last_thread_input_tokens: None,
            active_turn: None,
        };

        client.initialize().await?;
        Ok(client)
    }

    pub(super) async fn complete(
        &mut self,
        completion_id: u64,
        document_uri: &str,
        cwd: Option<String>,
        context_text: &str,
        cancellation: crate::completion::CompletionCancellation,
    ) -> Result<Option<String>> {
        if let Some(reason) = self.thread_rotation_reason(document_uri) {
            self.rotate_thread(reason);
        }

        let prompt_variant = if self.thread_id.is_some() {
            CompletionPromptVariant::ContinuationReminder
        } else {
            CompletionPromptVariant::FullContract
        };
        let prompt = decorate_completion_prompt(context_text, prompt_variant);

        let Some(thread_id) = self
            .ensure_thread(document_uri, cwd.clone(), &cancellation)
            .await?
        else {
            return Ok(None);
        };
        self.start_turn(
            completion_id,
            &thread_id,
            cwd,
            &prompt,
            CodexTurnKind::Completion(prompt_variant),
            cancellation,
        )
        .await
    }

    pub(super) async fn answer(
        &mut self,
        completion_id: u64,
        document_uri: &str,
        cwd: Option<String>,
        prompt: &str,
        cancellation: crate::completion::CompletionCancellation,
    ) -> Result<Option<String>> {
        let Some(thread_id) = self
            .start_answer_thread(document_uri, cwd.clone(), &cancellation)
            .await?
        else {
            return Ok(None);
        };

        self.start_turn(
            completion_id,
            &thread_id,
            cwd,
            prompt,
            CodexTurnKind::Answer,
            cancellation,
        )
        .await
    }

    async fn initialize(&mut self) -> Result<()> {
        let _: Value = self
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "tabpilot",
                        "title": "Tabpilot",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {
                        "experimentalApi": true
                    }
                }),
            )
            .await
            .context("codex app-server initialize failed")?;

        self.notify("initialized", json!({})).await?;
        Ok(())
    }

    async fn ensure_thread(
        &mut self,
        document_uri: &str,
        cwd: Option<String>,
        cancellation: &crate::completion::CompletionCancellation,
    ) -> Result<Option<String>> {
        if let Some(thread_id) = &self.thread_id {
            write_codex_debug_event(
                &self.config,
                json!({
                    "event": "thread_reused",
                    "thread_id": thread_id,
                    "document_uri": document_uri,
                    "completed_turns_in_thread": self.completed_turns_in_thread,
                    "cancelled_turns_in_thread": self.cancelled_turns_in_thread,
                    "last_input_tokens": self.last_thread_input_tokens,
                }),
            );
            return Ok(Some(thread_id.clone()));
        }

        let params = self.thread_start_params(cwd, "tabpilot-inline-completion");

        let response: Value = tokio::select! {
            response = self.request("thread/start", params) => {
                response.context("codex thread/start failed")?
            }
            _ = cancellation.cancelled() => return Ok(None),
        };
        let thread_id = response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .context("codex thread/start response did not include thread.id")?
            .to_owned();

        info!(
            %thread_id,
            model = %self.config.model,
            effort = %self.config.reasoning_effort,
            service_tier = self.config.service_tier.as_deref().unwrap_or("<default>"),
            "codex autocomplete thread started",
        );

        self.thread_id = Some(thread_id.clone());
        self.thread_document_uri = Some(document_uri.to_owned());
        self.completed_turns_in_thread = 0;
        self.cancelled_turns_in_thread = 0;
        self.last_thread_input_tokens = None;
        Ok(Some(thread_id))
    }

    async fn start_answer_thread(
        &mut self,
        document_uri: &str,
        cwd: Option<String>,
        cancellation: &crate::completion::CompletionCancellation,
    ) -> Result<Option<String>> {
        let params = self.thread_start_params(cwd, "tabpilot-inline-answer");

        let response: Value = tokio::select! {
            response = self.request("thread/start", params) => {
                response.context("codex answer thread/start failed")?
            }
            _ = cancellation.cancelled() => return Ok(None),
        };
        let thread_id = response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .context("codex answer thread/start response did not include thread.id")?
            .to_owned();

        info!(
            %thread_id,
            model = %self.config.model,
            effort = %self.config.reasoning_effort,
            service_tier = self.config.service_tier.as_deref().unwrap_or("<default>"),
            "codex answer thread started",
        );
        write_codex_debug_event(
            &self.config,
            json!({
                "event": "answer_thread_started",
                "thread_id": thread_id,
                "document_uri": document_uri,
            }),
        );

        Ok(Some(thread_id))
    }

    fn thread_start_params(&self, cwd: Option<String>, thread_source: &'static str) -> Value {
        let mut params = json!({
            "model": self.config.model,
            "approvalPolicy": "never",
            "sandbox": "read-only",
            "config": {
                "model_reasoning_effort": self.config.reasoning_effort
            },
            "ephemeral": true,
            "threadSource": thread_source
        });

        if let Some(cwd) = cwd {
            params["cwd"] = Value::String(cwd);
        }
        if let Some(service_tier) = &self.config.service_tier {
            params["serviceTier"] = Value::String(service_tier.clone());
            params["config"]["service_tier"] = Value::String(service_tier.clone());
        }

        params
    }

    async fn start_turn(
        &mut self,
        completion_id: u64,
        thread_id: &str,
        cwd: Option<String>,
        prompt: &str,
        turn_kind: CodexTurnKind,
        cancellation: crate::completion::CompletionCancellation,
    ) -> Result<Option<String>> {
        if cancellation.is_cancelled() {
            return Ok(None);
        }

        let request_id = self.next_id();
        let mut params = json!({
            "threadId": thread_id,
            "input": [
                {
                    "type": "text",
                    "text": prompt,
                    "text_elements": []
                }
            ],
            "model": self.config.model,
            "effort": self.config.reasoning_effort,
            "approvalPolicy": "never"
        });

        if turn_kind.counts_toward_completion_thread() {
            params["outputSchema"] = completion_output_schema();
        }

        if let Some(cwd) = &cwd {
            params["cwd"] = Value::String(cwd.clone());
        }
        if let Some(service_tier) = &self.config.service_tier {
            params["serviceTier"] = Value::String(service_tier.clone());
        }

        write_codex_debug_event(
            &self.config,
            json!({
                "event": "turn_start_request",
                "completion_id": completion_id,
                "thread_id": thread_id,
                "request_id": request_id,
                "cwd": cwd,
                "prompt_chars": prompt.len(),
                "turn_kind": turn_kind.as_str(),
                "prompt_variant": turn_kind
                    .completion_prompt_variant()
                    .map(|prompt_variant| format!("{prompt_variant:?}")),
                "model": self.config.model,
                "reasoning_effort": self.config.reasoning_effort,
                "service_tier": self.config.service_tier,
                "prompt": prompt,
            }),
        );
        info!(
            %thread_id,
            completion_id,
            request_id,
            prompt_chars = prompt.len(),
            turn_kind = turn_kind.as_str(),
            prompt_variant = ?turn_kind.completion_prompt_variant(),
            model = %self.config.model,
            effort = %self.config.reasoning_effort,
            service_tier = self.config.service_tier.as_deref().unwrap_or("<default>"),
            "codex turn/start sending",
        );
        self.write_json(&ClientMessage::request(request_id, "turn/start", params))
            .await?;
        info!(%thread_id, request_id, "codex turn/start sent");

        let mut turn_id = None;
        let mut response_received = false;
        let mut agent_message = String::new();
        let mut completed_agent_message = None;
        let turn_timeout = self.config.turn_timeout;

        timeout(turn_timeout, async {
            loop {
                let message = tokio::select! {
                    message = self.read_json() => message?,
                    _ = cancellation.cancelled() => {
                        info!(
                            %thread_id,
                            completion_id,
                            request_id,
                            turn_id = turn_id.as_deref().unwrap_or("<unknown>"),
                            "codex turn wait cancelled",
                        );
                        write_codex_debug_event(
                            &self.config,
                            json!({
                                "event": "turn_cancelled",
                                "completion_id": completion_id,
                                "thread_id": thread_id,
                                "request_id": request_id,
                                "turn_id": turn_id,
                            }),
                        );
                        return Ok(None);
                    }
                };

                if message.id == Some(request_id) {
                    response_received = true;
                    if let Some(error) = message.error {
                        bail!(
                            "codex turn/start failed: {}",
                            error_message(&error).unwrap_or_else(|| error.to_string())
                        );
                    }
                    if let Some(result) = message.result {
                        if let Some(id) = result.pointer("/turn/id").and_then(Value::as_str) {
                            turn_id = Some(id.to_owned());
                            self.active_turn = Some(ActiveCodexTurn {
                                completion_id,
                                thread_id: thread_id.to_owned(),
                                turn_id: id.to_owned(),
                                counts_toward_completion_thread: turn_kind
                                    .counts_toward_completion_thread(),
                            });
                            info!(
                                %thread_id,
                                completion_id,
                                request_id,
                                turn_id = %id,
                                "codex turn/start accepted",
                            );
                            write_codex_debug_event(
                                &self.config,
                                json!({
                                    "event": "turn_start_accepted",
                                    "completion_id": completion_id,
                                    "thread_id": thread_id,
                                    "request_id": request_id,
                                    "turn_id": id,
                                }),
                            );
                        }
                    }
                    continue;
                }

                match message.method.as_deref() {
                    Some("turn/started") => {}
                    Some("thread/tokenUsage/updated") => {
                        let matches_thread = same_thread(&message.params, thread_id);
                        let last_input_tokens = message
                            .params
                            .pointer("/tokenUsage/last/inputTokens")
                            .and_then(Value::as_u64);
                        let last_cached_input_tokens = message
                            .params
                            .pointer("/tokenUsage/last/cachedInputTokens")
                            .and_then(Value::as_u64);
                        let total_input_tokens = message
                            .params
                            .pointer("/tokenUsage/total/inputTokens")
                            .and_then(Value::as_u64);

                        if matches_thread && turn_kind.counts_toward_completion_thread() {
                            self.last_thread_input_tokens =
                                last_input_tokens.or(self.last_thread_input_tokens);
                        }

                        write_codex_debug_event(
                            &self.config,
                            json!({
                                "event": "thread_token_usage_updated",
                                "completion_id": completion_id,
                                "thread_id": thread_id,
                                "request_id": request_id,
                                "active_turn_id": turn_id,
                                "message_turn_id": notification_turn_id(&message.params),
                                "matches_thread": matches_thread,
                                "last_input_tokens": last_input_tokens,
                                "last_cached_input_tokens": last_cached_input_tokens,
                                "total_input_tokens": total_input_tokens,
                                "raw_params": message.params,
                            }),
                        );
                    }
                    Some("item/agentMessage/delta") => {
                        let matches_thread = same_thread(&message.params, thread_id);
                        let matches_turn = same_known_turn(&message.params, turn_id.as_deref());
                        if let Some(delta) = message.params.get("delta").and_then(Value::as_str) {
                            write_codex_debug_event(
                                &self.config,
                                json!({
                                    "event": "proposal_delta",
                                    "completion_id": completion_id,
                                    "thread_id": thread_id,
                                    "request_id": request_id,
                                    "active_turn_id": turn_id,
                                    "message_turn_id": notification_turn_id(&message.params),
                                    "matches_thread": matches_thread,
                                    "matches_turn": matches_turn,
                                    "accepted": matches_thread && matches_turn,
                                    "delta": delta,
                                }),
                            );
                        }

                        if matches_thread && matches_turn {
                            if let Some(delta) = message.params.get("delta").and_then(Value::as_str)
                            {
                                agent_message.push_str(delta);
                            }
                        }
                    }
                    Some("item/completed") => {
                        let matches_thread = same_thread(&message.params, thread_id);
                        let matches_turn = same_known_turn(&message.params, turn_id.as_deref());
                        let completed_response = item_completed_agent_message(&message.params);

                        write_codex_debug_event(
                            &self.config,
                            json!({
                                "event": "agent_message_completed",
                                "completion_id": completion_id,
                                "thread_id": thread_id,
                                "request_id": request_id,
                                "active_turn_id": turn_id,
                                "message_turn_id": notification_turn_id(&message.params),
                                "matches_thread": matches_thread,
                                "matches_turn": matches_turn,
                                "accepted": matches_thread && matches_turn,
                                "response_chars": completed_response.as_ref().map_or(0, String::len),
                                "response": completed_response,
                            }),
                        );

                        if matches_thread && matches_turn {
                            completed_agent_message = item_completed_agent_message(&message.params);
                        }
                    }
                    Some("turn/completed") => {
                        let matches_thread = same_thread(&message.params, thread_id);
                        let matches_turn = same_known_turn(&message.params, turn_id.as_deref());
                        let completed_response = completed_turn_agent_message(&message.params)
                            .or_else(|| completed_agent_message.clone())
                            .unwrap_or_default();

                        write_codex_debug_event(
                            &self.config,
                            json!({
                                "event": "turn_completed_seen",
                                "completion_id": completion_id,
                                "thread_id": thread_id,
                                "request_id": request_id,
                                "active_turn_id": turn_id,
                                "message_turn_id": notification_turn_id(&message.params),
                                "matches_thread": matches_thread,
                                "matches_turn": matches_turn,
                                "accepted": matches_thread && matches_turn,
                                "response_chars": completed_response.len(),
                                "response": completed_response.clone(),
                                "raw_params": message.params.clone(),
                            }),
                        );

                        if matches_thread && matches_turn {
                            if let Some(error) = completed_turn_error(&message.params) {
                                bail!("codex turn failed: {error}");
                            }

                            if agent_message.trim().is_empty() {
                                agent_message = completed_response;
                            }
                            if agent_message.trim().is_empty()
                                && turn_kind.counts_toward_completion_thread()
                            {
                                agent_message = r#"{"suggestions":[]}"#.to_owned();
                            }

                            if !response_received {
                                warn!("codex turn completed before turn/start response");
                            }

                            if turn_kind.counts_toward_completion_thread() {
                                self.completed_turns_in_thread += 1;
                                self.cancelled_turns_in_thread = 0;
                            }

                            info!(
                                %thread_id,
                                completion_id,
                                request_id,
                                turn_id = turn_id.as_deref().unwrap_or("<unknown>"),
                                response_chars = agent_message.len(),
                                "codex turn completed",
                            );
                            write_codex_debug_event(
                                &self.config,
                                json!({
                                    "event": "turn_completed",
                                    "completion_id": completion_id,
                                    "thread_id": thread_id,
                                    "request_id": request_id,
                                    "turn_id": turn_id,
                                    "response_chars": agent_message.len(),
                                    "response": agent_message,
                                }),
                            );
                            self.clear_active_turn(completion_id, turn_id.as_deref());
                            return Ok(Some(agent_message));
                        }
                    }
                    Some("error") => {
                        if let Some(message) = message.params.get("message").and_then(Value::as_str)
                        {
                            bail!("codex app-server error notification: {message}");
                        }
                    }
                    Some(method) => {
                        write_codex_debug_event(
                            &self.config,
                            json!({
                                "event": "app_server_notification_ignored",
                                "completion_id": completion_id,
                                "thread_id": thread_id,
                                "request_id": request_id,
                                "active_turn_id": turn_id,
                                "method": method,
                                "raw_params": message.params,
                            }),
                        );
                        debug!(%method, "ignoring codex app-server notification");
                    }
                    None => {}
                }
            }
        })
        .await
        .with_context(|| format!("codex turn timed out after {turn_timeout:?}"))?
    }

    async fn request<T>(&mut self, method: &str, params: Value) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let request_id = self.next_id();
        self.write_json(&ClientMessage::request(request_id, method, params))
            .await?;

        loop {
            let message = self.read_json().await?;
            if message.id != Some(request_id) {
                if let Some(method) = &message.method {
                    debug!(%method, "ignoring codex app-server notification while waiting for response");
                }
                continue;
            }

            if let Some(error) = message.error {
                bail!(
                    "codex app-server request {method} failed: {}",
                    error_message(&error).unwrap_or_else(|| error.to_string())
                );
            }

            let result = message
                .result
                .context("codex app-server response had neither result nor error")?;
            return serde_json::from_value(result).context("failed to parse codex response result");
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_json(&ClientMessage::notification(method, params))
            .await
    }

    async fn write_json<T>(&mut self, message: &T) -> Result<()>
    where
        T: Serialize,
    {
        let bytes = serde_json::to_vec(message).context("failed to serialize codex message")?;
        self.stdin
            .write_all(&bytes)
            .await
            .context("failed to write codex message")?;
        self.stdin
            .write_all(b"\n")
            .await
            .context("failed to write codex message newline")?;
        self.stdin
            .flush()
            .await
            .context("failed to flush codex message")?;
        Ok(())
    }

    async fn read_json(&mut self) -> Result<ServerMessage> {
        let line = self
            .stdout
            .next_line()
            .await
            .context("failed to read codex app-server stdout")?
            .context("codex app-server closed stdout")?;

        serde_json::from_str(&line).with_context(|| format!("invalid codex json message: {line}"))
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    pub(super) async fn interrupt_completion(&mut self, completion_id: u64) -> Result<()> {
        let Some(active_turn) = self.active_turn.clone() else {
            write_codex_debug_event(
                &self.config,
                json!({
                    "event": "cancel_skipped_no_active_turn",
                    "completion_id": completion_id,
                }),
            );
            return Ok(());
        };

        if active_turn.completion_id != completion_id {
            write_codex_debug_event(
                &self.config,
                json!({
                    "event": "cancel_skipped_completion_mismatch",
                    "completion_id": completion_id,
                    "active_completion_id": active_turn.completion_id,
                    "thread_id": active_turn.thread_id,
                    "turn_id": active_turn.turn_id,
                }),
            );
            return Ok(());
        }

        info!(
            thread_id = %active_turn.thread_id,
            turn_id = %active_turn.turn_id,
            completion_id,
            "interrupting codex turn",
        );
        write_codex_debug_event(
            &self.config,
            json!({
                "event": "turn_interrupt_request",
                "completion_id": completion_id,
                "thread_id": active_turn.thread_id,
                "turn_id": active_turn.turn_id,
            }),
        );

        let _: Value = self
            .request(
                "turn/interrupt",
                json!({
                    "threadId": active_turn.thread_id,
                    "turnId": active_turn.turn_id,
                }),
            )
            .await
            .context("codex turn/interrupt failed")?;

        if active_turn.counts_toward_completion_thread {
            self.cancelled_turns_in_thread += 1;
        }
        self.clear_active_turn(completion_id, Some(&active_turn.turn_id));
        write_codex_debug_event(
            &self.config,
            json!({
                "event": "turn_interrupt_completed",
                "completion_id": completion_id,
                "thread_id": active_turn.thread_id,
                "turn_id": active_turn.turn_id,
            }),
        );
        Ok(())
    }

    fn thread_rotation_reason(&self, document_uri: &str) -> Option<&'static str> {
        self.thread_id.as_ref()?;

        if self.active_turn.is_some() {
            return None;
        }

        if self
            .thread_document_uri
            .as_deref()
            .is_some_and(|thread_document_uri| thread_document_uri != document_uri)
        {
            return Some("document_changed");
        }

        if self.completed_turns_in_thread >= MAX_COMPLETED_TURNS_PER_THREAD {
            return Some("completed_turn_budget_exhausted");
        }

        if self.cancelled_turns_in_thread >= MAX_CANCELLED_TURNS_PER_THREAD {
            return Some("cancelled_turn_budget_exhausted");
        }

        if self
            .last_thread_input_tokens
            .is_some_and(|input_tokens| input_tokens > MAX_THREAD_INPUT_TOKENS)
        {
            return Some("input_token_budget_exhausted");
        }

        None
    }

    fn rotate_thread(&mut self, reason: &'static str) {
        let previous_thread_id = self.thread_id.take();
        let previous_document_uri = self.thread_document_uri.take();

        info!(
            reason,
            previous_thread_id = previous_thread_id.as_deref().unwrap_or("<none>"),
            completed_turns_in_thread = self.completed_turns_in_thread,
            cancelled_turns_in_thread = self.cancelled_turns_in_thread,
            last_input_tokens = self.last_thread_input_tokens,
            "rotating codex autocomplete thread",
        );
        write_codex_debug_event(
            &self.config,
            json!({
                "event": "thread_rotated",
                "reason": reason,
                "previous_thread_id": previous_thread_id,
                "previous_document_uri": previous_document_uri,
                "completed_turns_in_thread": self.completed_turns_in_thread,
                "cancelled_turns_in_thread": self.cancelled_turns_in_thread,
                "last_input_tokens": self.last_thread_input_tokens,
            }),
        );

        self.completed_turns_in_thread = 0;
        self.cancelled_turns_in_thread = 0;
        self.last_thread_input_tokens = None;
        self.active_turn = None;
    }

    fn clear_active_turn(&mut self, completion_id: u64, turn_id: Option<&str>) {
        let should_clear = self.active_turn.as_ref().is_some_and(|active_turn| {
            active_turn.completion_id == completion_id
                && turn_id.is_none_or(|turn_id| active_turn.turn_id == turn_id)
        });

        if should_clear {
            self.active_turn = None;
        }
    }
}
