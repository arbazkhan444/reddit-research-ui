use crate::model::Record;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

pub struct Store {
    pub dir: PathBuf,
    pub records: HashMap<String, Record>,
    pub authored: HashMap<String, usize>,
    cache: HashMap<String, Value>,
    dirty: usize,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        let records = fs::read_to_string(dir.join("results.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| value.get("records").cloned())
            .and_then(|value| serde_json::from_value::<Vec<Record>>(value).ok())
            .map(|list| {
                list.into_iter()
                    .map(|record| (record.id.clone(), record))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let cache: HashMap<String, Value> = fs::read_to_string(dir.join("cache.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            dir,
            records,
            authored: HashMap::new(),
            cache,
            dirty: 0,
        }
    }

    pub fn cache_key(backend: &str, path: &str, params: &[(&str, String)]) -> String {
        let mut pairs: Vec<(&str, &str)> = params
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        pairs.sort();
        let mut hasher = DefaultHasher::new();
        backend.hash(&mut hasher);
        path.hash(&mut hasher);
        for pair in pairs {
            pair.hash(&mut hasher);
        }
        format!("{:016x}", hasher.finish())
    }

    pub fn get_cached(&self, key: &str) -> Option<Value> {
        self.cache.get(key).cloned()
    }

    pub fn put_cached(&mut self, key: String, value: Value) {
        self.cache.insert(key, value);
        self.dirty += 1;
        if self.dirty >= 5 {
            self.save_cache();
        }
    }

    pub fn save_cache(&mut self) {
        self.dirty = 0;
        if let Ok(text) = serde_json::to_string(&self.cache) {
            let path = self.dir.join("cache.json");
            let temp = self.dir.join("cache.json.tmp");
            if fs::write(&temp, text).is_ok() {
                let _ = fs::remove_file(&path);
                let _ = fs::rename(temp, path);
            }
        }
    }

    pub fn total(&self) -> usize {
        self.records.len()
    }

    pub fn add(&mut self, record: Record, tactic: &str) -> bool {
        if record.id.is_empty() {
            return false;
        }
        if !record.author.is_empty() && record.author != "[deleted]" {
            *self.authored.entry(record.author.clone()).or_insert(0) += 1;
        }
        if let Some(existing) = self.records.get_mut(&record.id) {
            existing.merge(&record);
            false
        } else {
            self.records.insert(record.id.clone(), record);
            let _ = tactic;
            true
        }
    }

    pub fn save_with(
        &mut self,
        topic: &str,
        tactics: &[String],
        started: &str,
        error: &str,
        extra: &Value,
    ) -> Result<(), String> {
        self.save_cache();
        let mut records: Vec<&Record> = self.records.values().collect();
        records.sort_by(|a, b| {
            b.created_utc
                .partial_cmp(&a.created_utc)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut payload = json!({
            "schema_version": 1,
            "topic": topic,
            "tactics": tactics,
            "started": started,
            "finished": chrono::Local::now().to_rfc3339(),
            "error": error,
            "total": records.len(),
            "records": records,
        });
        if let (Some(target), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
        let text = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
        write_atomic(&self.dir.join("results.json"), &text)?;

        let mut csv = String::from(
            "id,kind,subreddit,thread_id,author,date,score,relevance,final_score,title,body,permalink,sources\n",
        );
        for record in &records {
            csv.push_str(&quote(&record.id));
            csv.push(',');
            csv.push_str(&quote(&record.kind));
            csv.push(',');
            csv.push_str(&quote(&record.subreddit));
            csv.push(',');
            csv.push_str(&quote(record.thread()));
            csv.push(',');
            csv.push_str(&quote(&record.author));
            csv.push(',');
            csv.push_str(&quote(&record.date()));
            csv.push(',');
            csv.push_str(&record.score.to_string());
            csv.push(',');
            csv.push_str(&format!("{:.3}", record.relevance));
            csv.push(',');
            csv.push_str(&format!("{:.3}", record.final_score));
            csv.push(',');
            csv.push_str(&quote(&record.title));
            csv.push(',');
            csv.push_str(&quote(&record.body));
            csv.push(',');
            csv.push_str(&quote(&record.permalink));
            csv.push(',');
            let mut sources: Vec<&String> = record.sources.iter().collect();
            sources.sort();
            csv.push_str(&quote(
                &sources
                    .iter()
                    .map(|item| item.as_str())
                    .collect::<Vec<_>>()
                    .join(";"),
            ));
            csv.push('\n');
        }
        write_atomic(&self.dir.join("results.csv"), &csv)?;
        Ok(())
    }
}

fn write_atomic(path: &std::path::Path, text: &str) -> Result<(), String> {
    let temp = path.with_extension("tmp");
    fs::write(&temp, text).map_err(|error| error.to_string())?;
    let _ = fs::remove_file(path);
    fs::rename(&temp, path).map_err(|error| error.to_string())
}

fn quote(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Record;
    use std::collections::HashSet;

    fn record(id: &str) -> Record {
        Record {
            id: id.to_string(),
            kind: "comment".to_string(),
            subreddit: "ADHD".to_string(),
            thread_id: "t3_x".to_string(),
            parent_excerpt: None,
            author: "tester".to_string(),
            created_utc: 1700000000.0,
            score: 1,
            url: String::new(),
            num_comments: 0,
            title: String::new(),
            body: "body, with comma".to_string(),
            permalink: "https://www.reddit.com/r/ADHD/comments/x/_/a/".to_string(),
            sources: HashSet::from(["archive_comments".to_string()]),
            sub_questions: Vec::new(),
            relevance: 0.0,
            final_score: 0.0,
            notes: Vec::new(),
        }
    }

    #[test]
    fn dedupes_and_writes_outputs() {
        let dir = std::env::temp_dir().join(format!("reddit-store-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = Store::new(dir.clone());
        assert!(store.add(record("t1_a"), "archive_comments"));
        assert!(!store.add(record("t1_a"), "archive_threads"));
        assert_eq!(store.total(), 1);
        store
            .save_with(
                "topic",
                &["archive_comments".to_string()],
                "now",
                "",
                &Value::Null,
            )
            .unwrap();
        assert!(dir.join("results.json").exists());
        assert!(dir.join("results.csv").exists());
        let csv = fs::read_to_string(dir.join("results.csv")).unwrap();
        assert!(csv.contains("\"body, with comma\""));
        let _ = fs::remove_dir_all(&dir);
    }
}
