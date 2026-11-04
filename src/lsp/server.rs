use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, ErrorCode, ResponseError};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument, Exit,
    Initialized, ShowMessage,
};
use lsp_types::request::{ExecuteCommand, Initialize, InlineCompletionRequest, Shutdown};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, ExecuteCommandOptions, InitializeResult, InitializedParams,
    InlineCompletionResponse, Location, MessageType, OneOf, ServerCapabilities, ShowMessageParams,
    TextDocumentSyncCapability, TextDocumentSyncKind,
};
use serde_json::{Value, json};
use tower::ServiceBuilder;
use tracing::{error, info, warn};

use crate::completion::{CompletionCancellation, CompletionEngine};
use crate::documents::{DocumentSnapshot, DocumentStore};
use crate::lsp::inline_completion::from_lsp_position;
use crate::protocol::TextRange;
use crate::providers::CompletionProvider;
use crate::providers::claude::ClaudeProvider;
use crate::providers::codex::CodexProvider;
use crate::providers::commonthread::CommonthreadProvider;
use crate::workspace::WorkspaceContext;

/// How long typing must pause before a completion request reaches the provider.
const TYPING_PAUSE: std::time::Duration = std::time::Duration::from_millis(500);
/// Returns the provider in use and the last completion error, for the editor's status view.
const STATUS_COMMAND: &str = "tabpilot.status";
/// Answers the question at a `Location`; the reply's `text` goes at the end of its last line.
const ANSWER_COMMAND: &str = "tabpilot.answerSelection";

pub struct CompletionServer {
    documents: Arc<DocumentStore>,
    completion: CompletionEngine,
    coordinator: Arc<CompletionCoordinator>,
    workspace: WorkspaceContext,
    client: ClientSocket,
    provider_name: &'static str,
    /// Shown to the user once when it first occurs, then kept for the status view.
    last_error: Arc<StdMutex<Option<String>>>,
}

impl CompletionServer {
    fn new(client: ClientSocket) -> Self {
        let (provider_name, provider) = completion_provider_from_env();
        info!(provider = provider_name, "completion provider selected");

        Self {
            documents: Arc::new(DocumentStore::default()),
            completion: CompletionEngine::new(provider),
            coordinator: Arc::new(CompletionCoordinator::default()),
            workspace: WorkspaceContext::default(),
            client,
            provider_name,
            last_error: Arc::default(),
        }
    }

    fn router(client: ClientSocket) -> Router<Self> {
        let mut router = Router::new(Self::new(client));

        router
            .request::<Initialize, _>(|server, params| {
                server.workspace = WorkspaceContext::from_initialize_params(&params);
                let workspace = server.workspace.clone();

                async move {
                    info!(
                        root_uri = ?workspace.root_uri,
                        name = ?workspace.name,
                        git_branch = ?workspace.git.as_ref().and_then(|git| git.branch.as_deref()),
                        "workspace initialized",
                    );

                    Ok(InitializeResult {
                        capabilities: ServerCapabilities {
                            text_document_sync: Some(TextDocumentSyncCapability::Kind(
                                TextDocumentSyncKind::FULL,
                            )),
                            inline_completion_provider: Some(OneOf::Left(true)),
                            execute_command_provider: Some(ExecuteCommandOptions {
                                commands: vec![
                                    STATUS_COMMAND.to_owned(),
                                    ANSWER_COMMAND.to_owned(),
                                ],
                                ..ExecuteCommandOptions::default()
                            }),
                            ..ServerCapabilities::default()
                        },
                        server_info: None,
                        offset_encoding: None,
                    })
                }
            })
            .request::<Shutdown, _>(|server, _| {
                let completion = server.completion.clone();
                async move {
                    completion.shutdown().await;
                    Ok(())
                }
            })
            .request::<ExecuteCommand, _>(|server, params| {
                let answer = (params.command == ANSWER_COMMAND)
                    .then(|| server.answer_selection(params.arguments.into_iter().next()));
                let status = (params.command == STATUS_COMMAND).then(|| {
                    json!({
                        "provider": server.provider_name,
                        "details": server.completion.status(),
                        "last_error": *server.last_error.lock().expect("last error lock"),
                    })
                });
                async move {
                    match (status, answer) {
                        (Some(status), _) => Ok(Some(status)),
                        (_, Some(answer)) => answer.await,
                        _ => Err(ResponseError::new(
                            ErrorCode::INVALID_PARAMS,
                            "unknown command",
                        )),
                    }
                }
            })
            .request::<InlineCompletionRequest, _>(|server, params| {
                let uri = params.text_document_position.text_document.uri.to_string();
                let position = from_lsp_position(params.text_document_position.position);
                let snapshot = server.documents.snapshot(&uri);
                let documents = Arc::clone(&server.documents);
                let completion = server.completion.clone();
                let permit = server.coordinator.begin();
                let coordinator = Arc::clone(&server.coordinator);
                let workspace = server.workspace.clone();
                let client = server.client.clone();
                let last_error = Arc::clone(&server.last_error);

                async move {
                    if let Some(completion_id) = permit.cancelled_completion_id {
                        if let Err(err) = completion.cancel(completion_id).await {
                            warn!(
                                %err,
                                completion_id,
                                "provider cancellation failed",
                            );
                        }
                    }

                    let Some(snapshot) = snapshot else {
                        info!(
                            %uri,
                            line = position.line,
                            character = position.character,
                            "skipping inline completion for unopened document",
                        );
                        return Ok(None);
                    };

                    // Wait for a typing pause so each keystroke does not start a model
                    // request; the next keystroke's request cancels this one.
                    tokio::select! {
                        _ = tokio::time::sleep(TYPING_PAUSE) => {}
                        _ = permit.cancellation.cancelled() => {}
                    }
                    if permit.cancellation.is_cancelled() {
                        info!(
                            %uri,
                            completion_id = permit.id,
                            "discarding cancelled inline completion before provider call",
                        );
                        return Ok(None);
                    }

                    let version = snapshot.version;
                    let candidates = completion
                        .complete(
                            permit.id,
                            snapshot,
                            position,
                            workspace,
                            permit.cancellation.clone(),
                        )
                        .await
                        .map_err(|err| {
                            error!(%err, "completion request failed");
                            let message = format!("{err:#}");
                            let previous = last_error
                                .lock()
                                .expect("last error lock")
                                .replace(message.clone());
                            if previous.as_ref() != Some(&message) {
                                let _ = client.notify::<ShowMessage>(ShowMessageParams {
                                    typ: MessageType::ERROR,
                                    message: format!("Tabpilot: {message}"),
                                });
                            }
                            ResponseError::new(
                                ErrorCode::INTERNAL_ERROR,
                                "completion request failed",
                            )
                        })?;
                    let candidate_count = candidates.len();

                    if permit.cancellation.is_cancelled() || !coordinator.is_latest(permit.id) {
                        info!(
                            %uri,
                            version,
                            completion_id = permit.id,
                            candidate_count,
                            "discarding stale inline completion result",
                        );
                        return Ok(None);
                    }

                    info!(
                        %uri,
                        line = position.line,
                        character = position.character,
                        version,
                        candidate_count,
                        "inline completion completed",
                    );

                    if !documents.has_version(&uri, version) {
                        info!(%uri, version, "discarding stale completion result");
                        return Ok(None);
                    }

                    Ok(Some(InlineCompletionResponse::Array(
                        candidates.into_iter().map(Into::into).collect(),
                    )))
                }
            })
            .notification::<Initialized>(Self::initialized)
            .notification::<DidOpenTextDocument>(Self::did_open)
            .notification::<DidChangeTextDocument>(Self::did_change)
            .notification::<DidSaveTextDocument>(Self::did_save)
            .notification::<DidCloseTextDocument>(Self::did_close)
            .notification::<Exit>(Self::exit);

        router
    }

    /// Answer the question selected at `location`, replying `{"text": answer}`.
    fn answer_selection(
        &self,
        location: Option<Value>,
    ) -> impl Future<Output = Result<Option<Value>, ResponseError>> + use<> {
        let location = location.map(serde_json::from_value::<Location>);
        let documents = Arc::clone(&self.documents);
        let completion = self.completion.clone();
        let workspace = self.workspace.clone();
        let completion_id = self.coordinator.next_id();

        async move {
            let invalid = |message: String| ResponseError::new(ErrorCode::INVALID_PARAMS, message);
            let location = location
                .ok_or_else(|| invalid("expected the selection's location".to_owned()))?
                .map_err(|err| invalid(format!("invalid selection location: {err}")))?;
            let uri = location.uri.to_string();
            let document = documents
                .snapshot(&uri)
                .ok_or_else(|| invalid(format!("{uri} is not open")))?;
            let selection = TextRange {
                start: from_lsp_position(location.range.start),
                end: from_lsp_position(location.range.end),
            };
            let answer = completion
                .answer_selection(
                    completion_id,
                    document,
                    selection,
                    workspace,
                    CompletionCancellation::new(),
                )
                .await
                .map_err(|err| {
                    error!(%err, "answer request failed");
                    ResponseError::new(ErrorCode::INTERNAL_ERROR, format!("{err:#}"))
                })?;
            Ok(Some(json!({ "text": answer })))
        }
    }

    fn initialized(&mut self, _: InitializedParams) -> ControlFlow<async_lsp::Result<()>> {
        info!("tabpilot-lsp initialized");
        self.completion.prepare(&self.workspace);
        ControlFlow::Continue(())
    }

    fn did_open(
        &mut self,
        params: DidOpenTextDocumentParams,
    ) -> ControlFlow<async_lsp::Result<()>> {
        let document = params.text_document;
        self.documents.open(DocumentSnapshot {
            uri: document.uri.to_string(),
            language_id: document.language_id,
            version: document.version,
            text: document.text,
        });
        ControlFlow::Continue(())
    }

    fn did_change(
        &mut self,
        params: DidChangeTextDocumentParams,
    ) -> ControlFlow<async_lsp::Result<()>> {
        let Some(change) = params.content_changes.into_iter().next() else {
            return ControlFlow::Continue(());
        };

        self.documents.change_full(
            params.text_document.uri.as_str(),
            params.text_document.version,
            change.text,
        );
        ControlFlow::Continue(())
    }

    fn did_close(
        &mut self,
        params: DidCloseTextDocumentParams,
    ) -> ControlFlow<async_lsp::Result<()>> {
        self.documents.close(params.text_document.uri.as_str());
        ControlFlow::Continue(())
    }

    fn did_save(
        &mut self,
        params: DidSaveTextDocumentParams,
    ) -> ControlFlow<async_lsp::Result<()>> {
        info!(uri = %params.text_document.uri, "document saved");
        ControlFlow::Continue(())
    }

    fn exit(&mut self, _: ()) -> ControlFlow<async_lsp::Result<()>> {
        info!("lsp exit received");
        ControlFlow::Break(Ok(()))
    }
}

fn completion_provider_from_env() -> (&'static str, Arc<dyn CompletionProvider>) {
    match std::env::var("TABPILOT_PROVIDER")
        .unwrap_or_else(|_| "codex".to_owned())
        .to_ascii_lowercase()
        .as_str()
    {
        "claude" => ("claude", Arc::new(ClaudeProvider::default())),
        "codex" => ("codex", Arc::new(CodexProvider::default())),
        "commonthread" | "" => ("commonthread", Arc::new(CommonthreadProvider::default())),
        unknown => {
            warn!(
                provider = unknown,
                "unknown TABPILOT_PROVIDER, falling back to commonthread",
            );
            ("commonthread", Arc::new(CommonthreadProvider::default()))
        }
    }
}

pub fn service(
    client: ClientSocket,
) -> impl async_lsp::LspService<Response = serde_json::Value, Error = ResponseError> {
    ServiceBuilder::new()
        .layer(TracingLayer::default())
        .layer(LifecycleLayer::default())
        .layer(CatchUnwindLayer::default())
        .layer(ConcurrencyLayer::default())
        .layer(ClientProcessMonitorLayer::new(client.clone()))
        .service(CompletionServer::router(client))
}

#[derive(Default)]
struct CompletionCoordinator {
    latest_id: AtomicU64,
    current_cancellation: StdMutex<Option<ActiveCompletion>>,
}

struct CompletionPermit {
    id: u64,
    cancellation: CompletionCancellation,
    cancelled_completion_id: Option<u64>,
}

struct ActiveCompletion {
    id: u64,
    cancellation: CompletionCancellation,
}

impl CompletionCoordinator {
    fn next_id(&self) -> u64 {
        self.latest_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn begin(&self) -> CompletionPermit {
        let id = self.next_id();
        let cancellation = CompletionCancellation::new();
        let previous = self
            .current_cancellation
            .lock()
            .expect("completion coordinator mutex poisoned")
            .replace(ActiveCompletion {
                id,
                cancellation: cancellation.clone(),
            });

        let cancelled_completion_id = previous.map(|previous| {
            previous.cancellation.cancel();
            previous.id
        });

        CompletionPermit {
            id,
            cancellation,
            cancelled_completion_id,
        }
    }

    fn is_latest(&self, id: u64) -> bool {
        self.latest_id.load(Ordering::SeqCst) == id
    }
}
