//! Evidence store work: dedupe, rank, diversity pack, and thread selection.
//!
//! The rules follow docs/design.md sections 8.2 to 8.4. Records are cleaned in
//! one pass, scored with the fixed weight formula, then sorted by final score.

use crate::engine;
use crate::model::Record;
use crate::plan::ResearchPlan;
use std::collections::{HashMap, HashSet};

pub const PACK_LIMIT: usize = 150;
pub const THREAD_CAP: usize = 5;
pub const AUTHOR_CAP: usize = 3;
pub const AUTHOR_CAP_WINDOW: usize = 40;
pub const SUBREDDIT_SHARE: f64 = 0.35;

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub input: usize,
    pub exact: usize,
    pub near: usize,
    pub crossposts: usize,
    pub reposts: usize,
    pub kept: usize,
    pub questions: usize,
}

impl Stats {
    pub fn removed(&self) -> usize {
        self.exact + self.near + self.crossposts + self.reposts
    }
}

/// Clean, score, and sort records. The input vector is replaced.
pub fn prepare(records: &mut Vec<Record>, plan: Option<&ResearchPlan>, topic: &str) -> Stats {
    let mut stats = Stats {
        input: records.len(),
        ..Stats::default()
    };

    // Rule 1: exact ID match.
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut unique: Vec<Record> = Vec::with_capacity(records.len());
    for record in records.drain(..) {
        if record.id.is_empty() {
            continue;
        }
        if let Some(&index) = by_id.get(&record.id) {
            unique[index].merge(&record);
            stats.exact += 1;
        } else {
            by_id.insert(record.id.clone(), unique.len());
            unique.push(record);
        }
    }

    // Rule 2: near-duplicate comment text in the same thread.
    let mut seen_text: HashMap<(String, String), usize> = HashMap::new();
    let mut deduped: Vec<Record> = Vec::with_capacity(unique.len());
    for record in unique {
        if record.kind == "comment" && !record.body.trim().is_empty() {
            let key = (record.thread().to_string(), text_hash(&record.body));
            if let Some(&index) = seen_text.get(&key) {
                deduped[index].merge(&record);
                stats.near += 1;
                continue;
            }
            seen_text.insert(key, deduped.len());
        }
        deduped.push(record);
    }

    // Rule 3: cross-posts with the same title and author.
    let mut cross_groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, record) in deduped.iter().enumerate() {
        if record.kind != "post" || record.title.trim().is_empty() {
            continue;
        }
        if record.author.trim().is_empty() || record.author == "[deleted]" {
            continue;
        }
        let key = format!(
            "{}|{}",
            normalize_title(&record.title),
            record.author.to_lowercase()
        );
        cross_groups.entry(key).or_default().push(index);
    }
    let mut drop = vec![false; deduped.len()];
    for indexes in cross_groups.values() {
        if indexes.len() < 2 {
            continue;
        }
        let best = *indexes
            .iter()
            .max_by_key(|&&index| {
                (
                    deduped[index].engagement(),
                    deduped[index].body.chars().count(),
                )
            })
            .unwrap_or(&indexes[0]);
        for &index in indexes {
            if index == best {
                continue;
            }
            let subreddit = deduped[index].subreddit.clone();
            if !subreddit.is_empty() {
                let note = format!("also posted in r/{}", subreddit);
                if !deduped[best].notes.contains(&note) {
                    deduped[best].notes.push(note);
                }
            }
            let sources: Vec<String> = deduped[index].sources.iter().cloned().collect();
            for source in sources {
                deduped[best].sources.insert(source);
            }
            drop[index] = true;
            stats.crossposts += 1;
        }
    }

    // Rule 4: reposts of the same URL keep the thread with more comments.
    let mut url_groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, record) in deduped.iter().enumerate() {
        if drop[index] || record.kind != "post" {
            continue;
        }
        let url = normalize_url(&record.url);
        if url.is_empty() {
            continue;
        }
        url_groups.entry(url).or_default().push(index);
    }
    for indexes in url_groups.values() {
        if indexes.len() < 2 {
            continue;
        }
        let best = *indexes
            .iter()
            .max_by(|&&a, &&b| {
                (deduped[a].num_comments, deduped[a].engagement())
                    .cmp(&(deduped[b].num_comments, deduped[b].engagement()))
                    .then_with(|| {
                        deduped[a]
                            .created_utc
                            .partial_cmp(&deduped[b].created_utc)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            })
            .unwrap_or(&indexes[0]);
        for &index in indexes {
            if index == best {
                continue;
            }
            let note = format!("same link as {}", deduped[index].id);
            if !deduped[best].notes.contains(&note) {
                deduped[best].notes.push(note);
            }
            let sources: Vec<String> = deduped[index].sources.iter().cloned().collect();
            for source in sources {
                deduped[best].sources.insert(source);
            }
            drop[index] = true;
            stats.reposts += 1;
        }
    }

    let mut kept: Vec<Record> = deduped
        .into_iter()
        .enumerate()
        .filter_map(
            |(index, record)| {
                if drop[index] {
                    None
                } else {
                    Some(record)
                }
            },
        )
        .collect();

    stats.questions = rank(&mut kept, plan, topic);
    stats.kept = kept.len();
    *records = kept;
    stats
}

/// Score every record and sort by final score.
pub fn rank(records: &mut [Record], plan: Option<&ResearchPlan>, topic: &str) -> usize {
    let questions = question_terms(plan, topic);
    let entity_terms = entity_terms(plan);
    let max_engagement = records
        .iter()
        .map(|record| (record.engagement() as f64).ln_1p())
        .fold(0.0, f64::max);
    let now = chrono::Utc::now().timestamp() as f64;

    for record in records.iter_mut() {
        let haystack = record.search_text();
        let mut best = 0.0f32;
        let mut matched: Vec<(String, f32)> = Vec::new();
        let mut best_id: Option<String> = None;
        for (id, terms) in &questions {
            let coverage = if terms.is_empty() {
                0.0
            } else {
                let hits = terms
                    .iter()
                    .filter(|term| haystack.contains(term.as_str()))
                    .count();
                hits as f32 / terms.len() as f32
            };
            let entities = entity_terms
                .iter()
                .filter(|terms| terms.iter().any(|term| haystack.contains(term.as_str())))
                .count()
                .min(2) as f32
                * 0.1;
            let score = (coverage + entities).min(1.0);
            if score > best {
                best = score;
                best_id = Some(id.clone());
            }
            if score >= 0.34 {
                matched.push((id.clone(), score));
            }
        }
        matched.retain(|(_, score)| *score + 0.001 >= best * 0.7);
        if matched.is_empty() {
            if let Some(id) = best_id {
                if best > 0.0 {
                    matched.push((id, best));
                }
            }
        }
        record.relevance = best;
        record.sub_questions = matched.into_iter().map(|(id, _)| id).collect();

        let engagement = if max_engagement > 0.0 {
            ((record.engagement() as f64).ln_1p() / max_engagement) as f32
        } else {
            0.0
        };
        let age_days = ((now - record.created_utc) / 86_400.0).max(0.0);
        let recency = 0.5f64.powf(age_days / 1_095.0) as f32;
        let reliability = source_reliability(record);
        let completeness = text_completeness(record);

        record.final_score = 0.45 * record.relevance
            + 0.20 * engagement
            + 0.15 * recency
            + 0.10 * reliability
            + 0.10 * completeness;
    }

    sort_ranked(records);
    questions.len()
}

/// Sort prepared records into rank order. A reloaded run needs this again because
/// `results.json` keeps date order.
pub fn sort_ranked(records: &mut [Record]) {
    records.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.created_utc
                    .partial_cmp(&a.created_utc)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Pick a smaller, diverse set for the draft. Caps follow section 8.4.
pub fn pack(records: &[Record], limit: usize) -> Vec<Record> {
    if limit == 0 || records.is_empty() {
        return Vec::new();
    }
    let subreddits: HashSet<&str> = records
        .iter()
        .map(|record| record.subreddit.as_str())
        .filter(|sub| !sub.is_empty())
        .collect();
    let single_subreddit = subreddits.len() <= 1;
    let subreddit_cap = ((limit as f64 * SUBREDDIT_SHARE).ceil() as usize).max(1);

    let mut selected: Vec<usize> = Vec::with_capacity(limit);
    let mut skipped: Vec<usize> = Vec::new();
    let mut sub_counts: HashMap<&str, usize> = HashMap::new();
    let mut thread_counts: HashMap<&str, usize> = HashMap::new();
    let mut author_counts: HashMap<&str, usize> = HashMap::new();

    for (index, record) in records.iter().enumerate() {
        if selected.len() >= limit {
            break;
        }
        if !single_subreddit
            && *sub_counts.get(record.subreddit.as_str()).unwrap_or(&0) >= subreddit_cap
        {
            skipped.push(index);
            continue;
        }
        if record.kind == "comment"
            && *thread_counts.get(record.thread()).unwrap_or(&0) >= THREAD_CAP
        {
            skipped.push(index);
            continue;
        }
        if selected.len() < AUTHOR_CAP_WINDOW && counts_author(&record.author) {
            let count = *author_counts.get(record.author.as_str()).unwrap_or(&0);
            if count >= AUTHOR_CAP {
                skipped.push(index);
                continue;
            }
        }
        selected.push(index);
        if !record.subreddit.is_empty() {
            *sub_counts.entry(record.subreddit.as_str()).or_insert(0) += 1;
        }
        if record.kind == "comment" {
            *thread_counts.entry(record.thread()).or_insert(0) += 1;
        }
        if counts_author(&record.author) {
            *author_counts.entry(record.author.as_str()).or_insert(0) += 1;
        }
    }

    // Fill spare room. The subreddit and thread caps stay hard, so the pack keeps its
    // balance. The author cap is soft, so skipped author copies may return.
    for index in skipped {
        if selected.len() >= limit {
            break;
        }
        if selected.contains(&index) {
            continue;
        }
        let record = &records[index];
        if !single_subreddit
            && *sub_counts.get(record.subreddit.as_str()).unwrap_or(&0) >= subreddit_cap
        {
            continue;
        }
        if record.kind == "comment"
            && *thread_counts.get(record.thread()).unwrap_or(&0) >= THREAD_CAP
        {
            continue;
        }
        selected.push(index);
        if !record.subreddit.is_empty() {
            *sub_counts.entry(record.subreddit.as_str()).or_insert(0) += 1;
        }
        if record.kind == "comment" {
            *thread_counts.entry(record.thread()).or_insert(0) += 1;
        }
    }

    keep_dissent(records, &mut selected, limit);
    selected.sort_unstable();
    selected.dedup();
    selected
        .into_iter()
        .filter_map(|index| records.get(index).cloned())
        .collect()
}

/// Best thread IDs for expansion. One thread per sub-question first.
pub fn seed_threads(
    records: &[Record],
    plan: Option<&ResearchPlan>,
    topic: &str,
    wanted: usize,
) -> Vec<String> {
    if wanted == 0 || records.is_empty() {
        return Vec::new();
    }
    let questions = question_terms(plan, topic);
    let mut seeds: Vec<String> = Vec::new();
    let mut used: HashSet<usize> = HashSet::new();

    for (_, terms) in &questions {
        let best = records
            .iter()
            .enumerate()
            .filter(|(index, _)| !used.contains(index))
            .max_by(|(_, a), (_, b)| {
                seed_score(a, terms)
                    .partial_cmp(&seed_score(b, terms))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(index, _)| index);
        if let Some(index) = best {
            used.insert(index);
            push_seed(&mut seeds, &records[index]);
        }
        if seeds.len() >= wanted {
            return seeds;
        }
    }

    let all_terms: Vec<String> = questions
        .iter()
        .flat_map(|(_, terms)| terms.iter().cloned())
        .collect();
    let mut order: Vec<usize> = (0..records.len()).collect();
    order.sort_by(|&a, &b| {
        seed_score(&records[b], &all_terms)
            .partial_cmp(&seed_score(&records[a], &all_terms))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for index in order {
        push_seed(&mut seeds, &records[index]);
        if seeds.len() >= wanted {
            break;
        }
    }
    seeds
}

fn push_seed(seeds: &mut Vec<String>, record: &Record) {
    let thread = record.thread().to_string();
    if thread.is_empty() || seeds.contains(&thread) {
        return;
    }
    seeds.push(thread);
}

fn seed_score(record: &Record, terms: &[String]) -> f64 {
    let haystack = record.search_text();
    let coverage = if terms.is_empty() {
        0.0
    } else {
        terms
            .iter()
            .filter(|term| haystack.contains(term.as_str()))
            .count() as f64
            / terms.len() as f64
    };
    let engagement = (record.engagement() as f64).ln_1p() / 10.0;
    coverage + engagement.min(0.5) + record.created_utc / 1.0e13
}

fn counts_author(author: &str) -> bool {
    !author.trim().is_empty() && author != "[deleted]"
}

fn keep_dissent(records: &[Record], selected: &mut Vec<usize>, limit: usize) {
    let questions: HashSet<String> = selected
        .iter()
        .flat_map(|&index| records[index].sub_questions.iter().cloned())
        .collect();
    for question in questions {
        let has_dissent = selected.iter().any(|&index| {
            records[index].sub_questions.contains(&question) && dissenting(&records[index].body)
        });
        if has_dissent {
            continue;
        }
        let candidate = records.iter().enumerate().find(|(index, record)| {
            !selected.contains(index)
                && record.sub_questions.contains(&question)
                && dissenting(&record.body)
        });
        let Some((index, _)) = candidate else {
            continue;
        };
        if let Some(position) = selected
            .iter()
            .rposition(|&item| records[item].sub_questions.contains(&question))
        {
            selected[position] = index;
        } else if selected.len() < limit {
            selected.push(index);
        }
    }
}

/// True when text has a clear negative or caution signal.
pub fn dissenting(text: &str) -> bool {
    let lower = text.to_lowercase();
    const CUES: [&str; 16] = [
        "didn't work",
        "didnt work",
        "did not work",
        "no effect",
        "not work",
        "worse",
        "side effect",
        "anxiety",
        "crash",
        "headache",
        "insomnia",
        "not help",
        "stopped",
        "quit",
        "bad experience",
        "negative",
    ];
    CUES.iter().any(|cue| lower.contains(cue))
}

fn question_terms(plan: Option<&ResearchPlan>, topic: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(plan) = plan {
        for question in &plan.sub_questions {
            let mut terms: Vec<String> = Vec::new();
            for keyword in &question.keywords {
                for term in engine::topic_terms(keyword) {
                    push_term(&mut terms, &term);
                }
            }
            if terms.is_empty() {
                for term in engine::topic_terms(&question.text) {
                    push_term(&mut terms, &term);
                }
            }
            let haystack =
                format!("{} {}", question.text, question.keywords.join(" ")).to_lowercase();
            for entity in &plan.entities {
                let entity_terms = engine::topic_terms(entity);
                if entity_terms
                    .iter()
                    .any(|term| haystack.contains(term.as_str()))
                {
                    for term in entity_terms {
                        push_term(&mut terms, &term);
                    }
                }
            }
            if terms.is_empty() {
                continue;
            }
            let id = if question.id.trim().is_empty() {
                format!("q{}", out.len() + 1)
            } else {
                question.id.trim().to_string()
            };
            out.push((id, terms));
        }
    }
    if out.is_empty() {
        let terms = engine::topic_terms(topic);
        if !terms.is_empty() {
            out.push(("q".to_string(), terms));
        }
    }
    out
}

fn entity_terms(plan: Option<&ResearchPlan>) -> Vec<Vec<String>> {
    plan.map(|plan| {
        plan.entities
            .iter()
            .map(|entity| engine::topic_terms(entity))
            .filter(|terms| !terms.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

fn push_term(terms: &mut Vec<String>, term: &str) {
    let term = term.trim().to_lowercase();
    if term.len() >= 3 && !terms.contains(&term) {
        terms.push(term);
    }
}

fn source_reliability(record: &Record) -> f32 {
    let mut best = 0.0f32;
    for source in &record.sources {
        let value = if source.starts_with("official") {
            1.0
        } else if source.starts_with("archive") {
            0.9
        } else if source.starts_with("web") {
            0.7
        } else {
            0.8
        };
        if value > best {
            best = value;
        }
    }
    if best == 0.0 {
        0.8
    } else {
        best
    }
}

fn text_completeness(record: &Record) -> f32 {
    let text = if !record.body.trim().is_empty() {
        record.body.trim()
    } else {
        record.title.trim()
    };
    let chars = text.chars().count();
    let mut score = (chars as f32 / 300.0).min(1.0);
    let lower = text.to_lowercase();
    let words = lower.split_whitespace().count();
    if lower.starts_with("http") || (lower.contains("http") && chars < 160 && words < 12) {
        score *= 0.2;
    }
    if record.body.trim().is_empty() {
        score = score.min(0.4);
    }
    score
}

fn normalize_title(title: &str) -> String {
    title
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_url(url: &str) -> String {
    let mut value = url.trim().to_lowercase();
    for prefix in ["https://", "http://"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            value = rest.to_string();
        }
    }
    if let Some(rest) = value.strip_prefix("www.") {
        value = rest.to_string();
    }
    while value.ends_with('/') {
        value.pop();
    }
    if value.contains("reddit.com") || value.contains("redd.it") {
        return String::new();
    }
    value
}

/// Normalized text hash: lowercase, collapse spaces, first 512 characters.
pub fn text_hash(text: &str) -> String {
    let normalized = text
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let head: String = normalized.chars().take(512).collect();
    sha256_hex(&head)
}

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256_hex(text: &str) -> String {
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bytes = text.as_bytes();
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut message = bytes.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks(64) {
        let mut words = [0u32; 64];
        for (index, slot) in words.iter_mut().take(16).enumerate() {
            let at = index * 4;
            *slot = u32::from_be_bytes([chunk[at], chunk[at + 1], chunk[at + 2], chunk[at + 3]]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];

        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(SHA256_K[index])
                .wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    state
        .iter()
        .map(|value| format!("{:08x}", value))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{ResearchPlan, SubQuestion};
    use std::collections::HashSet;

    fn record(id: &str, kind: &str, sub: &str, thread: &str, body: &str) -> Record {
        Record {
            id: id.to_string(),
            kind: kind.to_string(),
            subreddit: sub.to_string(),
            thread_id: thread.to_string(),
            parent_excerpt: None,
            author: format!("user_{}", id),
            created_utc: 1_700_000_000.0,
            score: 10,
            url: String::new(),
            num_comments: 0,
            title: if kind == "post" {
                "Armodafinil and study".to_string()
            } else {
                String::new()
            },
            body: body.to_string(),
            permalink: format!(
                "https://www.reddit.com/r/{}/comments/{}/_/{}/",
                sub, thread, id
            ),
            sources: HashSet::from(["archive_comments".to_string()]),
            sub_questions: Vec::new(),
            relevance: 0.0,
            final_score: 0.0,
            notes: Vec::new(),
        }
    }

    fn plan(questions: &[(&str, &[&str])]) -> ResearchPlan {
        ResearchPlan {
            sub_questions: questions
                .iter()
                .map(|(id, keywords)| SubQuestion {
                    id: id.to_string(),
                    text: keywords.join(" "),
                    keywords: keywords.iter().map(|value| value.to_string()).collect(),
                    success: String::new(),
                })
                .collect(),
            entities: vec!["armodafinil".to_string()],
            ..ResearchPlan::default()
        }
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn near_duplicates_in_one_thread_are_removed() {
        let mut records = vec![
            record("t1_a", "comment", "ADHD", "t3_x", "It  helped me focus."),
            record("t1_b", "comment", "ADHD", "t3_x", "it helped   me focus."),
            record("t1_c", "comment", "ADHD", "t3_y", "it helped me focus."),
        ];
        let stats = prepare(&mut records, None, "focus");
        assert_eq!(stats.near, 1);
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn crossposts_keep_one_and_record_the_other_subreddit() {
        let mut first = record("t3_a", "post", "Nootropics", "t3_a", "my story");
        let mut second = record("t3_b", "post", "ADHD", "t3_b", "my story");
        first.author = "same_user".to_string();
        second.author = "same_user".to_string();
        let mut records = vec![first, second];
        let stats = prepare(&mut records, None, "armodafinil");
        assert_eq!(stats.crossposts, 1);
        assert_eq!(records.len(), 1);
        assert!(records[0].notes.iter().any(|note| note.contains("r/")));
    }

    #[test]
    fn reposts_keep_the_thread_with_more_comments() {
        let mut first = record("t3_a", "post", "Nootropics", "t3_a", "story");
        let mut second = record("t3_b", "post", "Nootropics", "t3_b", "story");
        first.url = "https://example.com/paper".to_string();
        second.url = "https://example.com/paper/".to_string();
        first.num_comments = 4;
        second.num_comments = 40;
        first.title = "First title".to_string();
        second.title = "Second title".to_string();
        let mut records = vec![first, second];
        let stats = prepare(&mut records, None, "armodafinil");
        assert_eq!(stats.reposts, 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].num_comments, 40);
    }

    #[test]
    fn rank_puts_relevant_engaged_text_first() {
        let mut records = vec![
            record("t1_a", "comment", "ADHD", "t3_a", "unrelated words only"),
            record(
                "t1_b",
                "comment",
                "ADHD",
                "t3_b",
                "armodafinil helped my study focus and memory a lot",
            ),
        ];
        records[1].score = 500;
        records[1].created_utc = chrono::Utc::now().timestamp() as f64;
        let stats = prepare(
            &mut records,
            Some(&plan(&[("q1", &["armodafinil", "study"])])),
            "armodafinil study",
        );
        assert_eq!(stats.questions, 1);
        assert_eq!(records[0].id, "t1_b");
        assert!(records[0].relevance > records[1].relevance);
        assert!(records[0].final_score > records[1].final_score);
        assert_eq!(records[0].sub_questions, vec!["q1".to_string()]);
    }

    #[test]
    fn sort_ranked_restores_order_after_a_reload() {
        let mut records = vec![
            record("t1_a", "comment", "ADHD", "t3_a", "armodafinil study"),
            record(
                "t1_b",
                "comment",
                "ADHD",
                "t3_b",
                "armodafinil helped my study focus a lot",
            ),
        ];
        prepare(&mut records, None, "armodafinil study");
        let ranked: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
        records.reverse();
        sort_ranked(&mut records);
        let reloaded: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
        assert_eq!(ranked, reloaded);
    }

    #[test]
    fn pack_caps_one_subreddit_share() {
        let mut records = Vec::new();
        for index in 0..25 {
            let sub = if index < 15 {
                "ADHD"
            } else if index < 20 {
                "Nootropics"
            } else {
                "StackAdvice"
            };
            records.push(record(
                &format!("t1_{}", index),
                "comment",
                sub,
                &format!("t3_{}", index),
                "armodafinil helped my study",
            ));
        }
        prepare(&mut records, None, "armodafinil study");
        let packed = pack(&records, 10);
        assert_eq!(packed.len(), 10);
        let adhd = packed
            .iter()
            .filter(|item| item.subreddit == "ADHD")
            .count();
        assert!(adhd <= 4, "ADHD share was {}", adhd);
    }

    #[test]
    fn pack_keeps_a_dissenting_record() {
        let mut records = Vec::new();
        for index in 0..6 {
            records.push(record(
                &format!("t1_{}", index),
                "comment",
                if index % 2 == 0 { "ADHD" } else { "Nootropics" },
                &format!("t3_{}", index),
                "armodafinil helped my study focus",
            ));
        }
        records.push(record(
            "t1_bad",
            "comment",
            "ADHD",
            "t3_bad",
            "armodafinil gave me anxiety and did not work for study",
        ));
        prepare(
            &mut records,
            Some(&plan(&[("q1", &["armodafinil", "study"])])),
            "armodafinil study",
        );
        let packed = pack(&records, 3);
        assert!(packed.iter().any(|item| dissenting(&item.body)));
    }

    #[test]
    fn seed_threads_covers_each_question_first() {
        let mut records = vec![
            record("t1_a", "comment", "ADHD", "t3_a", "armodafinil focus story"),
            record(
                "t1_b",
                "comment",
                "ADHD",
                "t3_b",
                "sleep problems after use",
            ),
        ];
        records[0].score = 50;
        records[1].score = 5;
        let plan = plan(&[("q1", &["armodafinil"]), ("q2", &["sleep"])]);
        let seeds = seed_threads(&records, Some(&plan), "armodafinil sleep", 2);
        assert_eq!(seeds, vec!["t3_a".to_string(), "t3_b".to_string()]);
    }

    #[test]
    fn text_hash_ignores_case_and_extra_spaces() {
        assert_eq!(text_hash("It  helped ME"), text_hash("it helped me"));
    }
}
