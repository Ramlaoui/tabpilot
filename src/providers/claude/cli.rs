use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::completion::CompletionCancellation;

use super::config::ClaudeProviderConfig;

#[derive(Debug, Clone)]
pub(super) struct ClaudeCliClient {
    config: ClaudeProviderConfig,
}

impl ClaudeCliClient {
    pub(super) fn new(config: ClaudeProviderConfig) -> Self {
        Self { config }
    }

    pub(super) async fn run(
        &self,
        prompt: &str,
        schema: Option<Value>,
        cwd: Option<String>,
        cancellation: CompletionCancellation,
    ) -> Result<Option<String>> {
        if cancellation.is_cancelled() {
            return Ok(None);
        }

        let mut command = Command::new("claude");
        command.args([
            "-p",
            "--safe-mode",
            "--no-session-persistence",
            "--output-format",
            "json",
            "--tools",
            "",
            "--effort",
            &self.config.effort,
        ]);
        if let Some(model) = &self.config.model {
            command.args(["--model", model]);
        }
        let schema_json;
        if let Some(schema) = schema {
            schema_json = schema.to_string();
            command.args(["--json-schema", &schema_json]);
        }
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command
            .arg(prompt)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().context("failed to spawn claude")?;
        let stdout = child.stdout.take().context("claude stdout was not piped")?;
        let stderr = child.stderr.take().context("claude stderr was not piped")?;
        let stdout_task = tokio::spawn(read_pipe(stdout));
        let stderr_task = tokio::spawn(read_pipe(stderr));
        let timeout = self.config.timeout;

        let status = tokio::select! {
            status = child.wait() => status.context("failed to wait for claude")?,
            _ = cancellation.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                return Ok(None);
            }
            _ = sleep(timeout) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                bail!("claude timed out after {timeout:?}");
            }
        };

        let stdout = join_output(stdout_task).await?;
        let stderr = join_output(stderr_task).await?;
        let stdout = String::from_utf8_lossy(&stdout);
        let stderr = String::from_utf8_lossy(&stderr);
        let response = extract_result(&stdout)?;

        if !status.success() {
            bail!(
                "claude exited with status {status}: {}",
                if response.is_empty() {
                    stderr.trim().to_owned()
                } else {
                    response
                }
            );
        }

        Ok(Some(response))
    }
}

async fn read_pipe<R>(mut pipe: R) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

async fn join_output(handle: JoinHandle<std::io::Result<Vec<u8>>>) -> Result<Vec<u8>> {
    handle
        .await
        .context("claude output reader task failed")?
        .context("failed to read claude output")
}

fn extract_result(stdout: &str) -> Result<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }

    let Ok(output) = serde_json::from_str::<ClaudeOutput>(trimmed) else {
        return Ok(trimmed.to_owned());
    };

    if output.is_error.unwrap_or(false) {
        bail!(
            "claude failed{}: {}",
            output
                .api_error_status
                .map(|status| format!(" with API status {status}"))
                .unwrap_or_default(),
            output
                .result
                .unwrap_or_else(|| "unknown claude error".to_owned())
        );
    }

    Ok(output.result.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
struct ClaudeOutput {
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default)]
    api_error_status: Option<u16>,
    #[serde(default)]
    result: Option<String>,
}
