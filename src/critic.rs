//! Coverage checks and AI gap analysis for the critic loop.
//!
//! The rules follow docs/design.md section 10. After each collection round the
//! critic asks one question: does the evidence answer each sub-question? Local
//! checks run first. The model is asked only when a check fails, and it
//! returns short new search queries for the next round.

use crate::engine;
use crate::evidence;
use crate::llm::{self, LlmConfig, LlmRequest};
use crate::model::Record;
use crate::plan::ResearchPlan;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MIN_THREADS: usize = 5;
pub const MIN_SUBREDDITS: usize = 2;
pub const MIN_SPAN_DAYS: i64 = 180;
pub const MAX_GAPS: usize = 8;
pub const MAX_QUERIES_PER_GAP: usize = 3;

/// One evidence need from the plan, or the topic when there is no plan.
#[derive(Clone, Debug)]
pub struct Question {
    pub id: String,
    pub text: String,
    pub keywords: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Coverage {
    pub id: String,
    pub sub_question: String,
    pub records: usize,
    pub distinct_threads: usize,
    pub distinct_subreddits: usize,
    pub distinct_authors: usize,
    pub time_span_days: i64,
    pub has_positive: bool,
    pub has_negative: bool,
    pub first_hand: bool,
    pub evaluative: bool,
    pub meets_criteria: bool,
    pub unmet: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GapAnalysis {
    pub gaps: Vec<Gap>,
    pub stop: bool,
    pub notes: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Gap {
    pub sub_question_id: String,
    pub reason: String,
    pub new_queries: Vec<String>,
}

/// Plan sub-questions, or one question made from the topic.
pub fn questions(plan: Option<&ResearchPlan>, topic: &str) -> Vec<Question> {
    if let Some(plan) = plan {
        let mut out: Vec<Question> = Vec::new();
        for question in &plan.sub_questions {
            let id = question.id.trim();
            out.push(Question {
                id: if id.is_empty() {
                    format!("q{}", out.len() + 1)
                } else {
                    id.to_string()
                },
                text: question.text.clone(),
                keywords: question.keywords.clone(),
            });
        }
        if !out.is_empty() {
            return out;
        }
    }
    vec![Question {
        id: "q".to_string(),
        text: topic.trim().to_string(),
        keywords: engine::topic_terms(topic),
    }]
}

/// Check every sub-question against the prepared records.
pub fn check(records: &[Record], plan: Option<&ResearchPlan>, topic: &str) -> Vec<Coverage> {
    let planned_subreddits = plan.map(|plan| plan.subreddits.len()).unwrap_or(0);
    questions(plan, topic)
        .iter()
        .map(|question| coverage_for(records, question, planned_subreddits))
        .collect()
}

pub fn unmet_count(coverage: &[Coverage]) -> usize {
    coverage.iter().filter(|item| !item.meets_criteria).count()
}

fn coverage_for(records: &[Record], question: &Question, planned_subreddits: usize) -> Coverage {
    let matched: Vec<&Record> = records
        .iter()
        .filter(|record| record.sub_questions.iter().any(|id| id == &question.id))
        .collect();
    let mut threads: HashSet<&str> = HashSet::new();
    let mut subreddits: HashSet<String> = HashSet::new();
    let mut authors: HashSet<String> = HashSet::new();
    let mut min_created = f64::MAX;
    let mut max_created = f64::MIN;
    let mut has_positive = false;
    let mut has_negative = false;
    let mut first_hand = false;
    for record in &matched {
        if !record.thread().is_empty() {
            threads.insert(record.thread());
        }
        if !record.subreddit.trim().is_empty() {
            subreddits.insert(record.subreddit.trim().to_lowercase());
        }
        if !record.author.trim().is_empty() && record.author != "[deleted]" {
            authors.insert(record.author.trim().to_lowercase());
        }
        if record.created_utc > 0.0 {
            min_created = min_created.min(record.created_utc);
            max_created = max_created.max(record.created_utc);
        }
        let text = if record.body.trim().is_empty() {
            record.title.as_str()
        } else {
            record.body.as_str()
        };
        if positive(text) {
            has_positive = true;
        }
        if evidence::dissenting(text) {
            has_negative = true;
        }
        if first_person(text) {
            first_hand = true;
        }
    }
    let time_span_days = if max_created > 0.0 && min_created < f64::MAX {
        ((max_created - min_created) / 86_400.0).max(0.0) as i64
    } else {
        0
    };
    let evaluative = is_evaluative(question);

    let mut unmet: Vec<String> = Vec::new();
    if matched.is_empty() {
        unmet.push("no records yet".to_string());
    }
    if threads.len() < MIN_THREADS {
        unmet.push(format!("{} of {} threads", threads.len(), MIN_THREADS));
    }
    if planned_subreddits >= MIN_SUBREDDITS && subreddits.len() < MIN_SUBREDDITS {
        unmet.push(format!(
            "{} of {} subreddits",
            subreddits.len(),
            MIN_SUBREDDITS
        ));
    }
    if time_span_days < MIN_SPAN_DAYS {
        unmet.push(format!(
            "{} day time span, needs {}",
            time_span_days, MIN_SPAN_DAYS
        ));
    }
    if evaluative && !(has_positive && has_negative) {
        unmet.push("needs both positive and negative reports".to_string());
    }
    if !first_hand {
        unmet.push("needs one first-hand report".to_string());
    }

    Coverage {
        id: question.id.clone(),
        sub_question: question.text.clone(),
        records: matched.len(),
        distinct_threads: threads.len(),
        distinct_subreddits: subreddits.len(),
        distinct_authors: authors.len(),
        time_span_days,
        has_positive,
        has_negative,
        first_hand,
        evaluative,
        meets_criteria: unmet.is_empty(),
        unmet,
    }
}

/// True when the text reports a good result.
pub fn positive(text: &str) -> bool {
    let lower = text.to_lowercase();
    const CUES: [&str; 14] = [
        "helped",
        "works",
        "worked",
        "great",
        "improved",
        "better",
        "love",
        "recommend",
        "effective",
        "benefit",
        "worth",
        "success",
        "game changer",
        "lifesaver",
    ];
    CUES.iter().any(|cue| lower.contains(cue))
}

/// True when the text has a first-person marker.
pub fn first_person(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower
        .split(|ch: char| !ch.is_alphanumeric() && ch != '\'')
        .any(|word| {
            matches!(
                word,
                "i" | "i'm" | "i've" | "i'd" | "i'll" | "my" | "me" | "we" | "our" | "us"
            )
        })
}

fn is_evaluative(question: &Question) -> bool {
    let text = format!("{} {}", question.text, question.keywords.join(" ")).to_lowercase();
    const CUES: [&str; 18] = [
        "benefit",
        "help",
        "work",
        "effect",
        "experience",
        "worth",
        "safe",
        "risk",
        "problem",
        "side effect",
        "better",
        "worse",
        "compare",
        " vs ",
        "review",
        "opinion",
        "harm",
        "bad",
    ];
    CUES.iter().any(|cue| text.contains(cue))
}

/// Read and clean one critic reply.
pub fn parse_gap_analysis(text: &str, valid_ids: &[String]) -> Result<GapAnalysis, String> {
    let value = llm::parse_json_object(text)?;
    let analysis: GapAnalysis =
        serde_json::from_value(value).map_err(|error| format!("gap JSON shape: {}", error))?;
    Ok(clean(analysis, valid_ids))
}

fn clean(mut analysis: GapAnalysis, valid_ids: &[String]) -> GapAnalysis {
    let mut gaps: Vec<Gap> = Vec::new();
    for gap in std::mem::take(&mut analysis.gaps) {
        if gaps.len() >= MAX_GAPS {
            break;
        }
        let id = gap.sub_question_id.trim().to_ascii_lowercase();
        if !valid_ids.iter().any(|valid| valid.eq_ignore_ascii_case(&id)) {
            continue;
        }
        let mut queries: Vec<String> = Vec::new();
        for query in gap.new_queries {
            let query = collapse(&query);
            let query: String = query.chars().take(90).collect();
            if query.is_empty() || !query.chars().any(|ch| ch.is_alphanumeric()) {
                continue;
            }
            if queries
                .iter()
                .any(|seen| seen.eq_ignore_ascii_case(&query))
            {
                continue;
            }
            queries.push(query);
            if queries.len() >= MAX_QUERIES_PER_GAP {
                break;
            }
        }
        if queries.is_empty() {
            continue;
        }
        gaps.push(Gap {
            sub_question_id: id,
            reason: gap.reason.chars().take(200).collect::<String>().trim().to_string(),
            new_queries: queries,
        });
    }
    analysis.gaps = gaps;
    analysis.notes = analysis
        .notes
        .chars()
        .take(600)
        .collect::<String>()
        .trim()
        .to_string();
    analysis
}

/// Short queries that were not tried before.
pub fn new_queries(analysis: &GapAnalysis, run: &HashSet<String>, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for gap in &analysis.gaps {
        for query in &gap.new_queries {
            if out.len() >= cap {
                return out;
            }
            let key = query.to_lowercase();
            if run.contains(&key) || out.iter().any(|seen| seen.to_lowercase() == key) {
                continue;
            }
            out.push(query.clone());
        }
    }
    out
}

/// Compact summary for the critic. No full texts, only counts and titles.
pub fn analysis_user(
    topic: &str,
    coverages: &[Coverage],
    records: &[Record],
) -> String {
    let mut lines: Vec<String> = vec![format!("Research question: {}", topic.trim())];
    lines.push(String::new());
    lines.push("Coverage:".to_string());
    for coverage in coverages {
        lines.push(format!(
            "- {}: {} ({})",
            coverage.id, coverage.sub_question, status(coverage)
        ));
        if !coverage.unmet.is_empty() {
            lines.push(format!("  unmet: {}", coverage.unmet.join("; ")));
        }
        let titles = top_titles(records, &coverage.id, 5);
        if !titles.is_empty() {
            lines.push(format!("  top titles: {}", titles.join(" | ")));
        }
    }
    lines.push(String::new());
    lines.push(
        "Find real gaps only. Do not ask for more of the same data. For each gap, give 1 to 3 new short search queries of 2 to 5 words. Set stop to true when the evidence is good enough.".to_string(),
    );
    lines.join("\n")
}

fn status(coverage: &Coverage) -> String {
    if coverage.meets_criteria {
        return "criteria met".to_string();
    }
    format!("{} records", coverage.records)
}

fn top_titles(records: &[Record], question_id: &str, wanted: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for record in records {
        if out.len() >= wanted {
            break;
        }
        if !record.sub_questions.iter().any(|id| id == question_id) {
            continue;
        }
        let text = if !record.title.trim().is_empty() {
            record.title.trim()
        } else {
            record.body.trim()
        };
        let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let short: String = text.chars().take(90).collect();
        if !short.is_empty() && !out.iter().any(|seen| seen == &short) {
            out.push(short);
        }
    }
    out
}

/// Ask the model for gaps. One repair call if the JSON is broken.
pub fn analyze(
    config: &LlmConfig,
    user: &str,
    valid_ids: &[String],
) -> Result<(GapAnalysis, String), String> {
    let client = llm::client(config).map_err(|error| error.to_string())?;
    let temperature = config.temperature.min(0.3);
    let request = LlmRequest {
        system: llm::prompts::critic_system(),
        user: user.to_string(),
        json: true,
        max_tokens: 1200,
        temperature,
    };
    let response = client
        .complete(&request)
        .map_err(|error| error.to_string())?;
    match parse_gap_analysis(&response.text, valid_ids) {
        Ok(analysis) => Ok((analysis, response.model)),
        Err(first_error) => {
            let repair = LlmRequest {
                system: llm::prompts::critic_system(),
                user: llm::prompts::repair_json_user(&response.text, &first_error),
                json: true,
                max_tokens: 1200,
                temperature: 0.0,
            };
            let fixed = client
                .complete(&repair)
                .map_err(|error| format!("{}; the repair call failed: {}", first_error, error))?;
            let analysis = parse_gap_analysis(&fixed.text, valid_ids)?;
            Ok((analysis, fixed.model))
        }
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::SubQuestion;
    use std::collections::HashSet;

    fn record(id: &str, sub: &str, thread: &str, body: &str, created: f64) -> Record {
        Record {
            id: id.to_string(),
            kind: "comment".to_string(),
            subreddit: sub.to_string(),
            thread_id: thread.to_string(),
            parent_excerpt: None,
            author: format!("user_{}", id),
            created_utc: created,
            score: 10,
            url: String::new(),
            num_comments: 0,
            title: String::new(),
            body: body.to_string(),
            permalink: String::new(),
            sources: HashSet::from(["archive_comments".to_string()]),
            sub_questions: vec!["q1".to_string()],
            relevance: 1.0,
            final_score: 0.5,
            notes: Vec::new(),
        }
    }

    fn plan() -> ResearchPlan {
        ResearchPlan {
            sub_questions: vec![SubQuestion {
                id: "q1".to_string(),
                text: "What benefits do users report?".to_string(),
                keywords: vec!["adderall benefit".to_string()],
                success: String::new(),
            }],
            subreddits: vec![
                crate::plan::SubredditPick {
                    name: "ADHD".to_string(),
                    priority: 1,
                    why: String::new(),
                },
                crate::plan::SubredditPick {
                    name: "adderall".to_string(),
                    priority: 1,
                    why: String::new(),
                },
            ],
            ..ResearchPlan::default()
        }
    }

    fn spread() -> Vec<Record> {
        let start = 1_600_000_000.0;
        let mut records = Vec::new();
        for index in 0..6 {
            let sub = if index % 2 == 0 { "ADHD" } else { "adderall" };
            let body = if index == 0 {
                "I had a great first week, it helped me focus"
            } else if index == 1 {
                "It did not work for me and gave me anxiety"
            } else {
                "my focus is better after one year"
            };
            records.push(record(
                &format!("t1_{}", index),
                sub,
                &format!("t3_{}", index),
                body,
                start + index as f64 * 20_000_000.0,
            ));
        }
        records
    }

    #[test]
    fn coverage_meets_criteria_with_spread_evidence() {
        let coverages = check(&spread(), Some(&plan()), "adderall");
        assert_eq!(coverages.len(), 1);
        assert!(coverages[0].meets_criteria, "{:?}", coverages[0].unmet);
        assert!(coverages[0].has_positive);
        assert!(coverages[0].has_negative);
        assert!(coverages[0].first_hand);
    }

    #[test]
    fn coverage_lists_unmet_threads_and_subreddits() {
        let mut records = spread().into_iter().take(2).collect::<Vec<_>>();
        for record in &mut records {
            record.subreddit = "ADHD".to_string();
        }
        let coverages = check(&records, Some(&plan()), "adderall");
        assert!(!coverages[0].meets_criteria);
        assert!(coverages[0].unmet.iter().any(|item| item.contains("of 5 threads")));
        assert!(coverages[0].unmet.iter().any(|item| item.contains("of 2 subreddits")));
    }

    #[test]
    fn empty_question_needs_records() {
        let coverages = check(&[], Some(&plan()), "adderall");
        assert!(!coverages[0].meets_criteria);
        assert!(coverages[0].unmet.iter().any(|item| item == "no records yet"));
    }

    #[test]
    fn topic_only_check_uses_the_topic_question() {
        let mut records = spread();
        for record in &mut records {
            record.sub_questions = vec!["q".to_string()];
        }
        let questions = questions(None, "adderall");
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].id, "q");
        let coverages = check(&records, None, "adderall");
        assert_eq!(coverages[0].records, 6);
    }

    #[test]
    fn first_person_markers_are_detected() {
        assert!(first_person("I tried it and my focus improved"));
        assert!(first_person("we both quit after a month"));
        assert!(!first_person("the drug improves focus for many people"));
    }

    #[test]
    fn parses_and_cleans_gap_analysis() {
        let text = r#"{
            "gaps": [
                {"sub_question_id": "q1", "reason": "one subreddit only",
                 "new_queries": ["adderall focus", "adderall focus", "  adderall   crash  ", ""]},
                {"sub_question_id": "q9", "reason": "unknown", "new_queries": ["nope"]}
            ],
            "stop": false,
            "notes": "add a comparison"
        }"#;
        let analysis =
            parse_gap_analysis(text, &["q1".to_string()]).expect("gap JSON");
        assert_eq!(analysis.gaps.len(), 1);
        assert_eq!(analysis.gaps[0].new_queries, vec!["adderall focus", "adderall crash"]);
        assert!(!analysis.stop);
    }

    #[test]
    fn new_queries_skip_ones_already_run() {
        let analysis = GapAnalysis {
            gaps: vec![Gap {
                sub_question_id: "q1".to_string(),
                reason: String::new(),
                new_queries: vec![
                    "adderall crash".to_string(),
                    "adderall sleep".to_string(),
                ],
            }],
            stop: false,
            notes: String::new(),
        };
        let run = HashSet::from(["adderall crash".to_string()]);
        assert_eq!(new_queries(&analysis, &run, 6), vec!["adderall sleep"]);
    }

    #[test]
    fn summary_lists_counts_and_titles() {
        let records = spread();
        let coverages = check(&records, Some(&plan()), "adderall");
        let text = analysis_user("adderall first time", &coverages, &records);
        assert!(text.contains("q1"));
        assert!(text.contains("top titles"));
        assert!(text.contains("Find real gaps only"));
    }
}
