mod api;
mod config;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell, OwnedMutexGuard};
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::answer::AnswerRequest;
use crate::completion::{CompletionCancellation, CompletionCandidate, CompletionRequest};
use crate::providers::CompletionProvider;
use crate::providers::shared::{
    CompletionPromptVariant, answer_context_to_text, answer_to_candidate, completion_output_schema,
    context_to_text, decorate_answer_prompt, decorate_completion_prompt, suggestions_to_candidates,
    workspace_cwd,
};
use crate::workspace::WorkspaceContext;

use self::api::CommonthreadApi;
use self::config::{CommonthreadProviderConfig, MAX_SESSION_PROMPT_CHARS, default_agent};

pub use self::api::shutdown;

const COMPLETION_INSTRUCTIONS: &str = "You serve editor inline completions for this repository. \
The first message asks you to learn the repository with a few quick read-only lookups. Every \
later message is a completion request: answer it immediately from that message and what you \
already learned, without using any tools. Never edit files. Judge each request on its own: \
earlier answers, including empty ones, say nothing about the next. Suggest text whenever the \
file could plausibly continue at the cursor.";
const QUESTION_INSTRUCTIONS: &str = "You answer questions asked from an editor in this \
repository. Each message carries one question with the editor context around it; earlier \
questions and answers in this conversation can give it context. Answer immediately, without \
using any tools. Never edit files.";
const WARM_UP: &str = "Get familiar with this repository so later completions match its code: \
look at its layout, README, and main modules with a few quick read-only lookups (list, search, \
read). This message is not a completion request. Reply with one suggestion whose insert_text \
is a one-line summary of the repository.";
const WARM_UP_TIMEOUT: Duration = Duration::from_secs(180);
/// How long an interrupted turn may take to finish before its session is abandoned.
const INTERRUPT_GRACE: Duration = Duration::from_secs(10);

/// Completions through a Commonthread agent server. Turns reuse one native
/// session per document, rotated like the Codex provider's threads, so the
/// harness stays warm; superseded turns are interrupted by their exact turn ID.
/// A spare session explores the repository in the background, so completions
/// use a session that already knows the code: the first spare warms when the
/// editor opens, the next once the current session is halfway through its budget.
pub struct CommonthreadProvider {
    client: std::result::Result<Arc<Client>, String>,
}

impl Default for CommonthreadProvider {
    fn default() -> Self {
        let client = CommonthreadProviderConfig::from_env()
            .and_then(Client::new)
            .map(Arc::new)
            .map_err(|err| format!("{err:#}"));
        if let Err(err) = &client {
            warn!(error = %err, "commonthread provider is not configured");
        }
        Self { client }
    }
}

struct Client {
    config: CommonthreadProviderConfig,
    api: CommonthreadApi,
    profile: OnceCell<Profile>,
    completion: Arc<Mutex<Option<Session>>>,
    /// The session this editor's questions share, so follow-ups see earlier answers.
    questions: Arc<Mutex<Option<Session>>>,
    spare: std::sync::Mutex<Option<Spare>>,
    spent: Arc<std::sync::Mutex<Spent>>,
}

/// What this editor's turns cost, from the spending each finished turn reports.
#[derive(Debug, Default)]
struct Spent {
    turns: u32,
    tokens: u64,
    cost_usd: f64,
    /// Some usage had no price, so `cost_usd` is a lower bound.
    unpriced: bool,
}

impl Spent {
    fn add(&mut self, turn: &Value) {
        let spending = &turn["result"]["spending"];
        let count = |kind: &str| spending["tokens"][kind].as_u64().unwrap_or(0);
        self.turns += 1;
        self.tokens += ["input", "cache_read", "cache_write", "output"]
            .into_iter()
            .map(count)
            .sum::<u64>();
        self.cost_usd += spending["cost_usd"].as_f64().unwrap_or(0.0);
        self.unpriced |= spending["unpriced"] == true;
    }

    fn label(&self) -> String {
        let tokens = match self.tokens {
            n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
            n if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3),
            n => n.to_string(),
        };
        format!(
            "{}${:.4} · {tokens} tokens · {} turns",
            if self.unpriced { "≥ " } else { "" },
            self.cost_usd,
            self.turns
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Profile {
    id: String,
    name: String,
    harness: String,
}

/// A session warming up for a workspace; the next completion without a session takes it.
struct Spare {
    cwd: String,
    task: tokio::task::JoinHandle<Result<Session>>,
}

#[derive(Debug)]
struct Session {
    id: String,
    cwd: String,
    /// Last journal event consumed, so each turn streams only new events.
    cursor: i64,
    completed: u32,
    prompt_chars: usize,
}

impl Session {
    fn rotation_reason(&self, cwd: &str) -> Option<&'static str> {
        if self.cwd != cwd {
            Some("workspace_changed")
        } else if self.prompt_chars >= MAX_SESSION_PROMPT_CHARS {
            Some("prompt_limit")
        } else {
            None
        }
    }
}

#[async_trait]
impl CompletionProvider for CommonthreadProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionCandidate>> {
        let client = self.client()?;
        let mut slot = tokio::select! {
            slot = client.completion.clone().lock_owned() => slot,
            _ = request.cancellation.cancelled() => return Ok(Vec::new()),
        };
        let cwd = workspace_cwd(&request.workspace).context(NO_WORKSPACE)?;
        client.rotate(&mut slot, &cwd);
        if slot.is_none() {
            match client
                .completion_session(&cwd, &request.cancellation)
                .await?
            {
                Some(session) => *slot = Some(session),
                None => return Ok(Vec::new()),
            }
        }
        // Warm the replacement once this session is halfway through its budget.
        if slot
            .as_ref()
            .is_some_and(|s| s.prompt_chars >= MAX_SESSION_PROMPT_CHARS / 2)
        {
            client.warm_spare(&cwd);
        }
        let variant = match slot.as_ref().is_some_and(|s| s.completed > 0) {
            true => CompletionPromptVariant::ContinuationReminder,
            false => CompletionPromptVariant::FullContract,
        };
        let prompt = decorate_completion_prompt(&context_to_text(&request), variant);
        let Some(result) = client
            .run_turn(
                slot,
                &prompt,
                &request.cancellation,
                client.config.turn_timeout,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        // Structured output is authoritative; text is the fallback recorded answer.
        let response = match result.get("structured_output") {
            Some(output) if !output.is_null() => output.to_string(),
            _ => result["text"].as_str().unwrap_or_default().to_owned(),
        };
        let conversion = suggestions_to_candidates(&request, &response)?;
        info!(
            prompt_chars = prompt.len(),
            candidate_count = conversion.candidates.len(),
            rejection_count = conversion.rejections.len(),
            "commonthread provider completed",
        );
        Ok(conversion.candidates)
    }

    async fn answer(&self, request: AnswerRequest) -> Result<Vec<CompletionCandidate>> {
        let client = self.client()?;
        let mut slot = tokio::select! {
            slot = client.questions.clone().lock_owned() => slot,
            _ = request.cancellation.cancelled() => return Ok(Vec::new()),
        };
        let cwd = workspace_cwd(&request.workspace).context(NO_WORKSPACE)?;
        client.rotate(&mut slot, &cwd);
        if slot.is_none() {
            *slot = Some(client.create(&cwd, QUESTION_INSTRUCTIONS, None).await?);
        }
        let prompt = decorate_answer_prompt(&answer_context_to_text(&request), &request.question);
        let timeout = client.config.turn_timeout;
        let Some(result) = client
            .run_turn(slot, &prompt, &request.cancellation, timeout)
            .await?
        else {
            return Ok(Vec::new());
        };
        let response = result["text"].as_str().unwrap_or_default();
        Ok(answer_to_candidate(&request, response))
    }

    fn prepare(&self, workspace: &WorkspaceContext) {
        if let (Ok(client), Some(cwd)) = (&self.client, workspace_cwd(workspace)) {
            client.warm_spare(&cwd);
        }
    }

    async fn shutdown(&self) {
        if let Ok(client) = &self.client {
            client.close_sessions().await;
        }
    }

    fn status(&self) -> Value {
        let client = match &self.client {
            Ok(client) => client,
            Err(err) => return json!({ "error": err }),
        };
        let profile = client.profile.get();
        let (model, effort) = match profile {
            Some(profile) => client.agent(profile),
            None => (client.config.model.clone(), client.config.effort.clone()),
        };
        let server = client.api.server().map(|server| match server.built_in {
            true => format!("built-in ({})", server.endpoint.url),
            false => server.endpoint.url.clone(),
        });
        json!({
            "server": server.unwrap_or_else(|| "not started yet".into()),
            "harness": profile.map_or(&client.config.harness, |p| &p.harness),
            "profile": profile.map(|p| format!("{} ({})", p.name, p.id)),
            "model": model,
            "effort": effort,
            "spending": client.spent.lock().expect("spent lock").label(),
        })
    }
}

const NO_WORKSPACE: &str = "the commonthread provider needs an editor workspace folder";

impl CommonthreadProvider {
    fn client(&self) -> Result<&Arc<Client>> {
        self.client.as_ref().map_err(|err| anyhow!("{err}"))
    }
}

impl Client {
    fn new(config: CommonthreadProviderConfig) -> Result<Self> {
        Ok(Self {
            api: CommonthreadApi::new(&config)?,
            config,
            profile: OnceCell::new(),
            completion: Arc::new(Mutex::new(None)),
            questions: Arc::new(Mutex::new(None)),
            spare: std::sync::Mutex::new(None),
            spent: Arc::default(),
        })
    }

    /// The spare for `cwd`, waiting for it to finish warming, else a cold
    /// session. `None` means the request was cancelled while waiting; the
    /// spare keeps warming for the next one.
    async fn completion_session(
        self: &Arc<Self>,
        cwd: &str,
        cancellation: &CompletionCancellation,
    ) -> Result<Option<Session>> {
        let spare = self
            .spare
            .lock()
            .expect("spare lock")
            .take_if(|s| s.cwd == cwd);
        if let Some(mut spare) = spare {
            tokio::select! {
                warmed = &mut spare.task => match warmed {
                    Ok(Ok(session)) => {
                        info!(session = session.id, "using warmed commonthread session");
                        return Ok(Some(session));
                    }
                    Ok(Err(err)) => warn!(error = %format!("{err:#}"), "commonthread warm-up failed"),
                    Err(err) => warn!(%err, "commonthread warm-up task failed"),
                },
                _ = cancellation.cancelled() => {
                    self.spare.lock().expect("spare lock").get_or_insert(spare);
                    return Ok(None);
                }
            }
        }
        self.create(
            cwd,
            COMPLETION_INSTRUCTIONS,
            Some(completion_output_schema()),
        )
        .await
        .map(Some)
    }

    /// Close this editor's completion and question sessions so their workers stop
    /// now rather than at the server's idle timeout. A session busy with a turn or
    /// still warming is left to that timeout.
    async fn close_sessions(&self) {
        let spare = self.spare.lock().expect("spare lock").take();
        let warmed = match spare {
            Some(spare) if spare.task.is_finished() => spare.task.await.ok().and_then(Result::ok),
            Some(spare) => {
                spare.task.abort();
                None
            }
            None => None,
        };
        let idle = |slot: &Mutex<Option<Session>>| slot.try_lock().ok().and_then(|mut s| s.take());
        let current = [idle(&self.completion), idle(&self.questions)];
        for session in warmed.into_iter().chain(current.into_iter().flatten()) {
            self.api.close_now(&session.id).await;
            info!(
                session = session.id,
                "closed commonthread session on shutdown"
            );
        }
    }

    /// Close the session in `slot` if it should not take another turn in `cwd`.
    fn rotate(&self, slot: &mut Option<Session>, cwd: &str) {
        if let Some(reason) = slot.as_ref().and_then(|s| s.rotation_reason(cwd)) {
            let old = slot.take().expect("checked above");
            info!(session = old.id, reason, "rotating commonthread session");
            self.api.close(old.id);
        }
    }

    fn warm_spare(self: &Arc<Self>, cwd: &str) {
        let mut spare = self.spare.lock().expect("spare lock");
        if spare.as_ref().is_some_and(|s| s.cwd == cwd) {
            return;
        }
        let (client, task_cwd) = (Arc::clone(self), cwd.to_owned());
        let task = tokio::spawn(async move { client.warm_up(&task_cwd).await });
        *spare = Some(Spare {
            cwd: cwd.to_owned(),
            task,
        });
    }

    /// Create a completion session and let it read the repository once.
    async fn warm_up(&self, cwd: &str) -> Result<Session> {
        let session = self
            .create(
                cwd,
                COMPLETION_INSTRUCTIONS,
                Some(completion_output_schema()),
            )
            .await?;
        let id = session.id.clone();
        let slot = Arc::new(Mutex::new(Some(session)));
        let guard = Arc::clone(&slot).lock_owned().await;
        let never = CompletionCancellation::new();
        let result = self.run_turn(guard, WARM_UP, &never, WARM_UP_TIMEOUT).await;
        // The slot stays locked only if the turn is still winding down.
        let session = slot.try_lock().ok().and_then(|mut slot| slot.take());
        match (result, session) {
            (Ok(_), Some(mut session)) => {
                info!(session = id, "warmed commonthread session");
                (session.completed, session.prompt_chars) = (0, 0);
                Ok(session)
            }
            (result, _) => {
                self.api.close(id);
                Err(result
                    .err()
                    .unwrap_or_else(|| anyhow!("warm-up did not finish")))
            }
        }
    }

    async fn profile(&self) -> Result<&Profile> {
        self.profile
            .get_or_try_init(|| async {
                let listed = self.api.get("/v1/profiles").await?;
                pick_profile(
                    &listed,
                    self.config.profile.as_deref(),
                    &self.config.harness,
                )
            })
            .await
    }

    /// The configured model and effort, or the profile harness's fast defaults.
    fn agent(&self, profile: &Profile) -> (Option<String>, Option<String>) {
        let (model, effort) = default_agent(&profile.harness);
        (
            self.config.model.clone().or(model.map(Into::into)),
            self.config.effort.clone().or(effort.map(Into::into)),
        )
    }

    async fn create(
        &self,
        cwd: &str,
        instructions: &str,
        output_schema: Option<Value>,
    ) -> Result<Session> {
        let profile = self.profile().await?;
        let (model, effort) = self.agent(profile);
        let body = json!({
            "profile_id": profile.id,
            "workspace": {"path": cwd, "mode": "shared"},
            "agent": {
                "model": model,
                "effort": effort,
                "instructions": instructions,
                "output_schema": output_schema,
            },
        });
        let session = self.api.post("/v1/sessions", Some(body)).await?;
        let id = session["id"]
            .as_str()
            .context("Commonthread session has no ID")?;
        info!(session = id, "created commonthread session");
        Ok(Session {
            id: id.to_owned(),
            cwd: cwd.to_owned(),
            cursor: 0,
            completed: 0,
            prompt_chars: 0,
        })
    }

    /// Submit one turn and wait for its outcome. `None` means cancelled or
    /// interrupted. A session that fails is dropped from `slot` so the next
    /// request starts fresh; an interrupted one keeps the slot locked until the
    /// native turn has actually finished.
    async fn run_turn(
        &self,
        mut slot: OwnedMutexGuard<Option<Session>>,
        text: &str,
        cancellation: &CompletionCancellation,
        limit: Duration,
    ) -> Result<Option<Value>> {
        let session = slot.as_mut().context("no commonthread session")?;
        let id = session.id.clone();
        let opened = self.api.events(&id, session.cursor).await;
        let mut events = match opened {
            Ok(events) => Box::pin(events),
            Err(err) => {
                slot.take();
                self.api.close(id);
                return Err(err);
            }
        };
        let path = format!("/v1/sessions/{id}/turns");
        let turn = match self.api.post(&path, Some(json!({"text": text}))).await {
            Ok(turn) => turn,
            Err(err) => {
                slot.take();
                self.api.close(id);
                return Err(err);
            }
        };
        let turn_id = turn["id"].as_str().unwrap_or_default().to_owned();
        slot.as_mut().expect("held by this turn").prompt_chars += text.len();
        let deadline = sleep(limit);
        tokio::pin!(deadline);
        let stopped = loop {
            let event = tokio::select! {
                event = events.next() => event,
                _ = cancellation.cancelled() => break None,
                _ = &mut deadline => break Some(anyhow!("commonthread turn timed out after {limit:?}")),
            };
            let event = match event {
                Some(Ok(event)) => event,
                Some(Err(err)) => break Some(err),
                None => break Some(anyhow!("commonthread event stream ended")),
            };
            let session = slot.as_mut().expect("held by this turn");
            session.cursor = event.id;
            let for_turn = |field: &str| event.data[field].as_str() == Some(&turn_id);
            match event.kind.as_str() {
                "turn.finished" if for_turn("id") => {
                    self.spent.lock().expect("spent lock").add(&event.data);
                    let result = finish(&mut slot, &event.data);
                    if slot.is_none() {
                        self.api.close(id);
                    }
                    return result;
                }
                // Turns never wait on approvals; a request means the turn went off track.
                "interaction.requested" if for_turn("turn_id") => {
                    self.interrupt(&id, &turn_id).await;
                }
                _ => {}
            }
        };
        self.interrupt(&id, &turn_id).await;
        let (spent, api) = (Arc::clone(&self.spent), self.api.clone());
        tokio::spawn(async move {
            let finished = timeout(INTERRUPT_GRACE, async {
                while let Some(Ok(event)) = events.next().await {
                    if let Some(session) = slot.as_mut() {
                        session.cursor = event.id;
                    }
                    if event.kind == "turn.finished" && event.data["id"] == turn_id.as_str() {
                        spent.lock().expect("spent lock").add(&event.data);
                        return true;
                    }
                }
                false
            })
            .await;
            if !matches!(finished, Ok(true)) && slot.take().is_some() {
                api.close(id);
            }
        });
        match stopped {
            None => Ok(None),
            Some(err) => Err(err),
        }
    }

    async fn interrupt(&self, session_id: &str, turn_id: &str) {
        let path = format!("/v1/sessions/{session_id}/interrupt");
        let body = json!({"turn_id": turn_id});
        if let Err(err) = self.api.post(&path, Some(body)).await {
            info!(session = session_id, error = %format!("{err:#}"), "interrupt skipped");
        }
    }
}

/// The configured profile, else the harness's only profile or its default one.
fn pick_profile(listed: &Value, configured: Option<&str>, harness: &str) -> Result<Profile> {
    let profiles: Vec<Profile> = listed["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["capabilities"]["structured_output"] == true)
        .filter_map(|p| {
            Some(Profile {
                id: p["id"].as_str()?.to_owned(),
                name: p["name"].as_str().unwrap_or_default().to_owned(),
                harness: p["harness"].as_str()?.to_owned(),
            })
        })
        .collect();
    let choices = || {
        let names: Vec<_> = profiles
            .iter()
            .map(|p| format!("{} ({})", p.id, p.name))
            .collect();
        names.join(", ")
    };
    if let Some(id) = configured {
        return profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .with_context(|| {
                format!(
                    "profile {id} is not available; choose one of: {}",
                    choices()
                )
            });
    }
    let matching: Vec<_> = profiles.iter().filter(|p| p.harness == harness).collect();
    let default_name = format!("{harness} default");
    match matching.as_slice() {
        [only] => Ok((*only).clone()),
        many => many
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&default_name))
            .map(|p| (*p).clone())
            .with_context(|| {
                format!(
                    "no default {harness} profile; set TABPILOT_COMMONTHREAD_PROFILE to one of: {}",
                    choices()
                )
            }),
    }
}

fn finish(slot: &mut Option<Session>, turn: &Value) -> Result<Option<Value>> {
    let session = slot.as_mut().expect("held by this turn");
    match turn["status"].as_str() {
        Some("completed") => {
            session.completed += 1;
            Ok(Some(turn["result"].clone()))
        }
        Some("cancelled") => Ok(None),
        status => {
            slot.take();
            bail!(
                "commonthread turn {}: {}",
                status.unwrap_or("ended"),
                turn["result"]["error"]
                    .as_str()
                    .unwrap_or("no error reported")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(prompt_chars: usize) -> Session {
        Session {
            id: "s".into(),
            cwd: "/repo".into(),
            cursor: 0,
            completed: 0,
            prompt_chars,
        }
    }

    #[test]
    fn sessions_rotate_on_workspace_change_and_prompt_budget() {
        // Switching files or interrupting turns keeps the session.
        assert_eq!(
            session(MAX_SESSION_PROMPT_CHARS - 1).rotation_reason("/repo"),
            None
        );
        assert_eq!(
            session(0).rotation_reason("/other"),
            Some("workspace_changed")
        );
        assert_eq!(
            session(MAX_SESSION_PROMPT_CHARS).rotation_reason("/repo"),
            Some("prompt_limit")
        );
    }

    #[test]
    fn profiles_resolve_by_id_or_harness_default() {
        let profile = |id: &str, name: &str, harness: &str| {
            json!({"id": id, "name": name, "harness": harness,
                   "capabilities": {"structured_output": true}})
        };
        let listed = json!({"profiles": [
            profile("c1", "Claude default", "claude"),
            profile("c2", "Claude work", "claude"),
            profile("x1", "Codex default", "codex"),
            {"id": "old", "name": "Old", "harness": "codex", "capabilities": {}},
        ]});
        let pick = |configured, harness| pick_profile(&listed, configured, harness).map(|p| p.id);
        assert_eq!(pick(None, "claude").unwrap(), "c1");
        assert_eq!(pick(None, "codex").unwrap(), "x1");
        assert_eq!(pick(Some("c2"), "codex").unwrap(), "c2");
        let err = pick(Some("old"), "claude").unwrap_err().to_string();
        assert!(
            err.contains("not available") && err.contains("c2 (Claude work)"),
            "{err}"
        );
        let one = json!({"profiles": [profile("w", "Claude work", "claude")]});
        assert_eq!(pick_profile(&one, None, "claude").unwrap().id, "w");
        assert!(pick_profile(&one, None, "codex").is_err());
    }

    #[test]
    fn spending_adds_up_finished_turns() {
        let mut spent = Spent::default();
        spent.add(&json!({"result": {"spending": {
            "tokens": {"input": 12, "cache_read": 1500, "cache_write": 300, "output": 200, "reasoning": 50},
            "cost_usd": 0.0021, "unpriced": false}}}));
        spent.add(&json!({"result": {"error": "interrupted"}}));
        assert_eq!(spent.label(), "$0.0021 · 2.0k tokens · 2 turns");
        spent.add(&json!({"result": {"spending": {"tokens": {"output": 1}, "unpriced": true}}}));
        assert_eq!(spent.label(), "≥ $0.0021 · 2.0k tokens · 3 turns");
    }

    #[test]
    fn finished_turns_map_to_results_and_drop_failed_sessions() {
        let mut slot = Some(session(0));
        let done = json!({"status":"completed","result":{"text":"x"}});
        assert_eq!(finish(&mut slot, &done).unwrap().unwrap()["text"], "x");
        let cancelled = json!({"status":"cancelled","result":{}});
        assert!(finish(&mut slot, &cancelled).unwrap().is_none());
        assert_eq!(slot.as_ref().map(|s| s.completed), Some(1));
        let failed = json!({"status":"failed","result":{"error":"boom"}});
        let err = finish(&mut slot, &failed).unwrap_err().to_string();
        assert!(err.contains("failed: boom"), "{err}");
        assert!(slot.is_none());
    }
}
