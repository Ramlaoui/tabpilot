use crate::documents::DocumentSnapshot;
use crate::protocol::CursorPosition;

const CURSOR_WINDOW_BEFORE_LINES: usize = 30;
const CURSOR_WINDOW_AFTER_LINES: usize = 30;

#[derive(Debug, Clone)]
pub struct ContextPack {
    pub active_prefix: String,
    pub active_suffix: String,
    pub current_line_prefix: String,
    pub current_line_suffix: String,
    pub cursor_window: String,
    pub position_kind: PositionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionKind {
    BlankLine,
    Code,
    LineComment,
    BlockComment,
    StringLiteral,
}

impl PositionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BlankLine => "blank_line",
            Self::Code => "code",
            Self::LineComment => "line_comment",
            Self::BlockComment => "block_comment",
            Self::StringLiteral => "string_literal",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ContextEngine;

impl ContextEngine {
    pub fn build(&self, document: &DocumentSnapshot, position: CursorPosition) -> ContextPack {
        let Some((prefix, suffix)) = document.split_at_position(position) else {
            return ContextPack {
                active_prefix: String::new(),
                active_suffix: String::new(),
                current_line_prefix: String::new(),
                current_line_suffix: String::new(),
                cursor_window: String::new(),
                position_kind: PositionKind::BlankLine,
            };
        };

        let current_line_prefix = current_line_prefix(&prefix);
        let current_line_suffix = current_line_suffix(&suffix);
        let position_kind = classify_position(&prefix, &current_line_prefix, &current_line_suffix);
        let cursor_window = cursor_window(
            &document.text,
            position,
            &current_line_prefix,
            &current_line_suffix,
        );

        ContextPack {
            active_prefix: prefix,
            active_suffix: suffix,
            current_line_prefix,
            current_line_suffix,
            cursor_window,
            position_kind,
        }
    }
}

fn current_line_prefix(prefix: &str) -> String {
    prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, current_line)| current_line)
        .to_owned()
}

fn current_line_suffix(suffix: &str) -> String {
    suffix
        .split_once('\n')
        .map_or(suffix, |(current_line, _)| current_line)
        .to_owned()
}

fn classify_position(
    active_prefix: &str,
    current_line_prefix: &str,
    current_line_suffix: &str,
) -> PositionKind {
    if current_line_prefix.trim().is_empty() && current_line_suffix.trim().is_empty() {
        return PositionKind::BlankLine;
    }

    if current_line_prefix.trim_start().starts_with("//") {
        return PositionKind::LineComment;
    }

    if is_inside_block_comment(active_prefix) {
        return PositionKind::BlockComment;
    }

    if is_inside_string_literal(current_line_prefix) {
        return PositionKind::StringLiteral;
    }

    PositionKind::Code
}

fn is_inside_block_comment(active_prefix: &str) -> bool {
    let last_open = active_prefix.rfind("/*");
    let last_close = active_prefix.rfind("*/");

    matches!((last_open, last_close), (Some(open), None) if open < active_prefix.len())
        || matches!((last_open, last_close), (Some(open), Some(close)) if open > close)
}

fn is_inside_string_literal(current_line_prefix: &str) -> bool {
    let mut quote_count = 0usize;
    let mut escaped = false;

    for ch in current_line_prefix.chars() {
        if escaped {
            escaped = false;
            continue;
        }

        if ch == '\\' {
            escaped = true;
            continue;
        }

        if ch == '"' {
            quote_count += 1;
        }
    }

    quote_count % 2 == 1
}

fn cursor_window(
    text: &str,
    position: CursorPosition,
    current_line_prefix: &str,
    current_line_suffix: &str,
) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let cursor_line = position.line as usize;
    let start_line = cursor_line.saturating_sub(CURSOR_WINDOW_BEFORE_LINES);
    let end_line = lines
        .len()
        .min(cursor_line.saturating_add(CURSOR_WINDOW_AFTER_LINES + 1));
    let mut window = Vec::new();

    for line_index in start_line..end_line {
        let line_number = line_index + 1;
        if line_index == cursor_line {
            window.push(format!(
                "{line_number:>4}: {current_line_prefix}<|cursor|>{current_line_suffix}"
            ));
        } else if let Some(line) = lines.get(line_index) {
            window.push(format!("{line_number:>4}: {line}"));
        }
    }

    if window.is_empty() {
        format!(
            "{line_number:>4}: {current_line_prefix}<|cursor|>{current_line_suffix}",
            line_number = cursor_line + 1,
        )
    } else {
        window.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_current_line_and_position_kind() {
        let document = DocumentSnapshot {
            uri: "file:///tmp/test.rs".to_owned(),
            language_id: "rust".to_owned(),
            version: 1,
            text: "fn main() {\n    // hello\n}".to_owned(),
        };

        let pack = ContextEngine.build(&document, CursorPosition::new(1, 8));

        assert_eq!(pack.current_line_prefix, "    // h");
        assert_eq!(pack.current_line_suffix, "ello");
        assert_eq!(pack.position_kind, PositionKind::LineComment);
        assert!(pack.cursor_window.contains("   2:     // h<|cursor|>ello"));
    }

    #[test]
    fn classifies_blank_and_string_positions() {
        let document = DocumentSnapshot {
            uri: "file:///tmp/test.rs".to_owned(),
            language_id: "rust".to_owned(),
            version: 1,
            text: "let value = \"hel\";\n    ".to_owned(),
        };

        let string_pack = ContextEngine.build(&document, CursorPosition::new(0, 13));
        let blank_pack = ContextEngine.build(&document, CursorPosition::new(1, 4));

        assert_eq!(string_pack.position_kind, PositionKind::StringLiteral);
        assert_eq!(blank_pack.position_kind, PositionKind::BlankLine);
    }
}
