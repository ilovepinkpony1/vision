//! `when` of a step or a team member (section 11.2): the kind of a ticket and the width of its
//! plan decide whether the step runs. The kind names are ORed, `wide` and `narrow` are ORed,
//! and the two groups are ANDed.

use super::model::{Width, NARROW, WIDE};
use crate::adapters::jira_github::jobs::Glob;

/// The files that the section `## Files` of a document lists: one path on each line, as
/// `- path` or a bare path. A line `none`, a heading, and an empty line list nothing. Text
/// after the path on the same line, such as `- src/a.rs (new)`, does not count.
pub fn listed_files(text: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            inside = heading.trim().eq_ignore_ascii_case("files");
            continue;
        }
        if trimmed.starts_with("# ") {
            inside = false;
            continue;
        }
        if !inside || trimmed.is_empty() {
            continue;
        }
        let item = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
            .unwrap_or(trimmed)
            .trim();
        let Some(first) = item.split_whitespace().next() else {
            continue;
        };
        let path = first
            .trim_matches('`')
            .trim_end_matches([':', ',', ';'])
            .trim_matches('`');
        if path.is_empty() || path.eq_ignore_ascii_case("none") {
            continue;
        }
        if !files.iter().any(|f| f == path) {
            files.push(path.to_string());
        }
    }
    files
}

/// What [`Width::judge`] found in a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidthVerdict {
    pub wide: bool,
    pub files: Vec<String>,
    /// Why, for people: such as `3 files, and 3 or more is wide`.
    pub why: String,
}

impl WidthVerdict {
    /// `wide` or `narrow`.
    pub fn word(&self) -> &'static str {
        if self.wide {
            WIDE
        } else {
            NARROW
        }
    }
}

impl Width {
    /// Wide or narrow, from the text of the document of `[width]`.
    pub fn judge(&self, text: &str) -> WidthVerdict {
        let files = listed_files(text);
        let globs: Vec<(String, Glob)> = self
            .paths
            .iter()
            .map(|p| (p.clone(), Glob::new(p.trim())))
            .collect();
        let matched = files.iter().find_map(|file| {
            globs
                .iter()
                .find(|(_, glob)| glob.matches(file))
                .map(|(pattern, _)| (file.clone(), pattern.clone()))
        });
        let count = files.len();
        let (wide, why) = match matched {
            Some((file, pattern)) => (true, format!("{file} matches {pattern}")),
            None if count >= self.files => (
                true,
                format!("{count} files, and {} or more is wide", self.files),
            ),
            None => (
                false,
                format!(
                    "{count} file(s), fewer than {}, and none matches the paths of [width]",
                    self.files
                ),
            ),
        };
        WidthVerdict { wide, files, why }
    }
}

/// Whether a `when` holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holds {
    Yes,
    /// It does not hold: why, for the event `skipped`.
    No(String),
    /// A width word needs the document of `[width]`, which does not exist yet.
    NoDoc(String),
}

/// Whether `words` hold for a ticket of the kind `kind` and the width that `width` reads. The
/// closure gives `None` when the document of `[width]` does not exist. It runs only when a
/// width word needs it.
pub fn holds(
    words: &[String],
    kind: Option<&str>,
    width: &dyn Fn() -> Option<WidthVerdict>,
    doc: &str,
) -> Holds {
    if words.is_empty() {
        return Holds::Yes;
    }
    let kinds: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| *w != WIDE && *w != NARROW)
        .collect();
    let widths: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| *w == WIDE || *w == NARROW)
        .collect();
    if !kinds.is_empty() {
        match kind {
            Some(kind) if kinds.contains(&kind) => {}
            Some(kind) => {
                return Holds::No(format!(
                    "the ticket is a {kind}, and the step runs for {}",
                    kinds.join(" or ")
                ))
            }
            None => {
                return Holds::No(format!(
                    "the ticket has no kind, and the step runs for {}",
                    kinds.join(" or ")
                ))
            }
        }
    }
    if !widths.is_empty() {
        let Some(verdict) = width() else {
            return Holds::NoDoc(format!(
                "the step runs for a {} plan, but {doc} does not exist yet, so nothing says how wide the plan is. Fix the order of the steps in adapter.toml, then run vision retry",
                widths.join(" or ")
            ));
        };
        if !widths.contains(&verdict.word()) {
            return Holds::No(format!(
                "the plan is {} ({}), and the step runs for a {} plan",
                verdict.word(),
                verdict.why,
                widths.join(" or ")
            ));
        }
    }
    Holds::Yes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width() -> Width {
        Width {
            doc: "plan.md".into(),
            files: 3,
            paths: vec!["db/**".into()],
        }
    }

    #[test]
    fn the_files_section_lists_one_path_on_each_line() {
        let text = "# Plan\n\n## Plan\n\n- not a file\n\n## Files\n- src/a.rs\n* `src/b.rs` (new)\nsrc/c.rs: the parser\n- src/a.rs\nnone\n\n## Later\n- x.rs\n";
        assert_eq!(listed_files(text), ["src/a.rs", "src/b.rs", "src/c.rs"]);
        assert!(listed_files("## Files\nnone\n").is_empty());
        assert!(listed_files("no section").is_empty());
    }

    #[test]
    fn a_plan_is_wide_by_its_count_or_by_a_path() {
        let w = width();
        let narrow = w.judge("## Files\n- a.rs\n- b.rs\n");
        assert!(!narrow.wide, "{narrow:?}");
        assert_eq!(narrow.word(), "narrow");
        let wide = w.judge("## Files\n- a.rs\n- b.rs\n- c.rs\n");
        assert!(wide.wide);
        assert!(wide.why.contains("3 files"), "{}", wide.why);
        let db = w.judge("## Files\n- db/migrate/1_add.rb\n");
        assert!(db.wide);
        assert!(db.why.contains("matches db/**"), "{}", db.why);
    }

    #[test]
    fn kinds_are_ored_widths_are_ored_and_the_groups_are_anded() {
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        let narrow = || {
            Some(WidthVerdict {
                wide: false,
                files: vec![],
                why: "0 file(s)".into(),
            })
        };
        let none = || None;
        assert_eq!(holds(&[], None, &none, "plan.md"), Holds::Yes);
        assert_eq!(
            holds(&words(&["bug", "chore"]), Some("chore"), &none, "plan.md"),
            Holds::Yes
        );
        assert!(matches!(
            holds(&words(&["bug"]), Some("feature"), &none, "plan.md"),
            Holds::No(why) if why.contains("the ticket is a feature")
        ));
        assert_eq!(
            holds(&words(&["narrow", "wide"]), Some("bug"), &narrow, "plan.md"),
            Holds::Yes
        );
        assert!(matches!(
            holds(&words(&["bug", "wide"]), Some("bug"), &narrow, "plan.md"),
            Holds::No(why) if why.contains("the plan is narrow")
        ));
        assert!(matches!(
            holds(&words(&["wide"]), Some("bug"), &none, "plan.md"),
            Holds::NoDoc(why) if why.contains("plan.md does not exist yet")
        ));
        // A kind that does not hold wins before the width needs its document.
        assert!(matches!(
            holds(&words(&["feature", "wide"]), Some("bug"), &none, "plan.md"),
            Holds::No(_)
        ));
    }
}
