use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub const DEFAULT_SUBS: &str = "ADHD, adhdwomen, adderall, ADHD_partners, StopSpeeding, Stims, ADHDers, twoXADHD, askdrugs, Nootropics, ADHD_Programmers, Drugs, psychiatry, ADHDUK";

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedConfig {
    pub topic: String,
    pub subs: String,
    pub posts: bool,
    pub comments: bool,
    pub threads: bool,
    pub profiles: bool,
    pub all_reddit: bool,
    pub official_search: bool,
    pub official_threads: bool,
    pub limit: usize,
    pub official_limit: usize,
    pub threads_count: usize,
    pub profiles_count: usize,
    pub years_back: i64,
    pub rpm: u32,
    pub archive_interval: f32,
    pub pullpush_interval: f32,
    pub max_requests: usize,
    pub client_id: String,
    pub client_secret: String,
    pub username: String,
    pub dark: bool,
    pub llm_provider: String,
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm_temperature: f32,
    pub max_llm_calls: usize,
    pub max_llm_output_tokens: u32,
    pub max_rounds: usize,
    pub review_plan: bool,
}

impl Default for SavedConfig {
    fn default() -> Self {
        Self {
            topic: String::new(),
            subs: DEFAULT_SUBS.into(),
            posts: true,
            comments: true,
            threads: true,
            profiles: true,
            all_reddit: true,
            official_search: false,
            official_threads: false,
            limit: 25,
            official_limit: 100,
            threads_count: 8,
            profiles_count: 5,
            years_back: 0,
            rpm: 45,
            archive_interval: 1.5,
            pullpush_interval: 2.0,
            max_requests: 400,
            client_id: String::new(),
            client_secret: String::new(),
            username: String::new(),
            dark: true,
            llm_provider: "deepseek".into(),
            llm_base_url: "https://api.deepseek.com/v1".into(),
            llm_model: "deepseek-chat".into(),
            llm_temperature: 0.2,
            max_llm_calls: 12,
            max_llm_output_tokens: 3000,
            max_rounds: 2,
            review_plan: false,
        }
    }
}

pub fn app_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("RedditResearch")
}

pub fn runs_dir() -> PathBuf {
    app_dir().join("runs")
}

fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

pub fn load() -> SavedConfig {
    fs::read_to_string(config_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(config: &SavedConfig) {
    let dir = app_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(text) = serde_json::to_string_pretty(config) {
        let _ = fs::write(config_path(), text);
    }
}

pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "topic".into()
    } else {
        out
    }
}
