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
    Initialized,
};
use lsp_types::request::{Initialize, InlineCompletionRequest, Shutdown};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, InitializeResult, InitializedParams, InlineCompletionResponse,
    OneOf, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use tower::ServiceBuilder;
use tracing::{error, info, warn};

use crate::completion::{CompletionCancellation, CompletionEngine};
use crate::documents::{DocumentSnapshot, DocumentStore};
use crate::lsp::inline_completion::from_lsp_position;
use crate::providers::CompletionProvider;
use crate::providers::claude::ClaudeProvider;
use crate::providers::codex::CodexProvider;
use crate::workspace::WorkspaceContext;

pub struct CompletionServer {
    documents: Arc<DocumentStore>,
    completion: CompletionEngine,
    coordinator: Arc<CompletionCoordinator>,
    workspace: WorkspaceContext,
}

impl CompletionServer {
    fn new(_: ClientSocket) -> Self {
        let (provider_name, provider) = completion_provider_from_env();
        info!(provider = provider_name, "completion provider selected");

        Self {
            documents: Arc::new(DocumentStore::default()),
            completion: CompletionEngine::new(provider),
            coordinator: Arc::new(CompletionCoordinator::default()),
            workspace: WorkspaceContext::default(),
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
                            ..ServerCapabilities::default()
                        },
                        server_info: None,
                        offset_encoding: None,
                    })
                }
            })
            .request::<Shutdown, _>(|_, _| async move { Ok(()) })
            .request::<InlineCompletionRequest, _>(|server, params| {
                let uri = params.text_document_position.text_document.uri.to_string();
                let position = from_lsp_position(params.text_document_position.position);
                let snapshot = server.documents.snapshot(&uri);
                let documents = Arc::clone(&server.documents);
                let completion = server.completion.clone();
                let permit = server.coordinator.begin();
                let coordinator = Arc::clone(&server.coordinator);
                let workspace = server.workspace.clone();

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

    fn initialized(&mut self, _: InitializedParams) -> ControlFlow<async_lsp::Result<()>> {
        info!("tabpilot-lsp initialized");
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
        "codex" | "" => ("codex", Arc::new(CodexProvider::default())),
        unknown => {
            warn!(
                provider = unknown,
                "unknown TABPILOT_PROVIDER, falling back to codex",
            );
            ("codex", Arc::new(CodexProvider::default()))
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
    fn begin(&self) -> CompletionPermit {
        let id = self.latest_id.fetch_add(1, Ordering::SeqCst) + 1;
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
