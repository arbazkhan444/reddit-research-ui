//! Anthropic Messages API adapter.

use super::{post_json, LlmConfig, LlmError, LlmProvider, LlmRequest, LlmResponse, Usage};
use serde_json::{json, Value};
use std::time::Instant;

pub struct Anthropic {
    config: LlmConfig,
}

impl Anthropic {
    pub fn new(config: &LlmConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }
}

impl LlmProvider for Anthropic {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let url = self.config.endpoint("messages");
        let agent = ureq::AgentBuilder::new()
            .timeout(self.config.timeout)
            .build();
        let max_tokens = request.max_tokens.min(self.config.max_tokens).max(1);
        let body = json!({
            "model": self.config.model,
            "max_tokens": max_tokens,
            "system": request.system,
            "messages": [{"role": "user", "content": request.user}],
            "temperature": request.temperature.clamp(0.0, 1.0)
        });
        let headers = [
            ("x-api-key", self.config.api_key.clone()),
            ("anthropic-version", "2023-06-01".to_string()),
        ];
        let started = Instant::now();
        let text = post_json(&agent, &url, &headers, &body)?;
        parse_anthropic_response(
            &text,
            &self.config.model,
            started.elapsed().as_millis() as u64,
        )
    }
}

fn parse_anthropic_response(
    text: &str,
    fallback_model: &str,
    latency_ms: u64,
) -> Result<LlmResponse, LlmError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| LlmError::BadResponse(format!("invalid JSON: {}", error)))?;
    let parts = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| LlmError::BadResponse("missing content".to_string()))?;
    let content = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    if content.trim().is_empty() {
        return Err(LlmError::BadResponse("empty message content".to_string()));
    }
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();
    let input_tokens = value
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let output_tokens = value
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    Ok(LlmResponse {
        text: content,
        model,
        usage: Usage {
            input_tokens,
            output_tokens,
        },
        latency_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_messages_response() {
        let text = r#"{"model":"claude-sonnet-5-5","content":[{"type":"text","text":"hello"},{"type":"text","text":" world"}],"usage":{"input_tokens":4,"output_tokens":2}}"#;
        let response = parse_anthropic_response(text, "fallback", 8).unwrap();
        assert_eq!(response.text, "hello world");
        assert_eq!(response.model, "claude-sonnet-5-5");
        assert_eq!(response.usage.total(), 6);
    }
}
