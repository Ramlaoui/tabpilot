use serde_json::{Value, json};

use super::MAX_SUGGESTIONS;

pub(in crate::providers) fn completion_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "suggestions": {
                "type": "array",
                "maxItems": MAX_SUGGESTIONS,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "insert_text": {
                            "type": "string"
                        },
                        "score": {
                            "type": "number",
                            "minimum": 0.0,
                            "maximum": 1.0
                        }
                    },
                    "required": ["insert_text", "score"]
                }
            }
        },
        "required": ["suggestions"]
    })
}
