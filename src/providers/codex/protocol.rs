use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(super) struct ClientMessage<'a> {
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) id: Option<u64>,
    pub(super) params: Value,
}

impl<'a> ClientMessage<'a> {
    pub(super) fn request(id: u64, method: &'a str, params: Value) -> Self {
        Self {
            method,
            id: Some(id),
            params,
        }
    }

    pub(super) fn notification(method: &'a str, params: Value) -> Self {
        Self {
            method,
            id: None,
            params,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ServerMessage {
    pub(super) id: Option<u64>,
    pub(super) method: Option<String>,
    #[serde(default)]
    pub(super) params: Value,
    pub(super) result: Option<Value>,
    pub(super) error: Option<Value>,
}

pub(super) fn same_thread(params: &Value, thread_id: &str) -> bool {
    params
        .get("threadId")
        .and_then(Value::as_str)
        .is_some_and(|id| id == thread_id)
}

pub(super) fn same_known_turn(params: &Value, turn_id: Option<&str>) -> bool {
    let Some(turn_id) = turn_id else {
        return false;
    };

    notification_turn_id(params).is_some_and(|id| id == turn_id)
}

pub(super) fn notification_turn_id(params: &Value) -> Option<&str> {
    params
        .get("turnId")
        .and_then(Value::as_str)
        .or_else(|| params.pointer("/turn/id").and_then(Value::as_str))
}

pub(super) fn item_completed_agent_message(params: &Value) -> Option<String> {
    let item = params.get("item")?;

    if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
        item.get("text").and_then(Value::as_str).map(str::to_owned)
    } else {
        None
    }
}

pub(super) fn completed_turn_agent_message(params: &Value) -> Option<String> {
    let items = params.pointer("/turn/items")?.as_array()?;
    items.iter().rev().find_map(|item| {
        if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
            item.get("text").and_then(Value::as_str).map(str::to_owned)
        } else {
            None
        }
    })
}

pub(super) fn completed_turn_error(params: &Value) -> Option<String> {
    let status = params.pointer("/turn/status").and_then(Value::as_str)?;

    if status != "failed" {
        return None;
    }

    params
        .pointer("/turn/error/message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| Some("unknown codex turn error".to_owned()))
}

pub(super) fn error_message(error: &Value) -> Option<String> {
    error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
}
