//! Atlassian Document Format (ADF) to Markdown, and Markdown to ADF.
//!
//! [`to_markdown`] reads each node that Jira Cloud writes. A node with no Markdown form keeps
//! its text, such as a mention, a status, or a card. [`to_adf`] writes the elements that a
//! description needs: paragraphs, headings, bullet and numbered lists (nested), code blocks,
//! quotes, rules, inline code, bold, italic, strike, and links. A line break inside a paragraph
//! is a hard break, so that the lines of a text stay as they are.
//!
//! The two are each other's inverse for those elements: `to_markdown(to_adf(md)) == md` for a
//! text in the form that `to_markdown` writes.

use serde_json::{json, Map, Value};

use super::core::{falsy, py_str};

/// Inline characters that a text escapes with a backslash, so that it stays plain text.
const ESCAPE: &str = "\\`*[]~";
/// The characters that a backslash in a Markdown text escapes.
const ESCAPABLE: &str = "\\`*_[]~#>+-.!()|{}";

// ADF to Markdown -------------------------------------------------------------------------------

/// The Markdown text of an ADF document. Null and an empty document give "".
pub fn to_markdown(doc: &Value) -> String {
    match doc {
        Value::Object(map) => {
            let blocks = blocks(list(map.get("content")));
            let text = blocks
                .into_iter()
                .filter(|b| !b.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            text.trim_matches('\n').to_string()
        }
        Value::Null => String::new(),
        other => py_str(other),
    }
}

/// The items of a list value; each other value gives none.
fn list(value: Option<&Value>) -> &[Value] {
    match value {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

fn attrs(node: &Map<String, Value>) -> Map<String, Value> {
    match node.get("attrs") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn kind(node: &Map<String, Value>) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("")
}

fn str_of<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

/// An integer attribute: an int or a bool, as the first version of the scripts reads it.
fn int_attr(map: &Map<String, Value>, key: &str) -> Option<i64> {
    match map.get(key) {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64(),
        Some(Value::Bool(b)) => Some(i64::from(*b)),
        _ => None,
    }
}

/// The text of a truthy attribute, else of the next one, else "".
fn first_text(map: &Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| map.get(*k).filter(|v| !falsy(Some(v))))
        .map(py_str)
        .unwrap_or_default()
}

fn blocks(nodes: &[Value]) -> Vec<String> {
    nodes
        .iter()
        .filter_map(Value::as_object)
        .map(block)
        .collect()
}

fn block(node: &Map<String, Value>) -> String {
    let content = list(node.get("content"));
    let attrs = attrs(node);
    match kind(node) {
        "paragraph" => escape_starts(&inline(content)),
        "heading" => {
            let level = int_attr(&attrs, "level").unwrap_or(1).clamp(1, 6) as usize;
            format!(
                "{} {}",
                "#".repeat(level),
                inline(content).replace('\n', " ")
            )
        }
        "bulletList" => content
            .iter()
            .filter_map(Value::as_object)
            .map(|item| list_item("- ", item))
            .collect::<Vec<_>>()
            .join("\n"),
        "orderedList" => {
            let start = int_attr(&attrs, "order").unwrap_or(1);
            content
                .iter()
                .filter_map(Value::as_object)
                .enumerate()
                .map(|(n, item)| list_item(&format!("{}. ", start + n as i64), item))
                .collect::<Vec<_>>()
                .join("\n")
        }
        "taskList" => content
            .iter()
            .filter_map(Value::as_object)
            .map(|item| {
                let done = str_of(&self::attrs(item), "state") == Some("DONE");
                let mark = if done { "- [x] " } else { "- [ ] " };
                format!("{mark}{}", inline(list(item.get("content"))))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        "decisionList" => content
            .iter()
            .filter_map(Value::as_object)
            .map(|item| format!("- {}", inline(list(item.get("content")))))
            .collect::<Vec<_>>()
            .join("\n"),
        "codeBlock" => {
            let text: String = content
                .iter()
                .filter_map(Value::as_object)
                .map(|c| str_of(c, "text").unwrap_or(""))
                .collect();
            let mut fence = "```".to_string();
            while text.contains(&fence) {
                fence.push('`');
            }
            let language = str_of(&attrs, "language").unwrap_or("");
            format!("{fence}{language}\n{text}\n{fence}")
        }
        "blockquote" | "panel" => {
            let inner = blocks(content).join("\n\n");
            inner
                .split('\n')
                .map(|line| {
                    if line.is_empty() {
                        ">".to_string()
                    } else {
                        format!("> {line}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        "rule" => "---".into(),
        "expand" | "nestedExpand" => {
            let title = str_of(&attrs, "title").unwrap_or("");
            let inner = blocks(content).join("\n\n");
            if title.is_empty() {
                inner
            } else {
                format!("**{}**\n\n{inner}", escape(title))
            }
        }
        "table" => table(content),
        "mediaSingle" | "mediaGroup" | "media" => "[attachment]".into(),
        "blockCard" | "embedCard" => str_of(&attrs, "url").unwrap_or("").to_string(),
        k if k == "text" || INLINE.contains(&k) => inline(&[Value::Object(node.clone())]),
        _ => {
            // An unknown block: its children, or its text.
            if !falsy(node.get("content")) {
                blocks(content).join("\n\n")
            } else {
                str_of(node, "text").map(escape).unwrap_or_default()
            }
        }
    }
}

fn list_item(marker: &str, item: &Map<String, Value>) -> String {
    let pad = " ".repeat(marker.chars().count());
    let mut body = String::new();
    for (i, b) in list(item.get("content"))
        .iter()
        .filter_map(Value::as_object)
        .enumerate()
    {
        let text = block(b);
        if i == 0 {
            body.push_str(&text);
        } else if matches!(kind(b), "bulletList" | "orderedList" | "taskList") {
            body.push('\n');
            body.push_str(&text);
        } else {
            body.push_str("\n\n");
            body.push_str(&text);
        }
    }
    let mut lines = body.split('\n');
    let mut out = vec![format!("{marker}{}", lines.next().unwrap_or(""))];
    for line in lines {
        out.push(if line.is_empty() {
            String::new()
        } else {
            format!("{pad}{line}")
        });
    }
    out.join("\n")
}

fn table(rows: &[Value]) -> String {
    let mut cells: Vec<Vec<String>> = Vec::new();
    for row in rows.iter().filter_map(Value::as_object) {
        cells.push(
            list(row.get("content"))
                .iter()
                .filter_map(Value::as_object)
                .map(|c| {
                    inline_blocks(list(c.get("content")))
                        .replace('|', "\\|")
                        .replace('\n', " ")
                })
                .collect(),
        );
    }
    if cells.is_empty() {
        return String::new();
    }
    let width = cells.iter().map(Vec::len).max().unwrap_or(0);
    for row in &mut cells {
        row.resize(width, String::new());
    }
    let mut lines = vec![
        format!("| {} |", cells[0].join(" | ")),
        format!("|{}", " --- |".repeat(width)),
    ];
    for row in &cells[1..] {
        lines.push(format!("| {} |", row.join(" | ")));
    }
    lines.join("\n")
}

fn inline_blocks(nodes: &[Value]) -> String {
    nodes
        .iter()
        .filter_map(Value::as_object)
        .map(block)
        .collect::<Vec<_>>()
        .join(" ")
}

const INLINE: [&str; 9] = [
    "text",
    "hardBreak",
    "mention",
    "emoji",
    "inlineCard",
    "status",
    "date",
    "placeholder",
    "mediaInline",
];

const MARK_KINDS: [&str; 5] = ["strong", "em", "code", "strike", "link"];

/// The marks of a node that Markdown shows, with the address of a link.
fn marks(node: &Map<String, Value>) -> Vec<(String, Value)> {
    list(node.get("marks"))
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|m| {
            let kind = str_of(m, "type")?;
            if !MARK_KINDS.contains(&kind) {
                return None;
            }
            let href = attrs(m).get("href").cloned().unwrap_or(Value::Null);
            Some((kind.to_string(), href))
        })
        .collect()
}

fn href(node: &Map<String, Value>) -> Option<Value> {
    if kind(node) != "text" {
        return None;
    }
    for mark in list(node.get("marks")).iter().filter_map(Value::as_object) {
        if str_of(mark, "type") == Some("link") {
            let href = attrs(mark).get("href").cloned();
            return href.filter(|h| !falsy(Some(h)));
        }
    }
    None
}

fn without_link(node: &Map<String, Value>) -> Map<String, Value> {
    let marks: Vec<Value> = list(node.get("marks"))
        .iter()
        .filter(|m| !(m.is_object() && m.get("type").and_then(Value::as_str) == Some("link")))
        .cloned()
        .collect();
    let mut node = node.clone();
    node.insert("marks".into(), Value::Array(marks));
    node
}

fn inline(nodes: &[Value]) -> String {
    // Join neighbor text nodes with the same marks, so that `**a****b**` never happens.
    let mut merged: Vec<Map<String, Value>> = Vec::new();
    for node in nodes.iter().filter_map(Value::as_object) {
        if kind(node) == "text" {
            if let Some(last) = merged.last_mut() {
                if kind(last) == "text" && marks(last) == marks(node) {
                    let text = format!(
                        "{}{}",
                        str_of(last, "text").unwrap_or(""),
                        str_of(node, "text").unwrap_or("")
                    );
                    last.insert("text".into(), Value::String(text));
                    continue;
                }
            }
        }
        merged.push(node.clone());
    }
    // A run of nodes with the same link is one link, such as [`code` text](href).
    let mut out = String::new();
    let mut i = 0;
    while i < merged.len() {
        let Some(link) = href(&merged[i]) else {
            out.push_str(&inline_node(&merged[i]));
            i += 1;
            continue;
        };
        let mut j = i;
        while j < merged.len() && href(&merged[j]).as_ref() == Some(&link) {
            j += 1;
        }
        let label: String = merged[i..j]
            .iter()
            .map(|n| inline_node(&without_link(n)))
            .collect();
        out.push_str(&format!("[{label}]({})", url(&py_str(&link))));
        i = j;
    }
    out
}

/// The whole number of a value as `int()` reads it, or `None`.
fn int_of(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => s.trim().replace('_', "").parse().ok(),
        _ => None,
    }
}

fn date_of(value: Option<&Value>) -> String {
    let Some(ms) = int_of(value) else {
        return String::new();
    };
    match time::OffsetDateTime::from_unix_timestamp(ms.div_euclid(1000)) {
        Ok(at) => format!(
            "{:04}-{:02}-{:02}",
            at.year(),
            u8::from(at.month()),
            at.day()
        ),
        Err(_) => String::new(),
    }
}

fn inline_node(node: &Map<String, Value>) -> String {
    let attrs = attrs(node);
    match kind(node) {
        "hardBreak" => "\n".into(),
        "mention" => {
            let text = first_text(&attrs, &["text", "id"]);
            if text.starts_with('@') {
                text
            } else {
                format!("@{text}")
            }
        }
        "emoji" => first_text(&attrs, &["text", "shortName"]),
        "inlineCard" => first_text(&attrs, &["url"]),
        "status" => format!("[{}]", escape(&first_text(&attrs, &["text"]))),
        "date" => date_of(attrs.get("timestamp")),
        "mediaInline" => "[attachment]".into(),
        "text" => {
            let text = str_of(node, "text").unwrap_or("");
            if text.is_empty() {
                return String::new();
            }
            let kinds: Vec<String> = marks(node).into_iter().map(|(k, _)| k).collect();
            let has = |k: &str| kinds.iter().any(|m| m == k);
            if has("code") {
                return code_span(text);
            }
            let out = escape(text);
            // Emphasis around spaces does not parse: keep the spaces outside of the markers.
            let core = out.trim_matches(' ');
            if core.is_empty() {
                return out;
            }
            let lead = &out[..out.len() - out.trim_start_matches(' ').len()];
            let trail = &out[out.trim_end_matches(' ').len()..];
            let mut core = core.to_string();
            if has("strike") {
                core = format!("~~{core}~~");
            }
            if has("em") {
                // Inside `**`, an underscore keeps `***` from reading two ways.
                core = if has("strong") {
                    format!("_{core}_")
                } else {
                    format!("*{core}*")
                };
            }
            if has("strong") {
                core = format!("**{core}**");
            }
            format!("{lead}{core}{trail}")
        }
        _ => match node.get("content") {
            Some(Value::Array(content)) => inline(content),
            _ => str_of(node, "text").map(escape).unwrap_or_default(),
        },
    }
}

fn code_span(text: &str) -> String {
    let mut run = 1;
    while text.contains(&"`".repeat(run)) {
        run += 1;
    }
    let fence = "`".repeat(run);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

fn url(href: &str) -> String {
    href.replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
}

fn escape(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        if ESCAPE.contains(c) {
            out.push('\\');
            out.push(c);
        } else if c == '_' {
            // An underscore inside a word never makes emphasis, so it stays as it is.
            let before = if i > 0 { chars[i - 1] } else { ' ' };
            let after = chars.get(i + 1).copied().unwrap_or(' ');
            if before.is_alphanumeric() && after.is_alphanumeric() {
                out.push('_');
            } else {
                out.push_str("\\_");
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The count of leading ASCII digits.
fn digits(line: &str) -> usize {
    line.bytes().take_while(u8::is_ascii_digit).count()
}

/// The rest after a run of `c` of at least one, with the length of the run.
fn run_of(text: &str, c: char) -> (usize, &str) {
    let rest = text.trim_start_matches(c);
    ((text.len() - rest.len()) / c.len_utf8(), rest)
}

fn ends_or_space(rest: &str) -> bool {
    rest.is_empty() || rest.starts_with(char::is_whitespace)
}

/// True when Markdown reads a paragraph line as a block marker.
fn looks_like_block(line: &str) -> bool {
    let (hashes, rest) = run_of(line, '#');
    if (1..=6).contains(&hashes) && ends_or_space(rest) {
        return true;
    }
    if line.starts_with('>') || line.starts_with("```") || line.starts_with("~~~") {
        return true;
    }
    if let Some(rest) = line.strip_prefix(['-', '+']) {
        if ends_or_space(rest) {
            return true;
        }
    }
    let n = digits(line);
    if n > 0 {
        if let Some(rest) = line[n..].strip_prefix(['.', ')']) {
            if ends_or_space(rest) {
                return true;
            }
        }
    }
    for c in ['-', '_'] {
        let (count, rest) = run_of(line, c);
        if count >= 3 && rest.chars().all(char::is_whitespace) {
            return true;
        }
    }
    false
}

/// A paragraph line that Markdown reads as a block marker gets a backslash.
fn escape_starts(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            if !looks_like_block(line) {
                return line.to_string();
            }
            let n = digits(line);
            if n > 0 {
                format!("{}\\{}", &line[..n], &line[n..])
            } else {
                format!("\\{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Markdown to ADF -------------------------------------------------------------------------------

/// Up to three leading spaces off, or `None` when the line has four or more.
fn indent3(line: &str) -> Option<&str> {
    let spaces = line.len() - line.trim_start_matches(' ').len();
    if spaces > 3 {
        None
    } else {
        Some(&line[spaces..])
    }
}

/// An opening fence: the marker and the language.
fn fence(line: &str) -> Option<(char, usize, String)> {
    let rest = indent3(line)?;
    let c = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let (count, rest) = run_of(rest, c);
    if count < 3 {
        return None;
    }
    let rest = rest.trim_start();
    let end = rest
        .find(|ch: char| ch == '`' || ch.is_whitespace())
        .unwrap_or(rest.len());
    let (language, rest) = rest.split_at(end);
    if !rest.trim().is_empty() {
        return None;
    }
    Some((c, count, language.to_string()))
}

/// The closing fence of an opening fence of `count` times `c`.
fn closes(line: &str, c: char, count: usize) -> bool {
    let Some(rest) = indent3(line) else {
        return false;
    };
    let (n, rest) = run_of(rest, c);
    n >= count && rest.trim().is_empty()
}

/// A heading: the level and the text.
fn heading(line: &str) -> Option<(usize, String)> {
    let rest = indent3(line)?;
    let (level, rest) = run_of(rest, '#');
    if !(1..=6).contains(&level) || !ends_or_space(rest) {
        return None;
    }
    Some((level, rest.trim().to_string()))
}

fn is_rule(line: &str) -> bool {
    let Some(rest) = indent3(line) else {
        return false;
    };
    ['*', '-', '_'].iter().any(|&c| {
        rest.starts_with(c)
            && rest.chars().all(|ch| ch == c || ch.is_whitespace())
            && rest.chars().filter(|&ch| ch == c).count() >= 3
    })
}

/// A quote line: its text after `>` and one space.
fn quote(line: &str) -> Option<&str> {
    let rest = indent3(line)?.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// A list item: its indent, its marker, its spaces after the marker, and its text.
struct Item<'a> {
    indent: usize,
    marker: &'a str,
    gap: usize,
    text: &'a str,
}

impl Item<'_> {
    fn ordered(&self) -> bool {
        self.marker.starts_with(|c: char| c.is_ascii_digit())
    }
}

fn item(line: &str) -> Option<Item<'_>> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let marker_len = if rest.starts_with(['-', '*', '+']) {
        1
    } else {
        let n = digits(rest);
        if (1..=9).contains(&n) && rest[n..].starts_with(['.', ')']) {
            n + 1
        } else {
            return None;
        }
    };
    let marker = &rest[..marker_len];
    let after = &rest[marker_len..];
    let text = after.trim_start_matches(' ');
    let gap = after.len() - text.len();
    if gap == 0 && !after.is_empty() {
        return None;
    }
    Some(Item {
        indent,
        marker,
        gap,
        text,
    })
}

fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// The ADF document of a Markdown text.
pub fn to_adf(text: &str) -> Value {
    let text = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ");
    let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    json!({"type": "doc", "version": 1, "content": parse_blocks(&lines)})
}

fn starts_block(line: &str) -> bool {
    fence(line).is_some()
        || heading(line).is_some()
        || is_rule(line)
        || quote(line).is_some()
        || item(line).is_some_and(|i| !i.text.trim().is_empty())
}

fn parse_blocks(lines: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    let n = lines.len();
    let mut i = 0;
    while i < n {
        let line = lines[i].as_str();
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if let Some((c, count, language)) = fence(line) {
            let mut body = Vec::new();
            i += 1;
            while i < n && !closes(&lines[i], c, count) {
                body.push(lines[i].as_str());
                i += 1;
            }
            i += 1;
            let mut node = Map::new();
            node.insert("type".into(), json!("codeBlock"));
            if !language.is_empty() {
                node.insert("attrs".into(), json!({ "language": language }));
            }
            let code = body.join("\n");
            if !code.is_empty() {
                node.insert("content".into(), json!([{"type": "text", "text": code}]));
            }
            out.push(Value::Object(node));
            continue;
        }
        if let Some((level, text)) = heading(line) {
            let text = strip_closing_hashes(&text);
            let mut node = Map::new();
            node.insert("type".into(), json!("heading"));
            node.insert("attrs".into(), json!({ "level": level }));
            let content = parse_inline(&text, &[]);
            if !content.is_empty() {
                node.insert("content".into(), Value::Array(content));
            }
            out.push(Value::Object(node));
            i += 1;
            continue;
        }
        if is_rule(line) {
            out.push(json!({"type": "rule"}));
            i += 1;
            continue;
        }
        if quote(line).is_some() {
            let mut inner = Vec::new();
            while i < n && !lines[i].trim().is_empty() {
                let Some(text) = quote(&lines[i]) else {
                    break;
                };
                inner.push(text.to_string());
                i += 1;
            }
            let content = parse_blocks(&inner);
            let content = if content.is_empty() {
                vec![json!({"type": "paragraph"})]
            } else {
                content
            };
            out.push(json!({"type": "blockquote", "content": content}));
            continue;
        }
        if let Some(it) = item(line) {
            if !it.text.trim().is_empty() || it.gap == 0 {
                let (node, next) = parse_list(lines, i);
                out.push(node);
                i = next;
                continue;
            }
        }
        let mut para = vec![line.trim().to_string()];
        i += 1;
        while i < n && !lines[i].trim().is_empty() && !starts_block(&lines[i]) {
            para.push(lines[i].trim().to_string());
            i += 1;
        }
        let mut node = Map::new();
        node.insert("type".into(), json!("paragraph"));
        let content = parse_inline(&para.join("\n"), &[]);
        if !content.is_empty() {
            node.insert("content".into(), Value::Array(content));
        }
        out.push(Value::Object(node));
    }
    out
}

/// The text of a heading with no closing `#` marks: white space and then `#` at the end go.
fn strip_closing_hashes(text: &str) -> String {
    let rest = text.trim_end_matches('#');
    if rest.len() < text.len() && rest.ends_with(char::is_whitespace) {
        rest.trim_end().to_string()
    } else {
        text.to_string()
    }
}

fn parse_list(lines: &[String], start: usize) -> (Value, usize) {
    let n = lines.len();
    let mut i = start;
    let first = item(&lines[i]).expect("a list starts with an item");
    let indent = first.indent;
    let ordered = first.ordered();
    let mut node = Map::new();
    node.insert(
        "type".into(),
        json!(if ordered { "orderedList" } else { "bulletList" }),
    );
    if ordered {
        let number: i64 = first.marker[..first.marker.len() - 1].parse().unwrap_or(1);
        if number != 1 {
            node.insert("attrs".into(), json!({ "order": number }));
        }
    }
    let mut items = Vec::new();
    while i < n {
        let Some(it) = item(&lines[i]) else {
            break;
        };
        if it.indent != indent || it.ordered() != ordered {
            break;
        }
        let width = it.indent + it.marker.len() + it.gap.max(1);
        let mut body = vec![it.text.to_string()];
        i += 1;
        while i < n {
            let line = lines[i].as_str();
            if line.trim().is_empty() {
                // A blank line belongs to the item when the next text line is inside it.
                let mut j = i;
                while j < n && lines[j].trim().is_empty() {
                    j += 1;
                }
                if j < n && leading_spaces(&lines[j]) >= width {
                    body.extend(std::iter::repeat_n(String::new(), j - i));
                    i = j;
                    continue;
                }
                if j < n {
                    if let Some(sibling) = item(&lines[j]) {
                        if sibling.indent == indent && sibling.ordered() == ordered {
                            // A loose list: the next item of the same list after a blank line.
                            i = j;
                        }
                    }
                }
                break;
            }
            let spaces = leading_spaces(line);
            if spaces >= width {
                body.push(line[width..].to_string());
            } else if spaces > indent && item(line).is_some() {
                body.push(line[spaces..].to_string());
            } else if !starts_block(line) && spaces > indent {
                body.push(line.trim().to_string());
            } else {
                break;
            }
            i += 1;
        }
        let content = parse_blocks(&body);
        let content = if content.is_empty() {
            vec![json!({"type": "paragraph"})]
        } else {
            content
        };
        items.push(json!({"type": "listItem", "content": content}));
    }
    node.insert("content".into(), Value::Array(items));
    (Value::Object(node), i)
}

/// A mark of the inline parser: its kind and the address of a link.
type Mark = (&'static str, Option<String>);

const ORDER: [&str; 5] = ["link", "strong", "em", "strike", "code"];

const DELIMS: [(&str, &[&str]); 7] = [
    ("***", &["strong", "em"]),
    ("___", &["strong", "em"]),
    ("**", &["strong"]),
    ("__", &["strong"]),
    ("~~", &["strike"]),
    ("*", &["em"]),
    ("_", &["em"]),
];

fn starts_at(text: &[char], i: usize, delim: &str) -> bool {
    let d: Vec<char> = delim.chars().collect();
    i + d.len() <= text.len() && text[i..i + d.len()] == d[..]
}

fn find(text: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(text.len()));
    }
    (from..text.len().saturating_sub(needle.len() - 1))
        .find(|&j| text[j..j + needle.len()] == needle[..])
}

fn backtick_run(text: &[char], i: usize) -> usize {
    text[i..].iter().take_while(|&&c| c == '`').count()
}

/// The inline nodes of a text: text with marks, and a hard break for each line end.
fn parse_inline(text: &str, marks: &[Mark]) -> Vec<Value> {
    let chars: Vec<char> = text.chars().collect();
    parse_chars(&chars, marks)
}

fn parse_chars(text: &[char], marks: &[Mark]) -> Vec<Value> {
    let mut nodes: Vec<Value> = Vec::new();
    let mut buf = String::new();
    let flush = |nodes: &mut Vec<Value>, buf: &mut String| {
        if !buf.is_empty() {
            add_text(nodes, buf, marks);
            buf.clear();
        }
    };
    let n = text.len();
    let mut i = 0;
    'outer: while i < n {
        let c = text[i];
        if c == '\\' && i + 1 < n && ESCAPABLE.contains(text[i + 1]) {
            buf.push(text[i + 1]);
            i += 2;
            continue;
        }
        if c == '`' {
            let run = backtick_run(text, i);
            if let Some(end) = code_end(text, i + run, run) {
                flush(&mut nodes, &mut buf);
                let mut code: String = text[i + run..end].iter().collect();
                if code.chars().count() >= 2
                    && code.starts_with(' ')
                    && code.ends_with(' ')
                    && !code.trim().is_empty()
                {
                    code = code[1..code.len() - 1].to_string();
                }
                let mut with: Vec<Mark> = marks.iter().filter(|m| m.0 == "link").cloned().collect();
                with.push(("code", None));
                add_text(&mut nodes, &code, &with);
                i = end + run;
                continue;
            }
            buf.extend(&text[i..i + run]);
            i += run;
            continue;
        }
        if c == '[' {
            if let Some((label, href, end)) = find_link(text, i) {
                flush(&mut nodes, &mut buf);
                let mut with = marks.to_vec();
                with.push(("link", Some(href)));
                nodes.extend(parse_chars(&label, &with));
                i = end;
                continue;
            }
        }
        for (delim, kinds) in DELIMS {
            if starts_at(text, i, delim) && opens(text, i, delim) {
                let len = delim.chars().count();
                if let Some(end) = find_close(text, i + len, delim) {
                    flush(&mut nodes, &mut buf);
                    let mut with = marks.to_vec();
                    with.extend(kinds.iter().map(|k| (*k, None)));
                    nodes.extend(parse_chars(&text[i + len..end], &with));
                    i = end + len;
                    continue 'outer;
                }
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut nodes, &mut buf);
    nodes
}

/// Adds text with marks; each line end becomes a hard break.
fn add_text(nodes: &mut Vec<Value>, text: &str, marks: &[Mark]) {
    for (k, part) in text.split('\n').enumerate() {
        if k > 0 {
            nodes.push(json!({"type": "hardBreak"}));
        }
        if part.is_empty() {
            continue;
        }
        let mut adf_marks: Vec<Value> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for (mark, href) in marks {
            if seen.contains(mark) {
                continue;
            }
            seen.push(mark);
            if *mark == "link" {
                adf_marks.push(json!({"type": "link", "attrs": {"href": href}}));
            } else {
                adf_marks.push(json!({ "type": mark }));
            }
        }
        adf_marks.sort_by_key(|m| {
            let kind = m["type"].as_str().unwrap_or("");
            ORDER.iter().position(|o| *o == kind).unwrap_or(ORDER.len())
        });
        let mut node = Map::new();
        node.insert("type".into(), json!("text"));
        node.insert("text".into(), json!(part));
        if !adf_marks.is_empty() {
            node.insert("marks".into(), Value::Array(adf_marks));
        }
        if let Some(Value::Object(last)) = nodes.last_mut() {
            if last.get("type") == Some(&json!("text")) && last.get("marks") == node.get("marks") {
                let joined = format!("{}{part}", last["text"].as_str().unwrap_or(""));
                last.insert("text".into(), Value::String(joined));
                continue;
            }
        }
        nodes.push(Value::Object(node));
    }
}

fn opens(text: &[char], i: usize, delim: &str) -> bool {
    let len = delim.chars().count();
    let after = text.get(i + len).copied().unwrap_or(' ');
    if after.is_whitespace() {
        return false;
    }
    if delim.starts_with('_') {
        let before = if i > 0 { text[i - 1] } else { ' ' };
        if before.is_alphanumeric() {
            return false;
        }
    }
    true
}

fn find_close(text: &[char], start: usize, delim: &str) -> Option<usize> {
    let len = delim.chars().count();
    let first = delim.chars().next().unwrap_or(' ');
    let n = text.len();
    let mut i = start;
    while i < n {
        let c = text[i];
        if c == '\\' {
            i += 2;
            continue;
        }
        if c == '`' {
            let run = backtick_run(text, i);
            i = match code_end(text, i + run, run) {
                Some(end) => end + run,
                None => i + run,
            };
            continue;
        }
        if starts_at(text, i, delim) && i > start && !text[i - 1].is_whitespace() {
            let after = text.get(i + len).copied().unwrap_or(' ');
            if first == '_' && after.is_alphanumeric() {
                i += 1;
                continue;
            }
            if len == 1 && after == first {
                // `**` is not the end of `*`: skip the pair.
                i += 2;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

fn code_end(text: &[char], start: usize, run: usize) -> Option<usize> {
    let needle = vec!['`'; run];
    let mut i = start;
    loop {
        let j = find(text, &needle, i)?;
        let end = j + run;
        if (j == 0 || text[j - 1] != '`') && (end >= text.len() || text[end] != '`') {
            return Some(j);
        }
        i = j + 1;
        while i < text.len() && text[i] == '`' {
            i += 1;
        }
    }
}

fn find_link(text: &[char], i: usize) -> Option<(Vec<char>, String, usize)> {
    let n = text.len();
    let mut depth = 0i64;
    let mut j = i;
    let mut closed = false;
    while j < n {
        let c = text[j];
        if c == '\\' {
            j += 2;
            continue;
        }
        if c == '[' {
            depth += 1;
        } else if c == ']' {
            depth -= 1;
            if depth == 0 {
                closed = true;
                break;
            }
        }
        j += 1;
    }
    if !closed || j + 1 >= n || text[j + 1] != '(' {
        return None;
    }
    let end = find(text, &[')'], j + 2)?;
    let href: String = text[j + 2..end]
        .iter()
        .collect::<String>()
        .trim()
        .to_string();
    if href.is_empty() || href.contains(' ') || href.contains('\n') {
        return None;
    }
    Some((text[i + 1..j].to_vec(), href, end + 1))
}
