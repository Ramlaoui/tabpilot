use std::time::Duration;

use anyhow::{Context, Result, bail};

const DEFAULT_URL: &str = "http://127.0.0.1:8080";
/// Where a shared server's token for tabpilot lives, relative to `$HOME`.
const DEFAULT_TOKEN_FILE: &str = ".config/commonthread/tabpilot.token";
const TURN_TIMEOUT: Duration = Duration::from_secs(45);
/// Prompt text a session takes before a warmed spare replaces it. Every turn,
/// interrupted or not, stays in the conversation the next turn resends, while
/// a rotation rereads the repository.
pub(super) const MAX_SESSION_PROMPT_CHARS: usize = 120_000;

#[derive(Debug, Clone)]
pub(super) struct CommonthreadProviderConfig {
    /// A shared server to use while it is running; otherwise tabpilot starts its own.
    pub shared: Option<Endpoint>,
    /// Harness whose default profile is used when `profile` is unset.
    pub harness: String,
    /// Commonthread profile ID; resolved from `harness` when unset.
    pub profile: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub turn_timeout: Duration,
}

#[derive(Debug, Clone)]
pub(super) struct Endpoint {
    pub url: String,
    pub token: String,
}

impl CommonthreadProviderConfig {
    pub(super) fn from_env() -> Result<Self> {
        let url = std::env::var("TABPILOT_COMMONTHREAD_URL").ok();
        let token = match (
            std::env::var("TABPILOT_COMMONTHREAD_TOKEN"),
            std::env::var("TABPILOT_COMMONTHREAD_TOKEN_FILE"),
        ) {
            (Ok(token), _) => Some(token),
            (_, Ok(path)) => Some(
                std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read Commonthread token from {path}"))?,
            ),
            _ => std::env::var_os("HOME").and_then(|home| {
                std::fs::read_to_string(std::path::Path::new(&home).join(DEFAULT_TOKEN_FILE)).ok()
            }),
        };
        if url.is_some() && token.is_none() {
            bail!("TABPILOT_COMMONTHREAD_URL needs TABPILOT_COMMONTHREAD_TOKEN or _TOKEN_FILE");
        }
        let shared = token.map(|token| Endpoint {
            url: url
                .as_deref()
                .unwrap_or(DEFAULT_URL)
                .trim_end_matches('/')
                .to_owned(),
            token: token.trim().to_owned(),
        });
        let var = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
        Ok(Self {
            shared,
            harness: var("TABPILOT_HARNESS").unwrap_or_else(|| "claude".to_owned()),
            profile: var("TABPILOT_COMMONTHREAD_PROFILE"),
            model: var("TABPILOT_COMMONTHREAD_MODEL"),
            effort: var("TABPILOT_COMMONTHREAD_EFFORT"),
            turn_timeout: TURN_TIMEOUT,
        })
    }
}

/// A fast model and effort for each harness, so completions need no setup.
pub(super) fn default_agent(harness: &str) -> (Option<&'static str>, Option<&'static str>) {
    match harness {
        "claude" => (Some("haiku"), Some("low")),
        "codex" => (Some("gpt-6-luna"), Some("low")),
        _ => (None, None),
    }
}
