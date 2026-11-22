//! Extractive draft report.
//!
//! This module turns collected records into a report preview. It does not call
//! an AI model. It selects exact excerpts, counts them, and groups them under
//! the plan's sub-questions. The result is evidence, not a written conclusion.

use crate::engine;
use crate::model::Record;
use crate::plan::ResearchPlan;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;
const MAX_QUOTES: usize = 3;
const QUOTE_CHARS: usize = 560;
const MAX_THEME_WORDS: usize = 4;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftReport {
    pub schema_version: u32,
    pub title: String,
    pub question: String,
    pub generated: String,
    pub plan_source: String,
    pub total: usize,
    pub subreddits: usize,
    pub summary: String,
    pub sections: Vec<DraftSection>,
    pub method: Vec<String>,
    pub limitations: Vec<String>,
}

impl Default for DraftReport {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            title: String::new(),
            question: String::new(),
            generated: String::new(),
            plan_source: String::new(),
            total: 0,
            subreddits: 0,
            summary: String::new(),
            sections: Vec::new(),
            method: Vec::new(),
            limitations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftSection {
    pub id: String,
    pub question: String,
    pub summary: String,
    pub match_count: usize,
    pub quotes: Vec<DraftQuote>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftQuote {
    pub record_id: String,
    pub kind: String,
    pub text: String,
    pub truncated: bool,
    pub subreddit: String,
    pub author: String,
    pub score: i64,
    pub date: String,
    pub permalink: String,
    /// Text of the parent comment or post, so the quote reads in context.
    #[serde(default)]
    pub context: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct QuoteCheck {
    pub total: usize,
    pub linked: usize,
    pub exact: usize,
}

impl QuoteCheck {
    pub fn all_linked(&self) -> bool {
        self.total == 0 || self.linked == self.total
    }

    pub fn all_exact(&self) -> bool {
        self.total == 0 || self.exact == self.total
    }
}

/// Build the preview. Records should already be in the wanted display order.
pub fn build(
    plan: Option<&ResearchPlan>,
    topic: &str,
    records: &[Record],
    plan_source: &str,
    tactics: &[String],
) -> DraftReport {
    let question = if topic.trim().is_empty() {
        plan.map(|value| value.question.clone()).unwrap_or_default()
    } else {
        topic.trim().to_string()
    };
    let title = if question.is_empty() {
        "Reddit research draft".to_string()
    } else {
        format!("Reddit research: {}", question)
    };
    let subreddits = subreddit_count(records);
    let anchors = anchors_for(plan, &question);
    let mut used: HashSet<String> = HashSet::new();
    let mut sections = Vec::new();

    let questions: Vec<(String, String, Vec<String>)> = match plan {
        Some(plan) if !plan.sub_questions.is_empty() => plan
            .sub_questions
            .iter()
            .map(|item| {
                (
                    item.id.clone(),
                    item.text.clone(),
                    terms_for_question(&item.keywords, &item.text),
                )
            })
            .collect(),
        _ => vec![(
            "q1".to_string(),
            "What do users say?".to_string(),
            engine::topic_terms(&question).into_iter().take(8).collect(),
        )],
    };

    for (id, text, terms) in questions {
        let section = build_section(
            &id,
            &text,
            &terms,
            &anchors,
            records,
            plan.map(|value| value.exclude_terms.as_slice())
                .unwrap_or(&[]),
            &mut used,
        );
        sections.push(section);
    }

    let with_quotes = sections
        .iter()
        .filter(|item| !item.quotes.is_empty())
        .count();
    let quote_total: usize = sections.iter().map(|item| item.quotes.len()).sum();
    let summary = if records.is_empty() {
        "No records were collected. Check the source settings, the request budget, and the network."
            .to_string()
    } else if quote_total == 0 {
        format!(
            "{} records from {} subreddits are stored, but no excerpt matched the plan questions. Change the plan or add sources.",
            records.len(),
            subreddits
        )
    } else {
        format!(
            "{} records from {} subreddits. {} of {} plan questions have matching excerpts. {} excerpts are shown.",
            records.len(),
            subreddits,
            with_quotes,
            sections.len(),
            quote_total
        )
    };

    let mut method = Vec::new();
    method.push(format!(
        "Plan: {}",
        if plan_source.trim().is_empty() {
            "saved plan"
        } else {
            plan_source.trim()
        }
    ));
    method.push(format!(
        "Sources: {}",
        if tactics.is_empty() {
            "saved run".to_string()
        } else {
            tactics.join(", ")
        }
    ));
    if !tactics.is_empty() {
        method.push(format!(
            "Scope: {}.",
            if tactics.iter().any(|name| name == "all_reddit") {
                "all of Reddit, plus the seed subreddits"
            } else {
                "seed subreddits only"
            }
        ));
    }
    if let Some(plan) = plan {
        method.push(format!(
            "Queries: {} archive posts, {} archive comments, {} Reddit searches",
            plan.source_queries.archive_posts.len(),
            plan.source_queries.archive_comments.len(),
            plan.source_queries.reddit_search.len()
        ));
    }
    method.push(format!(
        "Records: {} from {} subreddits.",
        records.len(),
        subreddits
    ));
    method.push(format!(
        "Generated: {}.",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    ));

    let mut limitations = vec![
        "This is an extractive draft. It shows exact excerpts and counts, not a written answer."
            .to_string(),
        "Reddit reports are anecdotal and self-selected. They are not clinical evidence.".to_string(),
        "Coverage depends on the sources and queries that ran. A missing topic is not proof of absence."
            .to_string(),
    ];
    if plan.is_none() {
        limitations.push(
            "No research plan was available. Sections use the question keywords only.".to_string(),
        );
    }

    DraftReport {
        schema_version: SCHEMA_VERSION,
        title,
        question,
        generated: chrono::Local::now().to_rfc3339(),
        plan_source: plan_source.to_string(),
        total: records.len(),
        subreddits,
        summary,
        sections,
        method,
        limitations,
    }
}

fn build_section(
    id: &str,
    question: &str,
    terms: &[String],
    anchors: &[String],
    records: &[Record],
    exclude: &[String],
    used: &mut HashSet<String>,
) -> DraftSection {
    let (matched, strict) = match_candidates(records, terms, anchors, exclude);
    let match_count = matched.len();
    let mut quotes = Vec::new();
    let mut picked: HashSet<String> = HashSet::new();

    for record in &matched {
        if quotes.len() >= MAX_QUOTES {
            break;
        }
        if used.contains(&record.id) || picked.contains(&record.id) {
            continue;
        }
        if let Some(quote) = make_quote(record, anchors, terms) {
            picked.insert(record.id.clone());
            used.insert(record.id.clone());
            quotes.push(quote);
        }
    }

    let section_subs = matched
        .iter()
        .map(|record| record.subreddit.as_str())
        .filter(|sub| !sub.is_empty())
        .collect::<HashSet<_>>()
        .len();
    let themes = top_terms(&matched, terms);
    let summary = if match_count == 0 {
        "No records matched this question in this run. Try a broader query or another source."
            .to_string()
    } else {
        let mut text = format!(
            "{} related records across {} subreddits.",
            match_count, section_subs
        );
        if !themes.is_empty() {
            text.push_str(&format!(" Common words: {}.", themes.join(", ")));
        }
        if !strict {
            text.push_str(" These are broad matches.");
        }
        text.push_str(&format!(" {} excerpts shown.", quotes.len()));
        text
    };

    DraftSection {
        id: if id.trim().is_empty() {
            "q".to_string()
        } else {
            id.trim().to_string()
        },
        question: question.to_string(),
        summary,
        match_count,
        quotes,
    }
}

fn match_candidates<'a>(
    records: &'a [Record],
    terms: &[String],
    anchors: &[String],
    exclude: &[String],
) -> (Vec<&'a Record>, bool) {
    let anchored = |record: &Record| {
        anchors.is_empty() || anchor_index(&record.search_text(), anchors).is_some()
    };
    if terms.is_empty() {
        let matched = records
            .iter()
            .filter(|record| anchored(record) && !excluded(record, exclude))
            .collect::<Vec<_>>();
        return (matched, true);
    }
    let strict_hits = if terms.len() >= 2 { 2 } else { 1 };
    let mut strict = records
        .iter()
        .filter(|record| {
            anchored(record) && !excluded(record, exclude) && hits(record, terms) >= strict_hits
        })
        .collect::<Vec<_>>();
    if strict.is_empty() {
        strict = records
            .iter()
            .filter(|record| {
                anchored(record) && !excluded(record, exclude) && hits(record, terms) >= 1
            })
            .collect::<Vec<_>>();
        sort_by_relevance(&mut strict, terms);
        return (strict, false);
    }
    sort_by_relevance(&mut strict, terms);
    (strict, true)
}

fn anchors_for(plan: Option<&ResearchPlan>, question: &str) -> Vec<String> {
    let mut anchors = Vec::new();
    if let Some(plan) = plan {
        let lower = question.to_lowercase();
        let mut matched: Vec<String> = Vec::new();
        for entity in &plan.entities {
            let terms = engine::topic_terms(entity);
            if terms.iter().any(|term| lower.contains(term)) {
                matched.push(entity.trim().to_lowercase());
                for term in terms {
                    push_term(&mut anchors, &term);
                }
            }
        }
        // Only expand synonyms for a plan entity that the question names.
        // Synonyms of question verbs ("learning" -> "study") are attributes,
        // not anchors, and would let unrelated posts into the draft.
        for (key, values) in &plan.synonyms {
            let key = key.trim().to_lowercase();
            if !matched.iter().any(|entity| entity == &key) {
                continue;
            }
            for term in engine::topic_terms(&key) {
                push_term(&mut anchors, &term);
            }
            for value in values {
                for term in engine::topic_terms(value) {
                    push_term(&mut anchors, &term);
                }
            }
        }
    }
    if anchors.is_empty() {
        for term in engine::topic_terms(question).into_iter().take(6) {
            push_term(&mut anchors, &term);
        }
    }
    anchors
}

fn sort_by_relevance(records: &mut Vec<&Record>, terms: &[String]) {
    records.sort_by(|a, b| {
        let score_a = relevance(a, terms);
        let score_b = relevance(b, terms);
        score_b.cmp(&score_a).then_with(|| {
            b.created_utc
                .partial_cmp(&a.created_utc)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
}

fn relevance(record: &Record, terms: &[String]) -> i64 {
    let count = hits(record, terms) as i64;
    count * 10_000 + record.score.clamp(0, 9_000)
}

fn hits(record: &Record, terms: &[String]) -> usize {
    let hay = record.search_text();
    terms
        .iter()
        .filter(|term| !term.is_empty() && hay.contains(term.as_str()))
        .count()
}

fn excluded(record: &Record, exclude: &[String]) -> bool {
    if exclude.is_empty() {
        return false;
    }
    let hay = record.search_text();
    exclude
        .iter()
        .filter(|term| term.chars().count() >= 4)
        .any(|term| hay.contains(&term.to_lowercase()))
}

fn make_quote(record: &Record, anchors: &[String], terms: &[String]) -> Option<DraftQuote> {
    let body = record.body.trim();
    let title = record.title.trim();
    let body_anchor = if anchors.is_empty() {
        None
    } else {
        anchor_index(body, anchors)
    };
    let title_anchor = if anchors.is_empty() {
        None
    } else {
        anchor_index(title, anchors)
    };
    let (text, index) = if !body.is_empty() && (anchors.is_empty() || body_anchor.is_some()) {
        (body, body_anchor)
    } else if !title.is_empty() && title_anchor.is_some() {
        (title, title_anchor)
    } else if !body.is_empty() {
        (body, None)
    } else {
        return None;
    };
    if !anchors.is_empty() && index.is_none() {
        return None;
    }
    let focus = if index.is_some() { anchors } else { terms };
    let (text, truncated) = quote_window(text, index, focus, QUOTE_CHARS);
    Some(DraftQuote {
        record_id: record.id.clone(),
        kind: record.kind.clone(),
        text,
        truncated,
        subreddit: record.subreddit.clone(),
        author: record.author.clone(),
        score: record.score,
        date: record.date(),
        permalink: record.permalink.clone(),
        context: record
            .parent_excerpt
            .as_deref()
            .map(|text| quote_context(text, 240))
            .filter(|text| !text.is_empty()),
    })
}

fn quote_context(text: &str, limit: usize) -> String {
    let combined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if combined.chars().count() <= limit {
        return combined;
    }
    let mut out: String = combined.chars().take(limit).collect();
    out.push('…');
    out
}

fn quote_window(
    text: &str,
    index: Option<usize>,
    focus: &[String],
    limit: usize,
) -> (String, bool) {
    let total = text.chars().count();
    if total <= limit {
        return (text.trim().to_string(), false);
    }
    let chars: Vec<char> = text.chars().collect();
    let index = index.or_else(|| focus_index(text, focus)).unwrap_or(0);
    let mut start = index.saturating_sub(90);
    if start > 0 {
        while start < index && !chars[start].is_whitespace() {
            start += 1;
        }
        while start < index && chars[start].is_whitespace() {
            start += 1;
        }
    }
    let window: String = chars[start..].iter().collect();
    let (body, cut) = truncate_quote(&window, limit);
    (body, cut || start > 0)
}

fn anchor_index(text: &str, anchors: &[String]) -> Option<usize> {
    anchors
        .iter()
        .filter_map(|term| word_index(text, term))
        .min()
}

/// Find a whole-word match. Short synonyms such as "armo" must not match
/// inside "armor".
fn word_index(text: &str, term: &str) -> Option<usize> {
    if term.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let bytes = lower.as_bytes();
    let mut from = 0;
    while from < lower.len() {
        let found = lower[from..].find(term)?;
        let at = from + found;
        let end = at + term.len();
        let left_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        let right_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if left_ok && right_ok && text.is_char_boundary(at) {
            return Some(text[..at].chars().count());
        }
        from = at + term.len().max(1);
    }
    None
}

fn focus_index(text: &str, focus: &[String]) -> Option<usize> {
    let lower = text.to_lowercase();
    let mut best: Option<usize> = None;
    for term in focus {
        if term.is_empty() {
            continue;
        }
        if let Some(byte) = lower.find(term) {
            if text.is_char_boundary(byte) {
                let index = text[..byte].chars().count();
                best = Some(best.map_or(index, |value| value.min(index)));
            }
        }
    }
    best
}

fn truncate_quote(text: &str, limit: usize) -> (String, bool) {
    let mut out = String::new();
    let mut count = 0usize;
    let mut last_space = None;
    for ch in text.chars() {
        if count >= limit {
            break;
        }
        if ch.is_whitespace() {
            last_space = Some(out.len());
        }
        out.push(ch);
        count += 1;
    }
    if count < text.chars().count() {
        if let Some(index) = last_space {
            out.truncate(index);
        }
        (out.trim().to_string(), true)
    } else {
        (out.trim().to_string(), false)
    }
}

fn terms_for_question(keywords: &[String], text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for keyword in keywords {
        for term in engine::topic_terms(keyword) {
            push_term(&mut terms, &term);
        }
    }
    if terms.is_empty() {
        for term in engine::topic_terms(text).into_iter().take(6) {
            push_term(&mut terms, &term);
        }
    }
    terms
}

fn push_term(terms: &mut Vec<String>, term: &str) {
    let term = term.trim().to_lowercase();
    if term.is_empty() || terms.iter().any(|item| item == &term) {
        return;
    }
    terms.push(term);
}

fn top_terms(records: &[&Record], known: &[String]) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for record in records.iter().take(40) {
        let text = format!("{} {}", record.title, record.body);
        for term in engine::topic_terms(&text) {
            if term.chars().count() < 4 || known.iter().any(|item| item == &term) {
                continue;
            }
            *counts.entry(term).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .take(MAX_THEME_WORDS)
        .map(|(term, _)| term)
        .collect()
}

fn subreddit_count(records: &[Record]) -> usize {
    records
        .iter()
        .map(|record| record.subreddit.as_str())
        .filter(|sub| !sub.is_empty())
        .collect::<HashSet<_>>()
        .len()
}

/// Check every quote against the stored record and its Reddit link.
pub fn validate(draft: &DraftReport, records: &[Record]) -> QuoteCheck {
    let by_id: HashMap<&str, &Record> = records
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    let mut check = QuoteCheck::default();
    for section in &draft.sections {
        for quote in &section.quotes {
            check.total += 1;
            if quote.permalink.starts_with("https://www.reddit.com/") {
                check.linked += 1;
            }
            if let Some(record) = by_id.get(quote.record_id.as_str()) {
                let body = record.body.as_str();
                let title = record.title.as_str();
                if (!quote.text.is_empty() && body.contains(&quote.text))
                    || (!quote.text.is_empty() && title.contains(&quote.text))
                {
                    check.exact += 1;
                }
            }
        }
    }
    check
}

pub fn to_markdown(draft: &DraftReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", draft.title));
    if !draft.question.is_empty() {
        out.push_str(&format!("{}\n\n", draft.question));
    }
    out.push_str(&format!(
        "{} records from {} subreddits. Generated {}.\n\n",
        draft.total, draft.subreddits, draft.generated
    ));
    out.push_str("> Draft preview. Exact excerpts and counts only. No AI synthesis.\n\n");
    out.push_str("## Summary\n\n");
    out.push_str(&format!("{}\n\n", draft.summary));

    for section in &draft.sections {
        out.push_str(&format!("## {}: {}\n\n", section.id, section.question));
        out.push_str(&format!("{}\n\n", section.summary));
        for quote in &section.quotes {
            if let Some(context) = &quote.context {
                out.push_str(&format!("*In reply to: {}*\n\n", context));
            }
            let body = if quote.truncated {
                format!("{}…", quote.text)
            } else {
                quote.text.clone()
            };
            for line in body.lines() {
                out.push_str(&format!("> {}\n", line));
            }
            out.push('\n');
            let author = if quote.author.trim().is_empty() {
                "unknown".to_string()
            } else {
                format!("u/{}", quote.author)
            };
            let sub = if quote.subreddit.trim().is_empty() {
                "Reddit".to_string()
            } else {
                format!("r/{}", quote.subreddit)
            };
            out.push_str(&format!(
                "— {} in {} · {} · {} points",
                author, sub, quote.date, quote.score
            ));
            if !quote.permalink.is_empty() {
                out.push_str(&format!(" · [Open on Reddit]({})", quote.permalink));
            }
            out.push_str("\n\n");
        }
        if section.quotes.is_empty() {
            out.push_str("No matching excerpts in this run.\n\n");
        }
    }

    out.push_str("## Method\n\n");
    for line in &draft.method {
        out.push_str(&format!("- {}\n", line));
    }
    out.push_str("\n## Limitations\n\n");
    for line in &draft.limitations {
        out.push_str(&format!("- {}\n", line));
    }
    out.push_str("\nQuotes belong to their authors. This draft is for personal research.\n");
    out
}

pub fn write_markdown(draft: &DraftReport, path: &Path) -> Result<(), String> {
    fs::write(path, to_markdown(draft)).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{SourceQueries, SubQuestion};
    use std::collections::HashSet;

    fn record(id: &str, body: &str, subreddit: &str) -> Record {
        Record {
            id: id.to_string(),
            kind: "comment".to_string(),
            subreddit: subreddit.to_string(),
            thread_id: "t3_x".to_string(),
            parent_excerpt: None,
            author: "tester".to_string(),
            created_utc: 1_700_000_000.0,
            score: 12,
            url: String::new(),
            num_comments: 0,
            title: String::new(),
            body: body.to_string(),
            permalink: format!(
                "https://www.reddit.com/r/{}/comments/x/_/{}/",
                subreddit, id
            ),
            sources: HashSet::from(["archive_comments".to_string()]),
            sub_questions: Vec::new(),
            relevance: 0.0,
            final_score: 0.0,
            notes: Vec::new(),
        }
    }

    fn plan() -> ResearchPlan {
        ResearchPlan {
            question: "does armodafinil help with learning?".to_string(),
            sub_questions: vec![
                SubQuestion {
                    id: "q1".to_string(),
                    text: "Do users report better learning?".to_string(),
                    keywords: vec!["armodafinil learning".to_string()],
                    success: String::new(),
                },
                SubQuestion {
                    id: "q2".to_string(),
                    text: "What side effects appear?".to_string(),
                    keywords: vec!["armodafinil side effects".to_string()],
                    success: String::new(),
                },
            ],
            entities: vec!["armodafinil".to_string()],
            source_queries: SourceQueries {
                archive_posts: vec!["armodafinil learning".to_string()],
                archive_comments: vec!["armodafinil side effects".to_string()],
                reddit_search: vec!["armodafinil".to_string()],
                ..SourceQueries::default()
            },
            ..ResearchPlan::default()
        }
    }

    #[test]
    fn builds_sections_with_exact_quotes() {
        let records = vec![
            record(
                "t1_a",
                "Armodafinil helped my learning a lot.",
                "Nootropics",
            ),
            record("t1_b", "The side effects gave me a headache.", "ADHD"),
            record("t1_c", "Unrelated story about coffee.", "ADHD"),
        ];
        let draft = build(
            Some(&plan()),
            "does armodafinil help with learning?",
            &records,
            "DeepSeek · deepseek-chat",
            &["archive_comments".to_string()],
        );
        assert_eq!(draft.sections.len(), 2);
        assert_eq!(draft.sections[0].quotes.len(), 1);
        assert_eq!(
            draft.sections[0].quotes[0].text,
            "Armodafinil helped my learning a lot."
        );
        assert_eq!(draft.sections[0].quotes[0].permalink, records[0].permalink);
        assert!(draft.summary.contains("3 records"));
        assert!(to_markdown(&draft).contains("Armodafinil helped my learning a lot."));
    }

    #[test]
    fn method_states_all_reddit_scope() {
        let records = vec![record(
            "t1_a",
            "Armodafinil helped my learning.",
            "Nootropics",
        )];
        let draft = build(
            Some(&plan()),
            "armodafinil",
            &records,
            "test",
            &["all_reddit".to_string(), "archive_comments".to_string()],
        );
        assert!(to_markdown(&draft)
            .contains("Scope: all of Reddit, plus the seed subreddits."));
    }

    #[test]
    fn validates_quotes_and_links() {
        let records = vec![record(
            "t1_a",
            "Armodafinil improved my study.",
            "Nootropics",
        )];
        let draft = build(Some(&plan()), "armodafinil study", &records, "test", &[]);
        let check = validate(&draft, &records);
        assert_eq!(check.total, 1);
        assert_eq!(check.exact, 1);
        assert_eq!(check.linked, 1);
    }

    #[test]
    fn empty_records_still_produce_sections() {
        let draft = build(Some(&plan()), "armodafinil", &[], "test", &[]);
        assert_eq!(draft.sections.len(), 2);
        assert!(draft
            .sections
            .iter()
            .all(|section| section.quotes.is_empty()));
        assert!(draft.summary.contains("No records"));
        assert!(to_markdown(&draft).contains("No matching excerpts"));
    }

    #[test]
    fn quotes_are_not_repeated_when_other_records_exist() {
        let records = vec![
            record(
                "t1_a",
                "Armodafinil learning helped me study.",
                "Nootropics",
            ),
            record("t1_b", "Armodafinil side effects were mild.", "ADHD"),
        ];
        let draft = build(Some(&plan()), "armodafinil", &records, "test", &[]);
        let ids: Vec<&str> = draft
            .sections
            .iter()
            .flat_map(|section| section.quotes.iter())
            .map(|quote| quote.record_id.as_str())
            .collect();
        let unique: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len());
    }

    #[test]
    fn anchor_keeps_unrelated_quotes_out() {
        let records = vec![record(
            "t1_a",
            "My learning improved after I changed my sleep.",
            "ADHD",
        )];
        let draft = build(
            Some(&plan()),
            "does armodafinil help with learning?",
            &records,
            "test",
            &[],
        );
        assert!(draft
            .sections
            .iter()
            .all(|section| section.quotes.is_empty()));
    }

    fn plan_with_synonyms() -> ResearchPlan {
        let mut plan = plan();
        plan.entities.push("ADHD".to_string());
        plan.synonyms.insert(
            "armodafinil".to_string(),
            vec!["armo".to_string(), "nuvigil".to_string()],
        );
        plan.synonyms.insert(
            "learning".to_string(),
            vec!["studying".to_string(), "study".to_string()],
        );
        plan.synonyms
            .insert("ADHD".to_string(), vec!["attention deficit".to_string()]);
        plan
    }

    #[test]
    fn question_attribute_synonyms_are_not_anchors() {
        let records = vec![record(
            "t1_a",
            "I could not study and my learning was bad. My ADHD made it worse.",
            "ADHD",
        )];
        let draft = build(
            Some(&plan_with_synonyms()),
            "does armodafinil help with learning?",
            &records,
            "test",
            &[],
        );
        assert!(draft
            .sections
            .iter()
            .all(|section| section.quotes.is_empty()));
    }

    #[test]
    fn short_anchor_needs_a_whole_word() {
        let records = vec![record(
            "t1_a",
            "My armor was shiny and studying helped my learning.",
            "ADHD",
        )];
        let draft = build(
            Some(&plan_with_synonyms()),
            "does armodafinil help with learning?",
            &records,
            "test",
            &[],
        );
        assert!(draft
            .sections
            .iter()
            .all(|section| section.quotes.is_empty()));
    }

    #[test]
    fn quote_window_shows_the_matched_term() {
        let body = format!(
            "{} armodafinil helped me study. {}",
            "filler ".repeat(60),
            "tail ".repeat(60)
        );
        let record = record("t1_a", &body, "Nootropics");
        let quote = make_quote(&record, &["armodafinil".to_string()], &[]).unwrap();
        assert!(quote.text.contains("armodafinil"));
        assert!(quote.truncated);
        assert!(body.contains(&quote.text));
    }

    #[test]
    fn truncation_keeps_a_word_boundary() {
        let text = "one two three four five";
        let (short, cut) = truncate_quote(text, 12);
        assert!(cut);
        assert_eq!(short, "one two");
        assert!(text.starts_with(&short));
    }
}
