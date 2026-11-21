use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use ureq::Agent;

use crate::config::{endpoint, Settings};
use crate::feed::{Block, Feed};
use crate::presets::{Auth, Protocol};
use crate::tools;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostTier {
    Free,
    Cheap,
    Standard,
    Expensive,
}

impl CostTier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Cheap => "cheap",
            Self::Standard => "standard",
            Self::Expensive => "expensive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl ModelPricing {
    pub fn tier(self) -> CostTier {
        if self.input_per_million == 0.0 && self.output_per_million == 0.0 {
            CostTier::Free
        } else if self.output_per_million <= 2.0 && self.input_per_million <= 1.0 {
            CostTier::Cheap
        } else if self.output_per_million <= 20.0 && self.input_per_million <= 10.0 {
            CostTier::Standard
        } else {
            CostTier::Expensive
        }
    }

    pub fn format_label(self) -> String {
        let tier = self.tier();
        if tier == CostTier::Free {
            return tier.label().to_string();
        }
        format!(
            "{} · {}/{} per 1M",
            tier.label(),
            format_usd(self.input_per_million),
            format_usd(self.output_per_million)
        )
    }

    pub fn compact_label(self) -> String {
        let tier = self.tier();
        if tier == CostTier::Free {
            return "[free]".to_string();
        }
        let short_tier = match tier {
            CostTier::Free => "free",
            CostTier::Cheap => "cheap",
            CostTier::Standard => "std",
            CostTier::Expensive => "exp",
        };
        format!(
            "[{}] {}/{}",
            short_tier,
            format_usd(self.input_per_million),
            format_usd(self.output_per_million)
        )
    }

    pub fn sort_key(self) -> f64 {
        self.input_per_million + self.output_per_million
    }

    pub fn estimate_cost(self, input_tokens: u64, output_tokens: u64) -> f64 {
        (input_tokens as f64 / 1_000_000.0) * self.input_per_million
            + (output_tokens as f64 / 1_000_000.0) * self.output_per_million
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub pricing: Option<ModelPricing>,
}

impl ModelInfo {
    pub fn display_label(&self) -> String {
        match self.pricing {
            Some(pricing) => format!("{} ({})", self.id, pricing.format_label()),
            None => self.id.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SessionUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reported_cost_usd: f64,
    pub has_reported_cost: bool,
}

#[derive(Clone)]
pub struct Router {
    routes: Vec<Settings>,
    agent: Agent,
    counter: std::sync::Arc<AtomicU64>,
    input_tokens: std::sync::Arc<AtomicU64>,
    output_tokens: std::sync::Arc<AtomicU64>,
    reported_cost_nanodollars: std::sync::Arc<AtomicU64>,
    reported_cost_calls: std::sync::Arc<AtomicU64>,
}

impl Router {
    pub fn new(routes: Vec<Settings>) -> Self {
        assert!(!routes.is_empty(), "router needs at least one route");
        let timeout = routes[0].timeout;
        let agent = ureq::AgentBuilder::new().timeout(timeout).build();
        Self {
            routes,
            agent,
            counter: std::sync::Arc::new(AtomicU64::new(1)),
            input_tokens: std::sync::Arc::new(AtomicU64::new(0)),
            output_tokens: std::sync::Arc::new(AtomicU64::new(0)),
            reported_cost_nanodollars: std::sync::Arc::new(AtomicU64::new(0)),
            reported_cost_calls: std::sync::Arc::new(AtomicU64::new(0)),
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

    pub fn available_models(&self) -> Result<Vec<ModelInfo>, String> {
        let route = self.primary();
        fetch_models(
            &route.base_url,
            route.auth,
            &route.api_key,
            &route.headers,
            route.timeout,
        )
    }

    pub fn usage(&self) -> SessionUsage {
        SessionUsage {
            input_tokens: self.input_tokens.load(Ordering::Relaxed),
            output_tokens: self.output_tokens.load(Ordering::Relaxed),
            reported_cost_usd: self.reported_cost_nanodollars.load(Ordering::Relaxed) as f64
                / 1_000_000_000.0,
            has_reported_cost: self.reported_cost_calls.load(Ordering::Relaxed) > 0,
        }
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
        const MAX_ROUTE_RETRIES: u32 = 2;
        const MAX_RATE_LIMIT_RETRIES: u32 = 4;
        let mut errors = Vec::new();
        for (index, route) in self.routes.iter().enumerate() {
            let mut attempt = 0;
            loop {
                if crate::ui::is_interrupted() {
                    return Err("interrupted".to_string());
                }
                match self.complete_route(route, feed, system, max_tokens, include_tools) {
                    Ok(blocks) => return Ok(blocks),
                    Err((err, retry_after)) => {
                        let retryable = is_retryable(&err);
                        let is_rate_limit = err.starts_with("HTTP 429");
                        let max_retries = if is_rate_limit {
                            MAX_RATE_LIMIT_RETRIES
                        } else {
                            MAX_ROUTE_RETRIES
                        };
                        if retryable && attempt < max_retries {
                            let delay = retry_delay(attempt, retry_after, is_rate_limit);
                            attempt += 1;
                            if is_rate_limit {
                                eprintln!(
                                    "rate limited by {}, retrying in {}s...",
                                    route.name,
                                    delay.as_secs()
                                );
                            }
                            if sleep_interruptible(delay) {
                                return Err("interrupted".to_string());
                            }
                            continue;
                        }
                        errors.push(format!("{}: {err}", route.name));
                        if !retryable || index + 1 == self.routes.len() {
                            return Err(errors.join(" | "));
                        }
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
    ) -> Result<Vec<Block>, (String, Option<u64>)> {
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
        request = apply_auth(request, route.auth, &route.api_key);

        let response = match request.send_json(body) {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                let retry_after = parse_retry_after(response.header("retry-after"));
                let body = response.into_string().unwrap_or_default();
                return Err((format_http_error(code, &body), retry_after));
            }
            Err(ureq::Error::Transport(err)) => {
                return Err((format!("connection failed: {err}"), None));
            }
        };
        let data: Value = response
            .into_json()
            .map_err(|err| (format!("invalid JSON from provider: {err}"), None))?;
        self.parse_response(route.protocol, &data)
            .map_err(|err| (err, None))
    }

    fn parse_response(&self, protocol: Protocol, data: &Value) -> Result<Vec<Block>, String> {
        let blocks = match protocol {
            Protocol::Anthropic => self.parse_anthropic(data)?,
            Protocol::OpenAi => self.parse_openai(data)?,
        };
        self.record_usage(protocol, data);
        Ok(blocks)
    }

    fn record_usage(&self, protocol: Protocol, data: &Value) {
        let Some(usage) = data.get("usage") else {
            return;
        };
        let (input, output) = match protocol {
            Protocol::Anthropic => (
                usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ),
            Protocol::OpenAi => (
                usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .or_else(|| usage.get("input_tokens").and_then(Value::as_u64))
                    .unwrap_or(0),
                usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .or_else(|| usage.get("output_tokens").and_then(Value::as_u64))
                    .unwrap_or(0),
            ),
        };
        if input > 0 {
            self.input_tokens.fetch_add(input, Ordering::Relaxed);
        }
        if output > 0 {
            self.output_tokens.fetch_add(output, Ordering::Relaxed);
        }
        if let Some(cost) = usage.get("cost").and_then(parse_non_negative_f64) {
            let nanodollars = (cost * 1_000_000_000.0).round() as u64;
            self.reported_cost_nanodollars
                .fetch_add(nanodollars, Ordering::Relaxed);
            self.reported_cost_calls.fetch_add(1, Ordering::Relaxed);
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
                        .filter(|id| !id.is_empty())
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
                    .filter(|id| !id.is_empty())
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
                    Some(Value::String(text)) if text.trim().is_empty() => (json!({}), None),
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

pub fn fetch_models(
    base_url: &str,
    auth: Auth,
    api_key: &str,
    headers: &[(String, String)],
    timeout: Duration,
) -> Result<Vec<ModelInfo>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let mut request = agent.get(&url).set("accept", "application/json");
    for (header, value) in headers {
        request = request.set(header, value);
    }
    request = apply_auth(request, auth, api_key);

    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            return Err(format_http_error(code, &body));
        }
        Err(ureq::Error::Transport(err)) => {
            return Err(format!("connection failed: {err}"));
        }
    };
    let data: Value = response
        .into_json()
        .map_err(|err| format!("invalid model-list JSON from provider: {err}"))?;
    let mut models = parse_model_list(&data)?;
    if !is_local_url(base_url)
        && !base_url
            .trim_end_matches('/')
            .starts_with("https://openrouter.ai")
        && models.iter().any(|model| model.pricing.is_none())
    {
        enrich_pricing_from_public_catalog(&mut models);
    }
    Ok(models)
}

pub fn enrich_pricing_from_public_catalog(models: &mut [ModelInfo]) {
    if models.iter().all(|model| model.pricing.is_some()) {
        return;
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    let Ok(response) = agent
        .get("https://openrouter.ai/api/v1/models")
        .set("accept", "application/json")
        .call()
    else {
        return;
    };
    let Ok(data) = response.into_json::<Value>() else {
        return;
    };
    if let Ok(catalog) = parse_model_list(&data) {
        enrich_models_from_catalog(models, &catalog);
    }
}

pub fn enrich_models_from_catalog(models: &mut [ModelInfo], catalog: &[ModelInfo]) {
    let mut exact: HashMap<&str, ModelPricing> = HashMap::new();
    let mut normalized: HashMap<String, ModelPricing> = HashMap::new();
    for entry in catalog {
        let Some(pricing) = entry.pricing else {
            continue;
        };
        exact.entry(entry.id.as_str()).or_insert(pricing);
        let short = entry
            .id
            .rsplit_once('/')
            .map(|(_, name)| name)
            .unwrap_or(entry.id.as_str());
        exact.entry(short).or_insert(pricing);
        normalized
            .entry(normalize_model_key(short))
            .or_insert(pricing);
    }
    for model in models {
        if model.pricing.is_some() {
            continue;
        }
        let short = model
            .id
            .rsplit_once('/')
            .map(|(_, name)| name)
            .unwrap_or(model.id.as_str());
        model.pricing = exact
            .get(model.id.as_str())
            .copied()
            .or_else(|| exact.get(short).copied())
            .or_else(|| normalized.get(&normalize_model_key(short)).copied());
    }
}

fn normalize_model_key(id: &str) -> String {
    id.to_ascii_lowercase().replace('.', "-")
}

pub fn is_local_url(base_url: &str) -> bool {
    let lower = base_url.trim().to_ascii_lowercase();
    lower.contains("://localhost")
        || lower.contains("://127.0.0.1")
        || lower.contains("://[::1]")
        || lower.contains("://0.0.0.0")
}

pub fn parse_model_list(data: &Value) -> Result<Vec<ModelInfo>, String> {
    let entries = data
        .as_array()
        .or_else(|| data.get("data").and_then(Value::as_array))
        .or_else(|| data.get("models").and_then(Value::as_array))
        .ok_or_else(|| "provider response did not contain a model list".to_string())?;

    let mut models: Vec<ModelInfo> = entries
        .iter()
        .filter_map(|entry| {
            let id = entry
                .as_str()
                .or_else(|| entry.get("id").and_then(Value::as_str))
                .or_else(|| entry.get("name").and_then(Value::as_str))?
                .to_string();
            let pricing = entry
                .get("pricing")
                .and_then(parse_model_pricing)
                .or_else(|| entry.get("cost").and_then(parse_opencode_cost));
            Some(ModelInfo { id, pricing })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|later, kept| {
        if later.id == kept.id {
            if kept.pricing.is_none() {
                kept.pricing = later.pricing;
            }
            true
        } else {
            false
        }
    });
    if models.is_empty() {
        return Err("provider returned no model IDs".to_string());
    }
    Ok(models)
}

fn parse_opencode_cost(cost: &Value) -> Option<ModelPricing> {
    let text = cost.get("text")?;
    let input_per_million = text
        .get("input")
        .and_then(|v| v.get("price"))
        .and_then(parse_non_negative_f64)?;
    let output_per_million = text
        .get("output")
        .and_then(|v| v.get("price"))
        .and_then(parse_non_negative_f64)?;
    Some(ModelPricing {
        input_per_million,
        output_per_million,
    })
}

fn parse_model_pricing(pricing: &Value) -> Option<ModelPricing> {
    let input_per_token = pricing
        .get("prompt")
        .or_else(|| pricing.get("input"))
        .and_then(parse_non_negative_f64)?;
    let output_per_token = pricing
        .get("completion")
        .or_else(|| pricing.get("output"))
        .and_then(parse_non_negative_f64)?;
    Some(ModelPricing {
        input_per_million: input_per_token * 1_000_000.0,
        output_per_million: output_per_token * 1_000_000.0,
    })
}

fn parse_non_negative_f64(value: &Value) -> Option<f64> {
    let parsed = match value {
        Value::Number(num) => num.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if parsed.is_finite() && parsed >= 0.0 {
        Some(parsed)
    } else {
        None
    }
}

pub fn format_usd(amount: f64) -> String {
    if amount == 0.0 {
        return "$0".to_string();
    }
    if amount < 0.01 {
        let formatted = format!("{amount:.4}");
        let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
        return format!("${trimmed}");
    }
    format!("${amount:.2}")
}

fn apply_auth(request: ureq::Request, auth: Auth, api_key: &str) -> ureq::Request {
    if api_key.is_empty() {
        return request;
    }
    match auth {
        Auth::Bearer => request.set("authorization", &format!("Bearer {api_key}")),
        Auth::ApiKey => request.set("api-key", api_key),
        Auth::XApiKey => request.set("x-api-key", api_key),
        Auth::None => request,
    }
}

fn parse_retry_after(header: Option<&str>) -> Option<u64> {
    header?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| secs.clamp(1, 60))
}

fn retry_delay(attempt: u32, retry_after: Option<u64>, is_rate_limit: bool) -> Duration {
    let secs = retry_after.unwrap_or_else(|| {
        if is_rate_limit {
            (2u64 << attempt).min(30)
        } else {
            (1u64 << attempt).min(8)
        }
    });
    Duration::from_secs(secs)
}

fn sleep_interruptible(total: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < total {
        if crate::ui::is_interrupted() {
            return true;
        }
        let remaining = total.saturating_sub(start.elapsed());
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
    crate::ui::is_interrupted()
}

fn is_retryable(error: &str) -> bool {
    error.starts_with("HTTP 429")
        || error.starts_with("HTTP 5")
        || error.contains("connection failed")
        || error.contains("timed out")
}

pub(crate) fn format_http_error(code: u16, body: &str) -> String {
    let detail = extract_error(body);
    if code == 401 || code == 403 {
        format!("HTTP {code}: {detail} — check your API key (run /provider to reconfigure)")
    } else {
        format!("HTTP {code}: {detail}")
    }
}

pub fn extract_error(body: &str) -> String {
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

    #[test]
    fn auth_errors_include_provider_reconfigure_hint() {
        let msg = format_http_error(401, r#"{"error":{"message":"User Not Found"}}"#);
        assert!(msg.contains("HTTP 401: User Not Found"));
        assert!(msg.contains("/provider"));
    }

    #[test]
    fn parses_model_list_with_pricing_and_skips_negative() {
        let data = json!({
            "data": [
                {
                    "id": "anthropic/claude-sonnet-4.5",
                    "pricing": {"prompt": "0.000003", "completion": "0.000015"}
                },
                {
                    "id": "meta-llama/free-model",
                    "pricing": {"prompt": "0", "completion": "0"}
                },
                {
                    "id": "openrouter/auto",
                    "pricing": {"prompt": "-1", "completion": "-1"}
                },
                {
                    "id": "plain-model"
                }
            ]
        });
        let models = parse_model_list(&data).unwrap();
        assert_eq!(models.len(), 4);
        assert_eq!(models[0].id, "anthropic/claude-sonnet-4.5");
        assert_eq!(
            models[0].pricing,
            Some(ModelPricing {
                input_per_million: 3.0,
                output_per_million: 15.0,
            })
        );
        assert_eq!(
            models[0].display_label(),
            "anthropic/claude-sonnet-4.5 (standard · $3.00/$15.00 per 1M)"
        );
        assert_eq!(models[1].display_label(), "meta-llama/free-model (free)");
        assert_eq!(models[2].pricing, None);
        assert_eq!(models[3].pricing, None);
    }

    #[test]
    fn formats_small_and_standard_pricing_labels() {
        let small = ModelPricing {
            input_per_million: 0.0025,
            output_per_million: 0.01,
        };
        assert_eq!(small.format_label(), "cheap · $0.0025/$0.01 per 1M");
        let expensive = ModelPricing {
            input_per_million: 15.0,
            output_per_million: 75.0,
        };
        assert_eq!(expensive.tier(), CostTier::Expensive);
        assert_eq!(expensive.format_label(), "expensive · $15.00/$75.00 per 1M");
        assert!((small.estimate_cost(1_000_000, 500_000) - 0.0075).abs() < 1e-9);
    }

    #[test]
    fn records_anthropic_and_openai_usage_and_cost() {
        let router = router();
        router.record_usage(
            Protocol::Anthropic,
            &json!({"usage": {"input_tokens": 120, "output_tokens": 30}}),
        );
        router.record_usage(
            Protocol::OpenAi,
            &json!({"usage": {"prompt_tokens": 80, "completion_tokens": 20, "cost": 0.0015}}),
        );
        let usage = router.usage();
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 50);
        assert!(usage.has_reported_cost);
        assert!((usage.reported_cost_usd - 0.0015).abs() < 1e-9);
    }

    #[test]
    fn enriches_missing_pricing_from_catalog() {
        let catalog = vec![
            ModelInfo {
                id: "anthropic/claude-sonnet-4.5".to_string(),
                pricing: Some(ModelPricing {
                    input_per_million: 3.0,
                    output_per_million: 15.0,
                }),
            },
            ModelInfo {
                id: "deepseek/deepseek-chat".to_string(),
                pricing: Some(ModelPricing {
                    input_per_million: 0.27,
                    output_per_million: 1.1,
                }),
            },
        ];
        let existing_override = ModelPricing {
            input_per_million: 1.0,
            output_per_million: 2.0,
        };
        let mut models = vec![
            ModelInfo {
                id: "claude-sonnet-4-5".to_string(),
                pricing: None,
            },
            ModelInfo {
                id: "deepseek-chat".to_string(),
                pricing: Some(existing_override),
            },
            ModelInfo {
                id: "unknown-local".to_string(),
                pricing: None,
            },
        ];
        enrich_models_from_catalog(&mut models, &catalog);
        assert_eq!(models[0].pricing, catalog[0].pricing);
        assert_eq!(models[1].pricing, Some(existing_override));
        assert_eq!(models[2].pricing, None);
    }

    #[test]
    fn detects_local_urls() {
        assert!(is_local_url("http://localhost:11434/v1"));
        assert!(is_local_url("http://127.0.0.1:8080/v1"));
        assert!(!is_local_url("https://api.openai.com/v1"));
    }

    #[test]
    fn parses_opencode_cost_schema() {
        let data = json!({
            "data": [
                {
                    "id": "big-pickle",
                    "cost": {"text": {"input": {"price": 0}, "output": {"price": 0}}}
                },
                {
                    "id": "claude-fable-5",
                    "cost": {"text": {"input": {"price": 10}, "output": {"price": 50}}}
                }
            ]
        });
        let models = parse_model_list(&data).unwrap();
        assert_eq!(models[0].id, "big-pickle");
        assert_eq!(models[0].pricing.unwrap().tier(), CostTier::Free);
        assert_eq!(models[1].id, "claude-fable-5");
        assert_eq!(models[1].pricing.unwrap().tier(), CostTier::Expensive);
        assert_eq!(
            models[1].display_label(),
            "claude-fable-5 (expensive · $10.00/$50.00 per 1M)"
        );
    }
}
