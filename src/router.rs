use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use ureq::Agent;

use crate::config::{endpoint, Settings};
use crate::feed::{Block, Feed};
use crate::presets::{Auth, Protocol};
use crate::tools;

pub struct Router {
    routes: Vec<Settings>,
    agent: Agent,
    counter: AtomicU64,
}

impl Router {
    pub fn new(routes: Vec<Settings>) -> Self {
        assert!(!routes.is_empty(), "router needs at least one route");
        let timeout = routes[0].timeout;
        let agent = ureq::AgentBuilder::new().timeout(timeout).build();
        Self {
            routes,
            agent,
            counter: AtomicU64::new(1),
        }
    }

    pub fn primary(&self) -> &Settings {
        &self.routes[0]
    }

    pub fn primary_mut(&mut self) -> &mut Settings {
        &mut self.routes[0]
    }

    pub fn route_names(&self) -> Vec<&str> {
        self.routes
            .iter()
            .map(|route| route.name.as_str())
            .collect()
    }

    pub fn available_models(&self) -> Result<Vec<String>, String> {
        let route = self.primary();
        fetch_model_ids(
            &route.base_url,
            route.auth,
            &route.api_key,
            &route.headers,
            route.timeout,
        )
    }

    /// Route a model call through the primary and any fallback routes.
    pub fn complete(
        &self,
        feed: &Feed,
        system: &str,
        max_tokens: Option<u32>,
    ) -> Result<Vec<Block>, String> {
        self.complete_with_tools(feed, system, max_tokens, true)
    }

    pub fn complete_with_tools(
        &self,
        feed: &Feed,
        system: &str,
        max_tokens: Option<u32>,
        include_tools: bool,
    ) -> Result<Vec<Block>, String> {
        let mut errors = Vec::new();
        for (index, route) in self.routes.iter().enumerate() {
            match self.complete_route(route, feed, system, max_tokens, include_tools) {
                Ok(blocks) => return Ok(blocks),
                Err(err) => {
                    let retryable = is_retryable(&err);
                    errors.push(format!("{}: {err}", route.name));
                    if !retryable || index + 1 == self.routes.len() {
                        break;
                    }
                }
            }
        }
        Err(errors.join(" | "))
    }

    fn complete_route(
        &self,
        route: &Settings,
        feed: &Feed,
        system: &str,
        max_tokens: Option<u32>,
        include_tools: bool,
    ) -> Result<Vec<Block>, String> {
        let url = endpoint(route);
        let token_limit = max_tokens.unwrap_or(route.max_tokens);
        let mut body = match route.protocol {
            Protocol::Anthropic => json!({
                "model": route.model,
                "max_tokens": token_limit,
                "messages": feed.messages(Protocol::Anthropic),
            }),
            Protocol::OpenAi => json!({
                "model": route.model,
                "messages": feed.messages(Protocol::OpenAi),
            }),
        };
        if include_tools {
            body["tools"] = json!(tools::schemas(route.protocol));
        }

        if !system.is_empty() {
            match route.protocol {
                Protocol::Anthropic => {
                    body["system"] = json!(system);
                }
                Protocol::OpenAi => {
                    if let Some(messages) = body["messages"].as_array_mut() {
                        messages.insert(0, json!({"role": "system", "content": system}));
                    }
                }
            }
        }
        if route.protocol == Protocol::OpenAi {
            body[route.max_tokens_param.as_str()] = json!(token_limit);
        }
        if let Some(temperature) = route.temperature {
            body["temperature"] = json!(temperature);
        }

        let mut request = self
            .agent
            .post(&url)
            .set("content-type", "application/json");
        if route.protocol == Protocol::Anthropic {
            request = request.set("anthropic-version", "2023-06-01");
        }
        for (header, value) in &route.headers {
            request = request.set(header, value);
        }
        if !route.api_key.is_empty() {
            let value = format!("Bearer {}", route.api_key);
            request = match route.auth {
                Auth::Bearer => request.set("authorization", &value),
                Auth::ApiKey => request.set("api-key", &route.api_key),
                Auth::XApiKey => request.set("x-api-key", &route.api_key),
                Auth::None => request,
            };
        }

        let response = match request.send_json(body) {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                let body = response.into_string().unwrap_or_default();
                return Err(format!("HTTP {code}: {}", extract_error(&body)));
            }
            Err(ureq::Error::Transport(err)) => {
                return Err(format!("connection failed: {err}"));
            }
        };
        let data: Value = response
            .into_json()
            .map_err(|err| format!("invalid JSON from provider: {err}"))?;
        self.parse_response(route.protocol, &data)
    }

    fn parse_response(&self, protocol: Protocol, data: &Value) -> Result<Vec<Block>, String> {
        match protocol {
            Protocol::Anthropic => self.parse_anthropic(data),
            Protocol::OpenAi => self.parse_openai(data),
        }
    }

    fn parse_anthropic(&self, data: &Value) -> Result<Vec<Block>, String> {
        let content = data
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("unexpected response: {}", preview_json(data)))?;
        let mut blocks = Vec::new();
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            blocks.push(Block::Text(text.to_string()));
                        }
                    }
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| self.next_id("call"));
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    let (input, parse_error) = normalize_input(input);
                    blocks.push(Block::ToolUse {
                        id,
                        name,
                        input,
                        parse_error,
                    });
                }
                _ => {}
            }
        }
        Ok(blocks)
    }

    fn parse_openai(&self, data: &Value) -> Result<Vec<Block>, String> {
        let choices = data
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("unexpected response: {}", preview_json(data)))?;
        let message = choices
            .first()
            .and_then(|choice| choice.get("message"))
            .ok_or_else(|| format!("unexpected response: {}", preview_json(data)))?;
        let mut blocks = Vec::new();

        if let Some(content) = message.get("content") {
            if let Some(text) = content.as_str() {
                if !text.is_empty() {
                    blocks.push(Block::Text(text.to_string()));
                }
            } else if let Some(parts) = content.as_array() {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            blocks.push(Block::Text(text.to_string()));
                        }
                    }
                }
            }
        }

        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| self.next_id("call"));
                let function = call.get("function");
                let name = function
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let raw_args = function.and_then(|function| function.get("arguments"));
                let (input, parse_error) = match raw_args {
                    Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
                        Ok(value) => normalize_input(value),
                        Err(err) => (
                            json!({}),
                            Some(format!(
                                "invalid JSON for {name}: {err}; raw={}",
                                preview_text(text, 200)
                            )),
                        ),
                    },
                    Some(Value::Null) | None => (json!({}), None),
                    Some(value) => normalize_input(value.clone()),
                };
                blocks.push(Block::ToolUse {
                    id,
                    name,
                    input,
                    parse_error,
                });
            }
        }
        Ok(blocks)
    }

    fn next_id(&self, prefix: &str) -> String {
        let number = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}_{number}")
    }
}

fn normalize_input(input: Value) -> (Value, Option<String>) {
    if input.is_object() {
        (input, None)
    } else {
        (
            json!({}),
            Some(format!(
                "tool arguments must be a JSON object, got {}",
                value_type(&input)
            )),
        )
    }
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub fn fetch_model_ids(
    base_url: &str,
    auth: Auth,
    api_key: &str,
    headers: &[(String, String)],
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let mut request = agent.get(&url).set("accept", "application/json");
    for (header, value) in headers {
        request = request.set(header, value);
    }
    if !api_key.is_empty() {
        let value = format!("Bearer {api_key}");
        request = match auth {
            Auth::Bearer => request.set("authorization", &value),
            Auth::ApiKey => request.set("api-key", api_key),
            Auth::XApiKey => request.set("x-api-key", api_key),
            Auth::None => request,
        };
    }

    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            return Err(format!("HTTP {code}: {}", extract_error(&body)));
        }
        Err(ureq::Error::Transport(err)) => {
            return Err(format!("connection failed: {err}"));
        }
    };
    let data: Value = response
        .into_json()
        .map_err(|err| format!("invalid model-list JSON from provider: {err}"))?;
    let entries = data
        .as_array()
        .or_else(|| data.get("data").and_then(Value::as_array))
        .or_else(|| data.get("models").and_then(Value::as_array))
        .ok_or_else(|| "provider response did not contain a model list".to_string())?;

    let mut models: Vec<String> = entries
        .iter()
        .filter_map(|entry| {
            entry
                .as_str()
                .or_else(|| entry.get("id").and_then(Value::as_str))
                .or_else(|| entry.get("name").and_then(Value::as_str))
                .map(str::to_string)
        })
        .collect();
    models.sort();
    models.dedup();
    if models.is_empty() {
        return Err("provider returned no model IDs".to_string());
    }
    Ok(models)
}

fn is_retryable(error: &str) -> bool {
    error.starts_with("HTTP 429")
        || error.starts_with("HTTP 5")
        || error.contains("connection failed")
        || error.contains("timed out")
}

fn extract_error(body: &str) -> String {
    if let Ok(data) = serde_json::from_str::<Value>(body) {
        if let Some(message) = data.pointer("/error/message").and_then(Value::as_str) {
            return message.to_string();
        }
        if let Some(message) = data.get("message").and_then(Value::as_str) {
            return message.to_string();
        }
        if let Some(message) = data.get("error").and_then(Value::as_str) {
            return message.to_string();
        }
    }
    preview_text(body, 800)
}

fn preview_text(text: &str, limit: usize) -> String {
    let mut preview: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        preview.push_str("...");
    }
    preview
}

fn preview_json(value: &Value) -> String {
    preview_text(&value.to_string(), 500)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Router {
        Router::new(vec![Settings {
            name: "test".to_string(),
            protocol: Protocol::OpenAi,
            base_url: "https://example.com/v1".to_string(),
            model: "model".to_string(),
            api_key: String::new(),
            auth: Auth::None,
            headers: Vec::new(),
            max_tokens: 8192,
            max_tokens_param: "max_tokens".to_string(),
            temperature: None,
            timeout: Duration::from_secs(30),
            fallbacks: Vec::new(),
        }])
    }

    #[test]
    fn parses_anthropic_text_and_tool_use() {
        let data = json!({
            "content": [
                {"type": "text", "text": "working"},
                {"type": "tool_use", "id": "tool_1", "name": "read", "input": {"path": "a.rs"}}
            ]
        });
        let blocks = router().parse_anthropic(&data).unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], Block::Text(text) if text == "working"));
        assert!(matches!(
            &blocks[1],
            Block::ToolUse { id, name, parse_error: None, .. }
                if id == "tool_1" && name == "read"
        ));
    }

    #[test]
    fn parses_openai_text_and_reports_invalid_arguments() {
        let data = json!({
            "choices": [{
                "message": {
                    "content": [{"type": "text", "text": "working"}],
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {"name": "read", "arguments": "not json"}
                    }]
                }
            }]
        });
        let blocks = router().parse_openai(&data).unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], Block::Text(text) if text == "working"));
        assert!(matches!(
            &blocks[1],
            Block::ToolUse { id, parse_error: Some(error), .. }
                if id == "call_1" && error.contains("invalid JSON")
        ));
    }

    #[test]
    fn missing_response_shape_is_an_error() {
        let error = router().parse_anthropic(&json!({})).unwrap_err();
        assert!(error.contains("unexpected response"));
        let error = router().parse_openai(&json!({"choices": []})).unwrap_err();
        assert!(error.contains("unexpected response"));
    }

    #[test]
    fn non_object_tool_input_is_rejected() {
        let (input, error) = normalize_input(json!(["path"]));
        assert_eq!(input, json!({}));
        assert!(error.unwrap().contains("must be a JSON object"));
    }

    #[test]
    fn retryable_errors_include_rate_limits_and_transport_failures() {
        assert!(is_retryable("HTTP 429: slow down"));
        assert!(is_retryable("HTTP 503: unavailable"));
        assert!(is_retryable("connection failed: refused"));
        assert!(is_retryable("request timed out"));
        assert!(!is_retryable("HTTP 400: bad request"));
        assert!(!is_retryable("invalid JSON from provider"));
    }

    #[test]
    fn provider_error_bodies_are_condensed() {
        assert_eq!(
            extract_error(r#"{"error": {"message": "bad key"}}"#),
            "bad key"
        );
        assert_eq!(extract_error(r#"{"message": "bad model"}"#), "bad model");
        assert_eq!(extract_error("plain failure"), "plain failure");
    }
}
