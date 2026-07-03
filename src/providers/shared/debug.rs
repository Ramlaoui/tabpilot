use std::path::PathBuf;

use serde_json::{Value, json};

use crate::answer::AnswerRequest;
use crate::completion::{CompletionCandidate, CompletionRequest};
use crate::documents::DocumentSnapshot;
use crate::protocol::CursorPosition;
use crate::workspace::WorkspaceContext;

pub(in crate::providers) fn debug_document_metadata(request: &CompletionRequest) -> Value {
    debug_document_parts(&request.document, request.position, &request.workspace)
}

pub(in crate::providers) fn debug_answer_document_metadata(request: &AnswerRequest) -> Value {
    debug_document_parts(&request.document, request.position, &request.workspace)
}

fn debug_document_parts(
    document: &DocumentSnapshot,
    position: CursorPosition,
    workspace: &WorkspaceContext,
) -> Value {
    json!({
        "uri": document.uri,
        "language_id": document.language_id,
        "version": document.version,
        "position": {
            "line": position.line,
            "character": position.character,
        },
        "workspace": {
            "root_uri": workspace.root_uri,
            "name": workspace.name,
            "git_branch": workspace.git.as_ref().and_then(|git| git.branch.as_deref()),
            "git_head": workspace.git.as_ref().and_then(|git| git.head.as_deref()),
        },
    })
}

pub(in crate::providers) fn debug_candidates(candidates: &[CompletionCandidate]) -> Vec<Value> {
    candidates
        .iter()
        .map(|candidate| {
            json!({
                "insert_text": candidate.insert_text,
                "range": candidate.range.map(|range| {
                    json!({
                        "start": {
                            "line": range.start.line,
                            "character": range.start.character,
                        },
                        "end": {
                            "line": range.end.line,
                            "character": range.end.character,
                        },
                    })
                }),
            })
        })
        .collect()
}

pub(in crate::providers) fn provider_debug_transcript_path(
    provider_name: &str,
    provider_debug_env: &str,
    provider_debug_path_env: &str,
) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(provider_debug_path_env) {
        return Some(PathBuf::from(path));
    }

    if let Some(path) = std::env::var_os("TABPILOT_DEBUG_PATH") {
        return Some(PathBuf::from(path));
    }

    if std::env::var_os("TABPILOT_DEBUG").is_none()
        && std::env::var_os(provider_debug_env).is_none()
    {
        return None;
    }

    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")));

    Some(state_home?.join(format!("tabpilot/{provider_name}-debug.jsonl")))
}
