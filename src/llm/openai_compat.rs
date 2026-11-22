//! OpenAI-compatible chat completions: DeepSeek, OpenAI, Ollama, and custom.

use super::{post_json, LlmConfig, LlmError, LlmProvider, LlmRequest, LlmResponse, Usage};
use serde_json::{json, Value};
use std::time::Instant;

pub struct OpenAiCompat {
    config: LlmConfig,
}

impl OpenAiCompat {
    pub fn new(config: &LlmConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    fn omit_sampling(&self) -> bool {
        let model = self.config.model.to_ascii_lowercase();
        model.contains("reasoner")
            || model.starts_with("o1")
            || model.starts_with("o3")
            || model.starts_with("o4")
    }

    fn body(&self, request: &LlmRequest, json_mode: bool) -> Value {
        let max_tokens = request.max_tokens.min(self.config.max_tokens).max(1);
        let mut body = json!({
            "model": self.config.model,
            "messages": [
                {"role": "system", "content": request.system},
                {"role": "user", "content": request.user}
            ],
            "max_tokens": max_tokens,
            "stream": false
        });
        if !self.omit_sampling() {
            body["temperature"] = json!(request.temperature.clamp(0.0, 2.0));
        }
        if json_mode {
            body["response_format"] = json!({"type": "json_object"});
        }
        body
    }
}

impl LlmProvider for OpenAiCompat {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let url = self.config.endpoint("chat/completions");
        let agent = ureq::AgentBuilder::new()
            .timeout(self.config.timeout)
            .build();
        let mut headers: Vec<(&str, String)> = Vec::new();
        if !self.config.api_key.is_empty() {
            headers.push(("Authorization", format!("Bearer {}", self.config.api_key)));
        }
        let mut body = self.body(request, request.json);
        let started = Instant::now();
        let text = match post_json(&agent, &url, &headers, &body) {
            Ok(text) => text,
            Err(LlmError::BadRequest(_)) if request.json => {
                // Some local servers do not support response_format. Ask again without it.
                if let Some(map) = body.as_object_mut() {
                    map.remove("response_format");
                }
                post_json(&agent, &url, &headers, &body)?
            }
            Err(error) => return Err(error),
        };
        parse_chat_response(
            &text,
            &self.config.model,
            started.elapsed().as_millis() as u64,
        )
    }
}

pub(crate) fn parse_chat_response(
    text: &str,
    fallback_model: &str,
    latency_ms: u64,
) -> Result<LlmResponse, LlmError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| LlmError::BadResponse(format!("invalid JSON: {}", error)))?;
    let content = content_text(value.pointer("/choices/0/message/content"))
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| LlmError::BadResponse("empty message content".to_string()))?;
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();
    let input_tokens = value
        .pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let output_tokens = value
        .pointer("/usage/completion_tokens")
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

fn content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    let parts = value.as_array()?;
    let joined = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    (!joined.is_empty()).then_some(joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ProviderKind;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn parses_chat_completion() {
        let text = r#"{"model":"deepseek-chat","choices":[{"message":{"content":"{\"ok\":true}"}}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#;
        let response = parse_chat_response(text, "fallback", 12).unwrap();
        assert_eq!(response.text, "{\"ok\":true}");
        assert_eq!(response.model, "deepseek-chat");
        assert_eq!(response.usage.total(), 10);
    }

    #[test]
    fn posts_to_chat_completions_and_reads_content() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    head.push_str(&line);
                    break;
                }
                head.push_str(&line);
            }
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8_lossy(&body).to_string();
            assert!(head.starts_with("POST /v1/chat/completions "));
            assert!(head
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key"));
            assert!(body.contains("\"response_format\""));
            let payload = r#"{"model":"deepseek-chat","choices":[{"message":{"content":"{\"ok\":true}"}}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let config = LlmConfig {
            provider: ProviderKind::DeepSeek,
            base_url: format!("http://{}/v1", address),
            model: "deepseek-chat".to_string(),
            api_key: "test-key".to_string(),
            temperature: 0.0,
            max_tokens: 100,
            timeout: Duration::from_secs(5),
        };
        let client = OpenAiCompat::new(&config);
        let response = client
            .complete(&LlmRequest {
                system: "Return JSON.".to_string(),
                user: "ping".to_string(),
                json: true,
                max_tokens: 50,
                temperature: 0.0,
            })
            .unwrap();
        assert_eq!(response.text, "{\"ok\":true}");
        assert_eq!(response.usage.total(), 10);
        server.join().unwrap();
    }
}
