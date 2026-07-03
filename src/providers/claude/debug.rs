use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tracing::warn;

use super::config::ClaudeProviderConfig;

pub(super) fn write_claude_debug_event(config: &ClaudeProviderConfig, mut event: Value) {
    let Some(path) = &config.debug_transcript_path else {
        return;
    };

    if let Value::Object(fields) = &mut event {
        fields.insert("timestamp_ms".to_owned(), json!(timestamp_millis()));
    }

    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        warn!(path = %path.display(), %err, "failed to create claude debug transcript directory");
        return;
    }

    let mut file = match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => file,
        Err(err) => {
            warn!(path = %path.display(), %err, "failed to open claude debug transcript");
            return;
        }
    };

    if let Err(err) = serde_json::to_writer(&mut file, &event)
        .and_then(|()| file.write_all(b"\n").map_err(serde_json::Error::io))
    {
        warn!(path = %path.display(), %err, "failed to write claude debug transcript");
    }
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
