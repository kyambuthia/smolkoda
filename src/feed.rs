use std::time::SystemTime;

use serde_json::{json, Value};

use crate::presets::Protocol;

#[derive(Debug, Clone)]
pub enum Block {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
        parse_error: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub enum EventKind {
    User(String),
    Assistant(Vec<Block>),
    ToolResults(Vec<ToolResult>),
}

#[derive(Debug, Clone)]
pub struct Event {
    pub at: SystemTime,
    pub kind: EventKind,
}

#[derive(Debug, Clone, Default)]
pub struct Feed {
    events: Vec<Event>,
}

impl Feed {
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.events.push(Event {
            at: SystemTime::now(),
            kind: EventKind::User(text.into()),
        });
    }

    pub fn push_assistant(&mut self, blocks: Vec<Block>) {
        self.events.push(Event {
            at: SystemTime::now(),
            kind: EventKind::Assistant(blocks),
        });
    }

    pub fn push_tool_results(&mut self, results: Vec<ToolResult>) {
        self.events.push(Event {
            at: SystemTime::now(),
            kind: EventKind::ToolResults(results),
        });
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Convert the timeline into protocol-specific chat messages.
    pub fn messages(&self, protocol: Protocol) -> Vec<Value> {
        match protocol {
            Protocol::Anthropic => self.anthropic_messages(),
            Protocol::OpenAi => self.openai_messages(),
        }
    }

    fn anthropic_messages(&self) -> Vec<Value> {
        let mut messages = Vec::new();
        for event in &self.events {
            match &event.kind {
                EventKind::User(text) => {
                    messages.push(json!({"role": "user", "content": text}));
                }
                EventKind::Assistant(blocks) => {
                    let content: Vec<Value> = blocks
                        .iter()
                        .map(|block| match block {
                            Block::Text(text) => json!({"type": "text", "text": text}),
                            Block::ToolUse {
                                id, name, input, ..
                            } => json!({
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": input,
                            }),
                        })
                        .collect();
                    messages.push(json!({"role": "assistant", "content": content}));
                }
                EventKind::ToolResults(results) => {
                    let content: Vec<Value> = results
                        .iter()
                        .map(|result| {
                            json!({
                                "type": "tool_result",
                                "tool_use_id": result.tool_use_id,
                                "content": result.content,
                            })
                        })
                        .collect();
                    messages.push(json!({"role": "user", "content": content}));
                }
            }
        }
        messages
    }

    fn openai_messages(&self) -> Vec<Value> {
        let mut messages = Vec::new();
        for event in &self.events {
            match &event.kind {
                EventKind::User(text) => {
                    messages.push(json!({"role": "user", "content": text}));
                }
                EventKind::Assistant(blocks) => {
                    let text = blocks
                        .iter()
                        .filter_map(|block| match block {
                            Block::Text(text) => Some(text.as_str()),
                            Block::ToolUse { .. } => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let calls: Vec<Value> = blocks
                        .iter()
                        .filter_map(|block| match block {
                            Block::ToolUse {
                                id, name, input, ..
                            } => Some(json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": input.to_string(),
                                },
                            })),
                            Block::Text(_) => None,
                        })
                        .collect();
                    let mut message = json!({
                        "role": "assistant",
                        "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                    });
                    if !calls.is_empty() {
                        message["tool_calls"] = Value::Array(calls);
                    }
                    messages.push(message);
                }
                EventKind::ToolResults(results) => {
                    for result in results {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": result.tool_use_id,
                            "content": result.content,
                        }));
                    }
                }
            }
        }
        messages
    }

    /// Human-readable timeline entries: (role, preview, age in seconds).
    pub fn timeline(&self) -> Vec<(String, String, u64)> {
        self.events
            .iter()
            .map(|event| {
                let (role, preview) = match &event.kind {
                    EventKind::User(text) => ("user", first_line(text)),
                    EventKind::Assistant(blocks) => {
                        let text = blocks
                            .iter()
                            .filter_map(|block| match block {
                                Block::Text(text) => Some(text.as_str()),
                                Block::ToolUse { .. } => None,
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        if text.is_empty() {
                            let tools = blocks
                                .iter()
                                .filter_map(|block| match block {
                                    Block::ToolUse { name, .. } => Some(name.as_str()),
                                    Block::Text(_) => None,
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            ("assistant", format!("[tools: {tools}]"))
                        } else {
                            ("assistant", first_line(&text))
                        }
                    }
                    EventKind::ToolResults(results) => {
                        let preview = results
                            .first()
                            .map(|result| first_line(&result.content))
                            .unwrap_or_default();
                        ("tools", preview)
                    }
                };
                let age = SystemTime::now()
                    .duration_since(event.at)
                    .unwrap_or_default()
                    .as_secs();
                (role.to_string(), preview, age)
            })
            .collect()
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let mut preview: String = line.chars().take(70).collect();
    if line.chars().count() > 70 {
        preview.push_str("...");
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_feed() -> Feed {
        let mut feed = Feed::default();
        feed.push_user("inspect");
        feed.push_assistant(vec![
            Block::Text("checking".to_string()),
            Block::ToolUse {
                id: "tool_1".to_string(),
                name: "read".to_string(),
                input: json!({ "path": "file.txt" }),
                parse_error: None,
            },
        ]);
        feed.push_tool_results(vec![ToolResult {
            tool_use_id: "tool_1".to_string(),
            content: "contents".to_string(),
        }]);
        feed
    }

    #[test]
    fn anthropic_messages_keep_tool_blocks() {
        let messages = sample_feed().messages(Protocol::Anthropic);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][1]["type"], "tool_use");
        assert_eq!(messages[1]["content"][1]["id"], "tool_1");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "tool_1");
    }

    #[test]
    fn openai_messages_use_tool_calls_and_tool_role() {
        let messages = sample_feed().messages(Protocol::OpenAi);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["tool_calls"][0]["function"]["name"], "read");
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"file.txt\"}"
        );
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "tool_1");
    }
}
