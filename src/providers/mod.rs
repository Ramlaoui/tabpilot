use anyhow::Result;
use async_trait::async_trait;

use crate::answer::AnswerRequest;
use crate::completion::{CompletionCandidate, CompletionRequest};

pub mod claude;
pub mod codex;
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
}
