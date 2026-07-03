use crate::answer::AnswerRequest;
use crate::completion::CompletionRequest;
use crate::workspace::{GitContext, WorkspaceContext};
use lsp_types::Url;

use super::{MAX_CURSOR_WINDOW_CHARS, MAX_PREFIX_CHARS, MAX_SUFFIX_CHARS};

pub(in crate::providers) fn context_to_text(request: &CompletionRequest) -> String {
    request_context_to_text(
        &request.workspace,
        &request.document,
        request.position,
        &request.context,
    )
}

pub(in crate::providers) fn answer_context_to_text(request: &AnswerRequest) -> String {
    request_context_to_text(
        &request.workspace,
        &request.document,
        request.position,
        &request.context,
    )
}

pub(in crate::providers) fn workspace_cwd(workspace: &WorkspaceContext) -> Option<String> {
    let uri = workspace.root_uri.as_ref()?;
    let path = Url::parse(uri).ok()?.to_file_path().ok()?;
    Some(path.to_string_lossy().into_owned())
}

fn request_context_to_text(
    workspace: &WorkspaceContext,
    document: &crate::documents::DocumentSnapshot,
    position: crate::protocol::CursorPosition,
    context: &crate::context::ContextPack,
) -> String {
    let prefix = tail_chars(&context.active_prefix, MAX_PREFIX_CHARS);
    let suffix = head_chars(&context.active_suffix, MAX_SUFFIX_CHARS);
    let cursor_window = tail_chars(&context.cursor_window, MAX_CURSOR_WINDOW_CHARS);

    format!(
        "\
Workspace:
{workspace}

Active document:
- uri: {uri}
- language_id: {language_id}
- version: {version}
- cursor: line {line}, character {character}

Current cursor:
- position_kind: {position_kind}
- line_prefix: {current_line_prefix:?}
- line_suffix: {current_line_suffix:?}

Local cursor window:
{cursor_window}

Lower-priority raw text before cursor:
{prefix}

Lower-priority raw text after cursor:
{suffix}
",
        workspace = workspace_to_text(workspace),
        uri = document.uri,
        language_id = document.language_id,
        version = document.version,
        line = position.line,
        character = position.character,
        position_kind = context.position_kind.as_str(),
        current_line_prefix = &context.current_line_prefix,
        current_line_suffix = &context.current_line_suffix,
    )
}

fn workspace_to_text(workspace: &WorkspaceContext) -> String {
    let mut lines = Vec::new();

    if let Some(name) = &workspace.name {
        lines.push(format!("- name: {name}"));
    }

    if let Some(root_uri) = &workspace.root_uri {
        lines.push(format!("- root_uri: {root_uri}"));
    }

    if let Some(git) = &workspace.git {
        lines.extend(git_to_text(git));
    }

    if lines.is_empty() {
        "- unavailable".to_owned()
    } else {
        lines.join("\n")
    }
}

fn git_to_text(git: &GitContext) -> Vec<String> {
    let mut lines = Vec::new();

    if let Some(branch) = &git.branch {
        lines.push(format!("- git_branch: {branch}"));
    }

    if let Some(head) = &git.head {
        lines.push(format!("- git_head: {head}"));
    }

    if !git.remotes.is_empty() {
        let remotes = git
            .remotes
            .iter()
            .map(|remote| format!("{}={}", remote.name, remote.url))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("- git_remotes: {remotes}"));
    }

    lines
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();

    if char_count <= max_chars {
        return text.to_owned();
    }

    text.chars().skip(char_count - max_chars).collect()
}

fn head_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}
