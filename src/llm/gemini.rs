//! Google Gemini generateContent adapter.

use super::{
    percent_encode, post_json, LlmConfig, LlmError, LlmProvider, LlmRequest, LlmResponse, Usage,
};
use serde_json::{json, Value};
use std::time::Instant;

pub struct Gemini {
    config: LlmConfig,
}

impl Gemini {
    pub fn new(config: &LlmConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }
}

impl LlmProvider for Gemini {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let model = self.config.model.trim().trim_start_matches("models/");
        if model.is_empty() {
            return Err(LlmError::MissingConfig);
        }
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.config.base_url.trim_end_matches('/'),
            model,
            percent_encode(&self.config.api_key)
        );
        let agent = ureq::AgentBuilder::new()
            .timeout(self.config.timeout)
            .build();
        let max_tokens = request.max_tokens.min(self.config.max_tokens).max(1);
        let mut generation = json!({
            "temperature": request.temperature.clamp(0.0, 2.0),
            "maxOutputTokens": max_tokens
        });
        if request.json {
            generation["responseMimeType"] = json!("application/json");
        }
        let body = json!({
            "systemInstruction": {"parts": [{"text": request.system}]},
            "contents": [{"role": "user", "parts": [{"text": request.user}]}],
            "generationConfig": generation
        });
        let started = Instant::now();
        let text = post_json(&agent, &url, &[], &body)?;
        parse_gemini_response(
            &text,
            &self.config.model,
            started.elapsed().as_millis() as u64,
        )
    }
}

fn parse_gemini_response(
    text: &str,
    fallback_model: &str,
    latency_ms: u64,
) -> Result<LlmResponse, LlmError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| LlmError::BadResponse(format!("invalid JSON: {}", error)))?;
    if let Some(reason) = value
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        return Err(LlmError::BadResponse(format!(
            "the prompt was blocked: {}",
            reason
        )));
    }
    let content = value
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    if content.trim().is_empty() {
        return Err(LlmError::BadResponse("empty candidate text".to_string()));
    }
    let model = value
        .get("modelVersion")
        .and_then(Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();
    let input_tokens = value
        .pointer("/usageMetadata/promptTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let output_tokens = value
        .pointer("/usageMetadata/candidatesTokenCount")
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
    fn parses_generate_content_response() {
        let text = r#"{"modelVersion":"gemini-2.5-flash","candidates":[{"content":{"parts":[{"text":"{\"ok\":"},{"text":"true}"}]}}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":3}}"#;
        let response = parse_gemini_response(text, "fallback", 9).unwrap();
        assert_eq!(response.text, "{\"ok\":true}");
        assert_eq!(response.model, "gemini-2.5-flash");
        assert_eq!(response.usage.total(), 8);
    }
}
