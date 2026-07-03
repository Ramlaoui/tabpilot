use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use crate::providers::shared::provider_debug_transcript_path;

const DEFAULT_CODEX_MODEL: &str = "gpt-5.3-codex-spark";
const DEFAULT_REASONING_EFFORT: &str = "xhigh";
const TURN_TIMEOUT: Duration = Duration::from_secs(45);
pub(super) const MAX_COMPLETED_TURNS_PER_THREAD: u32 = 3;
pub(super) const MAX_CANCELLED_TURNS_PER_THREAD: u32 = 2;
pub(super) const MAX_THREAD_INPUT_TOKENS: u64 = 32_000;

#[derive(Debug, Clone)]
pub struct CodexProviderConfig {
    pub dry_run: bool,
    pub model: String,
    pub reasoning_effort: String,
    pub service_tier: Option<String>,
    pub turn_timeout: Duration,
    pub debug_transcript_path: Option<PathBuf>,
}

impl Default for CodexProviderConfig {
    fn default() -> Self {
        Self {
            dry_run: std::env::var("TABPILOT_CODEX_DRY_RUN").is_ok(),
            model: DEFAULT_CODEX_MODEL.to_owned(),
            reasoning_effort: DEFAULT_REASONING_EFFORT.to_owned(),
            service_tier: None,
            turn_timeout: TURN_TIMEOUT,
            debug_transcript_path: debug_transcript_path(),
        }
    }
}

fn debug_transcript_path() -> Option<PathBuf> {
    provider_debug_transcript_path("codex", "TABPILOT_CODEX_DEBUG", "TABPILOT_CODEX_DEBUG_PATH")
}

pub(super) fn configured_codex_mcp_server_names() -> Vec<String> {
    let Some(config_path) = codex_config_path() else {
        return Vec::new();
    };
    let Ok(contents) = fs::read_to_string(config_path) else {
        return Vec::new();
    };

    parse_codex_mcp_server_names(&contents)
}

fn codex_config_path() -> Option<PathBuf> {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(codex_home).join("config.toml"));
    }

    Some(PathBuf::from(std::env::var_os("HOME")?).join(".codex/config.toml"))
}

fn parse_codex_mcp_server_names(contents: &str) -> Vec<String> {
    let mut names = Vec::new();

    for line in contents.lines().map(str::trim) {
        let Some(name) = line
            .strip_prefix("[mcp_servers.")
            .and_then(|line| line.strip_suffix(']'))
            .map(str::trim)
        else {
            continue;
        };

        if is_bare_toml_key(name) && !names.iter().any(|existing| existing == name) {
            names.push(name.to_owned());
        }
    }

    names
}

fn is_bare_toml_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
