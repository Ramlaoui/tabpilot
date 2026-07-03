#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPosition {
    pub line: u32,
    pub character: u32,
}

impl CursorPosition {
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    pub start: CursorPosition,
    pub end: CursorPosition,
}

impl TextRange {
    pub const fn empty_at(position: CursorPosition) -> Self {
        Self {
            start: position,
            end: position,
        }
    }
}
