use std::collections::HashMap;
use std::sync::RwLock;

use crate::protocol::CursorPosition;

#[derive(Debug, Clone)]
pub struct DocumentSnapshot {
    pub uri: String,
    pub language_id: String,
    pub version: i32,
    pub text: String,
}

impl DocumentSnapshot {
    pub fn split_at_position(&self, position: CursorPosition) -> Option<(String, String)> {
        let byte_index = self.byte_index_for_position(position)?;
        Some((
            self.text[..byte_index].to_owned(),
            self.text[byte_index..].to_owned(),
        ))
    }

    fn byte_index_for_position(&self, position: CursorPosition) -> Option<usize> {
        let mut line_start = 0usize;
        let mut current_line = 0u32;

        for segment in self.text.split_inclusive('\n') {
            let line_text = segment.strip_suffix('\n').unwrap_or(segment);

            if current_line == position.line {
                return Some(
                    line_start + byte_offset_for_utf16_column(line_text, position.character),
                );
            }

            line_start += segment.len();
            current_line += 1;
        }

        if current_line == position.line {
            Some(self.text.len())
        } else {
            None
        }
    }
}

#[derive(Debug, Default)]
pub struct DocumentStore {
    documents: RwLock<HashMap<String, DocumentSnapshot>>,
}

impl DocumentStore {
    pub fn open(&self, snapshot: DocumentSnapshot) {
        self.documents
            .write()
            .expect("document store lock poisoned")
            .insert(snapshot.uri.clone(), snapshot);
    }

    pub fn change_full(&self, uri: &str, version: i32, text: String) {
        let mut documents = self
            .documents
            .write()
            .expect("document store lock poisoned");

        if let Some(snapshot) = documents.get_mut(uri) {
            snapshot.version = version;
            snapshot.text = text;
        }
    }

    pub fn close(&self, uri: &str) {
        self.documents
            .write()
            .expect("document store lock poisoned")
            .remove(uri);
    }

    pub fn snapshot(&self, uri: &str) -> Option<DocumentSnapshot> {
        self.documents
            .read()
            .expect("document store lock poisoned")
            .get(uri)
            .cloned()
    }

    pub fn has_version(&self, uri: &str, version: i32) -> bool {
        self.documents
            .read()
            .expect("document store lock poisoned")
            .get(uri)
            .is_some_and(|snapshot| snapshot.version == version)
    }
}

fn byte_offset_for_utf16_column(text: &str, target_column: u32) -> usize {
    let mut column = 0u32;

    for (byte_index, ch) in text.char_indices() {
        let width = ch.len_utf16() as u32;
        if column + width > target_column {
            return byte_index;
        }
        column += width;
    }

    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_ascii_text_at_position() {
        let snapshot = DocumentSnapshot {
            uri: "file:///tmp/test.rs".to_owned(),
            language_id: "rust".to_owned(),
            version: 1,
            text: "fn main() {\n}".to_owned(),
        };

        let (prefix, suffix) = snapshot
            .split_at_position(CursorPosition::new(0, 2))
            .expect("position should resolve");

        assert_eq!(prefix, "fn");
        assert_eq!(suffix, " main() {\n}");
    }

    #[test]
    fn handles_utf16_columns() {
        let snapshot = DocumentSnapshot {
            uri: "file:///tmp/test.txt".to_owned(),
            language_id: "text".to_owned(),
            version: 1,
            text: "a😀b".to_owned(),
        };

        let (prefix, suffix) = snapshot
            .split_at_position(CursorPosition::new(0, 3))
            .expect("position should resolve");

        assert_eq!(prefix, "a😀");
        assert_eq!(suffix, "b");
    }
}
