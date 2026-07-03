use std::path::PathBuf;
use std::time::Duration;

use crate::providers::shared::provider_debug_transcript_path;

const DEFAULT_CLAUDE_MODEL: &str = "haiku";
const DEFAULT_CLAUDE_EFFORT: &str = "low";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(super) struct ClaudeProviderConfig {
    pub dry_run: bool,
    pub model: Option<String>,
    pub effort: String,
    pub timeout: Duration,
    pub debug_transcript_path: Option<PathBuf>,
}

impl Default for ClaudeProviderConfig {
    fn default() -> Self {
        Self {
            dry_run: std::env::var("TABPILOT_CLAUDE_DRY_RUN").is_ok(),
            model: Some(
                std::env::var("TABPILOT_CLAUDE_MODEL")
                    .unwrap_or_else(|_| DEFAULT_CLAUDE_MODEL.to_owned()),
            ),
            effort: std::env::var("TABPILOT_CLAUDE_EFFORT")
                .unwrap_or_else(|_| DEFAULT_CLAUDE_EFFORT.to_owned()),
            timeout: timeout_from_env().unwrap_or(DEFAULT_TIMEOUT),
            debug_transcript_path: debug_transcript_path(),
        }
    }
}

fn timeout_from_env() -> Option<Duration> {
    let millis = std::env::var("TABPILOT_CLAUDE_TIMEOUT_MS")
        .ok()?
        .parse()
        .ok()?;
    Some(Duration::from_millis(millis))
}

fn debug_transcript_path() -> Option<PathBuf> {
    provider_debug_transcript_path(
        "claude",
        "TABPILOT_CLAUDE_DEBUG",
        "TABPILOT_CLAUDE_DEBUG_PATH",
    )
}
