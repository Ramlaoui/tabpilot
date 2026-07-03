use lsp_types::{InlineCompletionItem, Position, Range};

use crate::completion::CompletionCandidate;
use crate::protocol::{CursorPosition, TextRange};

impl From<CompletionCandidate> for InlineCompletionItem {
    fn from(candidate: CompletionCandidate) -> Self {
        Self {
            insert_text: candidate.insert_text,
            filter_text: None,
            range: candidate.range.map(to_lsp_range),
            command: None,
            insert_text_format: None,
        }
    }
}

pub fn from_lsp_position(position: Position) -> CursorPosition {
    CursorPosition::new(position.line, position.character)
}

fn to_lsp_range(range: TextRange) -> Range {
    Range {
        start: to_lsp_position(range.start),
        end: to_lsp_position(range.end),
    }
}

fn to_lsp_position(position: CursorPosition) -> Position {
    Position {
        line: position.line,
        character: position.character,
    }
}
