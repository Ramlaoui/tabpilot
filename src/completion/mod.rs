use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
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

    pub async fn complete(
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
}
