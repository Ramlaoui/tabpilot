use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::answer::AnswerRequest;
use crate::completion::{CompletionCandidate, CompletionRequest};
use crate::workspace::WorkspaceContext;

pub mod claude;
pub mod codex;
pub mod commonthread;
mod shared;

#[async_trait]
pub trait CompletionProvider: Send + Sync {
    async fn complete(&self, request: CompletionRequest) -> Result<Vec<CompletionCandidate>>;

    async fn answer(&self, _request: AnswerRequest) -> Result<Vec<CompletionCandidate>> {
        Ok(Vec::new())
    }

    async fn cancel(&self, _completion_id: u64) -> Result<()> {
        Ok(())
    }

    /// Start background work, such as warming a model session, once the
    /// editor workspace is known.
    fn prepare(&self, _workspace: &WorkspaceContext) {}

    /// Provider details for the editor's status command, such as the model in use.
    fn status(&self) -> Value {
        Value::Null
    }
}
