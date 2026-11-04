use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::watch;
use tracing::info;

use crate::answer::{AnswerRequest, parse_answer_command};
use crate::context::{ContextEngine, ContextPack};
use crate::documents::DocumentSnapshot;
use crate::protocol::{CursorPosition, TextRange};
use crate::providers::CompletionProvider;
use crate::workspace::WorkspaceContext;

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub completion_id: u64,
    pub document: DocumentSnapshot,
    pub position: CursorPosition,
    pub context: ContextPack,
    pub workspace: WorkspaceContext,
    pub cancellation: CompletionCancellation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionCandidate {
    pub insert_text: String,
    pub range: Option<TextRange>,
}

#[derive(Clone)]
pub struct CompletionCancellation {
    sender: watch::Sender<bool>,
    receiver: watch::Receiver<bool>,
}

impl CompletionCancellation {
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel(false);
        Self { sender, receiver }
    }

    pub fn cancel(&self) {
        let _ = self.sender.send(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.receiver.clone();

        if *receiver.borrow() {
            return;
        }

        while receiver.changed().await.is_ok() {
            if *receiver.borrow() {
                return;
            }
        }
    }
}

impl Default for CompletionCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CompletionCancellation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletionCancellation")
            .field("is_cancelled", &self.is_cancelled())
            .finish()
    }
}

/// Cancels a request whose future is dropped before it finishes.
struct CancelOnDrop(Option<CompletionCancellation>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.0 {
            cancellation.cancel();
        }
    }
}

#[derive(Clone)]
pub struct CompletionEngine {
    context: ContextEngine,
    provider: Arc<dyn CompletionProvider>,
}

impl CompletionEngine {
    pub fn new(provider: Arc<dyn CompletionProvider>) -> Self {
        Self {
            context: ContextEngine,
            provider,
        }
    }

    /// The LSP drops a request's future on `$/cancelRequest`. The provider runs
    /// in its own task instead and sees a cancellation, so it can wind down an
    /// agent turn rather than stop between two calls to the agent.
    pub async fn complete(
        &self,
        completion_id: u64,
        document: DocumentSnapshot,
        position: CursorPosition,
        workspace: WorkspaceContext,
        cancellation: CompletionCancellation,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut guard = CancelOnDrop(Some(cancellation.clone()));
        let engine = self.clone();
        let task = tokio::spawn(async move {
            engine
                .run(completion_id, document, position, workspace, cancellation)
                .await
        });
        let result = task.await.context("completion task failed")?;
        guard.0 = None;
        result
    }

    async fn run(
        &self,
        completion_id: u64,
        document: DocumentSnapshot,
        position: CursorPosition,
        workspace: WorkspaceContext,
        cancellation: CompletionCancellation,
    ) -> Result<Vec<CompletionCandidate>> {
        if cancellation.is_cancelled() {
            return Ok(Vec::new());
        }

        let context = self.context.build(&document, position);
        if let Some(question) = parse_answer_command(&context) {
            let request = AnswerRequest {
                completion_id,
                question,
                document,
                position,
                context,
                workspace,
                cancellation,
            };

            let provider_start = Instant::now();
            let candidates = self.provider.answer(request).await?;

            info!(
                provider_latency_ms = provider_start.elapsed().as_millis(),
                candidate_count = candidates.len(),
                "provider answered inline question",
            );

            return Ok(candidates);
        }

        let request = CompletionRequest {
            completion_id,
            document,
            position,
            context,
            workspace,
            cancellation,
        };

        let provider_start = Instant::now();
        let candidates = self.provider.complete(request).await?;

        info!(
            provider_latency_ms = provider_start.elapsed().as_millis(),
            candidate_count = candidates.len(),
            "provider completed",
        );

        Ok(candidates)
    }

    pub async fn cancel(&self, completion_id: u64) -> Result<()> {
        self.provider.cancel(completion_id).await
    }

    pub fn prepare(&self, workspace: &WorkspaceContext) {
        self.provider.prepare(workspace);
    }

    pub fn status(&self) -> serde_json::Value {
        self.provider.status()
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tokio::sync::oneshot;

    use super::*;

    /// Waits for its cancellation, then reports that it saw it.
    struct WaitsForCancel(std::sync::Mutex<Option<oneshot::Sender<()>>>);

    #[async_trait]
    impl CompletionProvider for WaitsForCancel {
        async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionCandidate>> {
            request.cancellation.cancelled().await;
            if let Some(saw) = self.0.lock().unwrap().take() {
                let _ = saw.send(());
            }
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn dropped_requests_cancel_a_provider_that_keeps_running() {
        let (saw, seen) = oneshot::channel();
        let engine = CompletionEngine::new(Arc::new(WaitsForCancel(Some(saw).into())));
        let document = DocumentSnapshot {
            uri: "file:///tmp/a.rs".to_owned(),
            language_id: "rust".to_owned(),
            version: 1,
            text: "fn main() {}".to_owned(),
        };
        let request = engine.complete(
            1,
            document,
            CursorPosition::new(0, 2),
            WorkspaceContext::default(),
            CompletionCancellation::new(),
        );
        // `$/cancelRequest` drops the request's future like this timeout does.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), request)
                .await
                .is_err()
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), seen)
            .await
            .expect("provider saw the cancellation")
            .unwrap();
    }
}
