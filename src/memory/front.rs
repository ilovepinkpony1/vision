//! The values of the front matter (section 9.1): a small subset of YAML.
//!
//! One key on each line, with a string, a number, `null`, a flat list such as `[a, b]`, or a
//! flat map such as `{used: 6, agreed: 5}`. A comment starts at a `#` after a space. A string
//! may be bare, or quoted with `"..."` (with the escapes of JSON) or `'...'` (with `''`).
//!
//! [`parse_entry`] reads one line, and [`render_value`] writes a value so that
//! [`parse_entry`] gives the same value back.

use std::fmt;

/// A single value: a string, a number (kept as its text), or `null`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scalar {
    Null,
    /// A number, such as `6` or `-0.5`, kept as it was written.
    Num(String),
    Str(String),
}

impl Scalar {
    /// The text of a string or a number. `None` for `null`.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Scalar::Null => None,
            Scalar::Num(text) | Scalar::Str(text) => Some(text),
        }
    }
}

/// The value of one key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Scalar(Scalar),
    List(Vec<Scalar>),
    Map(Vec<(String, Scalar)>),
}

impl Value {
    pub fn null() -> Value {
        Value::Scalar(Scalar::Null)
    }

    pub fn str(text: impl Into<String>) -> Value {
        Value::Scalar(Scalar::Str(text.into()))
    }

    pub fn num(n: u64) -> Value {
        Value::Scalar(Scalar::Num(n.to_string()))
    }

    /// A string, or `null` for `None`.
    pub fn opt_str(text: Option<&str>) -> Value {
        text.map_or_else(Value::null, Value::str)
    }

    /// A list of strings.
    pub fn list<I, S>(items: I) -> Value
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Value::List(items.into_iter().map(|s| Scalar::Str(s.into())).collect())
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Scalar(Scalar::Null))
    }

    /// The text of a string or a number.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Scalar(scalar) => scalar.as_text(),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Scalar]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[(String, Scalar)]> {
        match self {
            Value::Map(items) => Some(items),
            _ => None,
        }
    }

    /// A short name of the type, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Scalar(Scalar::Null) => "null",
            Value::Scalar(Scalar::Num(_)) => "a number",
            Value::Scalar(Scalar::Str(_)) => "a string",
            Value::List(_) => "a list",
            Value::Map(_) => "a map",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&render_value(self))
    }
}

/// One line of the front matter that holds a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedEntry {
    pub key: String,
    pub value: Value,
    /// The text after the value, from the first space before `#`, such as `   # a | b`.
    /// Empty when the line has no comment.
    pub comment: String,
}

/// What one line of the front matter holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedLine {
    /// An empty line, or a line with only a comment.
    Other,
    Entry(ParsedEntry),
}

/// Reads one line of the front matter. The error says what is wrong in words for people.
pub fn parse_entry(line: &str) -> Result<ParsedLine, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(ParsedLine::Other);
    }
    if line.starts_with([' ', '\t']) {
        return Err(
            "the line starts with a space. Each key starts at the start of its line, \
             and a value cannot span lines"
                .into(),
        );
    }
    let Some(colon) = line.find(':') else {
        return Err(format!(
            "`{trimmed}` is not `key: value`. Each line of the front matter holds one key"
        ));
    };
    let key = &line[..colon];
    if !is_key(key) {
        return Err(format!(
            "`{key}` is not a valid key. Use letters, digits, and `_`, such as asked_by"
        ));
    }
    let rest = &line[colon + 1..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return Err(format!("put a space after `{key}:`"));
    }
    let mut cursor = Cursor::new(rest);
    cursor.skip_space();
    let value = if cursor.at_end() || cursor.at_comment() {
        Value::null()
    } else {
        parse_value(&mut cursor)?
    };
    // A bare value stops at the comment, so its spaces before `#` belong to the comment.
    let comment_start = rest[..cursor.pos].trim_end().len();
    cursor.skip_space();
    if !cursor.at_end() && !cursor.at_comment() {
        return Err(format!(
            "unexpected text `{}` after the value of `{key}`. Quote a value that holds such text",
            cursor.rest().trim_end()
        ));
    }
    let comment = if cursor.at_end() {
        String::new()
    } else {
        rest[comment_start..].to_string()
    };
    Ok(ParsedLine::Entry(ParsedEntry {
        key: key.to_string(),
        value,
        comment,
    }))
}

/// True for a key such as `asked_by`: a letter or `_` first, then letters, digits, `_`, or `-`.
pub fn is_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The text of a value, as [`parse_entry`] reads it back.
pub fn render_value(value: &Value) -> String {
    match value {
        Value::Scalar(scalar) => render_scalar(scalar, Place::Top),
        Value::List(items) => {
            let items: Vec<String> = items
                .iter()
                .map(|s| render_scalar(s, Place::List))
                .collect();
            format!("[{}]", items.join(", "))
        }
        Value::Map(items) => {
            let items: Vec<String> = items
                .iter()
                .map(|(k, v)| {
                    let key = if is_key(k) { k.clone() } else { quote(k) };
                    format!("{key}: {}", render_scalar(v, Place::Map))
                })
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Top,
    List,
    Map,
}

fn render_scalar(scalar: &Scalar, place: Place) -> String {
    match scalar {
        Scalar::Null => "null".into(),
        Scalar::Num(text) => text.clone(),
        Scalar::Str(text) if is_plain(text, place) => text.clone(),
        Scalar::Str(text) => quote(text),
    }
}

fn quote(text: &str) -> String {
    // serde_json writes the escapes that the parser reads: \" \\ \n \t \r \uXXXX.
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{}\"", text.replace('"', "'")))
}

/// True when a string can be written bare and read back as the same string.
fn is_plain(text: &str, place: Place) -> bool {
    if text.is_empty() || text != text.trim() {
        return false;
    }
    if text.chars().any(|c| c.is_control()) {
        return false;
    }
    if classify_bare(text) != Scalar::Str(text.to_string()) {
        return false;
    }
    let first = text.chars().next().unwrap_or(' ');
    if "[]{}\"'#,&*!|>%@`".contains(first) {
        return false;
    }
    if ["- ", "? ", ": "].iter().any(|p| text.starts_with(p)) || text == "-" {
        return false;
    }
    if text.contains(": ") || text.contains(" #") || text.contains("\t#") || text.ends_with(':') {
        return false;
    }
    match place {
        Place::Top => true,
        Place::List | Place::Map => !text.contains([',', '[', ']', '{', '}']),
    }
}

/// The scalar of a bare word: `null` and `~` are null, a decimal number is a number, and any
/// other text is a string.
fn classify_bare(text: &str) -> Scalar {
    if text == "null" || text == "~" {
        return Scalar::Null;
    }
    if is_number(text) {
        return Scalar::Num(text.to_string());
    }
    Scalar::Str(text.to_string())
}

fn is_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = match digits.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (digits, None),
    };
    let whole_ok = !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && (whole == "0" || !whole.starts_with('0'));
    let fraction_ok =
        fraction.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()));
    whole_ok && fraction_ok
}

fn parse_value(cursor: &mut Cursor<'_>) -> Result<Value, String> {
    match cursor.peek() {
        Some('[') => {
            cursor.bump();
            let mut items = Vec::new();
            loop {
                cursor.skip_space();
                match cursor.peek() {
                    Some(']') => {
                        cursor.bump();
                        return Ok(Value::List(items));
                    }
                    None => return Err("the list has no closing `]`".into()),
                    _ => {}
                }
                items.push(parse_item(cursor, &[',', ']'])?);
                cursor.skip_space();
                match cursor.peek() {
                    Some(',') => cursor.bump(),
                    Some(']') => {}
                    None => return Err("the list has no closing `]`".into()),
                    Some(c) => return Err(format!("expected `,` or `]` in the list, not `{c}`")),
                }
            }
        }
        Some('{') => {
            cursor.bump();
            let mut items: Vec<(String, Scalar)> = Vec::new();
            loop {
                cursor.skip_space();
                match cursor.peek() {
                    Some('}') => {
                        cursor.bump();
                        return Ok(Value::Map(items));
                    }
                    None => return Err("the map has no closing `}`".into()),
                    _ => {}
                }
                let key = match parse_item(cursor, &[':', ',', '}'])? {
                    Scalar::Str(k) | Scalar::Num(k) => k,
                    Scalar::Null => return Err("a key of the map is null".into()),
                };
                cursor.skip_space();
                if cursor.peek() != Some(':') {
                    return Err(format!("the key `{key}` of the map needs `: value`"));
                }
                cursor.bump();
                cursor.skip_space();
                let value = parse_item(cursor, &[',', '}'])?;
                if items.iter().any(|(k, _)| *k == key) {
                    return Err(format!("the map has the key `{key}` two times"));
                }
                items.push((key, value));
                cursor.skip_space();
                match cursor.peek() {
                    Some(',') => cursor.bump(),
                    Some('}') => {}
                    None => return Err("the map has no closing `}`".into()),
                    Some(c) => return Err(format!("expected `,` or `}}` in the map, not `{c}`")),
                }
            }
        }
        Some('"') | Some('\'') => Ok(Value::Scalar(Scalar::Str(parse_quoted(cursor)?))),
        _ => {
            let start = cursor.pos;
            while !cursor.at_end() && !cursor.at_comment() {
                cursor.bump();
            }
            let text = cursor.text[start..cursor.pos].trim_end();
            Ok(Value::Scalar(classify_bare(text)))
        }
    }
}

/// One item of a list or a map: a quoted string, or bare text up to one of `stops`.
fn parse_item(cursor: &mut Cursor<'_>, stops: &[char]) -> Result<Scalar, String> {
    if matches!(cursor.peek(), Some('"') | Some('\'')) {
        return Ok(Scalar::Str(parse_quoted(cursor)?));
    }
    let start = cursor.pos;
    while let Some(c) = cursor.peek() {
        if stops.contains(&c) || c == '[' || c == '{' {
            break;
        }
        cursor.bump();
    }
    if matches!(cursor.peek(), Some('[') | Some('{')) {
        return Err("a list or a map cannot hold a list or a map".into());
    }
    let text = cursor.text[start..cursor.pos].trim();
    if text.is_empty() {
        return Err("a list or a map has an empty item".into());
    }
    Ok(classify_bare(text))
}

fn parse_quoted(cursor: &mut Cursor<'_>) -> Result<String, String> {
    let quote = cursor.peek().unwrap_or('"');
    cursor.bump();
    let mut out = String::new();
    loop {
        let Some(c) = cursor.peek() else {
            return Err(format!("the string has no closing {quote}"));
        };
        cursor.bump();
        if c == quote {
            if quote == '\'' && cursor.peek() == Some('\'') {
                cursor.bump();
                out.push('\'');
                continue;
            }
            return Ok(out);
        }
        if c == '\\' && quote == '"' {
            let Some(e) = cursor.peek() else {
                return Err("the string ends with a lone `\\`".into());
            };
            cursor.bump();
            match e {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => out.push(parse_unicode(cursor)?),
                other => return Err(format!("`\\{other}` is not a known escape in a string")),
            }
            continue;
        }
        out.push(c);
    }
}

fn parse_unicode(cursor: &mut Cursor<'_>) -> Result<char, String> {
    let first = hex4(cursor)?;
    if (0xD800..0xDC00).contains(&first) {
        if cursor.rest().starts_with("\\u") {
            cursor.bump();
            cursor.bump();
            let second = hex4(cursor)?;
            if (0xDC00..0xE000).contains(&second) {
                let code = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                return char::from_u32(code).ok_or_else(|| "a bad \\u escape".to_string());
            }
        }
        return Err("a \\u escape names half of a character".into());
    }
    char::from_u32(first).ok_or_else(|| "a bad \\u escape".to_string())
}

fn hex4(cursor: &mut Cursor<'_>) -> Result<u32, String> {
    let rest = cursor.rest();
    let digits: String = rest.chars().take(4).collect();
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("`\\u` needs 4 hex digits".into());
    }
    for _ in 0..4 {
        cursor.bump();
    }
    u32::from_str_radix(&digits, 16).map_err(|_| "`\\u` needs 4 hex digits".to_string())
}

struct Cursor<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Cursor<'a> {
        Cursor { text, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.pos += c.len_utf8();
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    /// True at a `#` that starts a comment: at the start, or after a space.
    fn at_comment(&self) -> bool {
        if self.peek() != Some('#') {
            return false;
        }
        self.pos == 0 || self.text[..self.pos].ends_with([' ', '\t'])
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t')) {
            self.bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(line: &str) -> ParsedEntry {
        match parse_entry(line) {
            Ok(ParsedLine::Entry(e)) => e,
            other => panic!("{line:?} gave {other:?}"),
        }
    }

    fn s(text: &str) -> Scalar {
        Scalar::Str(text.into())
    }

    #[test]
    fn reads_each_kind_of_value() {
        assert_eq!(entry("id: D-0142").value, Value::str("D-0142"));
        assert_eq!(
            entry("at: 2026-11-16T14:32:00Z").value,
            Value::str("2026-11-16T14:32:00Z")
        );
        assert_eq!(entry("n: 6").value, Value::Scalar(Scalar::Num("6".into())));
        assert_eq!(
            entry("n: -0.5").value,
            Value::Scalar(Scalar::Num("-0.5".into()))
        );
        assert_eq!(entry("supersedes: null").value, Value::null());
        assert_eq!(entry("supersedes: ~").value, Value::null());
        assert_eq!(entry("supersedes:").value, Value::null());
        assert_eq!(entry("area: []").value, Value::List(vec![]));
        assert_eq!(
            entry("area: [reports, exports]").value,
            Value::list(["reports", "exports"])
        );
        assert_eq!(
            entry("uses: {used: 6, agreed: 5, corrected: 0}").value,
            Value::Map(vec![
                ("used".into(), Scalar::Num("6".into())),
                ("agreed".into(), Scalar::Num("5".into())),
                ("corrected".into(), Scalar::Num("0".into())),
            ])
        );
        assert_eq!(entry("x: 0012").value, Value::str("0012"));
    }

    #[test]
    fn keeps_the_comment_with_its_spacing() {
        let e = entry("by: user                 # user | judge | import");
        assert_eq!(e.value, Value::str("user"));
        assert_eq!(e.comment, "                 # user | judge | import");
        let e = entry("area: [a, b] # parts");
        assert_eq!(e.value, Value::list(["a", "b"]));
        assert_eq!(e.comment, " # parts");
        let e = entry("supersedes: # none yet");
        assert_eq!(e.value, Value::null());
        assert_eq!(e.comment, " # none yet");
        // A `#` inside a word is part of the word.
        assert_eq!(entry("tag: C#").value, Value::str("C#"));
        assert_eq!(parse_entry("# only a comment"), Ok(ParsedLine::Other));
        assert_eq!(parse_entry("   "), Ok(ParsedLine::Other));
    }

    #[test]
    fn reads_quoted_strings_and_odd_spacing() {
        assert_eq!(entry(r#"q: "a # b""#).value, Value::str("a # b"));
        assert_eq!(
            entry(r#"q: "say \"hi\"\n""#).value,
            Value::str("say \"hi\"\n")
        );
        assert_eq!(entry("q: 'it''s'").value, Value::str("it's"));
        assert_eq!(entry(r#"q: "\u00e9\ud83d\ude00""#).value, Value::str("é😀"));
        assert_eq!(entry("q:\t  spaced   ").value, Value::str("spaced"));
        assert_eq!(
            entry(r#"area: [ "a, b" ,c ,  'd' ]"#).value,
            Value::List(vec![s("a, b"), s("c"), s("d")])
        );
        assert_eq!(
            entry("m: {  a:1,b : x  }").value,
            Value::Map(vec![
                ("a".into(), Scalar::Num("1".into())),
                ("b".into(), s("x"))
            ])
        );
        assert_eq!(entry("area: [a, ]").value, Value::list(["a"]));
    }

    #[test]
    fn refuses_bad_lines_with_a_reason() {
        for (line, word) in [
            ("no colon here", "is not `key: value`"),
            ("  indented: x", "starts with a space"),
            ("key:value", "put a space"),
            ("bad key: x", "not a valid key"),
            ("area: [a, b", "no closing `]`"),
            ("area: [a, [b]]", "cannot hold a list"),
            ("m: {a: 1", "no closing `}`"),
            ("m: {a 1}", "needs `: value`"),
            ("m: {a: 1, a: 2}", "two times"),
            (r#"q: "open"#, "no closing"),
            (r#"q: "x" y"#, "unexpected text"),
            (r#"q: "\q""#, "not a known escape"),
            ("area: [a, , b]", "empty item"),
        ] {
            let err = parse_entry(line).unwrap_err();
            assert!(err.contains(word), "{line:?}: {err}");
        }
    }

    #[test]
    fn writes_values_that_read_back_the_same() {
        let values = [
            Value::str("D-0142"),
            Value::str("2026-11-16T14:32:00Z"),
            Value::str(""),
            Value::str(" padded "),
            Value::str("null"),
            Value::str("12"),
            Value::str("a: b"),
            Value::str("x # y"),
            Value::str("[x]"),
            Value::str("line\nbreak \"q\""),
            Value::str("- item"),
            Value::str("ends:"),
            Value::str("C#"),
            Value::null(),
            Value::num(7),
            Value::list(["reports", "a, b", "x]", "12", "null", ""]),
            Value::List(vec![]),
            Value::Map(vec![
                ("used".into(), Scalar::Num("6".into())),
                ("odd key".into(), s("v, w")),
                ("n".into(), Scalar::Null),
            ]),
            Value::Map(vec![]),
        ];
        for value in values {
            let line = format!("k: {}", render_value(&value));
            assert_eq!(entry(&line).value, value, "{line}");
        }
        assert_eq!(render_value(&Value::list(["a", "b"])), "[a, b]");
        assert_eq!(render_value(&Value::str("plain text")), "plain text");
        assert_eq!(render_value(&Value::str("12")), "\"12\"");
    }
}
