//! A small Markdown previewer for the report tab.
//!
//! It supports the subset that `report.md` uses: headings, paragraphs, bullets,
//! numbered lists, quotes, rules, bold, italic, code, and links. Links to hosts
//! other than Reddit are shown as plain text.

use eframe::egui;

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading(usize, String),
    Paragraph(String),
    Bullet(String),
    Numbered(usize, String),
    Quote(String),
    Rule,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Span {
    Text(String),
    Bold(String),
    Italic(String),
    Code(String),
    Link(String, String),
}

pub fn parse_blocks(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            flush(&mut blocks, &mut paragraph);
            continue;
        }
        if line == "---" || line == "***" {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Rule);
            continue;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Heading(3, rest.trim().to_string()));
            continue;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Heading(2, rest.trim().to_string()));
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Heading(1, rest.trim().to_string()));
            continue;
        }
        if let Some(rest) = line.strip_prefix("> ") {
            flush(&mut blocks, &mut paragraph);
            push_quote(&mut blocks, rest.trim().to_string());
            continue;
        }
        if line == ">" {
            flush(&mut blocks, &mut paragraph);
            push_quote(&mut blocks, String::new());
            continue;
        }
        if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Bullet(rest.trim().to_string()));
            continue;
        }
        if let Some((number, rest)) = numbered(line) {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Numbered(number, rest.to_string()));
            continue;
        }
        paragraph.push(line.to_string());
    }
    flush(&mut blocks, &mut paragraph);
    blocks
}

fn flush(blocks: &mut Vec<Block>, paragraph: &mut Vec<String>) {
    if !paragraph.is_empty() {
        blocks.push(Block::Paragraph(paragraph.join(" ")));
        paragraph.clear();
    }
}

/// Consecutive quote lines form one block, as they do in Markdown.
fn push_quote(blocks: &mut Vec<Block>, text: String) {
    if let Some(Block::Quote(previous)) = blocks.last_mut() {
        if !text.is_empty() {
            if !previous.is_empty() {
                previous.push(' ');
            }
            previous.push_str(&text);
        }
    } else {
        blocks.push(Block::Quote(text));
    }
}

fn numbered(line: &str) -> Option<(usize, &str)> {
    let (digits, rest) = line.split_once(". ")?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let number = digits.parse::<usize>().ok()?;
    Some((number, rest))
}

pub fn parse_inline(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut buffer = String::new();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' && index + 1 < chars.len() {
            buffer.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if ch == '`' {
            if let Some(end) = find_from(&chars, index + 1, &['`']) {
                flush_span(&mut spans, &mut buffer);
                spans.push(Span::Code(chars[index + 1..end].iter().collect()));
                index = end + 1;
                continue;
            }
        }
        if ch == '*' && index + 1 < chars.len() && chars[index + 1] == '*' {
            if let Some(end) = find_from(&chars, index + 2, &['*', '*']) {
                flush_span(&mut spans, &mut buffer);
                spans.push(Span::Bold(chars[index + 2..end].iter().collect()));
                index = end + 2;
                continue;
            }
        }
        if ch == '*' {
            if let Some(end) = find_from(&chars, index + 1, &['*']) {
                if end > index + 1 {
                    flush_span(&mut spans, &mut buffer);
                    spans.push(Span::Italic(chars[index + 1..end].iter().collect()));
                    index = end + 1;
                    continue;
                }
            }
        }
        if ch == '[' {
            if let Some((label, url, next)) = parse_link(&chars, index) {
                flush_span(&mut spans, &mut buffer);
                if safe_url(&url) {
                    spans.push(Span::Link(label, url));
                } else {
                    spans.push(Span::Text(label));
                }
                index = next;
                continue;
            }
        }
        buffer.push(ch);
        index += 1;
    }
    flush_span(&mut spans, &mut buffer);
    spans
}

fn flush_span(spans: &mut Vec<Span>, buffer: &mut String) {
    if !buffer.is_empty() {
        spans.push(Span::Text(std::mem::take(buffer)));
    }
}

fn parse_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let close = find_from(chars, start + 1, &[']'])?;
    if close + 1 >= chars.len() || chars[close + 1] != '(' {
        return None;
    }
    let end = find_from(chars, close + 2, &[')'])?;
    if end <= close + 2 {
        return None;
    }
    let label: String = chars[start + 1..close].iter().collect();
    let url: String = chars[close + 2..end].iter().collect();
    Some((label, url, end + 1))
}

fn safe_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        host.as_str(),
        "www.reddit.com" | "reddit.com" | "old.reddit.com" | "i.reddit.com"
    )
}

fn find_from(chars: &[char], start: usize, needle: &[char]) -> Option<usize> {
    if needle.is_empty() || start >= chars.len() || chars.len() < needle.len() {
        return None;
    }
    (start..=chars.len() - needle.len()).find(|&index| chars[index..index + needle.len()] == *needle)
}

/// Draw one report. The caller supplies the scroll area.
pub fn show(ui: &mut egui::Ui, text: &str) {
    for block in parse_blocks(text) {
        match block {
            Block::Heading(level, text) => {
                if level <= 2 {
                    ui.add_space(10.0);
                } else {
                    ui.add_space(6.0);
                }
                let size = match level {
                    1 => 24.0,
                    2 => 19.0,
                    _ => 15.5,
                };
                ui.label(
                    egui::RichText::new(text)
                        .size(size)
                        .strong()
                        .color(ui.visuals().strong_text_color()),
                );
                if level <= 2 {
                    ui.add_space(3.0);
                }
            }
            Block::Paragraph(text) => {
                ui.horizontal_wrapped(|ui| inline_spans(ui, &text, true, false));
                ui.add_space(7.0);
            }
            Block::Bullet(text) => {
                bullet_line(ui, "•", &text);
            }
            Block::Numbered(number, text) => {
                bullet_line(ui, &format!("{}.", number), &text);
            }
            Block::Quote(text) => {
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| inline_spans(ui, &text, false, true));
                    });
                ui.add_space(4.0);
            }
            Block::Rule => {
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
            }
        }
    }
}

/// Draw Reddit post or comment text. Line breaks stay where the author put them.
pub fn show_reddit(ui: &mut egui::Ui, text: &str) {
    let base = ui.visuals().text_color();
    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            ui.add_space(6.0);
            continue;
        }
        if trimmed == "---" || trimmed == "***" {
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            quote_line(ui, rest);
            continue;
        }
        if trimmed == ">" {
            quote_line(ui, "");
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            bullet_line(ui, "•", rest);
            continue;
        }
        if let Some((number, rest)) = numbered(trimmed) {
            bullet_line(ui, &format!("{}.", number), rest);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("### ") {
            reddit_heading(ui, rest, 15.5);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            reddit_heading(ui, rest, 17.0);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            reddit_heading(ui, rest, 19.0);
            continue;
        }
        ui.horizontal_wrapped(|ui| {
            inline_spans_at(ui, trimmed, true, 13.5, base);
        });
        ui.add_space(2.0);
    }
}

fn reddit_heading(ui: &mut egui::Ui, text: &str, size: f32) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(text)
            .size(size)
            .strong()
            .color(ui.visuals().strong_text_color()),
    );
    ui.add_space(2.0);
}

fn quote_line(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                inline_spans_at(ui, text, false, 13.0, ui.visuals().weak_text_color());
            });
        });
    ui.add_space(2.0);
}

fn bullet_line(ui: &mut egui::Ui, prefix: &str, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(prefix)
                .size(13.0)
                .strong()
                .color(ui.visuals().weak_text_color()),
        );
        inline_spans(ui, text, true, false);
    });
    ui.add_space(3.0);
}

fn inline_spans(ui: &mut egui::Ui, text: &str, allow_links: bool, muted: bool) {
    let base = if muted {
        ui.visuals().weak_text_color()
    } else {
        ui.visuals().text_color()
    };
    inline_spans_at(ui, text, allow_links, 13.0, base);
}

fn inline_spans_at(
    ui: &mut egui::Ui,
    text: &str,
    allow_links: bool,
    size: f32,
    base: egui::Color32,
) {
    for span in parse_inline(text) {
        match span {
            Span::Text(value) => {
                ui.label(egui::RichText::new(value).size(size).color(base));
            }
            Span::Bold(value) => {
                ui.label(egui::RichText::new(value).size(size).strong().color(base));
            }
            Span::Italic(value) => {
                ui.label(egui::RichText::new(value).size(size).italics().color(base));
            }
            Span::Code(value) => {
                ui.label(
                    egui::RichText::new(value)
                        .monospace()
                        .size(size - 1.0)
                        .color(base),
                );
            }
            Span::Link(label, url) => {
                if allow_links {
                    ui.hyperlink_to(egui::RichText::new(label).size(size), url);
                } else {
                    ui.label(egui::RichText::new(label).size(size).color(base));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_headings_lists_and_quotes() {
        let text = "# Title\n\nA paragraph\nover two lines.\n\n- one\n- two\n\n1. first\n2. second\n\n> a quote\n\n---";
        let blocks = parse_blocks(text);
        assert_eq!(blocks[0], Block::Heading(1, "Title".to_string()));
        assert_eq!(
            blocks[1],
            Block::Paragraph("A paragraph over two lines.".to_string())
        );
        assert_eq!(blocks[2], Block::Bullet("one".to_string()));
        assert_eq!(blocks[3], Block::Bullet("two".to_string()));
        assert_eq!(blocks[4], Block::Numbered(1, "first".to_string()));
        assert_eq!(blocks[5], Block::Numbered(2, "second".to_string()));
        assert_eq!(blocks[6], Block::Quote("a quote".to_string()));
        assert_eq!(blocks[7], Block::Rule);
    }

    #[test]
    fn joins_quote_lines_into_one_block() {
        let blocks = parse_blocks("> a quote\n> — r/ADHD · u/a · 2026-01-01\n\nnext");
        assert_eq!(
            blocks[0],
            Block::Quote("a quote — r/ADHD · u/a · 2026-01-01".to_string())
        );
        assert_eq!(blocks[1], Block::Paragraph("next".to_string()));
    }

    #[test]
    fn reads_bold_italic_and_code() {
        let spans = parse_inline("**bold** and *italic* and `code`");
        assert_eq!(spans[0], Span::Bold("bold".to_string()));
        assert_eq!(spans[2], Span::Italic("italic".to_string()));
        assert_eq!(spans[4], Span::Code("code".to_string()));
    }

    #[test]
    fn keeps_reddit_links() {
        let spans = parse_inline("Read [the thread](https://www.reddit.com/r/ADHD/comments/a1/x/).");
        assert_eq!(spans[0], Span::Text("Read ".to_string()));
        assert_eq!(
            spans[1],
            Span::Link(
                "the thread".to_string(),
                "https://www.reddit.com/r/ADHD/comments/a1/x/".to_string()
            )
        );
    }

    #[test]
    fn drops_links_to_other_hosts() {
        let spans = parse_inline("[bad](https://evil.example/x)");
        assert_eq!(spans[0], Span::Text("bad".to_string()));
    }

    #[test]
    fn escaped_brackets_stay_text() {
        let spans = parse_inline("price \\[100\\] now");
        assert_eq!(spans[0], Span::Text("price [100] now".to_string()));
    }
}
