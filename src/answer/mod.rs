use crate::completion::CompletionCancellation;
use crate::context::ContextPack;
use crate::documents::DocumentSnapshot;
use crate::protocol::CursorPosition;
use crate::workspace::WorkspaceContext;

#[derive(Debug, Clone)]
pub struct AnswerRequest {
    pub completion_id: u64,
    pub question: String,
    pub document: DocumentSnapshot,
    pub position: CursorPosition,
    pub context: ContextPack,
    pub workspace: WorkspaceContext,
    pub cancellation: CompletionCancellation,
}

pub fn parse_answer_command(context: &ContextPack) -> Option<String> {
    if !context.current_line_suffix.trim().is_empty() {
        return None;
    }

    let marker_line = context.current_line_prefix.trim_start();
    if !marker_line.starts_with("--") {
        return None;
    }

    let after_open = &marker_line[2..];
    let close_index = after_open.rfind("--")?;
    let question = after_open[..close_index].trim();
    let trailing = after_open[close_index + 2..].trim();

    if question.is_empty() || !trailing.is_empty() {
        return None;
    }

    Some(question.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ContextPack, PositionKind};

    #[test]
    fn parses_completed_answer_command() {
        let question = parse_answer_command(&context("-- how to import matplotlib --", ""))
            .expect("answer command should parse");

        assert_eq!(question, "how to import matplotlib");
    }

    #[test]
    fn allows_indented_answer_command() {
        let question = parse_answer_command(&context("    -- explain this type --", ""))
            .expect("answer command should parse");

        assert_eq!(question, "explain this type");
    }

    #[test]
    fn ignores_unfinished_answer_command() {
        assert!(parse_answer_command(&context("-- how to import matplotlib", "")).is_none());
    }

    #[test]
    fn ignores_empty_answer_command() {
        assert!(parse_answer_command(&context("-- --", "")).is_none());
    }

    #[test]
    fn ignores_non_marker_lines() {
        assert!(parse_answer_command(&context("let value = a--b --", "")).is_none());
    }

    #[test]
    fn ignores_commands_with_nonempty_suffix() {
        assert!(
            parse_answer_command(&context("-- how to import matplotlib --", " trailing")).is_none()
        );
    }

    fn context(current_line_prefix: &str, current_line_suffix: &str) -> ContextPack {
        ContextPack {
            active_prefix: current_line_prefix.to_owned(),
            active_suffix: current_line_suffix.to_owned(),
            current_line_prefix: current_line_prefix.to_owned(),
            current_line_suffix: current_line_suffix.to_owned(),
            cursor_window: String::new(),
            position_kind: PositionKind::Code,
        }
    }
}
