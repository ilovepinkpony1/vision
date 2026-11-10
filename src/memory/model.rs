//! The typed values of the memory (section 9.1): ids, the enums of the design, the writer, and
//! the two kinds of file, a decision and a principle.
//!
//! [`Decision::from_doc`] and [`Principle::from_doc`] read a [`Doc`] and give each problem with
//! its line, so that the lint can print all of them at once.

use std::fmt;
use std::str::FromStr;

use anyhow::{bail, Result};
use serde::{Serialize, Serializer};

use super::doc::Doc;
use super::front::{Scalar, Value};
use crate::cli::{MemoryKind, MemoryStatus};
use crate::util::ids::sha256_hex;
use crate::util::time::parse_rfc3339;

/// The folder of the decisions in the memory repo.
pub const DECISIONS: &str = "decisions";
/// The folder of the principles in the memory repo.
pub const PRINCIPLES: &str = "principles";
/// The archive of lost lines at the top of the memory repo.
pub const ARCHIVE: &str = "ARCHIVE.md";
/// The value of `agent` for a principle that each agent loads.
pub const ALL_AGENTS: &str = "all";

/// The id of a decision, such as `D-0142`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DecisionId(u32);

/// The id of a principle, such as `P-007`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrincipleId(u32);

impl DecisionId {
    /// The id with this number. The number starts at 1.
    pub fn new(number: u32) -> Option<DecisionId> {
        (number > 0).then_some(DecisionId(number))
    }

    pub fn number(self) -> u32 {
        self.0
    }

    /// The path in the memory repo, such as `decisions/D-0142.md`.
    pub fn path(self) -> String {
        format!("{DECISIONS}/{self}.md")
    }

    pub fn parse(text: &str) -> Result<DecisionId> {
        match parse_number(text, 'D', 4) {
            Some(n) => Ok(DecisionId(n)),
            None => bail!(bad_id(text)),
        }
    }
}

impl PrincipleId {
    /// The id with this number. The number starts at 1.
    pub fn new(number: u32) -> Option<PrincipleId> {
        (number > 0).then_some(PrincipleId(number))
    }

    pub fn number(self) -> u32 {
        self.0
    }

    /// The path in the memory repo, such as `principles/P-007.md`.
    pub fn path(self) -> String {
        format!("{PRINCIPLES}/{self}.md")
    }

    pub fn parse(text: &str) -> Result<PrincipleId> {
        match parse_number(text, 'P', 3) {
            Some(n) => Ok(PrincipleId(n)),
            None => bail!(bad_id(text)),
        }
    }
}

impl fmt::Display for DecisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "D-{:04}", self.0)
    }
}

impl fmt::Display for PrincipleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "P-{:03}", self.0)
    }
}

/// The id of a decision or of a principle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MemoryId {
    Decision(DecisionId),
    Principle(PrincipleId),
}

impl MemoryId {
    /// Reads `D-0142` or `P-007`. The error shows the two forms.
    pub fn parse(text: &str) -> Result<MemoryId> {
        let text = text.trim();
        if let Some(n) = parse_number(text, 'D', 4) {
            return Ok(MemoryId::Decision(DecisionId(n)));
        }
        if let Some(n) = parse_number(text, 'P', 3) {
            return Ok(MemoryId::Principle(PrincipleId(n)));
        }
        bail!(bad_id(text))
    }

    /// The id of a file path in the memory repo, such as `principles/P-007.md`.
    pub fn from_path(path: &str) -> Option<MemoryId> {
        let (folder, name) = path.rsplit_once('/')?;
        let stem = name.strip_suffix(".md")?;
        let id = MemoryId::parse(stem).ok()?;
        (folder == id.folder()).then_some(id)
    }

    /// The path in the memory repo, such as `decisions/D-0142.md`.
    pub fn path(self) -> String {
        match self {
            MemoryId::Decision(id) => id.path(),
            MemoryId::Principle(id) => id.path(),
        }
    }

    /// `decisions` or `principles`.
    pub fn folder(self) -> &'static str {
        match self {
            MemoryId::Decision(_) => DECISIONS,
            MemoryId::Principle(_) => PRINCIPLES,
        }
    }

    pub fn is_decision(self) -> bool {
        matches!(self, MemoryId::Decision(_))
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryId::Decision(id) => id.fmt(f),
            MemoryId::Principle(id) => id.fmt(f),
        }
    }
}

impl FromStr for MemoryId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<MemoryId> {
        MemoryId::parse(s)
    }
}

impl From<DecisionId> for MemoryId {
    fn from(id: DecisionId) -> MemoryId {
        MemoryId::Decision(id)
    }
}

impl From<PrincipleId> for MemoryId {
    fn from(id: PrincipleId) -> MemoryId {
        MemoryId::Principle(id)
    }
}

macro_rules! serialize_as_text {
    ($($t:ty),*) => {$(
        impl Serialize for $t {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }
    )*};
}

serialize_as_text!(DecisionId, PrincipleId, MemoryId, Writer);

fn bad_id(text: &str) -> String {
    format!(
        "`{text}` is not a memory id. A decision id has the form D-0142, and a principle id has the form P-007"
    )
}

/// The number of `D-0142` with the prefix `D` and at least 4 digits. The text must be the
/// canonical form, so that one file has one id.
fn parse_number(text: &str, prefix: char, width: usize) -> Option<u32> {
    let digits = text.strip_prefix(prefix)?.strip_prefix('-')?;
    if digits.len() < width || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    (n > 0 && format!("{n:0width$}") == digits).then_some(n)
}

macro_rules! text_enum {
    ($(#[$doc:meta])* $name:ident { $($variant:ident => $text:literal),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
        #[serde(rename_all = "lowercase")]
        pub enum $name { $($variant),* }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),* }
            }

            /// Reads the text of a value. Case does not matter.
            pub fn parse(text: &str) -> Option<$name> {
                let text = text.trim();
                $name::ALL.iter().copied().find(|v| v.as_str().eq_ignore_ascii_case(text))
            }

            /// The values joined with ` | `, for error messages.
            pub fn choices() -> String {
                $name::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(" | ")
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum!(
    /// The kind of a question (section 8.1).
    Kind {
        Product => "product",
        Architecture => "architecture",
        Scope => "scope",
        Code => "code",
    }
);

text_enum!(
    /// Where a decision comes from: an answer of a person or an import, or the basis of an
    /// answer of the Judge (section 8.2).
    Basis {
        Answer => "answer",
        Decision => "decision",
        Principle => "principle",
        Inference => "inference",
    }
);

text_enum!(
    /// The status of a file. A decision is `active`, `superseded`, or `retired`. A principle is
    /// `candidate`, `active`, or `retired`.
    Status {
        Active => "active",
        Candidate => "candidate",
        Superseded => "superseded",
        Retired => "retired",
    }
);

text_enum!(
    /// One count of the uses of a principle.
    UseKind {
        Used => "used",
        Agreed => "agreed",
        Corrected => "corrected",
    }
);

impl Status {
    /// The statuses that a decision can have.
    pub const DECISION: &'static [Status] = &[Status::Active, Status::Superseded, Status::Retired];
    /// The statuses that a principle can have.
    pub const PRINCIPLE: &'static [Status] = &[Status::Candidate, Status::Active, Status::Retired];
}

impl From<MemoryKind> for Kind {
    fn from(kind: MemoryKind) -> Kind {
        match kind {
            MemoryKind::Product => Kind::Product,
            MemoryKind::Architecture => Kind::Architecture,
            MemoryKind::Scope => Kind::Scope,
            MemoryKind::Code => Kind::Code,
        }
    }
}

impl From<MemoryStatus> for Status {
    fn from(status: MemoryStatus) -> Status {
        match status {
            MemoryStatus::Active => Status::Active,
            MemoryStatus::Candidate => Status::Candidate,
            MemoryStatus::Superseded => Status::Superseded,
            MemoryStatus::Retired => Status::Retired,
        }
    }
}

/// The writer of a file and the author of its commit: the person, the Judge, the Curator, or
/// an import (section 9.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Writer {
    /// The person, with the value of the configuration key `user`.
    Person(String),
    Judge,
    Curator,
    Import,
}

/// The writer names that are not a person.
pub const RESERVED_WRITERS: [&str; 4] = ["judge", "curator", "import", "vision"];

impl Writer {
    /// The person with this name. The name cannot be one of the other writers.
    pub fn person(name: &str) -> Result<Writer> {
        let name = name.trim();
        if name.is_empty() {
            bail!("the writer name of the person is empty. Set `user` in config.toml, such as user = \"user\"");
        }
        if name
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>'))
        {
            bail!("the writer name `{name}` holds a character that git refuses in a name. Change `user` in config.toml");
        }
        if RESERVED_WRITERS
            .iter()
            .any(|r| r.eq_ignore_ascii_case(name))
        {
            bail!(
                "the writer name of the person cannot be `{name}`, because that name belongs to Vision. Change `user` in config.toml"
            );
        }
        Ok(Writer::Person(name.to_string()))
    }

    /// Reads the value of `by`. `judge`, `curator`, and `import` are those writers. Each other
    /// name is a person.
    pub fn parse(text: &str) -> Option<Writer> {
        let text = text.trim();
        match text {
            "" => None,
            "judge" => Some(Writer::Judge),
            "curator" => Some(Writer::Curator),
            "import" => Some(Writer::Import),
            "vision" => None,
            name => Writer::person(name).ok(),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Writer::Person(name) => name,
            Writer::Judge => "judge",
            Writer::Curator => "curator",
            Writer::Import => "import",
        }
    }

    pub fn is_person(&self) -> bool {
        matches!(self, Writer::Person(_))
    }

    /// The email of the commits of this writer, such as `judge@localhost`.
    pub fn email(&self) -> String {
        let local: String = self
            .name()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        format!("{local}@localhost")
    }
}

impl fmt::Display for Writer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The counts of `uses` of a principle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Uses {
    pub used: u64,
    pub agreed: u64,
    pub corrected: u64,
}

impl Uses {
    pub fn add(&mut self, kind: UseKind, n: u64) {
        let count = match kind {
            UseKind::Used => &mut self.used,
            UseKind::Agreed => &mut self.agreed,
            UseKind::Corrected => &mut self.corrected,
        };
        *count = count.saturating_add(n);
    }

    pub fn to_value(self) -> Value {
        Value::Map(vec![
            ("used".into(), Scalar::Num(self.used.to_string())),
            ("agreed".into(), Scalar::Num(self.agreed.to_string())),
            ("corrected".into(), Scalar::Num(self.corrected.to_string())),
        ])
    }
}

/// One problem of a file, with its path in the memory repo and its line (from 1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct Problem {
    pub path: String,
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.reason)
    }
}

/// An answered question (section 9.1).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Decision {
    pub id: DecisionId,
    /// RFC 3339 UTC, as the file holds it.
    pub at: String,
    pub by: Writer,
    pub basis: Basis,
    pub ticket: Option<String>,
    pub step: Option<String>,
    pub asked_by: Option<String>,
    pub kind: Option<Kind>,
    pub area: Vec<String>,
    pub status: Status,
    /// For an answer of the Judge: the ids that it applied.
    pub sources: Vec<MemoryId>,
    pub supersedes: Option<DecisionId>,
    /// The text of `## Question` without its `Choices:` line.
    pub question: String,
    pub choices: Vec<String>,
    pub answer: String,
    /// The reason: `## Why`. Empty when the person gave none.
    pub why: String,
    pub applies_when: String,
    /// The reason of a retirement: `## Retired`.
    pub retired: Option<String>,
    /// The stable hash of an imported answer.
    pub import_hash: Option<String>,
    /// The path in the memory repo.
    pub path: String,
    /// The first 12 hex digits of the SHA-256 of the file.
    pub version: String,
}

/// A rule of an agent (section 9.1).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Principle {
    pub id: PrincipleId,
    /// The agent that loads it, or `all`.
    pub agent: String,
    pub kind: Option<Kind>,
    pub area: Vec<String>,
    pub status: Status,
    /// The person who accepted or wrote it, or `curator` for a candidate.
    pub by: Writer,
    pub evidence: Vec<DecisionId>,
    pub uses: Uses,
    pub rule: String,
    pub because: String,
    pub not_when: String,
    /// The reason of a retirement: `## Retired`.
    pub retired: Option<String>,
    pub path: String,
    pub version: String,
}

/// A decision or a principle.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MemoryFile {
    Decision(Decision),
    Principle(Principle),
}

impl MemoryFile {
    pub fn id(&self) -> MemoryId {
        match self {
            MemoryFile::Decision(d) => d.id.into(),
            MemoryFile::Principle(p) => p.id.into(),
        }
    }

    pub fn status(&self) -> Status {
        match self {
            MemoryFile::Decision(d) => d.status,
            MemoryFile::Principle(p) => p.status,
        }
    }

    pub fn kind(&self) -> Option<Kind> {
        match self {
            MemoryFile::Decision(d) => d.kind,
            MemoryFile::Principle(p) => p.kind,
        }
    }

    pub fn area(&self) -> &[String] {
        match self {
            MemoryFile::Decision(d) => &d.area,
            MemoryFile::Principle(p) => &p.area,
        }
    }

    pub fn by(&self) -> &Writer {
        match self {
            MemoryFile::Decision(d) => &d.by,
            MemoryFile::Principle(p) => &p.by,
        }
    }

    pub fn path(&self) -> &str {
        match self {
            MemoryFile::Decision(d) => &d.path,
            MemoryFile::Principle(p) => &p.path,
        }
    }

    pub fn version(&self) -> &str {
        match self {
            MemoryFile::Decision(d) => &d.version,
            MemoryFile::Principle(p) => &p.version,
        }
    }

    /// The agent: `asked_by` of a decision, `agent` of a principle.
    pub fn agent(&self) -> Option<&str> {
        match self {
            MemoryFile::Decision(d) => d.asked_by.as_deref(),
            MemoryFile::Principle(p) => Some(&p.agent),
        }
    }

    /// The question of a decision, or the rule of a principle.
    pub fn headline(&self) -> &str {
        match self {
            MemoryFile::Decision(d) => &d.question,
            MemoryFile::Principle(p) => &p.rule,
        }
    }

    /// The texts that a free text search reads: the question, the answer, the reason, and the rule.
    pub fn search_texts(&self) -> Vec<&str> {
        match self {
            MemoryFile::Decision(d) => vec![&d.question, &d.answer, &d.why],
            MemoryFile::Principle(p) => vec![&p.rule, &p.because],
        }
    }

    /// Reads a file of the memory repo. `path` is relative to the repo, such as
    /// `decisions/D-0001.md`. Each problem names its line.
    pub fn parse(path: &str, text: &str) -> Result<MemoryFile, Vec<Problem>> {
        let problem = |line: usize, reason: String| Problem {
            path: path.to_string(),
            line,
            reason,
        };
        let doc = Doc::parse(text).map_err(|e| vec![problem(e.line, e.message)])?;
        let version = version_of(text);
        if path.starts_with(&format!("{DECISIONS}/")) {
            Decision::from_doc(path, &doc, version).map(MemoryFile::Decision)
        } else if path.starts_with(&format!("{PRINCIPLES}/")) {
            Principle::from_doc(path, &doc, version).map(MemoryFile::Principle)
        } else {
            Err(vec![problem(
                1,
                format!("a memory file belongs in {DECISIONS}/ or {PRINCIPLES}/"),
            )])
        }
    }
}

/// The version of a file: the first 12 hex digits of the SHA-256 of its bytes.
pub fn version_of(text: &str) -> String {
    sha256_hex(text.as_bytes())[..12].to_string()
}

/// Reads typed keys and sections of a doc and collects the problems.
struct Reader<'a> {
    path: &'a str,
    doc: &'a Doc,
    problems: Vec<Problem>,
}

impl<'a> Reader<'a> {
    fn new(path: &'a str, doc: &'a Doc) -> Reader<'a> {
        Reader {
            path,
            doc,
            problems: Vec::new(),
        }
    }

    fn line(&self, key: &str) -> usize {
        self.doc.key_line(key).unwrap_or(1)
    }

    fn problem(&mut self, line: usize, reason: String) {
        self.problems.push(Problem {
            path: self.path.to_string(),
            line,
            reason,
        });
    }

    /// A required key. A missing key is a problem.
    fn required(&mut self, key: &str) -> Option<&'a Value> {
        let value = self.doc.get(key);
        if value.is_none() {
            self.problem(1, format!("the required key `{key}` is missing"));
        }
        value
    }

    /// A required key with a text value.
    fn text(&mut self, key: &str) -> Option<&'a str> {
        let value = self.required(key)?;
        match value.as_text() {
            Some(t) if !t.trim().is_empty() => Some(t),
            _ => {
                let line = self.line(key);
                self.problem(
                    line,
                    format!("`{key}` must be a text, not {}", value.type_name()),
                );
                None
            }
        }
    }

    /// An optional key with a text value or `null`.
    fn opt_text(&mut self, key: &str) -> Option<String> {
        let value = self.doc.get(key)?;
        if value.is_null() {
            return None;
        }
        match value.as_text() {
            Some(t) => Some(t.to_string()),
            None => {
                let line = self.line(key);
                self.problem(
                    line,
                    format!("`{key}` must be a text or null, not {}", value.type_name()),
                );
                None
            }
        }
    }

    /// A required key with one of the values of an enum.
    fn choice<T: Copy + PartialEq>(
        &mut self,
        key: &str,
        parse: fn(&str) -> Option<T>,
        allowed: &[T],
        names: &str,
    ) -> Option<T> {
        let value = self.required(key)?;
        let line = self.line(key);
        match value.as_text().and_then(parse) {
            Some(v) if allowed.contains(&v) => Some(v),
            _ => {
                self.problem(
                    line,
                    format!("`{key}` is {value}, but it must be one of: {names}"),
                );
                None
            }
        }
    }

    /// A key that must be present, with one of the kinds or `null`.
    fn kind(&mut self) -> Option<Kind> {
        let value = self.required("kind")?;
        if value.is_null() {
            return None;
        }
        let line = self.line("kind");
        match value.as_text().and_then(Kind::parse) {
            Some(kind) => Some(kind),
            None => {
                self.problem(
                    line,
                    format!(
                        "`kind` is {value}, but it must be one of: {} | null",
                        Kind::choices()
                    ),
                );
                None
            }
        }
    }

    /// An optional flat list of texts. A missing key is an empty list.
    fn list(&mut self, key: &str) -> Vec<String> {
        let Some(value) = self.doc.get(key) else {
            return Vec::new();
        };
        if value.is_null() {
            return Vec::new();
        }
        let line = self.line(key);
        let Some(items) = value.as_list() else {
            self.problem(
                line,
                format!(
                    "`{key}` must be a list such as [a, b], not {}",
                    value.type_name()
                ),
            );
            return Vec::new();
        };
        let mut out = Vec::new();
        for item in items {
            match item.as_text() {
                Some(t) if !t.trim().is_empty() => out.push(t.trim().to_string()),
                _ => self.problem(line, format!("`{key}` has an empty or null item")),
            }
        }
        out
    }

    fn ids<T>(&mut self, key: &str, parse: fn(&str) -> Result<T>, what: &str) -> Vec<T> {
        let line = self.line(key);
        let mut out = Vec::new();
        for text in self.list(key) {
            match parse(&text) {
                Ok(id) => out.push(id),
                Err(_) => {
                    self.problem(line, format!("`{key}` names `{text}`, which is not {what}"))
                }
            }
        }
        out
    }

    fn id<T>(&mut self, parse: fn(&str) -> Result<T>, expected: &str) -> Option<T> {
        let text = self.text("id")?;
        let line = self.line("id");
        let stem = self
            .path
            .rsplit('/')
            .next()
            .and_then(|n| n.strip_suffix(".md"))
            .unwrap_or("");
        let id = match parse(text) {
            Ok(id) => id,
            Err(_) => {
                self.problem(line, format!("`id` is `{text}`, which is not {expected}"));
                return None;
            }
        };
        if text != stem {
            self.problem(
                line,
                format!(
                    "`id` is {text}, but the file name says {stem}. Rename the file or fix the id"
                ),
            );
        }
        Some(id)
    }

    fn writer(&mut self, allowed: fn(&Writer) -> bool, names: &str) -> Option<Writer> {
        let value = self.required("by")?;
        let line = self.line("by");
        match value.as_text().and_then(Writer::parse) {
            Some(w) if allowed(&w) => Some(w),
            _ => {
                self.problem(line, format!("`by` is {value}, but it must be {names}"));
                None
            }
        }
    }

    /// The text of a section. A required section must exist and must not be empty.
    fn section(&mut self, name: &str, required: bool) -> String {
        match self.doc.section(name) {
            Some(text) if !text.is_empty() || !required => text,
            Some(_) => {
                let line = self.doc.section_line(name).unwrap_or(1);
                self.problem(line, format!("the section `## {name}` is empty"));
                String::new()
            }
            None if required => {
                self.problem(1, format!("the required section `## {name}` is missing"));
                String::new()
            }
            None => String::new(),
        }
    }

    fn repeated_sections(&mut self) {
        for name in self.doc.repeated_sections() {
            let line = self.doc.section_line(&name).unwrap_or(1);
            self.problem(
                line,
                format!("the section `## {name}` is here more than once"),
            );
        }
    }

    fn retired(&mut self, status: Option<Status>) -> Option<String> {
        let text = self.doc.section(RETIRED_SECTION).filter(|t| !t.is_empty());
        if status == Some(Status::Retired) && text.is_none() {
            self.problem(
                self.line("status"),
                format!("a retired file needs the reason in a section `## {RETIRED_SECTION}`"),
            );
        }
        text
    }
}

/// The section that keeps the reason of a retirement.
pub const RETIRED_SECTION: &str = "Retired";

impl Decision {
    pub fn from_doc(path: &str, doc: &Doc, version: String) -> Result<Decision, Vec<Problem>> {
        let mut r = Reader::new(path, doc);
        let id = r.id(DecisionId::parse, "a decision id such as D-0142");
        let at = r.text("at").map(str::to_string);
        if let Some(text) = &at {
            if parse_rfc3339(text).is_err() {
                let line = r.line("at");
                r.problem(line, format!("`at` is `{text}`, which is not an RFC 3339 time such as 2026-11-16T14:32:00Z"));
            }
        }
        let by = r.writer(
            |w| !matches!(w, Writer::Curator),
            "the person, judge, or import",
        );
        let basis = r.choice("basis", Basis::parse, Basis::ALL, &Basis::choices());
        let ticket = r.opt_text("ticket");
        let step = r.opt_text("step");
        let asked_by = r.opt_text("asked_by");
        let kind = r.kind();
        let area = r.list("area");
        let status = r.choice(
            "status",
            Status::parse,
            Status::DECISION,
            "active | superseded | retired",
        );
        let sources = r.ids("sources", MemoryId::parse, "a memory id");
        let supersedes = match r.opt_text("supersedes") {
            Some(text) => match DecisionId::parse(&text) {
                Ok(id) => Some(id),
                Err(_) => {
                    let line = r.line("supersedes");
                    r.problem(
                        line,
                        format!("`supersedes` is `{text}`, which is not a decision id"),
                    );
                    None
                }
            },
            None => None,
        };
        let import_hash = r.opt_text("import_hash");
        let question_text = r.section("Question", true);
        let answer = r.section("Answer", true);
        let why = r.section("Why", false);
        let applies_when = r.section("Applies when", false);
        r.repeated_sections();
        let retired = r.retired(status);
        let (question, choices) = split_choices(&question_text);
        if !r.problems.is_empty() {
            return Err(r.problems);
        }
        match (id, at, by, basis, status) {
            (Some(id), Some(at), Some(by), Some(basis), Some(status)) => Ok(Decision {
                id,
                at,
                by,
                basis,
                ticket,
                step,
                asked_by,
                kind,
                area,
                status,
                sources,
                supersedes,
                question,
                choices,
                answer,
                why,
                applies_when,
                retired,
                import_hash,
                path: path.to_string(),
                version,
            }),
            _ => Err(vec![Problem {
                path: path.to_string(),
                line: 1,
                reason: "the file is not a valid decision".into(),
            }]),
        }
    }
}

impl Principle {
    pub fn from_doc(path: &str, doc: &Doc, version: String) -> Result<Principle, Vec<Problem>> {
        let mut r = Reader::new(path, doc);
        let id = r.id(PrincipleId::parse, "a principle id such as P-007");
        let agent = r.text("agent").map(str::to_string);
        let kind = r.kind();
        let area = r.list("area");
        let status = r.choice(
            "status",
            Status::parse,
            Status::PRINCIPLE,
            "candidate | active | retired",
        );
        let by = r.writer(
            |w| matches!(w, Writer::Person(_) | Writer::Curator),
            "the person who wrote or accepted it, or curator",
        );
        let evidence = r.ids(
            "evidence",
            DecisionId::parse,
            "a decision id such as D-0142",
        );
        let uses = match doc.get("uses") {
            None => Uses::default(),
            Some(value) => {
                let line = r.line("uses");
                match parse_uses(value) {
                    Ok(uses) => uses,
                    Err(reason) => {
                        r.problem(line, reason);
                        Uses::default()
                    }
                }
            }
        };
        let rule = r.section("Rule", true);
        let because = r.section("Because", false);
        let not_when = r.section("Not when", false);
        r.repeated_sections();
        let retired = r.retired(status);
        if !r.problems.is_empty() {
            return Err(r.problems);
        }
        match (id, agent, status, by) {
            (Some(id), Some(agent), Some(status), Some(by)) => Ok(Principle {
                id,
                agent,
                kind,
                area,
                status,
                by,
                evidence,
                uses,
                rule,
                because,
                not_when,
                retired,
                path: path.to_string(),
                version,
            }),
            _ => Err(vec![Problem {
                path: path.to_string(),
                line: 1,
                reason: "the file is not a valid principle".into(),
            }]),
        }
    }
}

fn parse_uses(value: &Value) -> Result<Uses, String> {
    let shape = "`uses` must be a map such as {used: 6, agreed: 5, corrected: 0}";
    let items = value.as_map().ok_or_else(|| shape.to_string())?;
    let mut uses = Uses::default();
    for (key, count) in items {
        let n: Option<u64> = match count {
            Scalar::Num(text) => text.parse().ok(),
            _ => None,
        };
        let n = n.ok_or_else(|| format!("`uses.{key}` must be a whole number"))?;
        match UseKind::parse(key) {
            Some(kind) => uses.add(kind, n),
            None => {
                return Err(format!(
                    "`uses` has the key `{key}`, but its keys are used, agreed, and corrected"
                ))
            }
        }
    }
    Ok(uses)
}

/// Splits the `Choices: a | b` line from the end of a question.
fn split_choices(text: &str) -> (String, Vec<String>) {
    let mut lines: Vec<&str> = text.lines().collect();
    if let Some(last) = lines.last() {
        if let Some(rest) = last.trim().strip_prefix("Choices:") {
            let choices = rest
                .split('|')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_string)
                .collect();
            lines.pop();
            return (lines.join("\n").trim_end().to_string(), choices);
        }
    }
    (text.to_string(), Vec::new())
}

/// The text of `## Question`: the question, then a `Choices:` line when it has choices.
pub fn question_text(question: &str, choices: &[String]) -> String {
    let question = question.trim();
    if choices.is_empty() {
        return question.to_string();
    }
    let choices: Vec<String> = choices
        .iter()
        .map(|c| c.replace(['\n', '|'], " "))
        .collect();
    format!("{question}\nChoices: {}", choices.join(" | "))
}

/// The input of a new decision. The store gives the id, the status, and `supersedes`.
#[derive(Clone, Debug, PartialEq)]
pub struct NewDecision {
    /// The time of the answer. `None` means now.
    pub at: Option<time::OffsetDateTime>,
    /// The person, the Judge, or an import. The writer is also the author of the commit.
    pub by: Writer,
    /// `answer` for a person or an import. `decision`, `principle`, or `inference` for the Judge.
    pub basis: Basis,
    pub ticket: Option<String>,
    pub step: Option<String>,
    /// The agent that asked the card.
    pub asked_by: Option<String>,
    pub kind: Option<Kind>,
    /// The area of the card: one to three nouns, such as `[reports, exports]`.
    pub area: Vec<String>,
    /// For an answer of the Judge: the ids of the files that it applied.
    pub sources: Vec<MemoryId>,
    pub question: String,
    pub choices: Vec<String>,
    pub answer: String,
    /// The reason. Empty when the person gave none.
    pub why: String,
    /// Where the decision applies. Empty when nobody said.
    pub applies_when: String,
}

impl NewDecision {
    /// An answer of the person to a question, with no ticket and no reason yet.
    pub fn answer(by: Writer, question: &str, answer: &str) -> NewDecision {
        NewDecision {
            at: None,
            by,
            basis: Basis::Answer,
            ticket: None,
            step: None,
            asked_by: None,
            kind: None,
            area: Vec::new(),
            sources: Vec::new(),
            question: question.to_string(),
            choices: Vec::new(),
            answer: answer.to_string(),
            why: String::new(),
            applies_when: String::new(),
        }
    }

    /// The text of the decision file with this id.
    pub fn render(
        &self,
        id: DecisionId,
        supersedes: Option<DecisionId>,
        import_hash: Option<&str>,
    ) -> String {
        let at = crate::util::time::rfc3339(self.at.unwrap_or_else(crate::util::time::now));
        let mut keys = vec![
            ("id", Value::str(id.to_string())),
            ("at", Value::str(at)),
            ("by", Value::str(self.by.name())),
            ("basis", Value::str(self.basis.as_str())),
            ("ticket", Value::opt_str(self.ticket.as_deref())),
            ("step", Value::opt_str(self.step.as_deref())),
            ("asked_by", Value::opt_str(self.asked_by.as_deref())),
            ("kind", Value::opt_str(self.kind.map(Kind::as_str))),
            ("area", Value::list(clean_area(&self.area))),
            ("status", Value::str(Status::Active.as_str())),
            (
                "sources",
                Value::list(self.sources.iter().map(|s| s.to_string())),
            ),
            (
                "supersedes",
                Value::opt_str(supersedes.map(|s| s.to_string()).as_deref()),
            ),
        ];
        if let Some(hash) = import_hash {
            keys.push(("import_hash", Value::str(hash)));
        }
        let question = question_text(&self.question, &self.choices);
        Doc::new(
            &keys,
            &[
                ("Question", &question),
                ("Answer", self.answer.trim()),
                ("Why", self.why.trim()),
                ("Applies when", self.applies_when.trim()),
            ],
        )
        .to_text()
    }
}

/// The input of a new principle. The store gives the id and the status: `active` when a person
/// writes it, `candidate` when the Curator does.
#[derive(Clone, Debug, PartialEq)]
pub struct NewPrinciple {
    /// The agent that loads it, or `all`.
    pub agent: String,
    pub kind: Option<Kind>,
    pub area: Vec<String>,
    /// The person or the Curator.
    pub by: Writer,
    pub evidence: Vec<DecisionId>,
    pub rule: String,
    pub because: String,
    pub not_when: String,
}

impl NewPrinciple {
    /// The status that the store gives it.
    pub fn status(&self) -> Status {
        if self.by.is_person() {
            Status::Active
        } else {
            Status::Candidate
        }
    }

    /// The text of the principle file with this id.
    pub fn render(&self, id: PrincipleId) -> String {
        Doc::new(
            &[
                ("id", Value::str(id.to_string())),
                ("agent", Value::str(self.agent.trim())),
                ("kind", Value::opt_str(self.kind.map(Kind::as_str))),
                ("area", Value::list(clean_area(&self.area))),
                ("status", Value::str(self.status().as_str())),
                ("by", Value::str(self.by.name())),
                (
                    "evidence",
                    Value::list(self.evidence.iter().map(|e| e.to_string())),
                ),
                ("uses", Uses::default().to_value()),
            ],
            &[
                ("Rule", self.rule.trim()),
                ("Because", self.because.trim()),
                ("Not when", self.not_when.trim()),
            ],
        )
        .to_text()
    }
}

/// The area nouns without blanks and repeats, in their order.
pub fn clean_area(area: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in area {
        let item = item.trim();
        if !item.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(item)) {
            out.push(item.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_one_canonical_form() {
        assert_eq!(DecisionId::parse("D-0142").unwrap().number(), 142);
        assert_eq!(DecisionId::new(7).unwrap().to_string(), "D-0007");
        assert_eq!(DecisionId::new(12345).unwrap().to_string(), "D-12345");
        assert_eq!(DecisionId::parse("D-12345").unwrap().number(), 12345);
        assert_eq!(PrincipleId::new(7).unwrap().to_string(), "P-007");
        assert_eq!(PrincipleId::new(1000).unwrap().to_string(), "P-1000");
        for bad in [
            "D-1", "D-00001", "D-0000", "d-0001", "D0001", "D-00a1", "P-07", "P-0007", "",
        ] {
            assert!(MemoryId::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            MemoryId::parse("P-007").unwrap(),
            MemoryId::Principle(PrincipleId(7))
        );
        assert_eq!(
            MemoryId::from_path("decisions/D-0003.md"),
            Some(MemoryId::Decision(DecisionId(3)))
        );
        assert_eq!(MemoryId::from_path("principles/D-0003.md"), None);
        assert_eq!(MemoryId::from_path("decisions/notes.md"), None);
        assert!(DecisionId::new(0).is_none());
    }

    #[test]
    fn writers_keep_the_reserved_names() {
        assert_eq!(Writer::parse("judge"), Some(Writer::Judge));
        assert_eq!(Writer::parse("user"), Some(Writer::Person("user".into())));
        assert_eq!(Writer::parse("vision"), None);
        assert!(Writer::person("Judge").is_err());
        assert!(Writer::person("").is_err());
        assert!(Writer::person("a <b>").is_err());
        assert_eq!(
            Writer::person("Ann Lee").unwrap().email(),
            "ann-lee@localhost"
        );
        assert_eq!(Writer::Import.email(), "import@localhost");
    }

    fn decision_text() -> String {
        let mut new = NewDecision::answer(
            Writer::Person("user".into()),
            "Must the Export button make a CSV file or an XLSX file?",
            "CSV",
        );
        new.at = Some(time::macros::datetime!(2026-11-16 14:32:00 UTC));
        new.ticket = Some("DEMO-12".into());
        new.kind = Some(Kind::Product);
        new.area = vec!["reports".into(), "exports".into(), "Reports".into()];
        new.choices = vec!["CSV".into(), "XLSX".into()];
        new.why = "The finance team imports CSV.".into();
        new.render(DecisionId(142), None, None)
    }

    #[test]
    fn a_new_decision_reads_back() {
        let text = decision_text();
        assert!(text.starts_with("---\nid: D-0142\nat: 2026-11-16T14:32:00Z\nby: user\nbasis: answer\nticket: DEMO-12\nstep: null\n"), "{text}");
        assert!(text.contains("area: [reports, exports]\nstatus: active\nsources: []\nsupersedes: null\n---\n## Question\n"), "{text}");
        let MemoryFile::Decision(d) = MemoryFile::parse("decisions/D-0142.md", &text).unwrap()
        else {
            panic!("not a decision")
        };
        assert_eq!(
            d.question,
            "Must the Export button make a CSV file or an XLSX file?"
        );
        assert_eq!(d.choices, ["CSV", "XLSX"]);
        assert_eq!(d.answer, "CSV");
        assert_eq!(d.why, "The finance team imports CSV.");
        assert_eq!(d.applies_when, "");
        assert_eq!(d.area, ["reports", "exports"]);
        assert_eq!(d.kind, Some(Kind::Product));
        assert_eq!(d.version, version_of(&text));
    }

    #[test]
    fn a_new_principle_reads_back() {
        let new = NewPrinciple {
            agent: "judge".into(),
            kind: None,
            area: vec![],
            by: Writer::Person("user".into()),
            evidence: vec![DecisionId(1)],
            rule: "When X, make sure that Y.".into(),
            because: "Z.".into(),
            not_when: String::new(),
        };
        let text = new.render(PrincipleId(3));
        let MemoryFile::Principle(p) = MemoryFile::parse("principles/P-003.md", &text).unwrap()
        else {
            panic!("not a principle")
        };
        assert_eq!(p.status, Status::Active);
        assert_eq!(p.uses, Uses::default());
        assert_eq!(p.evidence, [DecisionId(1)]);
        assert_eq!(p.kind, None);
        assert!(
            text.contains("uses: {used: 0, agreed: 0, corrected: 0}"),
            "{text}"
        );
    }

    #[test]
    fn each_bad_key_is_one_problem_with_its_line() {
        let text = "---\nid: D-0009\nat: yesterday\nby: curator\nbasis: guess\nkind: food\nstatus: active\narea: reports\nsources: [X-1]\n---\n## Question\n\n## Why\nW\n";
        let problems = MemoryFile::parse("decisions/D-0001.md", text).unwrap_err();
        let lines: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
        let expect = [
            "decisions/D-0001.md:2: `id` is D-0009, but the file name says D-0001",
            "decisions/D-0001.md:3: `at` is `yesterday`",
            "decisions/D-0001.md:4: `by` is curator",
            "decisions/D-0001.md:5: `basis` is guess",
            "decisions/D-0001.md:6: `kind` is food",
            "decisions/D-0001.md:8: `area` must be a list",
            "decisions/D-0001.md:9: `sources` names `X-1`",
            "decisions/D-0001.md:11: the section `## Question` is empty",
            "decisions/D-0001.md:1: the required section `## Answer` is missing",
        ];
        for want in expect {
            assert!(
                lines.iter().any(|l| l.starts_with(want)),
                "missing {want:?} in {lines:#?}"
            );
        }
        assert_eq!(lines.len(), expect.len(), "{lines:#?}");
    }

    #[test]
    fn a_retired_file_needs_its_reason() {
        let text =
            "---\nid: P-001\nagent: all\nkind: null\nstatus: retired\nby: user\n---\n## Rule\nR\n";
        let problems = MemoryFile::parse("principles/P-001.md", text).unwrap_err();
        assert!(problems[0].reason.contains("## Retired"), "{problems:?}");
        let ok = format!("{text}\n## Retired\nNo longer true.\n");
        let MemoryFile::Principle(p) = MemoryFile::parse("principles/P-001.md", &ok).unwrap()
        else {
            panic!()
        };
        assert_eq!(p.retired.as_deref(), Some("No longer true."));
    }

    #[test]
    fn uses_must_be_counts() {
        let base = "---\nid: P-001\nagent: all\nkind: code\nstatus: active\nby: user\n";
        let ok = format!("{base}uses: {{used: 6, agreed: 5, corrected: 0}}\n---\n## Rule\nR\n");
        let MemoryFile::Principle(p) = MemoryFile::parse("principles/P-001.md", &ok).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            p.uses,
            Uses {
                used: 6,
                agreed: 5,
                corrected: 0
            }
        );
        for bad in ["uses: [1]", "uses: {used: x}", "uses: {seen: 1}"] {
            let text = format!("{base}{bad}\n---\n## Rule\nR\n");
            let problems = MemoryFile::parse("principles/P-001.md", &text).unwrap_err();
            assert_eq!(problems[0].line, 7, "{bad}: {problems:?}");
        }
    }
}
