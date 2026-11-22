//! Turns a research question into a validated search plan.
//!
//! The model plans. This module checks the shape, removes bad entries, and
//! builds a simple local plan when no model is available.

use crate::config;
use crate::engine;
use crate::llm::{self, LlmConfig, LlmRequest};
use crate::secrets;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::sync::Arc;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ResearchPlan {
    pub schema_version: u32,
    pub question: String,
    pub language: String,
    pub time_window: TimeWindow,
    pub sub_questions: Vec<SubQuestion>,
    pub entities: Vec<String>,
    pub synonyms: BTreeMap<String, Vec<String>>,
    pub subreddits: Vec<SubredditPick>,
    pub source_queries: SourceQueries,
    pub exclude_terms: Vec<String>,
    pub notes: String,
}

impl Default for ResearchPlan {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            question: String::new(),
            language: "en".to_string(),
            time_window: TimeWindow::default(),
            sub_questions: Vec::new(),
            entities: Vec::new(),
            synonyms: BTreeMap::new(),
            subreddits: Vec::new(),
            source_queries: SourceQueries::default(),
            exclude_terms: Vec::new(),
            notes: String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeWindow {
    pub after: Option<String>,
    pub before: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SubQuestion {
    pub id: String,
    pub text: String,
    pub keywords: Vec<String>,
    pub success: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SubredditPick {
    pub name: String,
    pub priority: u32,
    pub why: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceQueries {
    pub archive_posts: Vec<String>,
    pub archive_comments: Vec<String>,
    pub reddit_search: Vec<String>,
    pub web: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PlanOutcome {
    pub plan: ResearchPlan,
    pub source: String,
    pub ai_used: bool,
    pub warning: Option<String>,
    pub warnings: Vec<String>,
}

/// Build a plan with the model. Fall back to a local plan on any failure.
pub fn build(config: LlmConfig, query: &str, subs: &[String]) -> PlanOutcome {
    let query = query.trim();
    if !config.is_ready() {
        let reason = if config.provider.needs_key() && config.api_key.trim().is_empty() {
            "no API key".to_string()
        } else {
            "the base URL or model is empty".to_string()
        };
        return PlanOutcome {
            plan: heuristic(query, subs),
            source: "Local fallback".to_string(),
            ai_used: false,
            warning: Some(reason),
            warnings: Vec::new(),
        };
    }
    match ai_plan(&config, query, subs) {
        Ok((plan, model, warnings)) => PlanOutcome {
            plan,
            source: format!(
                "{} · {}",
                config.provider.label(),
                if model.trim().is_empty() {
                    config.model.clone()
                } else {
                    model
                }
            ),
            ai_used: true,
            warning: None,
            warnings,
        },
        Err(error) => PlanOutcome {
            plan: heuristic(query, subs),
            source: "Local fallback".to_string(),
            ai_used: false,
            warning: Some(error),
            warnings: Vec::new(),
        },
    }
}

fn ai_plan(
    config: &LlmConfig,
    query: &str,
    subs: &[String],
) -> Result<(ResearchPlan, String, Vec<String>), String> {
    let client = llm::client(config).map_err(|error| error.to_string())?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let request = LlmRequest {
        system: llm::prompts::planner_system(),
        user: llm::prompts::planner_user(query, subs, &today),
        json: true,
        max_tokens: 2400,
        temperature: config.temperature,
    };
    let response = client
        .complete(&request)
        .map_err(|error| error.to_string())?;
    match parse_plan(&response.text, query, subs) {
        Ok((plan, warnings)) => Ok((plan, response.model, warnings)),
        Err(first_error) => {
            let repair = LlmRequest {
                system: llm::prompts::planner_system(),
                user: llm::prompts::repair_user(&response.text, &first_error),
                json: true,
                max_tokens: 2400,
                temperature: 0.0,
            };
            let fixed = client
                .complete(&repair)
                .map_err(|error| format!("{}; the repair call failed: {}", first_error, error))?;
            let (plan, warnings) = parse_plan(&fixed.text, query, subs)?;
            Ok((plan, fixed.model, warnings))
        }
    }
}

/// Parse and clean one model reply.
pub fn parse_plan(
    text: &str,
    query: &str,
    subs: &[String],
) -> Result<(ResearchPlan, Vec<String>), String> {
    let value = llm::parse_json_object(text)?;
    let plan: ResearchPlan =
        serde_json::from_value(value).map_err(|error| format!("plan JSON shape: {}", error))?;
    let (plan, warnings) = validate(plan, query, subs);
    if plan.sub_questions.is_empty() {
        return Err("plan has no usable sub-questions".to_string());
    }
    Ok((plan, warnings))
}

pub fn validate(
    mut plan: ResearchPlan,
    query: &str,
    default_subs: &[String],
) -> (ResearchPlan, Vec<String>) {
    let mut warnings = Vec::new();
    plan.schema_version = SCHEMA_VERSION;
    let question = collapse(&plan.question);
    plan.question = if question.is_empty() {
        query.trim().to_string()
    } else {
        question
    };
    plan.language = if plan.language.trim().is_empty() {
        "en".to_string()
    } else {
        truncate(&plan.language.trim().to_lowercase(), 12)
    };
    plan.time_window.after = clean_date(plan.time_window.after, "after", &mut warnings);
    plan.time_window.before = clean_date(plan.time_window.before, "before", &mut warnings);

    let original_questions = plan.sub_questions.len();
    let mut questions = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for (index, question) in std::mem::take(&mut plan.sub_questions)
        .into_iter()
        .enumerate()
    {
        if questions.len() >= 8 {
            break;
        }
        let text = collapse(&question.text);
        if text.is_empty() {
            warnings.push("dropped a sub-question with no text".to_string());
            continue;
        }
        let mut id = question.id.trim().to_ascii_lowercase();
        if id.is_empty() {
            id = format!("q{}", index + 1);
        }
        let base = id.clone();
        let mut suffix = 2;
        while ids.iter().any(|seen| seen == &id) {
            id = format!("{}-{}", base, suffix);
            suffix += 1;
        }
        let mut keywords = clean_list(question.keywords, 12, 40);
        if keywords.is_empty() {
            keywords = engine::topic_terms(&text).into_iter().take(5).collect();
        }
        ids.push(id.clone());
        questions.push(SubQuestion {
            id,
            text,
            keywords,
            success: truncate(&collapse(&question.success), 240),
        });
    }
    if original_questions > 8 {
        warnings.push(format!(
            "kept the first 8 of {} sub-questions",
            original_questions
        ));
    }
    plan.sub_questions = questions;

    plan.entities = clean_list(std::mem::take(&mut plan.entities), 10, 50);

    let mut synonyms = BTreeMap::new();
    for (key, values) in std::mem::take(&mut plan.synonyms) {
        let key = collapse(&key);
        if key.is_empty() {
            continue;
        }
        let values = clean_list(values, 5, 40);
        if values.is_empty() {
            continue;
        }
        synonyms.insert(key, values);
        if synonyms.len() >= 12 {
            break;
        }
    }
    plan.synonyms = synonyms;

    let original_subreddits = plan.subreddits.len();
    let mut picks: Vec<SubredditPick> = Vec::new();
    for pick in std::mem::take(&mut plan.subreddits) {
        if picks.len() >= 10 {
            break;
        }
        let name = clean_subreddit(&pick.name);
        if !valid_subreddit(&name) {
            if !name.is_empty() {
                warnings.push(format!("dropped an invalid subreddit name: {}", name));
            }
            continue;
        }
        if picks
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        picks.push(SubredditPick {
            name,
            priority: pick.priority.clamp(1, 5),
            why: truncate(&collapse(&pick.why), 200),
        });
    }
    if picks.is_empty() && !default_subs.is_empty() {
        for (index, name) in default_subs.iter().take(8).enumerate() {
            let name = clean_subreddit(name);
            if valid_subreddit(&name) {
                picks.push(SubredditPick {
                    name,
                    priority: if index < 3 { 1 } else { 3 },
                    why: "Default subreddit list".to_string(),
                });
            }
        }
    }
    if original_subreddits > 10 {
        warnings.push(format!(
            "kept the first 10 of {} subreddits",
            original_subreddits
        ));
    }
    plan.subreddits = picks;

    let base = fallback_query(&plan);
    plan.source_queries.archive_posts =
        clean_queries(std::mem::take(&mut plan.source_queries.archive_posts), 12);
    plan.source_queries.archive_comments = clean_queries(
        std::mem::take(&mut plan.source_queries.archive_comments),
        12,
    );
    plan.source_queries.reddit_search =
        clean_queries(std::mem::take(&mut plan.source_queries.reddit_search), 12);
    plan.source_queries.web = clean_queries(std::mem::take(&mut plan.source_queries.web), 12);
    if !base.is_empty() {
        if plan.source_queries.archive_posts.is_empty() {
            plan.source_queries.archive_posts.push(base.clone());
        }
        if plan.source_queries.archive_comments.is_empty() {
            plan.source_queries.archive_comments.push(base.clone());
        }
        if plan.source_queries.reddit_search.is_empty() {
            plan.source_queries.reddit_search.push(base.clone());
        }
        if plan.source_queries.web.is_empty() {
            plan.source_queries
                .web
                .push(format!("site:reddit.com {}", base));
        }
    }

    plan.exclude_terms = clean_list(std::mem::take(&mut plan.exclude_terms), 10, 40);
    plan.notes = truncate(&collapse(&plan.notes), 600);
    (plan, warnings)
}

/// A simple plan for use with no model.
pub fn heuristic(query: &str, subs: &[String]) -> ResearchPlan {
    let terms = engine::topic_terms(query);
    let seed: Vec<String> = if terms.is_empty() {
        query
            .split_whitespace()
            .map(|word| {
                word.trim_matches(|ch: char| !ch.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|word| !word.is_empty())
            .take(4)
            .collect()
    } else {
        terms.clone()
    };
    let base = if seed.is_empty() {
        truncate(&collapse(query), 80)
    } else {
        seed.iter().take(5).cloned().collect::<Vec<_>>().join(" ")
    };
    let web_queries = if base.is_empty() {
        Vec::new()
    } else {
        vec![format!("site:reddit.com {}", base)]
    };
    let source_queries = SourceQueries {
        archive_posts: if base.is_empty() {
            Vec::new()
        } else {
            vec![base.clone()]
        },
        archive_comments: if base.is_empty() {
            Vec::new()
        } else {
            vec![base.clone()]
        },
        reddit_search: if base.is_empty() {
            Vec::new()
        } else {
            vec![base.clone()]
        },
        web: web_queries,
    };
    let questions = vec![
        SubQuestion {
            id: "q1".to_string(),
            text: "What benefits and results do users report?".to_string(),
            keywords: with_extra(&seed, "benefit"),
            success: "At least three first-hand reports of a benefit.".to_string(),
        },
        SubQuestion {
            id: "q2".to_string(),
            text: "What side effects and problems do users report?".to_string(),
            keywords: with_extra(&seed, "side effects"),
            success: "Specific side effects, severity, and recovery time.".to_string(),
        },
        SubQuestion {
            id: "q3".to_string(),
            text: "What practical advice, doses, or comparisons do users share?".to_string(),
            keywords: with_extra(&seed, "advice"),
            success: "Concrete steps or comparisons from people who tried it.".to_string(),
        },
        SubQuestion {
            id: "q4".to_string(),
            text: "Which claims are disputed or have little detail?".to_string(),
            keywords: with_extra(&seed, "warning"),
            success: "A clear list of weak, mixed, or missing evidence.".to_string(),
        },
    ];
    let picks = subs
        .iter()
        .take(8)
        .enumerate()
        .map(|(index, name)| SubredditPick {
            name: clean_subreddit(name),
            priority: if index < 3 { 1 } else { 3 },
            why: "Default subreddit list".to_string(),
        })
        .collect();
    let plan = ResearchPlan {
        question: query.trim().to_string(),
        entities: seed,
        sub_questions: questions,
        subreddits: picks,
        source_queries,
        notes: "Local fallback plan. Add an AI key in Settings > Model for richer queries and subreddits."
            .to_string(),
        ..ResearchPlan::default()
    };
    validate(plan, query, subs).0
}

/// Debug CLI. Writes plan-preview.json and plan-preview.log under the app folder.
pub fn plan_only(query: &str) -> i32 {
    let query = query.trim();
    let dir = config::app_dir();
    let _ = fs::create_dir_all(&dir);
    let plan_path = dir.join("plan-preview.json");
    let log_path = dir.join("plan-preview.log");
    if query.is_empty() {
        let message = "No query. Use --plan-only \"your question\".\n";
        let _ = fs::write(&log_path, message);
        println!("{}", message.trim_end());
        return 2;
    }
    let saved = config::load();
    let key = secrets::load(&secrets::target(&saved.llm_provider)).unwrap_or_default();
    let config = LlmConfig::from_saved(&saved, key);
    let subs = engine::parse_subs(&saved.subs);
    let outcome = build(config, query, &subs);
    let json = serde_json::to_string_pretty(&outcome.plan).unwrap_or_default();
    let write_error = fs::write(&plan_path, &json).err();
    let mut log = format!(
        "query: {}\nsource: {}\nsub-questions: {}\nsubreddits: {}\nfile: {}\n",
        query,
        outcome.source,
        outcome.plan.sub_questions.len(),
        outcome.plan.subreddits.len(),
        plan_path.display()
    );
    if let Some(warning) = &outcome.warning {
        log.push_str(&format!("note: {}\n", warning));
    }
    for warning in &outcome.warnings {
        log.push_str(&format!("warning: {}\n", warning));
    }
    if let Some(error) = &write_error {
        log.push_str(&format!("write error: {}\n", error));
    }
    let _ = fs::write(&log_path, &log);
    println!("{}", log);
    if write_error.is_some() {
        return 2;
    }
    if outcome.ai_used || outcome.warning.as_deref() == Some("no API key") {
        0
    } else {
        3
    }
}

/// Headless acceptance check: build a plan, collect records, write a draft.
pub fn collect_only(query: &str) -> i32 {
    let query = query.trim();
    let dir = config::app_dir();
    let _ = fs::create_dir_all(&dir);
    let log_path = dir.join("collect-preview.log");
    if query.is_empty() {
        let message = "No query. Use --collect-only \"your question\".\n";
        let _ = fs::write(&log_path, message);
        println!("{}", message.trim_end());
        return 2;
    }
    let saved = config::load();
    let key = secrets::load(&secrets::target(&saved.llm_provider)).unwrap_or_default();
    let llm = LlmConfig::from_saved(&saved, key);
    let subs = engine::parse_subs(&saved.subs);
    let outcome = build(llm.clone(), query, &subs);
    let mut settings = engine::Settings::from_config(&saved, String::new());
    settings.topic = query.to_string();
    settings.plan = Some(outcome.plan.clone());
    settings.plan_source = outcome.source.clone();
    settings.llm = Some(llm);
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    engine::spawn(settings, tx, cancel);
    let mut lines = vec![
        format!("query: {}", query),
        format!("plan: {}", outcome.source),
    ];
    if let Some(warning) = &outcome.warning {
        lines.push(format!("note: {}", warning));
    }
    let mut saw_stage = false;
    let mut code = 2;
    for message in rx {
        match message {
            engine::Msg::Log(line) => lines.push(line),
            engine::Msg::Progress { total, requests } => {
                lines.push(format!(
                    "progress: {} records, {} requests",
                    total, requests
                ));
            }
            engine::Msg::Stage(stage) => {
                saw_stage = true;
                lines.push(format!(
                    "stage {}: {} ({}) {}",
                    stage.name, stage.status, stage.count, stage.note
                ));
            }
            engine::Msg::Auth(_) => {}
            engine::Msg::Done {
                total,
                dir,
                error,
                draft,
                stages,
                ..
            } => {
                if !saw_stage {
                    for stage in &stages {
                        lines.push(format!(
                            "stage {}: {} ({}) {}",
                            stage.name, stage.status, stage.count, stage.note
                        ));
                    }
                }
                if let Some(draft) = &draft {
                    lines.push(format!(
                        "draft: {} sections, {} records, file {}",
                        draft.sections.len(),
                        draft.total,
                        dir
                    ));
                }
                lines.push(match &error {
                    Some(err) => {
                        format!("done with error: {} ({} records in {})", err, total, dir)
                    }
                    None => format!("done: {} records in {}", total, dir),
                });
                code = if error.is_none() && total > 0 { 0 } else { 2 };
                break;
            }
        }
    }
    let text = lines.join("\n");
    let _ = fs::write(&log_path, &text);
    println!("{}", text);
    println!("log: {}", log_path.display());
    code
}

fn with_extra(terms: &[String], extra: &str) -> Vec<String> {
    let mut out: Vec<String> = terms.iter().take(4).cloned().collect();
    if !out.iter().any(|word| word.eq_ignore_ascii_case(extra)) {
        out.push(extra.to_string());
    }
    out
}

fn fallback_query(plan: &ResearchPlan) -> String {
    let mut words: Vec<String> = Vec::new();
    for question in plan.sub_questions.iter().take(2) {
        for keyword in &question.keywords {
            if !words.iter().any(|word| word.eq_ignore_ascii_case(keyword)) {
                words.push(keyword.clone());
            }
        }
    }
    if words.is_empty() {
        for entity in &plan.entities {
            if !words.iter().any(|word| word.eq_ignore_ascii_case(entity)) {
                words.push(entity.clone());
            }
        }
    }
    if words.is_empty() {
        words = engine::topic_terms(&plan.question);
    }
    if words.is_empty() {
        words = plan
            .question
            .split_whitespace()
            .map(|word| {
                word.trim_matches(|ch: char| !ch.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|word| !word.is_empty())
            .take(5)
            .collect();
    }
    truncate(&words.into_iter().take(5).collect::<Vec<_>>().join(" "), 80)
}

fn clean_date(value: Option<String>, label: &str, warnings: &mut Vec<String>) -> Option<String> {
    let value = value?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return None;
    }
    if chrono::NaiveDate::parse_from_str(&value, "%Y-%m-%d").is_ok() {
        Some(value)
    } else {
        warnings.push(format!("dropped an invalid {} date: {}", label, value));
        None
    }
}

fn clean_list(items: Vec<String>, max_items: usize, max_len: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let item = truncate(&collapse(&item), max_len);
        if item.is_empty() {
            continue;
        }
        if out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&item))
        {
            continue;
        }
        out.push(item);
        if out.len() >= max_items {
            break;
        }
    }
    out
}

fn clean_queries(items: Vec<String>, max_items: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let item = truncate(&collapse(&item), 90);
        if item.is_empty() || !item.chars().any(|ch| ch.is_alphanumeric()) {
            continue;
        }
        if out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&item))
        {
            continue;
        }
        out.push(item);
        if out.len() >= max_items {
            break;
        }
    }
    out
}

fn clean_subreddit(name: &str) -> String {
    name.trim()
        .trim_start_matches("r/")
        .trim_start_matches('/')
        .trim()
        .to_string()
}

fn valid_subreddit(name: &str) -> bool {
    let length = name.chars().count();
    (2..=21).contains(&length)
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(text: &str, max_chars: usize) -> String {
    text.chars()
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan() -> ResearchPlan {
        ResearchPlan {
            question: "How does armodafinil help with study?".to_string(),
            sub_questions: vec![
                SubQuestion {
                    id: String::new(),
                    text: "What benefits do students report?".to_string(),
                    keywords: Vec::new(),
                    success: String::new(),
                },
                SubQuestion {
                    id: "q1".to_string(),
                    text: "What side effects appear?".to_string(),
                    keywords: vec!["armodafinil side effects".to_string()],
                    success: "specific effects".to_string(),
                },
            ],
            subreddits: vec![
                SubredditPick {
                    name: "r/afinil".to_string(),
                    priority: 9,
                    why: String::new(),
                },
                SubredditPick {
                    name: "bad name!".to_string(),
                    priority: 1,
                    why: String::new(),
                },
                SubredditPick {
                    name: "afinil".to_string(),
                    priority: 1,
                    why: "duplicate".to_string(),
                },
            ],
            ..ResearchPlan::default()
        }
    }

    #[test]
    fn validation_cleans_and_fills() {
        let (plan, warnings) = validate(sample_plan(), "fallback question", &[]);
        assert_eq!(plan.question, "How does armodafinil help with study?");
        assert_eq!(plan.sub_questions.len(), 2);
        assert_eq!(plan.sub_questions[0].id, "q1");
        assert_eq!(plan.sub_questions[1].id, "q1-2");
        assert!(!plan.sub_questions[0].keywords.is_empty());
        assert_eq!(plan.subreddits.len(), 1);
        assert_eq!(plan.subreddits[0].name, "afinil");
        assert_eq!(plan.subreddits[0].priority, 5);
        assert!(!plan.source_queries.archive_posts.is_empty());
        assert!(plan.source_queries.web[0].starts_with("site:reddit.com "));
        assert!(warnings.iter().any(|warning| warning.contains("bad name!")));
    }

    #[test]
    fn parse_rejects_plain_text() {
        assert!(parse_plan("no json here", "question", &[]).is_err());
    }

    #[test]
    fn parse_accepts_wrapped_json() {
        let text = r#"```json
{"question":"q","sub_questions":[{"text":"what happens?","keywords":["what happens"]}]}
```"#;
        let (plan, _) = parse_plan(text, "q", &[]).unwrap();
        assert_eq!(plan.sub_questions.len(), 1);
        assert_eq!(plan.subreddits.len(), 0);
    }

    #[test]
    fn heuristic_plan_is_usable() {
        let plan = heuristic(
            "how does armodafinil help with learning",
            &["afinil".to_string(), "Nootropics".to_string()],
        );
        assert_eq!(plan.sub_questions.len(), 4);
        assert_eq!(plan.subreddits.len(), 2);
        assert!(!plan.source_queries.archive_posts.is_empty());
        assert!(plan.source_queries.web[0].starts_with("site:reddit.com "));
    }
}
