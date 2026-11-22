//! Provider-neutral LLM access for planning, criticism, and synthesis.

pub mod anthropic;
pub mod gemini;
pub mod openai_compat;
pub mod prompts;

use crate::config::SavedConfig;
use serde_json::Value;
use std::fmt;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProviderKind {
    DeepSeek,
    OpenAi,
    Anthropic,
    Gemini,
    Ollama,
    Custom,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 6] = [
        ProviderKind::DeepSeek,
        ProviderKind::OpenAi,
        ProviderKind::Anthropic,
        ProviderKind::Gemini,
        ProviderKind::Ollama,
        ProviderKind::Custom,
    ];

    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::DeepSeek => "deepseek",
            ProviderKind::OpenAi => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Ollama => "ollama",
            ProviderKind::Custom => "custom",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::DeepSeek => "DeepSeek",
            ProviderKind::OpenAi => "OpenAI",
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::Gemini => "Google Gemini",
            ProviderKind::Ollama => "Ollama (local)",
            ProviderKind::Custom => "Custom (OpenAI format)",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::DeepSeek => "https://api.deepseek.com/v1",
            ProviderKind::OpenAi => "https://api.openai.com/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com/v1",
            ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta",
            ProviderKind::Ollama => "http://127.0.0.1:11434/v1",
            ProviderKind::Custom => "",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            ProviderKind::DeepSeek => "deepseek-chat",
            ProviderKind::OpenAi => "gpt-4.1-mini",
            ProviderKind::Anthropic => "claude-sonnet-5-5",
            ProviderKind::Gemini => "gemini-2.5-flash",
            ProviderKind::Ollama => "llama3.2",
            ProviderKind::Custom => "",
        }
    }

    /// Ollama runs on this computer and needs no API key.
    pub fn needs_key(self) -> bool {
        self != ProviderKind::Ollama
    }

    pub fn from_id(id: &str) -> Self {
        match id.trim().to_ascii_lowercase().as_str() {
            "openai" => ProviderKind::OpenAi,
            "anthropic" => ProviderKind::Anthropic,
            "gemini" | "google" => ProviderKind::Gemini,
            "ollama" => ProviderKind::Ollama,
            "custom" => ProviderKind::Custom,
            _ => ProviderKind::DeepSeek,
        }
    }
}

/// Debug never prints the key. Only its length is shown.
impl std::fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key_len", &self.api_key.len())
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .finish()
    }
}

#[derive(Clone)]
pub struct LlmConfig {
    pub provider: ProviderKind,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub timeout: Duration,
}

impl LlmConfig {
    pub fn from_saved(config: &SavedConfig, api_key: String) -> Self {
        let provider = ProviderKind::from_id(&config.llm_provider);
        let base_url = if config.llm_base_url.trim().is_empty() {
            provider.default_base_url().to_string()
        } else {
            config.llm_base_url.trim().to_string()
        };
        let model = if config.llm_model.trim().is_empty() {
            provider.default_model().to_string()
        } else {
            config.llm_model.trim().to_string()
        };
        Self {
            provider,
            base_url,
            model,
            api_key: api_key.trim().to_string(),
            temperature: config.llm_temperature.clamp(0.0, 2.0),
            max_tokens: config.max_llm_output_tokens.max(64),
            timeout: Duration::from_secs(90),
        }
    }

    pub fn is_ready(&self) -> bool {
        !self.base_url.is_empty()
            && !self.model.is_empty()
            && (!self.provider.needs_key() || !self.api_key.is_empty())
    }

    pub fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

#[derive(Clone)]
pub struct LlmRequest {
    pub system: String,
    pub user: String,
    pub json: bool,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl Usage {
    pub fn total(self) -> u32 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

#[derive(Clone, Debug)]
pub struct LlmResponse {
    pub text: String,
    pub model: String,
    pub usage: Usage,
    pub latency_ms: u64,
}

#[derive(Debug)]
pub enum LlmError {
    MissingConfig,
    Network(String),
    Timeout,
    Auth(String),
    Balance(String),
    RateLimit(String),
    BadRequest(String),
    BadResponse(String),
}

impl fmt::Display for LlmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmError::MissingConfig => write!(formatter, "AI model is not configured"),
            LlmError::Network(message) => write!(formatter, "network error: {}", message),
            LlmError::Timeout => write!(formatter, "the model did not answer in time"),
            LlmError::Auth(message) => write!(formatter, "API key rejected: {}", message),
            LlmError::Balance(message) => {
                write!(formatter, "API balance is not enough: {}", message)
            }
            LlmError::RateLimit(message) => write!(formatter, "provider rate limit: {}", message),
            LlmError::BadRequest(message) => {
                write!(formatter, "provider rejected the request: {}", message)
            }
            LlmError::BadResponse(message) => {
                write!(
                    formatter,
                    "provider returned an invalid response: {}",
                    message
                )
            }
        }
    }
}

impl std::error::Error for LlmError {}

pub trait LlmProvider: Send + Sync {
    fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError>;
}

pub fn client(config: &LlmConfig) -> Result<Box<dyn LlmProvider>, LlmError> {
    if !config.is_ready() {
        return Err(LlmError::MissingConfig);
    }
    match config.provider {
        ProviderKind::Anthropic => Ok(Box::new(anthropic::Anthropic::new(config))),
        ProviderKind::Gemini => Ok(Box::new(gemini::Gemini::new(config))),
        _ => Ok(Box::new(openai_compat::OpenAiCompat::new(config))),
    }
}

pub(crate) fn post_json(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
) -> Result<String, LlmError> {
    let mut request = agent.post(url).set("Content-Type", "application/json");
    for (name, value) in headers {
        request = request.set(name, value);
    }
    match request.send_json(body) {
        Ok(response) => response
            .into_string()
            .map_err(|error| LlmError::Network(error.to_string())),
        Err(error) => {
            let mapped = match error {
                ureq::Error::Status(code, response) => {
                    map_status(code, &response.into_string().unwrap_or_default())
                }
                other => map_transport(other.to_string()),
            };
            Err(mapped)
        }
    }
}

fn map_status(code: u16, body: &str) -> LlmError {
    let detail = compact(body);
    match code {
        401 | 403 => LlmError::Auth(if detail.is_empty() {
            "check the key and its permissions".to_string()
        } else {
            detail
        }),
        402 => LlmError::Balance("add credit or choose another provider".to_string()),
        429 => LlmError::RateLimit(if detail.is_empty() {
            "wait, then try again".to_string()
        } else {
            detail
        }),
        400 | 404 | 422 => LlmError::BadRequest(detail),
        408 | 504 => LlmError::Timeout,
        _ => LlmError::Network(format!("HTTP {} {}", code, detail)),
    }
}

fn map_transport(text: String) -> LlmError {
    let lower = text.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        LlmError::Timeout
    } else {
        LlmError::Network(text)
    }
}

fn compact(body: &str) -> String {
    let one_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(320).collect()
}

/// Read a JSON object from model text. Handles fences and short prose.
pub fn parse_json_object(text: &str) -> Result<Value, String> {
    let cleaned = clean_text(text);
    if let Ok(value) = serde_json::from_str::<Value>(&cleaned) {
        if value.is_object() {
            return Ok(value);
        }
    }
    if let Some(block) = balanced_object(&cleaned) {
        if let Ok(value) = serde_json::from_str::<Value>(block) {
            if value.is_object() {
                return Ok(value);
            }
        }
    }
    Err("model did not return a JSON object".to_string())
}

fn clean_text(text: &str) -> String {
    let mut text = text
        .trim()
        .trim_start_matches('\u{feff}')
        .trim()
        .to_string();
    if text.starts_with("```") {
        let after = text.trim_start_matches("```");
        let after = after
            .split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or(after);
        text = after.trim().to_string();
        if let Some(stripped) = text.strip_suffix("```") {
            text = stripped.trim().to_string();
        }
    }
    text
}

fn balanced_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + offset + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_json() {
        let text = "```json\n{\"ok\": true}\n```";
        let value = parse_json_object(text).unwrap();
        assert_eq!(value["ok"], true);
    }

    #[test]
    fn extracts_json_after_prose() {
        let text = "Here is the plan:\n{\"question\": \"a {strange} question\", \"n\": 2}\nDone.";
        let value = parse_json_object(text).unwrap();
        assert_eq!(value["n"], 2);
        assert_eq!(value["question"], "a {strange} question");
    }

    #[test]
    fn rejects_plain_text() {
        assert!(parse_json_object("no json here").is_err());
    }

    #[test]
    fn encodes_gemini_keys() {
        assert_eq!(percent_encode("ab-_.~ /"), "ab-_.~%20%2F");
    }
}
