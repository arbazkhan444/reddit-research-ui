use crate::model::Record;
use std::fs;
use std::path::Path;

const TEMPLATE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>__TITLE__</title>
<style>
:root{--bg:#101418;--panel:#171d24;--line:#28313b;--text:#e6edf3;--muted:#8b98a5;--accent:#ff8a5c}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font:15px/1.5 "Segoe UI",system-ui,sans-serif}
header{padding:28px 24px 18px;border-bottom:1px solid var(--line)}
h1{margin:0 0 4px;font-size:26px}
.meta{color:var(--muted);font-size:13px}
.controls{display:flex;gap:8px;flex-wrap:wrap;margin-top:14px}
input,select{background:var(--panel);color:var(--text);border:1px solid var(--line);border-radius:8px;padding:9px 12px;font:inherit}
input{flex:1;min-width:220px}
main{max-width:960px;margin:0 auto;padding:20px 24px 60px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:14px 16px;margin-bottom:12px}
.row{display:flex;gap:8px;flex-wrap:wrap;align-items:center;color:var(--muted);font-size:12px;margin-bottom:6px}
.badge{background:#23303c;color:var(--accent);border-radius:999px;padding:1px 9px;font-weight:600}
.title{font-weight:600;margin:2px 0 6px}
.context{font-size:12px;color:var(--muted);font-style:italic;margin:0 0 6px}
.body{white-space:pre-wrap;word-wrap:break-word;color:#c9d4de;font-size:14px;max-height:260px;overflow:auto}
.body.clamped{max-height:7.5em;overflow:hidden}
.more{background:none;border:0;color:var(--accent);cursor:pointer;padding:2px 0;font:inherit;font-size:13px}
a{color:var(--accent)}
.empty{text-align:center;color:var(--muted);padding:60px 0}
</style>
</head>
<body>
<header>
<h1>__TITLE__</h1>
<div class="meta">__COUNT__ records &middot; built __DATE__ &middot; archive and official API sources</div>
<div class="controls">
<input id="q" placeholder="Filter text...">
<select id="sub"><option value="">All subreddits</option></select>
<select id="kind"><option value="">All kinds</option><option value="post">Posts</option><option value="comment">Comments</option></select>
<span class="meta" id="shown"></span>
</div>
</header>
<main id="list"></main>
<script id="data" type="application/json">__DATA__</script>
<script>
const records = JSON.parse(document.getElementById('data').textContent);
const list = document.getElementById('list');
const q = document.getElementById('q');
const sub = document.getElementById('sub');
const kind = document.getElementById('kind');
const shown = document.getElementById('shown');
const subs = [...new Set(records.map(r => r.subreddit).filter(Boolean))].sort();
for (const name of subs) {
  const option = document.createElement('option');
  option.value = name;
  option.textContent = 'r/' + name;
  sub.append(option);
}
function render() {
  const needle = q.value.trim().toLowerCase();
  const wantedSub = sub.value;
  const wantedKind = kind.value;
  list.replaceChildren();
  let count = 0;
  for (const record of records) {
    if (wantedSub && record.subreddit !== wantedSub) continue;
    if (wantedKind && record.kind !== wantedKind) continue;
    if (needle) {
      const haystack = (record.title + ' ' + record.body + ' ' + record.author + ' ' + record.subreddit).toLowerCase();
      if (!haystack.includes(needle)) continue;
    }
    count += 1;
    if (count > 400) continue;
    list.append(card(record));
  }
  shown.textContent = count + ' shown';
  if (!count) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    empty.textContent = 'No records match the current filters.';
    list.append(empty);
  }
}
function card(record) {
  const wrapper = document.createElement('article');
  wrapper.className = 'card';
  const row = document.createElement('div');
  row.className = 'row';
  const badge = document.createElement('span');
  badge.className = 'badge';
  badge.textContent = record.kind;
  row.append(badge);
  const meta = ['r/' + record.subreddit, 'u/' + record.author, record.score + ' points', (record.created_utc ? new Date(record.created_utc * 1000).toISOString().slice(0, 10) : '')];
  if (record.final_score) meta.push('rank ' + Number(record.final_score).toFixed(2));
  if (record.relevance) meta.push('rel ' + Number(record.relevance).toFixed(2));
  for (const text of meta) {
    if (!text) continue;
    const span = document.createElement('span');
    span.textContent = text;
    row.append(span);
  }
  wrapper.append(row);
  if (record.title) {
    const title = document.createElement('div');
    title.className = 'title';
    title.textContent = record.title;
    wrapper.append(title);
  }
  if (record.parent_excerpt) {
    const context = document.createElement('div');
    context.className = 'context';
    context.textContent = 'In reply to: ' + record.parent_excerpt;
    wrapper.append(context);
  }
  const body = document.createElement('div');
  body.className = 'body clamped';
  body.textContent = record.body || '(no text)';
  wrapper.append(body);
  const more = document.createElement('button');
  more.className = 'more';
  more.textContent = 'Show more';
  more.onclick = () => {
    body.classList.toggle('clamped');
    more.textContent = body.classList.contains('clamped') ? 'Show more' : 'Show less';
  };
  wrapper.append(more);
  if (record.permalink) {
    const link = document.createElement('a');
    link.href = record.permalink;
    link.target = '_blank';
    link.rel = 'noopener';
    link.textContent = 'Open on Reddit';
    link.style.marginLeft = '12px';
    link.style.fontSize = '13px';
    wrapper.append(link);
  }
  return wrapper;
}
q.addEventListener('input', render);
sub.addEventListener('change', render);
kind.addEventListener('change', render);
render();
</script>
</body>
</html>
"#;

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn write_report(records: &[Record], topic: &str, path: &Path) -> Result<(), String> {
    let data = serde_json::to_string(records).map_err(|error| error.to_string())?;
    let data = data.replace("</", "<\\/");
    let title = escape_html(if topic.is_empty() {
        "Reddit research"
    } else {
        topic
    });
    let html = TEMPLATE
        .replace("__TITLE__", &title)
        .replace("__COUNT__", &records.len().to_string())
        .replace(
            "__DATE__",
            &chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        )
        .replace("__DATA__", &data);
    fs::write(path, html).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Record;
    use std::collections::HashSet;

    #[test]
    fn writes_self_contained_report() {
        let record = Record {
            id: "t1_a".to_string(),
            kind: "comment".to_string(),
            subreddit: "ADHD".to_string(),
            thread_id: "t3_x".to_string(),
            parent_excerpt: Some("the parent text".to_string()),
            author: "someone".to_string(),
            created_utc: 1700000000.0,
            score: 4,
            url: String::new(),
            num_comments: 0,
            title: String::new(),
            body: "A personal story".to_string(),
            permalink: "https://www.reddit.com/r/ADHD/comments/x/_/a/".to_string(),
            sources: HashSet::from(["archive_comments".to_string()]),
            sub_questions: Vec::new(),
            relevance: 0.55,
            final_score: 0.5,
            notes: Vec::new(),
        };
        let path =
            std::env::temp_dir().join(format!("reddit-report-test-{}.html", std::process::id()));
        write_report(&[record], "adderall", &path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("A personal story"));
        assert!(text.contains("const records = JSON.parse"));
        assert!(!text.contains("<script src="));
        assert!(!text.contains("__TITLE__"));
        let _ = fs::remove_file(path);
    }
}
