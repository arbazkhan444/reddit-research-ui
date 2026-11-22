use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

pub const REDDIT: &str = "https://www.reddit.com";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub kind: String,
    pub subreddit: String,
    /// Thread that holds this record. Comments use the parent post ID.
    #[serde(default)]
    pub thread_id: String,
    /// Short text of the parent comment or post, for reading a quote in context.
    #[serde(default)]
    pub parent_excerpt: Option<String>,
    pub author: String,
    pub created_utc: f64,
    pub score: i64,
    /// Post link target, used to stop reposts of the same URL.
    #[serde(default)]
    pub url: String,
    /// Comment count on the thread. Comments store 0.
    #[serde(default)]
    pub num_comments: i64,
    pub title: String,
    pub body: String,
    pub permalink: String,
    pub sources: HashSet<String>,
    /// Plan sub-question IDs that this record matches.
    #[serde(default)]
    pub sub_questions: Vec<String>,
    /// Term coverage of the best matching sub-question, from 0 to 1.
    #[serde(default)]
    pub relevance: f32,
    /// Weighted rank score, from 0 to 1.
    #[serde(default)]
    pub final_score: f32,
    /// Dedupe notes, such as another subreddit with the same cross-post.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Record {
    pub fn date(&self) -> String {
        chrono::DateTime::from_timestamp(self.created_utc as i64, 0)
            .map(|moment| moment.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    }

    pub fn search_text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.title, self.body, self.author, self.subreddit
        )
        .to_lowercase()
    }

    /// Thread ID for grouping. Posts are their own thread.
    pub fn thread(&self) -> &str {
        if self.thread_id.is_empty() {
            &self.id
        } else {
            &self.thread_id
        }
    }

    /// Simple engagement value. Negative scores count as zero.
    pub fn engagement(&self) -> i64 {
        self.score.max(0) + 2 * self.num_comments.max(0)
    }

    pub fn merge(&mut self, other: &Record) {
        for source in &other.sources {
            self.sources.insert(source.clone());
        }
        if self.thread_id.is_empty() {
            self.thread_id = other.thread_id.clone();
        }
        if self.parent_excerpt.is_none() {
            self.parent_excerpt = other.parent_excerpt.clone();
        }
        if other.score > self.score {
            self.score = other.score;
        }
        if self.url.is_empty() {
            self.url = other.url.clone();
        }
        if other.num_comments > self.num_comments {
            self.num_comments = other.num_comments;
        }
        if other.body.len() > self.body.len() {
            self.body = other.body.clone();
        }
        if other.title.len() > self.title.len() {
            self.title = other.title.clone();
        }
        for question in &other.sub_questions {
            if !self.sub_questions.contains(question) {
                self.sub_questions.push(question.clone());
            }
        }
        if other.relevance > self.relevance {
            self.relevance = other.relevance;
        }
        if other.final_score > self.final_score {
            self.final_score = other.final_score;
        }
        for note in &other.notes {
            if !self.notes.contains(note) {
                self.notes.push(note.clone());
            }
        }
    }
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string()
}

fn number(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(|item| item.as_i64()).unwrap_or(0)
}

fn float(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(|item| item.as_f64()).unwrap_or(0.0)
}

fn comment_permalink(subreddit: &str, link_id: &str, comment_id: &str) -> String {
    if subreddit.is_empty() || link_id.is_empty() || comment_id.is_empty() {
        return String::new();
    }
    format!(
        "{}/r/{}/comments/{}/_/{}/",
        REDDIT, subreddit, link_id, comment_id
    )
}

pub fn archive_comment(value: &Value, source: &str) -> Option<Record> {
    let id = text(value, "id");
    if id.is_empty() {
        return None;
    }
    let link = text(value, "link_id");
    let link = link.strip_prefix("t3_").unwrap_or(&link);
    let subreddit = text(value, "subreddit");
    let mut sources = HashSet::new();
    sources.insert(source.to_string());
    Some(Record {
        id: format!("t1_{}", id),
        kind: "comment".into(),
        subreddit: subreddit.clone(),
        thread_id: if link.is_empty() {
            String::new()
        } else {
            format!("t3_{}", link)
        },
        parent_excerpt: None,
        author: {
            let author = text(value, "author");
            if author.is_empty() {
                "[deleted]".into()
            } else {
                author
            }
        },
        created_utc: float(value, "created_utc"),
        score: number(value, "score"),
        url: String::new(),
        num_comments: 0,
        title: String::new(),
        body: text(value, "body"),
        permalink: comment_permalink(&subreddit, link, &id),
        sources,
        sub_questions: Vec::new(),
        relevance: 0.0,
        final_score: 0.0,
        notes: Vec::new(),
    })
}

pub fn archive_post(value: &Value, source: &str) -> Option<Record> {
    let id = text(value, "id");
    if id.is_empty() {
        return None;
    }
    let subreddit = text(value, "subreddit");
    let mut sources = HashSet::new();
    sources.insert(source.to_string());
    Some(Record {
        id: format!("t3_{}", id),
        kind: "post".into(),
        subreddit: subreddit.clone(),
        thread_id: format!("t3_{}", id),
        parent_excerpt: None,
        author: {
            let author = text(value, "author");
            if author.is_empty() {
                "[deleted]".into()
            } else {
                author
            }
        },
        created_utc: float(value, "created_utc"),
        score: number(value, "score"),
        url: text(value, "url"),
        num_comments: number(value, "num_comments"),
        title: text(value, "title"),
        body: text(value, "selftext"),
        permalink: if subreddit.is_empty() {
            String::new()
        } else {
            format!("{}/r/{}/comments/{}/", REDDIT, subreddit, id)
        },
        sources,
        sub_questions: Vec::new(),
        relevance: 0.0,
        final_score: 0.0,
        notes: Vec::new(),
    })
}

pub fn reddit_post(value: &Value, source: &str) -> Option<Record> {
    let name = text(value, "name");
    let id = text(value, "id");
    if name.is_empty() && id.is_empty() {
        return None;
    }
    let mut permalink = text(value, "permalink");
    if permalink.starts_with('/') {
        permalink = format!("{}{}", REDDIT, permalink);
    }
    let mut sources = HashSet::new();
    sources.insert(source.to_string());
    let full_id = if name.is_empty() {
        format!("t3_{}", id)
    } else {
        name
    };
    Some(Record {
        id: full_id.clone(),
        kind: "post".into(),
        subreddit: text(value, "subreddit"),
        thread_id: full_id,
        parent_excerpt: None,
        author: {
            let author = text(value, "author");
            if author.is_empty() {
                "[deleted]".into()
            } else {
                author
            }
        },
        created_utc: float(value, "created_utc"),
        score: number(value, "score"),
        url: text(value, "url"),
        num_comments: number(value, "num_comments"),
        title: text(value, "title"),
        body: text(value, "selftext"),
        permalink,
        sources,
        sub_questions: Vec::new(),
        relevance: 0.0,
        final_score: 0.0,
        notes: Vec::new(),
    })
}

pub fn reddit_comment(value: &Value, source: &str) -> Option<Record> {
    let name = text(value, "name");
    let id = text(value, "id");
    if name.is_empty() && id.is_empty() {
        return None;
    }
    let mut sources = HashSet::new();
    sources.insert(source.to_string());
    let link_id = text(value, "link_id");
    Some(Record {
        id: if name.is_empty() {
            format!("t1_{}", id)
        } else {
            name
        },
        kind: "comment".into(),
        subreddit: text(value, "subreddit"),
        thread_id: if link_id.is_empty() {
            String::new()
        } else if link_id.starts_with("t3_") {
            link_id
        } else {
            format!("t3_{}", link_id)
        },
        parent_excerpt: None,
        author: {
            let author = text(value, "author");
            if author.is_empty() {
                "[deleted]".into()
            } else {
                author
            }
        },
        created_utc: float(value, "created_utc"),
        score: number(value, "score"),
        url: String::new(),
        num_comments: 0,
        title: String::new(),
        body: text(value, "body"),
        permalink: {
            let link = text(value, "permalink");
            if link.starts_with('/') {
                format!("{}{}", REDDIT, link)
            } else {
                link
            }
        },
        sources,
        sub_questions: Vec::new(),
        relevance: 0.0,
        final_score: 0.0,
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn archive_comment_builds_permalink() {
        let value = json!({
            "id": "abc",
            "link_id": "t3_def",
            "subreddit": "ADHD",
            "author": "user",
            "created_utc": 1700000000.0,
            "score": 5,
            "body": "hello"
        });
        let record = archive_comment(&value, "archive_comments").unwrap();
        assert_eq!(record.id, "t1_abc");
        assert_eq!(record.thread_id, "t3_def");
        assert_eq!(record.thread(), "t3_def");
        assert_eq!(
            record.permalink,
            "https://www.reddit.com/r/ADHD/comments/def/_/abc/"
        );
        assert_eq!(record.date(), "2023-11-14");
    }

    #[test]
    fn merge_keeps_sources_and_longer_body() {
        let mut first = archive_comment(
            &json!({"id": "a", "subreddit": "ADHD", "body": "short", "score": 1}),
            "archive_comments",
        )
        .unwrap();
        let second = archive_comment(
            &json!({"id": "a", "subreddit": "ADHD", "body": "a much longer body", "score": 9}),
            "archive_threads",
        )
        .unwrap();
        first.merge(&second);
        assert_eq!(first.body, "a much longer body");
        assert_eq!(first.score, 9);
        assert!(first.sources.contains("archive_comments"));
        assert!(first.sources.contains("archive_threads"));
    }

    #[test]
    fn merge_keeps_parent_context() {
        let mut first = archive_comment(
            &json!({"id": "a", "subreddit": "ADHD", "body": "short", "score": 1}),
            "archive_comments",
        )
        .unwrap();
        let mut second = archive_comment(
            &json!({"id": "a", "subreddit": "ADHD", "body": "short", "score": 1}),
            "archive_threads",
        )
        .unwrap();
        second.parent_excerpt = Some("the parent text".to_string());
        first.merge(&second);
        assert_eq!(first.parent_excerpt.as_deref(), Some("the parent text"));
    }
}
