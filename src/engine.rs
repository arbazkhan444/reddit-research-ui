use crate::config::{self, SavedConfig};
use crate::critic::{self, Coverage, GapAnalysis};
use crate::draft::{self, DraftReport};
use crate::evidence;
use crate::llm::LlmConfig;
use crate::model::{self, Record};
use crate::plan::ResearchPlan;
use crate::report;
use crate::store::Store;
use crate::synth;
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ARCHIVE: &str = "https://arctic-shift.photon-reddit.com/api";
/// Public Reddit archive with keyword search across all subreddits.
const PULLPUSH: &str = "https://api.pullpush.io/reddit/search";
const TOKEN_URL: &str = "https://www.reddit.com/api/v1/access_token";
const OAUTH: &str = "https://oauth.reddit.com";

#[derive(Clone, Debug)]
pub struct Creds {
    pub client_id: String,
    pub client_secret: String,
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub topic: String,
    pub subs: Vec<String>,
    pub plan: Option<ResearchPlan>,
    pub plan_source: String,
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
    pub max_rounds: usize,
    pub max_llm_calls: usize,
    pub llm: Option<LlmConfig>,
    pub account: Option<Creds>,
}

impl Settings {
    pub fn from_config(cfg: &SavedConfig, password: String) -> Self {
        let account = if !cfg.client_id.trim().is_empty()
            && !cfg.client_secret.trim().is_empty()
            && !cfg.username.trim().is_empty()
            && !password.is_empty()
        {
            Some(Creds {
                client_id: cfg.client_id.trim().to_string(),
                client_secret: cfg.client_secret.trim().to_string(),
                username: cfg.username.trim().to_string(),
                password,
            })
        } else {
            None
        };
        Self {
            topic: cfg.topic.trim().to_string(),
            subs: parse_subs(&cfg.subs),
            plan: None,
            plan_source: String::new(),
            posts: cfg.posts,
            comments: cfg.comments,
            threads: cfg.threads,
            profiles: cfg.profiles,
            all_reddit: cfg.all_reddit,
            official_search: cfg.official_search,
            official_threads: cfg.official_threads,
            limit: cfg.limit,
            official_limit: cfg.official_limit,
            threads_count: cfg.threads_count,
            profiles_count: cfg.profiles_count,
            years_back: cfg.years_back,
            rpm: cfg.rpm,
            archive_interval: cfg.archive_interval,
            pullpush_interval: cfg.pullpush_interval.clamp(1.0, 10.0),
            max_requests: cfg.max_requests,
            max_rounds: cfg.max_rounds.min(3),
            max_llm_calls: cfg.max_llm_calls.max(1),
            llm: None,
            account,
        }
    }

    pub fn enabled_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if self.all_reddit {
            names.push("all_reddit".to_string());
        }
        if self.posts {
            names.push("archive_posts".to_string());
        }
        if self.comments {
            names.push("archive_comments".to_string());
        }
        if self.threads {
            names.push("archive_threads".to_string());
        }
        if self.profiles {
            names.push("archive_profiles".to_string());
        }
        if self.official_search && self.account.is_some() {
            names.push("official_search".to_string());
        }
        if self.official_threads && self.account.is_some() {
            names.push("official_threads".to_string());
        }
        names
    }

    pub fn any_tactic(&self) -> bool {
        !self.enabled_names().is_empty()
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct StageInfo {
    pub name: String,
    pub status: String,
    pub count: usize,
    pub note: String,
}

impl Default for StageInfo {
    fn default() -> Self {
        Self {
            name: String::new(),
            status: "pending".to_string(),
            count: 0,
            note: String::new(),
        }
    }
}

pub fn initial_stages() -> Vec<StageInfo> {
    [
        ("Plan", "Waiting for the plan."),
        ("Collect", "Waiting for the sources."),
        ("Rank", "Waiting for records."),
        ("Review", "AI review comes later."),
        ("Write", "Waiting for the draft."),
        ("Validate", "Waiting for quotes."),
        ("Export", "Waiting for save."),
    ]
    .into_iter()
    .map(|(name, note)| StageInfo {
        name: name.to_string(),
        status: "pending".to_string(),
        count: 0,
        note: note.to_string(),
    })
    .collect()
}

pub enum Msg {
    Log(String),
    Progress {
        total: usize,
        requests: usize,
    },
    Stage(StageInfo),
    Auth(Result<String, String>),
    Done {
        records: Vec<Record>,
        total: usize,
        dir: String,
        error: Option<String>,
        draft: Option<DraftReport>,
        /// `report.md` text, from AI synthesis or the extractive fallback.
        report_markdown: Option<String>,
        /// True when the model wrote the report.
        report_ai: bool,
        stages: Vec<StageInfo>,
    },
}

#[derive(Debug)]
enum HttpErr {
    Cancelled,
    Budget(String),
    TooHeavy,
    Forbidden,
    NotFound,
    Auth(String),
    Other(String),
}

impl HttpErr {
    fn fatal(&self) -> bool {
        matches!(
            self,
            HttpErr::Cancelled | HttpErr::Budget(_) | HttpErr::Auth(_)
        )
    }
}

impl std::fmt::Display for HttpErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpErr::Cancelled => write!(f, "cancelled"),
            HttpErr::Budget(message) => write!(f, "{}", message),
            HttpErr::TooHeavy => write!(f, "query too heavy for the archive"),
            HttpErr::Forbidden => write!(f, "no access"),
            HttpErr::NotFound => write!(f, "not found"),
            HttpErr::Auth(message) => write!(f, "{}", message),
            HttpErr::Other(message) => write!(f, "{}", message),
        }
    }
}

impl From<String> for HttpErr {
    fn from(value: String) -> Self {
        HttpErr::Other(value)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    Posts,
    Comments,
}

struct Limiter {
    interval: Duration,
    next: Instant,
}

impl Limiter {
    fn new(seconds: f64) -> Self {
        Self {
            interval: Duration::from_secs_f64(seconds.max(0.05)),
            next: Instant::now(),
        }
    }

    fn wait(&mut self, cancel: &AtomicBool) -> Result<(), String> {
        let now = Instant::now();
        if self.next > now {
            let mut remaining = self.next - now;
            while remaining > Duration::ZERO {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }
                let step = remaining.min(Duration::from_millis(100));
                std::thread::sleep(step);
                remaining -= step;
            }
        }
        let base = Instant::now().max(self.next);
        self.next = base + self.interval;
        Ok(())
    }
}

pub fn parse_subs(text: &str) -> Vec<String> {
    let mut subs = Vec::new();
    for part in text.split([',', '\n', ';', ' ']) {
        let name = part
            .trim()
            .trim_start_matches("r/")
            .trim_start_matches('/')
            .trim();
        if !name.is_empty() && !subs.iter().any(|item: &String| item == name) {
            subs.push(name.to_string());
        }
    }
    subs
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

const STOP_WORDS: &[&str] = &[
    "a", "about", "after", "again", "against", "all", "also", "an", "and", "any", "are", "because",
    "been", "before", "being", "best", "between", "both", "but", "can", "could", "did", "do",
    "does", "doing", "down", "during", "each", "few", "for", "from", "get", "gets", "getting",
    "good", "had", "has", "have", "having", "help", "helping", "helps", "her", "here", "him",
    "his", "how", "into", "is", "it", "its", "just", "like", "make", "makes", "many", "me", "more",
    "most", "much", "must", "my", "need", "needs", "not", "now", "of", "off", "on", "once", "only",
    "or", "other", "our", "out", "over", "own", "same", "she", "should", "some", "still", "such",
    "than", "that", "the", "their", "them", "then", "there", "these", "they", "this", "those",
    "through", "to", "too", "under", "until", "use", "using", "very", "was", "way", "we", "were",
    "what", "when", "where", "which", "while", "who", "why", "will", "with", "work", "works",
    "would", "you", "your",
];

/// Reduce a topic to the words worth searching for.
pub(crate) fn topic_terms(topic: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in normalize(topic).split_whitespace() {
        let word: String = word.chars().filter(|ch| ch.is_alphanumeric()).collect();
        if word.len() < 3 || STOP_WORDS.contains(&word.as_str()) || terms.contains(&word) {
            continue;
        }
        terms.push(word);
    }
    terms
}

fn api_query(topic: &str) -> String {
    let terms = topic_terms(topic);
    if terms.is_empty() {
        normalize(topic)
    } else {
        terms.into_iter().take(5).collect::<Vec<_>>().join(" ")
    }
}

fn primary_query(topic: &str) -> String {
    topic_terms(topic)
        .into_iter()
        .max_by_key(|term| term.len())
        .unwrap_or_else(|| normalize(topic))
}

/// Add one query to the all-Reddit list. The `site:reddit.com` prefix belongs
/// to web search engines, so it is removed before a Reddit archive search.
fn push_global_query(out: &mut Vec<String>, text: &str) {
    let text = text.trim();
    let text = if text.to_ascii_lowercase().starts_with("site:reddit.com") {
        text[15..].trim()
    } else {
        text
    };
    let text = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect::<String>();
    if text.is_empty() || out.iter().any(|item| item.eq_ignore_ascii_case(&text)) {
        return;
    }
    out.push(text);
}

fn matches_topic(text: &str, topic: &str) -> bool {
    let topic = normalize(topic);
    if topic.is_empty() {
        return true;
    }
    let haystack = normalize(text);
    if haystack.contains(&topic) {
        return true;
    }
    let terms = topic_terms(&topic);
    if terms.is_empty() {
        let parts: Vec<&str> = topic
            .split_whitespace()
            .filter(|part| part.len() >= 3)
            .collect();
        return !parts.is_empty() && parts.iter().any(|part| haystack.contains(part));
    }
    terms.iter().all(|term| haystack.contains(term))
}

/// A plan query match is looser than an all-word topic match.
fn query_matches(text: &str, query: &str) -> bool {
    let haystack = normalize(text);
    let needle = normalize(query);
    if needle.is_empty() {
        return true;
    }
    if haystack.contains(&needle) {
        return true;
    }
    let terms = topic_terms(query);
    if terms.is_empty() {
        return true;
    }
    let needed = if terms.len() >= 2 { 2 } else { 1 };
    terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count()
        >= needed
}

fn primary_term(query: &str) -> String {
    topic_terms(query)
        .into_iter()
        .max_by_key(|term| term.len())
        .unwrap_or_else(|| normalize(query))
}

fn push_words(words: &mut Vec<String>, text: &str) {
    for term in topic_terms(text) {
        if !words.iter().any(|item| item == &term) {
            words.push(term);
        }
    }
}

fn date_start(value: &str) -> Option<f64> {
    chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|moment| moment.and_utc().timestamp() as f64)
}

fn noise(data: &Value) -> bool {
    let author = data
        .get("author")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if matches!(author, "AutoModerator" | "RemindMeBot" | "BotDefense") {
        return true;
    }
    let body = data
        .get("body")
        .or_else(|| data.get("selftext"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    matches!(body, "[removed]" | "[deleted]")
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(|item| item.as_str()).unwrap_or("")
}

fn array_of(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

fn query(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn user_agent(settings: &Settings) -> String {
    match &settings.account {
        Some(account) if !account.username.is_empty() => format!(
            "windows:reddit-research-ui:v1.0 (by /u/{})",
            account.username
        ),
        _ => "windows:reddit-research-ui:v1.0".to_string(),
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build()
}

fn basic_auth(user: &str, password: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", user, password))
}

fn login(agent: &ureq::Agent, creds: &Creds) -> Result<(String, f64), String> {
    let body = format!(
        "grant_type=password&username={}&password={}",
        encode(&creds.username),
        encode(&creds.password)
    );
    let response = agent
        .post(TOKEN_URL)
        .set(
            "Authorization",
            &format!(
                "Basic {}",
                basic_auth(&creds.client_id, &creds.client_secret)
            ),
        )
        .set(
            "User-Agent",
            &format!("windows:reddit-research-ui:v1.0 (by /u/{})", creds.username),
        )
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&body);
    match response {
        Ok(resp) => {
            let value = resp
                .into_json::<Value>()
                .map_err(|error| error.to_string())?;
            let token = value
                .get("access_token")
                .and_then(|item| item.as_str())
                .unwrap_or("")
                .to_string();
            if token.is_empty() {
                let message = value
                    .get("message")
                    .or_else(|| value.get("error"))
                    .and_then(|item| item.as_str())
                    .unwrap_or("no access token");
                return Err(format!("login failed: {}", message));
            }
            let expires = value
                .get("expires_in")
                .and_then(|item| item.as_f64())
                .unwrap_or(3600.0);
            Ok((token, expires))
        }
        Err(ureq::Error::Status(code, _)) => {
            if code == 401 {
                Err(
                    "login rejected: check the client ID, secret, username, and password"
                        .to_string(),
                )
            } else {
                Err(format!("login rejected (HTTP {})", code))
            }
        }
        Err(ureq::Error::Transport(error)) => Err(format!("network error: {}", error)),
    }
}

pub fn test_account(creds: Creds) -> Result<String, String> {
    let agent = agent();
    let (token, _) = login(&agent, &creds)?;
    let response = agent
        .get("https://oauth.reddit.com/api/v1/me?raw_json=1")
        .set("Authorization", &format!("bearer {}", token))
        .set(
            "User-Agent",
            &format!("windows:reddit-research-ui:v1.0 (by /u/{})", creds.username),
        )
        .call();
    match response {
        Ok(resp) => {
            let value = resp
                .into_json::<Value>()
                .map_err(|error| error.to_string())?;
            let name = value
                .get("name")
                .and_then(|item| item.as_str())
                .unwrap_or(&creds.username)
                .to_string();
            Ok(format!("Logged in as u/{}", name))
        }
        Err(ureq::Error::Status(code, _)) => Err(format!("account check failed (HTTP {})", code)),
        Err(ureq::Error::Transport(error)) => Err(format!("network error: {}", error)),
    }
}

pub fn spawn(settings: Settings, tx: Sender<Msg>, cancel: Arc<AtomicBool>) {
    std::thread::spawn(move || run(settings, tx, cancel));
}

fn write_json<T: serde::Serialize>(path: &std::path::Path, value: &T) -> Option<String> {
    match serde_json::to_string_pretty(value) {
        Ok(text) => fs::write(path, text).err().map(|error| error.to_string()),
        Err(error) => Some(error.to_string()),
    }
}

fn planned_query_count(plan: Option<&ResearchPlan>) -> usize {
    plan.map(|plan| {
        plan.source_queries.archive_posts.len()
            + plan.source_queries.archive_comments.len()
            + plan.source_queries.reddit_search.len()
            + plan.source_queries.web.len()
    })
    .unwrap_or(0)
}

fn cited_sources(report: &synth::SynthReport) -> usize {
    let mut ids: HashSet<&str> = HashSet::new();
    for section in &report.sections {
        for claim in &section.claims {
            for quote in &claim.quotes {
                ids.insert(quote.evidence_id.as_str());
            }
            for id in &claim.evidence_ids {
                ids.insert(id.as_str());
            }
        }
    }
    for item in &report.disagreements {
        for side in &item.sides {
            for id in &side.evidence_ids {
                ids.insert(id.as_str());
            }
        }
    }
    ids.len()
}

fn run(settings: Settings, tx: Sender<Msg>, cancel: Arc<AtomicBool>) {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = config::runs_dir().join(format!("{}-{}", stamp, config::slug(&settings.topic)));
    let topic = settings.topic.clone();
    let tactics = settings.enabled_names();
    let plan = settings.plan.clone();
    let plan_source = settings.plan_source.clone();
    let started = chrono::Local::now().to_rfc3339();
    let mut engine = Engine {
        settings,
        tx: tx.clone(),
        cancel,
        store: Store::new(dir),
        agent: agent(),
        archive_limiter: Limiter::new(1.2),
        reddit_limiter: Limiter::new(1.35),
        pullpush_limiter: Limiter::new(2.0),
        requests: 0,
        token: None,
        token_expiry: None,
        post_seeds: HashMap::new(),
        stages: initial_stages(),
        collect_notes: Vec::new(),
        queries_run: HashSet::new(),
        llm_calls: 0,
        round_limit: usize::MAX,
    };
    engine.archive_limiter = Limiter::new(engine.settings.archive_interval as f64);
    engine.reddit_limiter = Limiter::new(60.0 / engine.settings.rpm.max(1) as f64);
    engine.pullpush_limiter = Limiter::new(engine.settings.pullpush_interval as f64);
    engine.mark_plan();
    let error = engine.execute().err();
    let stats = engine.prepare_records();
    let review = if error.is_none() {
        engine.review_loop()
    } else {
        engine.set_stage("Review", "skipped", 0, "Collection stopped before review.");
        ReviewOutcome::default()
    };
    let final_stats = engine.prepare_records();
    let mut records: Vec<Record> = engine.store.records.values().cloned().collect();
    evidence::sort_ranked(&mut records);
    let total = records.len();
    engine.set_stage(
        "Rank",
        "done",
        records.len(),
        format!(
            "{} duplicates removed · {} ranked · {} sub-questions",
            stats.removed(),
            final_stats.kept,
            final_stats.questions
        ),
    );

    let pack = evidence::pack(&records, evidence::PACK_LIMIT);
    let draft = draft::build(plan.as_ref(), &topic, &pack, &plan_source, &tactics);
    let check = draft::validate(&draft, &records);

    // Final report. The model reads a fixed evidence pack. Without AI, the
    // extractive draft becomes the report.
    let synth_pack = synth::pack(
        &records,
        synth::MAX_EVIDENCE_RECORDS,
        synth::EVIDENCE_BUDGET_CHARS,
    );
    let subreddits: HashSet<String> = records
        .iter()
        .map(|record| record.subreddit.trim().to_lowercase())
        .filter(|sub| !sub.is_empty())
        .collect();
    let seed_subreddits = engine.planned_subs().len();
    let scope = if engine.settings.all_reddit {
        format!("all of Reddit plus {} seed subreddits", seed_subreddits)
    } else {
        format!("{} seed subreddits only", seed_subreddits)
    };
    let mut method = synth::MethodFacts {
        topic: topic.clone(),
        records: total,
        pack: synth_pack.len(),
        scope,
        subreddits: subreddits.len(),
        queries: planned_query_count(plan.as_ref()) + review.queries,
        requests: engine.requests,
        rounds: review.rounds,
        met: review.met,
        total: review.total,
        stop: review.stop.clone(),
        model: String::new(),
    };
    let mut synth_outcome: Option<synth::SynthOutcome> = None;
    let mut synth_error: Option<String> = None;
    let mut synth_skip = "no AI model is set";
    if let Some(config) = engine.settings.llm.clone() {
        if !config.is_ready() {
            synth_skip = "the AI model is not ready";
        } else if engine.llm_calls >= engine.settings.max_llm_calls {
            synth_skip = "the AI call limit is reached";
        } else if synth_pack.is_empty() {
            synth_skip = "the evidence pack is empty";
        } else if engine.cancel.load(Ordering::Relaxed) {
            synth_skip = "the run was stopped";
        } else {
            engine.set_stage(
                "Write",
                "running",
                0,
                format!("Writing the report with {}...", config.model),
            );
            engine.log("[synth] Writing the final report.".to_string());
            let outcome = synth::synthesize(&config, &topic, plan.as_ref(), &synth_pack);
            engine.llm_calls += 1;
            match outcome {
                Ok(outcome) => {
                    engine.log(format!(
                        "[synth] {} claims, {}/{} exact quotes, model {}.",
                        outcome.check.claims_kept,
                        outcome.check.quotes_ok,
                        outcome.check.quotes,
                        outcome.model
                    ));
                    method.model = outcome.model.clone();
                    synth_outcome = Some(outcome);
                }
                Err(error) => {
                    engine.log(format!(
                        "[synth] Failed: {}. Using the extractive draft.",
                        error
                    ));
                    synth_error = Some(error);
                }
            }
        }
    }
    let write_note = match &synth_outcome {
        Some(outcome) => format!(
            "{} sections · {} claims · {} exact quotes",
            outcome.report.sections.len(),
            outcome.check.claims_kept,
            outcome.check.quotes_ok
        ),
        None => match &synth_error {
            Some(error) => format!(
                "{} sections · {} excerpts (AI synthesis failed: {})",
                draft.sections.len(),
                check.total,
                error.chars().take(120).collect::<String>()
            ),
            None => format!(
                "{} sections · {} excerpts ({}; extractive draft)",
                draft.sections.len(),
                check.total,
                synth_skip
            ),
        },
    };
    let write_count = synth_outcome
        .as_ref()
        .map(|outcome| outcome.report.sections.len())
        .unwrap_or(draft.sections.len());
    engine.set_stage("Write", "done", write_count, write_note);
    let synth_check = synth_outcome.as_ref().map(|outcome| outcome.check);
    let cited = synth_outcome
        .as_ref()
        .map(|outcome| cited_sources(&outcome.report))
        .unwrap_or(0);
    let validation_status = match synth_check {
        Some(check) if check.claims_kept == 0 => "error",
        Some(_) => "done",
        None if check.total == 0 => "skipped",
        None if check.all_exact() && check.all_linked() => "done",
        None => "error",
    };
    let validate_note = match synth_check {
        Some(check) => format!(
            "{}/{} claims cited · {}/{} quotes exact · {} sources linked",
            check.claims_kept, check.claims, check.quotes_ok, check.quotes, cited
        ),
        None => format!(
            "{}/{} excerpts exact · {}/{} links",
            check.exact, check.total, check.linked, check.total
        ),
    };
    let validate_count = check
        .total
        .max(synth_check.map(|check| check.claims_kept).unwrap_or(0));
    engine.set_stage("Validate", validation_status, validate_count, validate_note);

    let markdown_text = match &synth_outcome {
        Some(outcome) => synth::to_markdown(&outcome.report, &synth_pack, &method),
        None => draft::to_markdown(&draft),
    };
    let extra = json!({
        "plan_source": plan_source.clone(),
        "plan": plan.clone(),
        "draft": draft.clone(),
        "synth": synth_outcome.as_ref().map(|outcome| json!({
            "report": &outcome.report,
            "check": outcome.check,
            "model": &outcome.model,
            "failures": &outcome.failures,
        })),
        "synth_error": synth_error.clone(),
        "stages": engine.stages.clone(),
    });
    let save_error = engine
        .store
        .save_with(
            &topic,
            &tactics,
            &started,
            error.as_deref().unwrap_or(""),
            &extra,
        )
        .err();
    let plan_error = plan
        .as_ref()
        .and_then(|value| write_json(&engine.store.dir.join("plan.json"), value));
    let draft_json_error = write_json(&engine.store.dir.join("draft-report.json"), &draft);
    let draft_md_error =
        draft::write_markdown(&draft, &engine.store.dir.join("draft-report.md")).err();
    let report_md_error =
        synth::write_markdown(&markdown_text, &engine.store.dir.join("report.md")).err();
    let report_json_error = synth_outcome.as_ref().and_then(|outcome| {
        write_json(
            &engine.store.dir.join("report.json"),
            &json!({
                "schema_version": 1,
                "topic": topic,
                "ai": true,
                "model": &outcome.model,
                "check": outcome.check,
                "failures": &outcome.failures,
                "report": &outcome.report,
            }),
        )
    });
    let evidence_error = write_json(
        &engine.store.dir.join("evidence.json"),
        &json!({
            "schema_version": 1,
            "topic": topic,
            "plan_source": plan_source,
            "pack": pack.len(),
            "stats": {
                "input": stats.input,
                "exact": stats.exact,
                "near": stats.near,
                "crossposts": stats.crossposts,
                "reposts": stats.reposts,
                "kept": final_stats.kept,
            },
            "review": {
                "rounds": review.rounds,
                "met": review.met,
                "total": review.total,
                "queries": review.queries,
                "llm_used": review.llm_used,
                "stop": review.stop,
                "coverage": review.coverage,
            },
            "records": &records,
        }),
    );
    let run_meta = json!({
        "schema_version": 1,
        "topic": topic,
        "plan_source": plan_source,
        "started": started,
        "finished": chrono::Local::now().to_rfc3339(),
        "error": error.clone().unwrap_or_default(),
        "total": total,
        "tactics": tactics.clone(),
        "stages": engine.stages.clone(),
        "ranking": {
            "input": stats.input,
            "exact": stats.exact,
            "near": stats.near,
            "crossposts": stats.crossposts,
            "reposts": stats.reposts,
            "kept": final_stats.kept,
            "pack": pack.len(),
        },
        "review": {
            "rounds": review.rounds,
            "met": review.met,
            "total": review.total,
            "queries": review.queries,
            "llm_used": review.llm_used,
            "stop": review.stop,
            "coverage": review.coverage,
        },
        "quote_check": {
            "total": check.total,
            "exact": check.exact,
            "linked": check.linked,
        },
        "synthesis": {
            "used": synth_outcome.is_some(),
            "model": method.model,
            "claims": synth_check.map(|check| check.claims).unwrap_or(0),
            "claims_kept": synth_check.map(|check| check.claims_kept).unwrap_or(0),
            "quotes": synth_check.map(|check| check.quotes).unwrap_or(0),
            "quotes_ok": synth_check.map(|check| check.quotes_ok).unwrap_or(0),
            "sources": cited,
            "error": synth_error,
        },
    });
    let run_error = write_json(&engine.store.dir.join("run.json"), &run_meta);
    let report_error =
        report::write_report(&records, &topic, &engine.store.dir.join("report.html")).err();
    let dir_text = engine.store.dir.display().to_string();
    let file_count = fs::read_dir(&engine.store.dir)
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    let export_error = save_error.is_some()
        || report_error.is_some()
        || plan_error.is_some()
        || draft_json_error.is_some()
        || draft_md_error.is_some()
        || report_md_error.is_some()
        || report_json_error.is_some()
        || evidence_error.is_some()
        || run_error.is_some();
    engine.set_stage(
        "Export",
        if export_error { "error" } else { "done" },
        file_count,
        format!("{} files in {}", file_count, dir_text),
    );
    let mut final_error = error;
    for item in [
        save_error,
        report_error,
        plan_error,
        draft_json_error,
        draft_md_error,
        report_md_error,
        report_json_error,
        evidence_error,
        run_error,
    ] {
        if final_error.is_none() {
            final_error = item;
        }
    }
    let stages = engine.stages.clone();
    let _ = tx.send(Msg::Done {
        records,
        total,
        dir: dir_text,
        error: final_error,
        draft: Some(draft),
        report_markdown: Some(markdown_text),
        report_ai: synth_outcome.is_some(),
        stages,
    });
}

struct Engine {
    settings: Settings,
    tx: Sender<Msg>,
    cancel: Arc<AtomicBool>,
    store: Store,
    agent: ureq::Agent,
    archive_limiter: Limiter,
    reddit_limiter: Limiter,
    pullpush_limiter: Limiter,
    requests: usize,
    token: Option<String>,
    token_expiry: Option<Instant>,
    post_seeds: HashMap<String, Record>,
    stages: Vec<StageInfo>,
    collect_notes: Vec<String>,
    /// Short queries already sent to a source, lowercased.
    queries_run: HashSet<String>,
    llm_calls: usize,
    /// Request number where the current follow-up round must stop.
    round_limit: usize,
}

#[derive(Clone, Debug, Default)]
struct ReviewOutcome {
    rounds: usize,
    met: usize,
    total: usize,
    queries: usize,
    llm_used: bool,
    stop: String,
    coverage: Vec<Coverage>,
}

/// Why no new round may start. `None` means a round may start.
fn round_block(
    unmet: usize,
    rounds: usize,
    max_rounds: usize,
    low_budget: bool,
    cancelled: bool,
) -> Option<&'static str> {
    if unmet == 0 {
        Some("all criteria met")
    } else if rounds >= max_rounds {
        Some("round limit reached")
    } else if low_budget {
        Some("request budget is low")
    } else if cancelled {
        Some("cancelled")
    } else {
        None
    }
}

impl Engine {
    fn log(&self, line: String) {
        let _ = self.tx.send(Msg::Log(line));
    }

    fn emit_progress(&self) {
        let _ = self.tx.send(Msg::Progress {
            total: self.store.total(),
            requests: self.requests,
        });
    }

    fn set_stage(&mut self, name: &str, status: &str, count: usize, note: impl Into<String>) {
        let note = note.into();
        let mut update = None;
        if let Some(stage) = self.stages.iter_mut().find(|stage| stage.name == name) {
            stage.status = status.to_string();
            stage.count = count;
            stage.note = note;
            update = Some(stage.clone());
        }
        if let Some(stage) = update {
            let _ = self.tx.send(Msg::Stage(stage));
        }
    }

    fn mark_plan(&mut self) {
        let (status, count, note) = match &self.settings.plan {
            Some(plan) => {
                let source = if self.settings.plan_source.trim().is_empty() {
                    "saved plan"
                } else {
                    self.settings.plan_source.trim()
                };
                (
                    "done",
                    plan.sub_questions.len(),
                    format!(
                        "{} · {} sub-questions · {} seed subreddits",
                        source,
                        plan.sub_questions.len(),
                        plan.subreddits.len()
                    ),
                )
            }
            None => ("skipped", 0, "No plan. Keyword search only.".to_string()),
        };
        self.set_stage("Plan", status, count, note);
    }

    /// Dedupe and rank the whole store again. Used after every collection round.
    fn prepare_records(&mut self) -> evidence::Stats {
        let topic = self.settings.topic.clone();
        let mut records: Vec<Record> = self.store.records.values().cloned().collect();
        let stats = evidence::prepare(&mut records, self.settings.plan.as_ref(), &topic);
        self.store.records = records
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect();
        stats
    }

    fn prepared_records(&self) -> Vec<Record> {
        self.store.records.values().cloned().collect()
    }

    fn remember_plan_queries(&mut self) {
        if let Some(plan) = &self.settings.plan {
            for list in [
                &plan.source_queries.archive_posts,
                &plan.source_queries.archive_comments,
                &plan.source_queries.reddit_search,
                &plan.source_queries.web,
            ] {
                for query in list {
                    let query = query.trim().to_lowercase();
                    if !query.is_empty() {
                        self.queries_run.insert(query);
                    }
                }
            }
        }
    }

    fn low_budget(&self) -> bool {
        self.settings.max_requests > 0
            && self.requests >= self.settings.max_requests * 8 / 10
    }

    /// Each follow-up round may use at most 40% of the remaining request budget.
    fn begin_round_budget(&mut self) {
        let max = self.settings.max_requests;
        self.round_limit = if max == 0 {
            usize::MAX
        } else {
            let remaining = max.saturating_sub(self.requests);
            self.requests + ((remaining as f64 * 0.4).ceil() as usize).min(remaining)
        };
    }

    fn round_full(&self) -> bool {
        self.requests >= self.round_limit
    }

    /// Check coverage, ask for gaps, and collect one more round when needed.
    /// The loop stops at the round limit, a low request budget, a model stop
    /// signal, no new queries, or a user cancel.
    fn review_loop(&mut self) -> ReviewOutcome {
        let topic = self.settings.topic.clone();
        let plan = self.settings.plan.clone();
        let mut coverage = critic::check(&self.prepared_records(), plan.as_ref(), &topic);
        let mut outcome = ReviewOutcome {
            met: coverage.len() - critic::unmet_count(&coverage),
            total: coverage.len(),
            coverage: coverage.clone(),
            ..ReviewOutcome::default()
        };
        self.remember_plan_queries();
        if self.settings.max_rounds == 0 {
            outcome.stop = "review rounds are off".to_string();
            self.set_stage(
                "Review",
                "skipped",
                0,
                format!("{}/{} covered · review rounds are off", outcome.met, outcome.total),
            );
            return outcome;
        }
        loop {
            let unmet = critic::unmet_count(&coverage);
            if let Some(reason) = round_block(
                unmet,
                outcome.rounds,
                self.settings.max_rounds,
                self.low_budget(),
                self.cancel.load(Ordering::Relaxed),
            ) {
                outcome.stop = reason.to_string();
                break;
            }
            let Some(analysis) = self.gap_analysis(&coverage) else {
                outcome.stop = "no AI gap analysis".to_string();
                break;
            };
            outcome.llm_used = true;
            self.log(format!(
                "[critic] round {}: {} of {} sub-questions need more evidence{}",
                outcome.rounds + 1,
                unmet,
                coverage.len(),
                if analysis.notes.trim().is_empty() {
                    String::new()
                } else {
                    format!(" · {}", analysis.notes.trim())
                }
            ));
            if analysis.stop {
                outcome.stop = "the model said stop".to_string();
                break;
            }
            let queries = critic::new_queries(&analysis, &self.queries_run, 6);
            if queries.is_empty() {
                outcome.stop = "no new queries".to_string();
                break;
            }
            self.begin_round_budget();
            match self.follow_up(&queries) {
                Ok(added) => {
                    self.log(format!(
                        "[critic] round {} +{} records from {} queries",
                        outcome.rounds + 1,
                        added,
                        queries.len()
                    ));
                }
                Err(HttpErr::Cancelled) => {
                    outcome.stop = "cancelled".to_string();
                    break;
                }
                Err(err) => {
                    self.log(format!("[critic] round stopped: {}", err));
                    outcome.stop = err.to_string();
                    break;
                }
            }
            outcome.rounds += 1;
            outcome.queries += queries.len();
            self.prepare_records();
            coverage = critic::check(&self.prepared_records(), plan.as_ref(), &topic);
        }
        outcome.total = coverage.len();
        outcome.met = coverage.len() - critic::unmet_count(&coverage);
        outcome.coverage = coverage;

        let status = if outcome.total == 0 { "skipped" } else { "done" };
        let note = if outcome.total == 0 {
            "No sub-questions to check.".to_string()
        } else if outcome.rounds == 0 && outcome.met == outcome.total {
            format!(
                "all {} criteria met · no extra round needed",
                outcome.total
            )
        } else {
            format!(
                "{} round{} · {}/{} covered · {} new queries · {}",
                outcome.rounds,
                if outcome.rounds == 1 { "" } else { "s" },
                outcome.met,
                outcome.total,
                outcome.queries,
                outcome.stop
            )
        };
        self.set_stage("Review", status, outcome.rounds, note.clone());
        self.log(format!("[critic] {}", note));
        outcome
    }

    fn gap_analysis(&mut self, coverage: &[Coverage]) -> Option<GapAnalysis> {
        let config = self.settings.llm.clone()?;
        if !config.is_ready() {
            self.log("[critic] AI model is not ready; local checks only".to_string());
            return None;
        }
        if self.llm_calls >= self.settings.max_llm_calls {
            self.log(format!(
                "[critic] AI call limit reached ({})",
                self.settings.max_llm_calls
            ));
            return None;
        }
        let topic = self.settings.topic.clone();
        let plan = self.settings.plan.clone();
        let ids: Vec<String> = critic::questions(plan.as_ref(), &topic)
            .iter()
            .map(|question| question.id.clone())
            .collect();
        let records = self.prepared_records();
        let user = critic::analysis_user(&topic, coverage, &records);
        self.llm_calls += 1;
        self.log("[critic] asking the AI model for gaps".to_string());
        match critic::analyze(&config, &user, &ids) {
            Ok((analysis, model)) => {
                let model = if model.trim().is_empty() {
                    config.model.clone()
                } else {
                    model
                };
                self.log(format!(
                    "[critic] AI found {} gaps · {}",
                    analysis.gaps.len(),
                    model
                ));
                Some(analysis)
            }
            Err(error) => {
                self.log(format!("[critic] AI gap analysis failed: {}", error));
                None
            }
        }
    }

    /// Run new gap queries on the archive and the official API.
    fn follow_up(&mut self, queries: &[String]) -> Result<usize, HttpErr> {
        let subs = self.planned_subs();
        let after = self.plan_after();
        let post_limit = self.settings.limit.max(10);
        let comment_limit = (self.settings.limit.max(20) * 2).clamp(40, 100);
        let mut added = 0usize;
        for query in queries {
            self.check_cancel()?;
            if self.round_full() {
                self.log("  round request budget reached".to_string());
                break;
            }
            let mut query_added = 0usize;
            for sub in subs.iter().take(3) {
                if self.round_full() {
                    break;
                }
                self.check_cancel()?;
                match self.search_archive(
                    ArchiveKind::Posts,
                    Some(sub.as_str()),
                    query,
                    post_limit,
                    after.as_deref(),
                ) {
                    Ok((rows, used)) => query_added += self.absorb_posts(rows, &used),
                    Err(err) if err.fatal() => return Err(err),
                    Err(err) => self.log(format!(
                        "  r/{} post query \"{}\" failed: {}",
                        sub, query, err
                    )),
                }
            }
            for sub in subs.iter().take(3) {
                if self.round_full() {
                    break;
                }
                self.check_cancel()?;
                match self.search_archive(
                    ArchiveKind::Comments,
                    Some(sub.as_str()),
                    query,
                    comment_limit,
                    after.as_deref(),
                ) {
                    Ok((rows, used)) => query_added += self.absorb_comments(rows, &used),
                    Err(err) if err.fatal() => return Err(err),
                    Err(err) => self.log(format!(
                        "  r/{} comment query \"{}\" failed: {}",
                        sub, query, err
                    )),
                }
            }
            if self.settings.official_search
                && self.settings.account.is_some()
                && !self.round_full()
            {
                self.check_cancel()?;
                match self.reddit_search(None, query, self.settings.official_limit) {
                    Ok(value) => query_added += self.absorb_official_posts(&value, query),
                    Err(err) if err.fatal() => return Err(err),
                    Err(err) => {
                        self.log(format!("  Reddit query \"{}\" failed: {}", query, err));
                    }
                }
            }
            self.queries_run.insert(query.to_lowercase());
            self.log(format!("[critic] \"{}\" +{} new", query, query_added));
            added += query_added;
            self.emit_progress();
        }
        Ok(added)
    }

    fn check_cancel(&self) -> Result<(), HttpErr> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(HttpErr::Cancelled)
        } else {
            Ok(())
        }
    }

    fn take_budget(&mut self) -> Result<(), HttpErr> {
        if self.settings.max_requests > 0 && self.requests >= self.settings.max_requests {
            return Err(HttpErr::Budget(format!(
                "request budget reached ({}). Increase the limit or run again later.",
                self.settings.max_requests
            )));
        }
        self.requests += 1;
        Ok(())
    }

    fn sleep_cancel(&self, seconds: f64) -> Result<(), HttpErr> {
        let mut remaining = seconds;
        while remaining > 0.0 {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(HttpErr::Cancelled);
            }
            let step = remaining.min(0.2);
            std::thread::sleep(Duration::from_secs_f64(step));
            remaining -= step;
        }
        Ok(())
    }

    fn after_param(&self) -> Option<String> {
        let years = self.settings.years_back;
        if years <= 0 {
            return None;
        }
        let moment = chrono::Utc::now() - chrono::Duration::days(years * 365);
        Some(moment.format("%Y-%m-%d").to_string())
    }

    fn execute(&mut self) -> Result<(), String> {
        self.log(format!("run folder: {}", self.store.dir.display()));
        self.log(format!("topic: {}", self.settings.topic));
        let tactics = self.settings.enabled_names();
        if tactics.is_empty() {
            self.set_stage("Collect", "error", 0, "No source tactics selected.");
            return Err("no tactics selected".to_string());
        }
        self.log(format!("tactics: {}", tactics.join(", ")));
        self.set_stage("Collect", "running", 0, "Starting sources...");
        for name in tactics {
            if let Err(err) = self.check_cancel() {
                self.set_stage("Collect", "error", self.store.total(), "Cancelled.");
                return Err(err.to_string());
            }
            let before = self.store.total();
            let result = match name.as_str() {
                "all_reddit" => self.all_reddit_search(),
                "archive_posts" => self.archive_posts(),
                "archive_comments" => self.archive_comments(),
                "archive_threads" => self.archive_threads(),
                "archive_profiles" => self.archive_profiles(),
                "official_search" => self.official_search(),
                "official_threads" => self.official_threads(),
                _ => Ok(()),
            };
            match result {
                Ok(()) => {
                    let after = self.store.total();
                    self.collect_notes
                        .push(format!("{} +{}", name, after - before));
                    let note = self.collect_notes.join(" · ");
                    self.set_stage("Collect", "running", after, note);
                    self.emit_progress();
                }
                Err(err @ HttpErr::Cancelled) => {
                    self.log(format!("[{}] stopped: {}", name, err));
                    self.collect_notes.push(format!("{} stopped", name));
                    let note = self.collect_notes.join(" · ");
                    self.set_stage("Collect", "error", self.store.total(), note);
                    return Err("cancelled".to_string());
                }
                Err(err) if err.fatal() => {
                    self.log(format!("[{}] stopped: {}", name, err));
                    self.collect_notes.push(format!("{} stopped", name));
                    let note = self.collect_notes.join(" · ");
                    self.set_stage("Collect", "error", self.store.total(), note);
                    return Err(err.to_string());
                }
                Err(err) => {
                    self.log(format!("[{}] failed: {}. Continuing.", name, err));
                    self.collect_notes.push(format!("{} failed", name));
                    let note = self.collect_notes.join(" · ");
                    self.set_stage("Collect", "running", self.store.total(), note);
                    self.emit_progress();
                }
            }
        }
        let total = self.store.total();
        let note = self.collect_notes.join(" · ");
        self.set_stage("Collect", "done", total, note);
        Ok(())
    }

    fn archive_get(
        &mut self,
        path: &str,
        params: Vec<(&str, String)>,
    ) -> Result<Vec<Value>, HttpErr> {
        let key = Store::cache_key("archive", path, &params);
        if let Some(value) = self.store.get_cached(&key) {
            return Ok(array_of(value));
        }
        self.take_budget()?;
        self.archive_limiter.wait(&self.cancel)?;
        let url = format!("{}{}?{}", ARCHIVE, path, query(&params));
        let ua = user_agent(&self.settings);
        let mut attempt = 0u32;
        loop {
            let response = self
                .agent
                .get(&url)
                .set("User-Agent", &ua)
                .set("Accept", "application/json")
                .call();
            match response {
                Ok(resp) => {
                    let value = resp
                        .into_json::<Value>()
                        .map_err(|error| HttpErr::Other(error.to_string()))?;
                    let rows = array_of(
                        value
                            .get("data")
                            .cloned()
                            .unwrap_or(Value::Array(Vec::new())),
                    );
                    self.store.put_cached(key, Value::Array(rows.clone()));
                    self.emit_progress();
                    return Ok(rows);
                }
                Err(ureq::Error::Status(422, _)) => return Err(HttpErr::TooHeavy),
                Err(ureq::Error::Status(403, _)) => return Err(HttpErr::Forbidden),
                Err(ureq::Error::Status(404, _)) => return Err(HttpErr::NotFound),
                Err(ureq::Error::Status(code, _))
                    if (code == 429 || code >= 500) && attempt < 3 =>
                {
                    attempt += 1;
                    let wait = 2u64.pow(attempt) as f64 + (attempt as f64 * 0.3);
                    self.log(format!(
                        "  retry {} in {:.1}s (HTTP {})",
                        attempt, wait, code
                    ));
                    self.sleep_cancel(wait)?;
                }
                Err(ureq::Error::Status(code, _)) => {
                    return Err(HttpErr::Other(format!("HTTP {}", code)))
                }
                Err(ureq::Error::Transport(error)) if attempt < 3 => {
                    attempt += 1;
                    self.log(format!("  network retry {}: {}", attempt, error));
                    self.sleep_cancel(2.0 * attempt as f64)?;
                }
                Err(ureq::Error::Transport(error)) => {
                    return Err(HttpErr::Other(error.to_string()))
                }
            }
        }
    }

    fn reddit_auth(&mut self) -> Result<(), HttpErr> {
        if self.token.is_some() {
            if let Some(expiry) = self.token_expiry {
                if Instant::now() < expiry {
                    return Ok(());
                }
            }
        }
        let creds = self
            .settings
            .account
            .clone()
            .ok_or_else(|| HttpErr::Auth("no Reddit account configured".to_string()))?;
        let (token, expires) = login(&self.agent, &creds).map_err(HttpErr::Auth)?;
        self.token = Some(token);
        self.token_expiry =
            Some(Instant::now() + Duration::from_secs_f64((expires - 60.0).max(60.0)));
        self.log("  logged in to Reddit".to_string());
        Ok(())
    }

    fn reddit_get(&mut self, path: &str, params: Vec<(&str, String)>) -> Result<Value, HttpErr> {
        let key = Store::cache_key("reddit", path, &params);
        if let Some(value) = self.store.get_cached(&key) {
            return Ok(value);
        }
        self.reddit_auth()?;
        self.take_budget()?;
        self.reddit_limiter.wait(&self.cancel)?;
        let url = format!("{}{}?{}", OAUTH, path, query(&params));
        let ua = user_agent(&self.settings);
        let mut attempt = 0u32;
        loop {
            let token = self
                .token
                .clone()
                .ok_or_else(|| HttpErr::Auth("no access token".to_string()))?;
            let response = self
                .agent
                .get(&url)
                .set("Authorization", &format!("bearer {}", token))
                .set("User-Agent", &ua)
                .set("Accept", "application/json")
                .call();
            match response {
                Ok(resp) => {
                    let remaining = resp
                        .header("x-ratelimit-remaining")
                        .and_then(|value| value.parse::<f64>().ok());
                    let reset = resp
                        .header("x-ratelimit-reset")
                        .and_then(|value| value.parse::<f64>().ok());
                    let value = resp
                        .into_json::<Value>()
                        .map_err(|error| HttpErr::Other(error.to_string()))?;
                    if let (Some(remaining), Some(reset)) = (remaining, reset) {
                        if remaining <= 5.0 {
                            let wait = reset.clamp(1.0, 60.0);
                            self.log(format!("  near Reddit rate limit, waiting {:.0}s", wait));
                            self.sleep_cancel(wait)?;
                        }
                    }
                    self.store.put_cached(key, value.clone());
                    self.emit_progress();
                    return Ok(value);
                }
                Err(ureq::Error::Status(401, _)) if attempt == 0 => {
                    attempt += 1;
                    self.token = None;
                    self.token_expiry = None;
                    self.reddit_auth()?;
                }
                Err(ureq::Error::Status(401, _)) => {
                    return Err(HttpErr::Auth(
                        "login expired or credentials were rejected".to_string(),
                    ))
                }
                Err(ureq::Error::Status(403, _)) => return Err(HttpErr::Forbidden),
                Err(ureq::Error::Status(404, _)) => return Err(HttpErr::NotFound),
                Err(ureq::Error::Status(429, _)) if attempt < 3 => {
                    attempt += 1;
                    let wait = 5.0 * attempt as f64;
                    self.log(format!("  rate limited, waiting {:.0}s", wait));
                    self.sleep_cancel(wait)?;
                }
                Err(ureq::Error::Status(code, _)) if code >= 500 && attempt < 3 => {
                    attempt += 1;
                    self.sleep_cancel(2.0 * attempt as f64)?;
                }
                Err(ureq::Error::Status(code, _)) => {
                    return Err(HttpErr::Other(format!("HTTP {}", code)))
                }
                Err(ureq::Error::Transport(error)) if attempt < 3 => {
                    attempt += 1;
                    self.log(format!("  network retry {}: {}", attempt, error));
                    self.sleep_cancel(2.0 * attempt as f64)?;
                }
                Err(ureq::Error::Transport(error)) => {
                    return Err(HttpErr::Other(error.to_string()))
                }
            }
        }
    }

    fn archive_posts(&mut self) -> Result<(), HttpErr> {
        if self.settings.plan.is_some() {
            self.archive_posts_planned()
        } else {
            self.archive_posts_legacy()
        }
    }

    fn archive_posts_legacy(&mut self) -> Result<(), HttpErr> {
        let subs = self.settings.subs.clone();
        let topic = self.settings.topic.clone();
        let query = api_query(&topic);
        let narrow_query = primary_query(&topic);
        let limit = self.settings.limit.to_string();
        let after = self.after_param();
        let mut added = 0usize;
        let mut retried: Vec<String> = Vec::new();
        let mut sampled: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for sub in subs {
            self.check_cancel()?;
            let mut params: Vec<(&str, String)> = vec![
                ("subreddit", sub.clone()),
                ("query", query.clone()),
                ("limit", limit.clone()),
            ];
            if let Some(value) = &after {
                params.push(("after", value.clone()));
            }
            let rows = match self.archive_get("/posts/search", params) {
                Ok(rows) => rows,
                Err(err) if err.fatal() => return Err(err),
                Err(HttpErr::TooHeavy) => {
                    let mut narrowed = None;
                    if narrow_query != query {
                        let mut gentle: Vec<(&str, String)> = vec![
                            ("subreddit", sub.clone()),
                            ("query", narrow_query.clone()),
                            ("limit", limit.clone()),
                        ];
                        if let Some(value) = &after {
                            gentle.push(("after", value.clone()));
                        }
                        match self.archive_get("/posts/search", gentle) {
                            Ok(rows) => {
                                retried.push(sub.clone());
                                narrowed = Some(rows);
                            }
                            Err(err) if err.fatal() => return Err(err),
                            Err(HttpErr::TooHeavy) => {}
                            Err(err) => {
                                self.log(format!("  skip r/{}: {}", sub, err));
                                skipped.push(sub.clone());
                                continue;
                            }
                        }
                    }
                    match narrowed {
                        Some(rows) => rows,
                        None => {
                            let mut fallback: Vec<(&str, String)> =
                                vec![("subreddit", sub.clone()), ("limit", limit.clone())];
                            if let Some(value) = &after {
                                fallback.push(("after", value.clone()));
                            }
                            match self.archive_get("/posts/search", fallback) {
                                Ok(rows) => {
                                    sampled.push(sub.clone());
                                    rows
                                }
                                Err(err) if err.fatal() => return Err(err),
                                Err(err) => {
                                    self.log(format!("  skip r/{}: {}", sub, err));
                                    skipped.push(sub.clone());
                                    continue;
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    self.log(format!("  skip r/{}: {}", sub, err));
                    skipped.push(sub.clone());
                    continue;
                }
            };
            for row in rows {
                if noise(&row) {
                    continue;
                }
                let text = format!("{} {}", str_of(&row, "title"), str_of(&row, "selftext"));
                if !matches_topic(&text, &topic) {
                    continue;
                }
                if let Some(record) = model::archive_post(&row, "archive_posts") {
                    let id = record.id.clone();
                    if self.store.add(record.clone(), "archive_posts") {
                        added += 1;
                    }
                    self.post_seeds.insert(id, record);
                }
            }
        }
        let mut note = String::new();
        if !retried.is_empty() {
            note.push_str(&format!(" (short keyword: {})", retried.join(", ")));
        }
        if !sampled.is_empty() {
            note.push_str(&format!(
                " (sampled without keyword: {})",
                sampled.join(", ")
            ));
        }
        if !skipped.is_empty() {
            note.push_str(&format!(" (skipped: {})", skipped.join(", ")));
        }
        self.log(format!("[archive_posts] +{} new{}", added, note));
        Ok(())
    }

    fn archive_comments(&mut self) -> Result<(), HttpErr> {
        if self.settings.plan.is_some() {
            self.archive_comments_planned()
        } else {
            self.archive_comments_legacy()
        }
    }

    fn archive_comments_legacy(&mut self) -> Result<(), HttpErr> {
        let subs = self.settings.subs.clone();
        let topic = self.settings.topic.clone();
        let query = api_query(&topic);
        let narrow_query = primary_query(&topic);
        let limit = self.settings.limit.to_string();
        let fallback_limit = (self.settings.limit * 4).clamp(40, 100).to_string();
        let after = self.after_param();
        let mut added = 0usize;
        let mut retried: Vec<String> = Vec::new();
        let mut sampled: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for sub in subs {
            self.check_cancel()?;
            let mut params: Vec<(&str, String)> = vec![
                ("subreddit", sub.clone()),
                ("body", query.clone()),
                ("limit", limit.clone()),
            ];
            if let Some(value) = &after {
                params.push(("after", value.clone()));
            }
            let rows = match self.archive_get("/comments/search", params) {
                Ok(rows) => rows,
                Err(err) if err.fatal() => return Err(err),
                Err(HttpErr::TooHeavy) => {
                    let mut narrowed = None;
                    if narrow_query != query {
                        let mut gentle: Vec<(&str, String)> = vec![
                            ("subreddit", sub.clone()),
                            ("body", narrow_query.clone()),
                            ("limit", limit.clone()),
                        ];
                        if let Some(value) = &after {
                            gentle.push(("after", value.clone()));
                        }
                        match self.archive_get("/comments/search", gentle) {
                            Ok(rows) => {
                                retried.push(sub.clone());
                                narrowed = Some(rows);
                            }
                            Err(err) if err.fatal() => return Err(err),
                            Err(HttpErr::TooHeavy) => {}
                            Err(err) => {
                                self.log(format!("  skip r/{}: {}", sub, err));
                                skipped.push(sub.clone());
                                continue;
                            }
                        }
                    }
                    match narrowed {
                        Some(rows) => rows,
                        None => {
                            let mut fallback: Vec<(&str, String)> = vec![
                                ("subreddit", sub.clone()),
                                ("limit", fallback_limit.clone()),
                            ];
                            if let Some(value) = &after {
                                fallback.push(("after", value.clone()));
                            }
                            match self.archive_get("/comments/search", fallback) {
                                Ok(rows) => {
                                    sampled.push(sub.clone());
                                    rows
                                }
                                Err(err) if err.fatal() => return Err(err),
                                Err(err) => {
                                    self.log(format!("  skip r/{}: {}", sub, err));
                                    skipped.push(sub.clone());
                                    continue;
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    self.log(format!("  skip r/{}: {}", sub, err));
                    skipped.push(sub.clone());
                    continue;
                }
            };
            for row in rows {
                if noise(&row) {
                    continue;
                }
                if !matches_topic(str_of(&row, "body"), &topic) {
                    continue;
                }
                if let Some(record) = model::archive_comment(&row, "archive_comments") {
                    if self.store.add(record, "archive_comments") {
                        added += 1;
                    }
                }
            }
        }
        let mut note = String::new();
        if !retried.is_empty() {
            note.push_str(&format!(" (short keyword: {})", retried.join(", ")));
        }
        if !sampled.is_empty() {
            note.push_str(&format!(
                " (sampled without keyword: {})",
                sampled.join(", ")
            ));
        }
        if !skipped.is_empty() {
            note.push_str(&format!(" (skipped: {})", skipped.join(", ")));
        }
        self.log(format!("[archive_comments] +{} new{}", added, note));
        Ok(())
    }

    fn planned_queries(&self, list: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for item in list {
            let item = item.trim();
            if item.is_empty() || out.iter().any(|value| value.eq_ignore_ascii_case(item)) {
                continue;
            }
            out.push(item.to_string());
            if out.len() >= 6 {
                break;
            }
        }
        out
    }

    fn planned_subs(&self) -> Vec<String> {
        let Some(plan) = &self.settings.plan else {
            return Vec::new();
        };
        let mut picks = plan.subreddits.clone();
        picks.sort_by_key(|pick| pick.priority);
        let mut subs: Vec<String> = Vec::new();
        for pick in picks {
            let name = pick
                .name
                .trim()
                .trim_start_matches("r/")
                .trim_start_matches('/')
                .to_string();
            if name.is_empty() || subs.iter().any(|item| item.eq_ignore_ascii_case(&name)) {
                continue;
            }
            subs.push(name);
        }
        if subs.is_empty() {
            self.settings.subs.clone()
        } else {
            subs
        }
    }

    fn plan_after(&self) -> Option<String> {
        let config_after = self.after_param();
        let plan_after = self
            .settings
            .plan
            .as_ref()
            .and_then(|plan| plan.time_window.after.clone());
        match (config_after, plan_after) {
            (Some(a), Some(b)) => Some(if a >= b { a } else { b }),
            (a, b) => a.or(b),
        }
    }

    fn in_window(&self, row: &Value) -> bool {
        let Some(plan) = &self.settings.plan else {
            return true;
        };
        let created = row
            .get("created_utc")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0);
        if created <= 0.0 {
            return true;
        }
        if let Some(after) = plan.time_window.after.as_deref().and_then(date_start) {
            if created < after {
                return false;
            }
        }
        if let Some(before) = plan.time_window.before.as_deref().and_then(date_start) {
            if created >= before + 86_400.0 {
                return false;
            }
        }
        true
    }

    fn plan_excluded(&self, text: &str, score: i64) -> bool {
        let hay = normalize(text);
        const STRONG: &[&str] = &[
            "vendor",
            "coupon",
            "referral",
            "advertisement",
            "spam",
            "buy now",
            "for sale",
            "promo code",
        ];
        if STRONG.iter().any(|term| hay.contains(term)) {
            return true;
        }
        if score <= 2 {
            if let Some(plan) = &self.settings.plan {
                return plan
                    .exclude_terms
                    .iter()
                    .filter(|term| term.chars().count() >= 4)
                    .any(|term| hay.contains(&term.to_lowercase()));
            }
        }
        false
    }

    fn plan_base_query(&self) -> String {
        if let Some(plan) = &self.settings.plan {
            let mut words: Vec<String> = Vec::new();
            for entity in &plan.entities {
                push_words(&mut words, entity);
            }
            for question in &plan.sub_questions {
                for keyword in &question.keywords {
                    push_words(&mut words, keyword);
                }
            }
            if !words.is_empty() {
                words.truncate(5);
                return words.join(" ");
            }
        }
        api_query(&self.settings.topic)
    }

    fn search_archive(
        &mut self,
        kind: ArchiveKind,
        sub: Option<&str>,
        query: &str,
        limit: usize,
        after: Option<&str>,
    ) -> Result<(Vec<Value>, String), HttpErr> {
        let path = match kind {
            ArchiveKind::Posts => "/posts/search",
            ArchiveKind::Comments => "/comments/search",
        };
        let key = match kind {
            ArchiveKind::Posts => "query",
            ArchiveKind::Comments => "body",
        };
        let params = |query: Option<&str>| -> Vec<(&str, String)> {
            let mut params: Vec<(&str, String)> = Vec::new();
            if let Some(sub) = sub {
                params.push(("subreddit", sub.to_string()));
            }
            if let Some(query) = query {
                params.push((key, query.to_string()));
            }
            params.push(("limit", limit.to_string()));
            if let Some(after) = after {
                params.push(("after", after.to_string()));
            }
            params
        };
        match self.archive_get(path, params(Some(query))) {
            Ok(rows) => Ok((rows, query.to_string())),
            Err(HttpErr::TooHeavy) => {
                let narrow = primary_term(query);
                if !narrow.eq_ignore_ascii_case(query) {
                    match self.archive_get(path, params(Some(&narrow))) {
                        Ok(rows) => {
                            self.log(format!("  narrowed \"{}\" to \"{}\"", query, narrow));
                            return Ok((rows, narrow));
                        }
                        Err(err) if err.fatal() => return Err(err),
                        Err(HttpErr::TooHeavy) => {}
                        Err(err) => self.log(format!("  narrow query failed: {}", err)),
                    }
                }
                if let Some(sub) = sub {
                    match self.archive_get(path, params(None)) {
                        Ok(rows) => {
                            self.log(format!("  sampled r/{} without a keyword", sub));
                            Ok((rows, narrow))
                        }
                        Err(err) => Err(err),
                    }
                } else {
                    Err(HttpErr::TooHeavy)
                }
            }
            Err(err) => Err(err),
        }
    }

    fn absorb_posts(&mut self, rows: Vec<Value>, query: &str) -> usize {
        self.absorb_posts_from(rows, query, "archive_posts")
    }

    fn absorb_posts_from(&mut self, rows: Vec<Value>, query: &str, source: &str) -> usize {
        let mut added = 0usize;
        for row in rows {
            if noise(&row) || !self.in_window(&row) {
                continue;
            }
            let text = format!("{} {}", str_of(&row, "title"), str_of(&row, "selftext"));
            if !query_matches(&text, query) {
                continue;
            }
            let score = row
                .get("score")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            if self.plan_excluded(&text, score) {
                continue;
            }
            if let Some(record) = model::archive_post(&row, source) {
                let id = record.id.clone();
                if self.store.add(record.clone(), source) {
                    added += 1;
                }
                self.post_seeds.insert(id, record);
            }
        }
        added
    }

    fn absorb_comments(&mut self, rows: Vec<Value>, query: &str) -> usize {
        self.absorb_comments_from(rows, query, "archive_comments")
    }

    fn absorb_comments_from(&mut self, rows: Vec<Value>, query: &str, source: &str) -> usize {
        let mut added = 0usize;
        for row in rows {
            if noise(&row) || !self.in_window(&row) {
                continue;
            }
            let body = str_of(&row, "body");
            if !query_matches(body, query) {
                continue;
            }
            let score = row
                .get("score")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            if self.plan_excluded(body, score) {
                continue;
            }
            if let Some(record) = model::archive_comment(&row, source) {
                if self.store.add(record, source) {
                    added += 1;
                }
            }
        }
        added
    }

    /// Queries that can run without a subreddit filter. They combine the
    /// planner's Reddit and web query lists with the question keywords.
    fn global_queries(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(plan) = &self.settings.plan {
            for list in [
                &plan.source_queries.reddit_search,
                &plan.source_queries.web,
                &plan.source_queries.archive_posts,
            ] {
                for query in list {
                    push_global_query(&mut out, query);
                }
            }
            for question in &plan.sub_questions {
                push_global_query(&mut out, &question.keywords.join(" "));
            }
            push_global_query(&mut out, &self.plan_base_query());
        } else {
            push_global_query(&mut out, &api_query(&self.settings.topic));
        }
        out.truncate(8);
        out
    }

    /// Whole-Reddit discovery through the PullPush archive. It needs no
    /// account. The seed subreddits stay as extra targeted searches.
    fn all_reddit_search(&mut self) -> Result<(), HttpErr> {
        let queries = self.global_queries();
        if queries.is_empty() {
            self.log("[all_reddit] no global queries".to_string());
            return Ok(());
        }
        let after = self.plan_after();
        let limit = self.settings.limit.clamp(25, 100);
        let mut added = 0usize;
        let mut searches = 0usize;
        let mut failed = 0usize;
        for query in &queries {
            self.check_cancel()?;
            match self.pullpush_get(ArchiveKind::Posts, query, limit, after.as_deref()) {
                Ok(rows) => {
                    searches += 1;
                    added += self.absorb_posts_from(rows, query, "all_reddit");
                }
                Err(err) if err.fatal() => return Err(err),
                Err(err) => {
                    failed += 1;
                    self.log(format!("  all-Reddit post query \"{}\" failed: {}", query, err));
                }
            }
            self.check_cancel()?;
            match self.pullpush_get(ArchiveKind::Comments, query, limit, after.as_deref()) {
                Ok(rows) => {
                    searches += 1;
                    added += self.absorb_comments_from(rows, query, "all_reddit");
                }
                Err(err) if err.fatal() => return Err(err),
                Err(err) => {
                    failed += 1;
                    self.log(format!(
                        "  all-Reddit comment query \"{}\" failed: {}",
                        query, err
                    ));
                }
            }
            self.queries_run.insert(query.to_lowercase());
        }
        self.log(format!(
            "[all_reddit] +{} new from {} global searches across all Reddit ({} failed)",
            added, searches, failed
        ));
        Ok(())
    }

    fn pullpush_get(
        &mut self,
        kind: ArchiveKind,
        text: &str,
        limit: usize,
        after: Option<&str>,
    ) -> Result<Vec<Value>, HttpErr> {
        let endpoint = match kind {
            ArchiveKind::Posts => "submission",
            ArchiveKind::Comments => "comment",
        };
        let mut params: Vec<(&str, String)> = vec![
            ("q", text.to_string()),
            ("size", limit.clamp(1, 100).to_string()),
            ("sort", "desc".to_string()),
            ("sort_type", "score".to_string()),
        ];
        if let Some(after) = after.and_then(date_start) {
            params.push(("after", (after as i64).to_string()));
        }
        let key = Store::cache_key("pullpush", endpoint, &params);
        if let Some(value) = self.store.get_cached(&key) {
            return Ok(array_of(value));
        }
        self.take_budget()?;
        self.pullpush_limiter.wait(&self.cancel)?;
        let url = format!("{}/{}?{}", PULLPUSH, endpoint, query(&params));
        let ua = user_agent(&self.settings);
        let mut attempt = 0u32;
        loop {
            let response = self
                .agent
                .get(&url)
                .set("User-Agent", &ua)
                .set("Accept", "application/json")
                .call();
            match response {
                Ok(resp) => {
                    let value = resp
                        .into_json::<Value>()
                        .map_err(|error| HttpErr::Other(error.to_string()))?;
                    let rows = array_of(
                        value
                            .get("data")
                            .cloned()
                            .unwrap_or(Value::Array(Vec::new())),
                    );
                    self.store.put_cached(key, Value::Array(rows.clone()));
                    self.emit_progress();
                    return Ok(rows);
                }
                Err(ureq::Error::Status(403, _)) => return Err(HttpErr::Forbidden),
                Err(ureq::Error::Status(404, _)) => return Err(HttpErr::NotFound),
                Err(ureq::Error::Status(code, _))
                    if (code == 429 || code >= 500) && attempt < 3 =>
                {
                    attempt += 1;
                    let wait = 5.0 * attempt as f64;
                    self.log(format!("  all-Reddit retry {} in {:.0}s (HTTP {})", attempt, wait, code));
                    self.sleep_cancel(wait)?;
                }
                Err(ureq::Error::Status(code, _)) => {
                    return Err(HttpErr::Other(format!("HTTP {}", code)))
                }
                Err(ureq::Error::Transport(error)) if attempt < 3 => {
                    attempt += 1;
                    self.log(format!("  all-Reddit network retry {}: {}", attempt, error));
                    self.sleep_cancel(2.0 * attempt as f64)?;
                }
                Err(ureq::Error::Transport(error)) => {
                    return Err(HttpErr::Other(error.to_string()))
                }
            }
        }
    }

    fn archive_posts_planned(&mut self) -> Result<(), HttpErr> {
        let Some(plan) = self.settings.plan.clone() else {
            return Ok(());
        };
        let queries = self.planned_queries(&plan.source_queries.archive_posts);
        if queries.is_empty() {
            self.log("[archive_posts] plan has no post queries".to_string());
            return Ok(());
        }
        let subs = self.planned_subs();
        let after = self.plan_after();
        let limit = self.settings.limit.max(10);
        let mut added = 0usize;
        let mut searches = 0usize;
        let mut failed = 0usize;
        for sub in subs.iter().take(6) {
            for query in queries.iter().take(4) {
                self.check_cancel()?;
                match self.search_archive(
                    ArchiveKind::Posts,
                    Some(sub),
                    query,
                    limit,
                    after.as_deref(),
                ) {
                    Ok((rows, used)) => {
                        searches += 1;
                        added += self.absorb_posts(rows, &used);
                    }
                    Err(err) if err.fatal() => return Err(err),
                    Err(err) => {
                        failed += 1;
                        self.log(format!(
                            "  r/{} post query \"{}\" failed: {}",
                            sub, query, err
                        ));
                    }
                }
            }
        }
        self.log(format!(
            "[archive_posts] seed subreddits +{} new from {} searches in {} subreddits ({} failed)",
            added,
            searches,
            subs.len().min(6),
            failed
        ));
        Ok(())
    }

    fn archive_comments_planned(&mut self) -> Result<(), HttpErr> {
        let Some(plan) = self.settings.plan.clone() else {
            return Ok(());
        };
        let queries = self.planned_queries(&plan.source_queries.archive_comments);
        if queries.is_empty() {
            self.log("[archive_comments] plan has no comment queries".to_string());
            return Ok(());
        }
        let subs = self.planned_subs();
        let after = self.plan_after();
        let limit = (self.settings.limit.max(20) * 2).clamp(40, 100);
        let mut added = 0usize;
        let mut searches = 0usize;
        let mut failed = 0usize;
        for sub in subs.iter().take(6) {
            for query in queries.iter().take(4) {
                self.check_cancel()?;
                match self.search_archive(
                    ArchiveKind::Comments,
                    Some(sub),
                    query,
                    limit,
                    after.as_deref(),
                ) {
                    Ok((rows, used)) => {
                        searches += 1;
                        added += self.absorb_comments(rows, &used);
                    }
                    Err(err) if err.fatal() => return Err(err),
                    Err(err) => {
                        failed += 1;
                        self.log(format!(
                            "  r/{} comment query \"{}\" failed: {}",
                            sub, query, err
                        ));
                    }
                }
            }
        }
        self.log(format!(
            "[archive_comments] seed subreddits +{} new from {} searches in {} subreddits ({} failed)",
            added,
            searches,
            subs.len().min(6),
            failed
        ));
        Ok(())
    }

    fn reddit_search(
        &mut self,
        target: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Value, HttpErr> {
        let mut params: Vec<(&str, String)> = vec![
            ("q", query.to_string()),
            ("sort", "relevance".to_string()),
            ("t", "all".to_string()),
            ("limit", limit.to_string()),
            ("type", "link".to_string()),
            ("raw_json", "1".to_string()),
        ];
        let path = match target {
            Some(sub) => {
                params.push(("restrict_sr", "1".to_string()));
                format!("/r/{}/search", sub)
            }
            None => "/search".to_string(),
        };
        self.reddit_get(&path, params)
    }

    fn absorb_official_posts(&mut self, value: &Value, query: &str) -> usize {
        let rows = value
            .get("data")
            .and_then(|data| data.get("children"))
            .and_then(|children| children.as_array())
            .cloned()
            .unwrap_or_default();
        let mut added = 0usize;
        for child in rows {
            if child.get("kind").and_then(|kind| kind.as_str()) != Some("t3") {
                continue;
            }
            let data = child.get("data").cloned().unwrap_or(Value::Null);
            if noise(&data) {
                continue;
            }
            let text = format!("{} {}", str_of(&data, "title"), str_of(&data, "selftext"));
            if !query_matches(&text, query) {
                continue;
            }
            let score = data
                .get("score")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            if self.plan_excluded(&text, score) {
                continue;
            }
            if let Some(record) = model::reddit_post(&data, "official_search") {
                let id = record.id.clone();
                if self.store.add(record.clone(), "official_search") {
                    added += 1;
                }
                self.post_seeds.insert(id, record);
            }
        }
        added
    }

    fn official_search_planned(&mut self) -> Result<(), HttpErr> {
        if self.settings.account.is_none() {
            self.log("[official_search] skipped (no account)".to_string());
            return Ok(());
        }
        if self.settings.plan.is_none() {
            return Ok(());
        }
        let queries = self.global_queries();
        if queries.is_empty() {
            self.log("[official_search] plan has no Reddit queries".to_string());
            return Ok(());
        }
        let subs = self.planned_subs();
        let limit = self.settings.official_limit;
        let mut global_added = 0usize;
        let mut failed = 0usize;
        for query in &queries {
            self.check_cancel()?;
            match self.reddit_search(None, query, limit) {
                Ok(value) => {
                    global_added += self.absorb_official_posts(&value, query);
                }
                Err(err) if err.fatal() => return Err(err),
                Err(err) => {
                    failed += 1;
                    self.log(format!("  all-Reddit query \"{}\" failed: {}", query, err));
                }
            }
        }
        self.log(format!(
            "[official_search] all Reddit +{} new from {} queries",
            global_added,
            queries.len()
        ));
        let mut seed_added = 0usize;
        for index in 0..subs.iter().take(6).count() {
            self.check_cancel()?;
            let sub = &subs[index];
            let query = &queries[index % queries.len()];
            match self.reddit_search(Some(sub), query, limit) {
                Ok(value) => {
                    seed_added += self.absorb_official_posts(&value, query);
                }
                Err(err) if err.fatal() => return Err(err),
                Err(err) => {
                    failed += 1;
                    self.log(format!("  r/{} Reddit query failed: {}", sub, err));
                }
            }
        }
        self.log(format!(
            "[official_search] seed subreddits +{} new from {} searches ({} failed total)",
            seed_added,
            subs.len().min(6),
            failed
        ));
        Ok(())
    }

    fn archive_threads(&mut self) -> Result<(), HttpErr> {
        let wanted = self.settings.threads_count.max(1);
        let seeds = self.thread_seeds(wanted);
        if seeds.is_empty() {
            self.log("[archive_threads] no candidate threads to expand".to_string());
            return Ok(());
        }
        let seed_total = seeds.len();
        let mut added = 0usize;
        let mut failed = 0usize;
        for seed in seeds {
            self.check_cancel()?;
            let params: Vec<(&str, String)> = vec![
                ("link_id", seed.clone()),
                ("limit", "100".to_string()),
                ("sort", "asc".to_string()),
            ];
            match self.archive_get("/comments/search", params) {
                Ok(rows) => {
                    // Parent text is kept so each comment can be read in context.
                    let mut bodies: HashMap<String, String> = HashMap::new();
                    for record in self.store.records.values() {
                        let text = if record.body.trim().is_empty() {
                            record.title.as_str()
                        } else {
                            record.body.as_str()
                        };
                        if !text.trim().is_empty() {
                            bodies
                                .entry(record.id.clone())
                                .or_insert_with(|| clip_text(text, 240));
                        }
                    }
                    for row in &rows {
                        let id = str_of(row, "id");
                        let text = str_of(row, "body");
                        if !id.is_empty() && !text.trim().is_empty() {
                            bodies
                                .entry(format!("t1_{}", id))
                                .or_insert_with(|| clip_text(text, 240));
                        }
                    }
                    for row in rows {
                        if noise(&row) {
                            continue;
                        }
                        if let Some(mut record) = model::archive_comment(&row, "archive_threads") {
                            if let Some(parent) = parent_id_of(&row) {
                                record.parent_excerpt = bodies.get(&parent).cloned();
                            }
                            if self.store.add(record, "archive_threads") {
                                added += 1;
                            }
                        }
                    }
                }
                Err(err) if err.fatal() => return Err(err),
                Err(HttpErr::TooHeavy) => failed += 1,
                Err(err) => {
                    failed += 1;
                    self.log(format!("  skip thread {}: {}", seed, err));
                }
            }
        }
        let note = if failed > 0 {
            format!(" ({} threads failed)", failed)
        } else {
            String::new()
        };
        self.log(format!(
            "[archive_threads] +{} new from {} threads{}",
            added, seed_total, note
        ));
        Ok(())
    }

    fn archive_profiles(&mut self) -> Result<(), HttpErr> {
        let mut counts: Vec<(String, usize)> = self
            .store
            .authored
            .iter()
            .map(|(author, count)| (author.clone(), *count))
            .collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        counts.truncate(self.settings.profiles_count.max(1));
        if counts.is_empty() {
            self.log("[archive_profiles] no authors found yet".to_string());
            return Ok(());
        }
        let topic = self.settings.topic.clone();
        let plan_mode = self.settings.plan.is_some();
        let query = self.plan_base_query();
        let after = self.plan_after();
        let mut added = 0usize;
        for (author, _) in counts {
            self.check_cancel()?;
            let mut params: Vec<(&str, String)> = vec![
                ("author", author.clone()),
                ("body", query.clone()),
                ("limit", "50".to_string()),
            ];
            if let Some(value) = &after {
                params.push(("after", value.clone()));
            }
            let rows = match self.archive_get("/comments/search", params) {
                Ok(rows) => rows,
                Err(err) if err.fatal() => return Err(err),
                Err(HttpErr::TooHeavy) => {
                    let mut fallback: Vec<(&str, String)> =
                        vec![("author", author.clone()), ("limit", "100".to_string())];
                    if let Some(value) = &after {
                        fallback.push(("after", value.clone()));
                    }
                    match self.archive_get("/comments/search", fallback) {
                        Ok(rows) => rows,
                        Err(err) if err.fatal() => return Err(err),
                        Err(err) => {
                            self.log(format!("  skip u/{}: {}", author, err));
                            continue;
                        }
                    }
                }
                Err(err) => {
                    self.log(format!("  skip u/{}: {}", author, err));
                    continue;
                }
            };
            let mut kept = 0usize;
            for row in rows {
                if noise(&row) {
                    continue;
                }
                let keep = if plan_mode {
                    query_matches(str_of(&row, "body"), &query)
                } else {
                    matches_topic(str_of(&row, "body"), &topic)
                };
                if !keep {
                    continue;
                }
                if let Some(record) = model::archive_comment(&row, "archive_profiles") {
                    if self.store.add(record, "archive_profiles") {
                        added += 1;
                        kept += 1;
                    }
                }
            }
            self.log(format!("  u/{}: +{}", author, kept));
        }
        self.log(format!("[archive_profiles] +{} new", added));
        Ok(())
    }

    /// Best threads for expansion, one per plan sub-question first.
    fn thread_seeds(&self, wanted: usize) -> Vec<String> {
        let records: Vec<Record> = self.store.records.values().cloned().collect();
        let mut seeds = evidence::seed_threads(
            &records,
            self.settings.plan.as_ref(),
            &self.settings.topic,
            wanted,
        );
        if seeds.is_empty() {
            let mut posts: Vec<Record> = self.post_seeds.values().cloned().collect();
            posts.sort_by(|a, b| b.score.cmp(&a.score));
            seeds = posts
                .into_iter()
                .take(wanted)
                .map(|post| post.thread().to_string())
                .filter(|thread| !thread.is_empty())
                .collect();
        }
        seeds
    }

    fn official_search(&mut self) -> Result<(), HttpErr> {
        if self.settings.plan.is_some() {
            self.official_search_planned()
        } else {
            self.official_search_legacy()
        }
    }

    fn official_search_legacy(&mut self) -> Result<(), HttpErr> {
        if self.settings.account.is_none() {
            self.log("[official_search] skipped (no account)".to_string());
            return Ok(());
        }
        let topic = self.settings.topic.clone();
        let limit = self.settings.official_limit.to_string();
        let targets: Vec<Option<String>> = if self.settings.subs.is_empty() {
            vec![None]
        } else {
            self.settings.subs.iter().cloned().map(Some).collect()
        };
        let mut added = 0usize;
        for target in targets {
            self.check_cancel()?;
            let mut params: Vec<(&str, String)> = vec![
                ("q", topic.clone()),
                ("sort", "relevance".to_string()),
                ("t", "all".to_string()),
                ("limit", limit.clone()),
                ("type", "link".to_string()),
                ("raw_json", "1".to_string()),
            ];
            let path = match &target {
                Some(sub) => {
                    params.push(("restrict_sr", "1".to_string()));
                    format!("/r/{}/search", sub)
                }
                None => "/search".to_string(),
            };
            let label = target
                .as_ref()
                .map(|sub| format!("r/{}", sub))
                .unwrap_or_else(|| "all Reddit".to_string());
            match self.reddit_get(&path, params) {
                Ok(value) => {
                    let rows = value
                        .get("data")
                        .and_then(|data| data.get("children"))
                        .and_then(|children| children.as_array())
                        .cloned()
                        .unwrap_or_default();
                    for child in rows {
                        if child.get("kind").and_then(|kind| kind.as_str()) != Some("t3") {
                            continue;
                        }
                        let data = child.get("data").cloned().unwrap_or(Value::Null);
                        if noise(&data) {
                            continue;
                        }
                        if let Some(record) = model::reddit_post(&data, "official_search") {
                            let id = record.id.clone();
                            if self.store.add(record.clone(), "official_search") {
                                added += 1;
                            }
                            self.post_seeds.insert(id, record);
                        }
                    }
                }
                Err(err) if err.fatal() => return Err(err),
                Err(err) => self.log(format!("  skip {}: {}", label, err)),
            }
        }
        self.log(format!("[official_search] +{} new", added));
        Ok(())
    }

    fn official_threads(&mut self) -> Result<(), HttpErr> {
        if self.settings.account.is_none() {
            self.log("[official_threads] skipped (no account)".to_string());
            return Ok(());
        }
        let wanted = self.settings.threads_count.max(1);
        let seeds = self.thread_seeds(wanted);
        if seeds.is_empty() {
            self.log("[official_threads] no candidate threads".to_string());
            return Ok(());
        }
        let mut added = 0usize;
        for seed in &seeds {
            self.check_cancel()?;
            let id = seed.strip_prefix("t3_").unwrap_or(seed);
            let params: Vec<(&str, String)> = vec![
                ("limit", "500".to_string()),
                ("sort", "top".to_string()),
                ("depth", "8".to_string()),
                ("raw_json", "1".to_string()),
            ];
            match self.reddit_get(&format!("/api/comments/{}", id), params) {
                Ok(value) => {
                    for record in parse_official_thread(&value, "official_threads") {
                        if self.store.add(record, "official_threads") {
                            added += 1;
                        }
                    }
                }
                Err(err) if err.fatal() => return Err(err),
                Err(err) => self.log(format!("  skip thread {}: {}", seed, err)),
            }
        }
        self.log(format!(
            "[official_threads] +{} new from {} threads",
            added,
            seeds.len()
        ));
        Ok(())
    }
}

#[cfg(test)]
fn thread_id_from_permalink(permalink: &str) -> Option<String> {
    let marker = "/comments/";
    let start = permalink.find(marker)? + marker.len();
    let rest = &permalink[start..];
    let id: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn parse_official_thread(value: &Value, source: &str) -> Vec<Record> {
    let mut out = Vec::new();
    if let Some(items) = value.as_array() {
        let mut root: Option<String> = None;
        if let Some(post) = items
            .first()
            .and_then(|listing| listing.get("data"))
            .and_then(|data| data.get("children"))
            .and_then(|children| children.as_array())
            .and_then(|children| children.first())
            .and_then(|child| child.get("data"))
        {
            let body = str_of(post, "selftext");
            let text = if body.trim().is_empty() {
                str_of(post, "title")
            } else {
                body
            };
            if !text.trim().is_empty() {
                root = Some(clip_text(text, 240));
            }
        }
        if items.len() > 1 {
            if let Some(children) = items[1]
                .get("data")
                .and_then(|data| data.get("children"))
                .and_then(|children| children.as_array())
            {
                walk_comments(children, source, root.as_deref(), &mut out);
            }
        }
    }
    out
}

fn walk_comments(children: &[Value], source: &str, parent: Option<&str>, out: &mut Vec<Record>) {
    for child in children {
        if child.get("kind").and_then(|kind| kind.as_str()) != Some("t1") {
            continue;
        }
        let data = child.get("data").cloned().unwrap_or(Value::Null);
        if !noise(&data) {
            if let Some(mut record) = model::reddit_comment(&data, source) {
                if let Some(parent) = parent {
                    if !parent.trim().is_empty() {
                        record.parent_excerpt = Some(clip_text(parent, 240));
                    }
                }
                out.push(record);
            }
        }
        if let Some(replies) = data.get("replies") {
            if replies.is_object() {
                if let Some(children) = replies
                    .get("data")
                    .and_then(|info| info.get("children"))
                    .and_then(|children| children.as_array())
                {
                    let body = str_of(&data, "body");
                    walk_comments(children, source, Some(body), out);
                }
            }
        }
    }
}

fn parent_id_of(row: &Value) -> Option<String> {
    let parent = str_of(row, "parent_id").trim();
    if parent.is_empty() {
        return None;
    }
    if parent.starts_with("t1_") || parent.starts_with("t3_") {
        Some(parent.to_string())
    } else {
        Some(format!("t1_{}", parent))
    }
}

/// One-line excerpt with a hard character cap.
fn clip_text(text: &str, limit: usize) -> String {
    let combined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = combined.chars().count();
    if count <= limit {
        return combined;
    }
    let mut out: String = combined.chars().take(limit).collect();
    out.push('…');
    out
}

pub fn selftest(topic: &str) -> i32 {
    let cfg = config::load();
    let key =
        crate::secrets::load(&crate::secrets::target(&cfg.llm_provider)).unwrap_or_default();
    let settings = Settings {
        topic: topic.to_string(),
        subs: parse_subs(config::DEFAULT_SUBS),
        plan: None,
        plan_source: String::new(),
        posts: true,
        comments: true,
        threads: true,
        profiles: true,
        all_reddit: true,
        official_search: false,
        official_threads: false,
        limit: 15,
        official_limit: 50,
        threads_count: 4,
        profiles_count: 3,
        years_back: 0,
        rpm: 45,
        archive_interval: 1.2,
        pullpush_interval: 2.0,
        max_requests: 80,
        max_rounds: 2,
        max_llm_calls: 12,
        llm: Some(LlmConfig::from_saved(&cfg, key)),
        account: None,
    };
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    spawn(settings, tx, cancel);
    let mut lines: Vec<String> = Vec::new();
    let mut code = 2;
    for message in rx {
        match message {
            Msg::Log(line) => {
                let _ = writeln!(std::io::stdout(), "{}", line);
                lines.push(line);
            }
            Msg::Progress { total, requests } => {
                let line = format!("  ... {} records, {} requests", total, requests);
                let _ = writeln!(std::io::stdout(), "{}", line);
                lines.push(line);
            }
            Msg::Stage(stage) => {
                let line = format!(
                    "stage {}: {} ({}) {}",
                    stage.name, stage.status, stage.count, stage.note
                );
                let _ = writeln!(std::io::stdout(), "{}", line);
                lines.push(line);
            }
            Msg::Auth(_) => {}
            Msg::Done {
                total,
                dir,
                error,
                records: _,
                draft,
                report_markdown: _,
                report_ai: _,
                stages: _,
            } => {
                if let Some(draft) = &draft {
                    let line = format!(
                        "draft: {} sections, {} records",
                        draft.sections.len(),
                        draft.total
                    );
                    let _ = writeln!(std::io::stdout(), "{}", line);
                    lines.push(line);
                }
                let line = match &error {
                    Some(err) => format!("done with error: {} ({} records, {})", err, total, dir),
                    None => format!("done: {} records in {}", total, dir),
                };
                let _ = writeln!(std::io::stdout(), "{}", line);
                lines.push(line);
                code = if error.is_none() && total > 0 { 0 } else { 2 };
                break;
            }
        }
    }
    let _ = std::fs::create_dir_all(config::app_dir());
    let log_path = config::app_dir().join("selftest.log");
    let _ = std::fs::write(&log_path, lines.join("\n"));
    let _ = writeln!(std::io::stdout(), "log: {}", log_path.display());
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_round_runs_only_when_criteria_are_unmet() {
        assert_eq!(round_block(0, 0, 2, false, false), Some("all criteria met"));
        assert_eq!(round_block(1, 0, 2, false, false), None);
        assert_eq!(
            round_block(1, 2, 2, false, false),
            Some("round limit reached")
        );
        assert_eq!(
            round_block(1, 0, 2, true, false),
            Some("request budget is low")
        );
        assert_eq!(round_block(1, 0, 2, false, true), Some("cancelled"));
    }

    #[test]
    fn topic_matching_accepts_phrase_and_all_words() {
        assert!(matches_topic(
            "Adderall side effects after a week",
            "adderall"
        ));
        assert!(matches_topic("my focus is bad today", "focus bad"));
        assert!(!matches_topic("unrelated text", "adderall"));
    }

    #[test]
    fn topic_terms_drop_question_words() {
        let sentence = "how armodafinil helps in learning";
        assert_eq!(api_query(sentence), "armodafinil learning");
        assert_eq!(primary_query(sentence), "armodafinil");
        assert!(matches_topic(
            "Armodafinil helped my learning a lot",
            sentence
        ));
        assert!(!matches_topic("armodafinil only", sentence));
    }

    #[test]
    fn parses_thread_id_from_permalink() {
        assert_eq!(
            thread_id_from_permalink("https://www.reddit.com/r/ADHD/comments/abc123/title/"),
            Some("abc123".to_string())
        );
        assert_eq!(thread_id_from_permalink(""), None);
    }

    #[test]
    fn parses_subreddit_list() {
        assert_eq!(
            parse_subs("ADHD, r/adderall\nStims"),
            vec![
                "ADHD".to_string(),
                "adderall".to_string(),
                "Stims".to_string()
            ]
        );
    }

    #[test]
    fn parent_ids_get_the_comment_prefix() {
        assert_eq!(
            parent_id_of(&serde_json::json!({"parent_id": "abc"})),
            Some("t1_abc".to_string())
        );
        assert_eq!(
            parent_id_of(&serde_json::json!({"parent_id": "t1_abc"})),
            Some("t1_abc".to_string())
        );
        assert_eq!(
            parent_id_of(&serde_json::json!({"parent_id": "t3_root"})),
            Some("t3_root".to_string())
        );
        assert_eq!(parent_id_of(&serde_json::json!({})), None);
    }

    #[test]
    fn thread_walk_keeps_root_and_parent_context() {
        let thread = serde_json::json!([
            {"data": {"children": [{"data": {"title": "root title", "selftext": "root body"}}]}},
            {"data": {"children": [{
                "kind": "t1",
                "data": {
                    "name": "t1_a", "id": "a", "subreddit": "ADHD", "author": "one",
                    "body": "parent comment", "link_id": "t3_x",
                    "permalink": "/r/ADHD/comments/x/_/a/",
                    "replies": {"data": {"children": [{
                        "kind": "t1",
                        "data": {
                            "name": "t1_b", "id": "b", "subreddit": "ADHD", "author": "two",
                            "body": "reply comment", "link_id": "t3_x",
                            "permalink": "/r/ADHD/comments/x/_/b/"
                        }
                    }]}}
                }
            }]}}
        ]);
        let records = parse_official_thread(&thread, "reddit_api");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].parent_excerpt.as_deref(), Some("root body"));
        assert_eq!(records[1].parent_excerpt.as_deref(), Some("parent comment"));
    }

    #[test]
    fn clip_text_collapses_spaces_and_caps_length() {
        assert_eq!(clip_text("  one   two\nthree ", 40), "one two three");
        assert_eq!(clip_text("one two three", 5), "one t…");
    }

    #[test]
    fn global_queries_drop_the_site_prefix_and_dedupe() {
        let mut out = Vec::new();
        push_global_query(&mut out, "site:reddit.com adderall crash");
        push_global_query(&mut out, "adderall crash");
        push_global_query(&mut out, "  ");
        assert_eq!(out, vec!["adderall crash".to_string()]);
    }
}
