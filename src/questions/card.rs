//! The card (section 8.1), its test in code, and the value of an answer.
//!
//! Claude Code tests the result of a work turn against `agents/turn.schema.json`, but a result
//! can be hand-made or come from a changed schema, so [`check`] tests each card again: a
//! question text, two or more choices that each have a label, and a valid kind.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::memory::Kind;

/// One choice of a card.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Choice {
    pub label: String,
    pub means: String,
}

/// One question of an agent, as the result object of its turn holds it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Card {
    pub text: String,
    /// Why the agent asks.
    pub context: String,
    pub example: String,
    pub options: Vec<Choice>,
    /// The index of the recommended choice, from 0 as in the output of the Judge.
    pub recommended: Option<i64>,
    /// The reason for the recommendation.
    pub why: String,
    pub evidence: String,
    /// `product`, `architecture`, `scope`, or `code`.
    pub kind: String,
    pub area: Vec<String>,
    pub topics: Vec<String>,
}

/// The letters of the choices in `vision needs`. `q`, `s`, and `t` are keys of the view, so no
/// choice gets them.
pub const LETTERS: &str = "abcdefghijklmnopruvwxyz";

/// The letter of the choice with this index, or `None` past the last letter.
pub fn letter(index: usize) -> Option<char> {
    LETTERS.chars().nth(index)
}

/// The index of the choice with this letter.
pub fn index_of_letter(c: char) -> Option<usize> {
    LETTERS.chars().position(|l| l == c)
}

impl Card {
    /// The labels of the choices.
    pub fn labels(&self) -> Vec<String> {
        self.options
            .iter()
            .map(|o| o.label.trim().to_string())
            .collect()
    }

    /// The kind of the card for a decision file.
    pub fn memory_kind(&self) -> Option<Kind> {
        Kind::parse(&self.kind)
    }

    /// The recommended choice, when it names one of the choices.
    pub fn recommended_index(&self) -> Option<usize> {
        self.recommended
            .and_then(|i| usize::try_from(i).ok())
            .filter(|i| *i < self.options.len())
    }

    /// The index of the choice whose label is this text, after trimming and case folding.
    pub fn choice_named(&self, text: &str) -> Option<usize> {
        let want = fold(text);
        if want.is_empty() {
            return None;
        }
        self.options.iter().position(|o| fold(&o.label) == want)
    }
}

/// The text for a comparison: trimmed, with folded case and single spaces.
pub fn fold(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Reads one card of a result object and tests it. The error lists each problem.
pub fn check(value: &Value) -> Result<Card, Vec<String>> {
    if !value.is_object() {
        return Err(vec!["the card is not a JSON object".into()]);
    }
    let card: Card = match serde_json::from_value(value.clone()) {
        Ok(card) => card,
        Err(e) => return Err(vec![format!("the card does not fit the schema: {e}")]),
    };
    let mut problems = Vec::new();
    if card.text.trim().is_empty() {
        problems.push("the card has no question text".to_string());
    }
    if card.options.len() < 2 {
        problems.push(format!(
            "the card has {} choice(s), but it needs two or more",
            card.options.len()
        ));
    }
    if let Some(n) = card.options.iter().position(|o| o.label.trim().is_empty()) {
        problems.push(format!("the choice {} has no label", n + 1));
    }
    if card.memory_kind().is_none() {
        problems.push(format!(
            "the kind `{}` is not one of {}",
            card.kind,
            Kind::choices()
        ));
    }
    if problems.is_empty() {
        Ok(card)
    } else {
        Err(problems)
    }
}

/// An answer to a card: one of its choices, or a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The index of a choice, from 0.
    Choice(usize),
    Text(String),
}

impl Reply {
    /// The text of the answer: the label of the choice, or the text.
    pub fn text(&self, card: &Card) -> String {
        match self {
            Reply::Choice(i) => card
                .options
                .get(*i)
                .map(|o| o.label.trim().to_string())
                .unwrap_or_default(),
            Reply::Text(text) => text.trim().to_string(),
        }
    }

    /// The index of the choice of the answer: its own, or the choice whose label it names.
    pub fn choice(&self, card: &Card) -> Option<usize> {
        match self {
            Reply::Choice(i) => Some(*i),
            Reply::Text(text) => card.choice_named(text),
        }
    }

    /// True when two answers are the same: the same choice, or the same text after trimming
    /// and case folding.
    pub fn same(&self, other: &Reply, card: &Card) -> bool {
        match (self.choice(card), other.choice(card)) {
            (Some(a), Some(b)) => a == b,
            _ => fold(&self.text(card)) == fold(&other.text(card)),
        }
    }
}

/// The final answer of a card as the row keeps it in `answer`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Answer {
    /// The index of the choice, from 0, or `None` for an answer in words.
    pub choice: Option<usize>,
    /// The label of the choice, or the words.
    pub text: String,
    /// `judge`, or the writer name of the person.
    pub by: String,
    pub why: String,
    /// Where the decision applies.
    pub when: String,
}

impl Answer {
    /// The answer of `by`. Words that name a choice become that choice.
    pub fn new(reply: &Reply, card: &Card, by: &str) -> Answer {
        let choice = reply.choice(card);
        let text = match choice {
            Some(i) => Reply::Choice(i).text(card),
            None => reply.text(card),
        };
        Answer {
            choice,
            text,
            by: by.to_string(),
            why: String::new(),
            when: String::new(),
        }
    }

    /// The answer as a [`Reply`].
    pub fn reply(&self) -> Reply {
        match self.choice {
            Some(i) => Reply::Choice(i),
            None => Reply::Text(self.text.clone()),
        }
    }

    pub fn by_judge(&self) -> bool {
        self.by == "judge"
    }

    /// The answer for people, such as `CSV (choice 1)`.
    pub fn show(&self) -> String {
        match self.choice {
            Some(i) => format!("{} (choice {})", self.text, i + 1),
            None => self.text.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card_value() -> Value {
        json!({"text": "CSV or XLSX?", "context": "The export button.", "example": "March",
               "evidence": "src/export.rs:12", "kind": "product", "area": ["exports"],
               "options": [{"label": "CSV", "means": "plain"}, {"label": "XLSX", "means": "sheet"}],
               "recommended": 0, "why": "Finance imports CSV."})
    }

    #[test]
    fn a_valid_card_reads_back() {
        let card = check(&card_value()).unwrap();
        assert_eq!(card.labels(), ["CSV", "XLSX"]);
        assert_eq!(card.memory_kind(), Some(Kind::Product));
        assert_eq!(card.recommended_index(), Some(0));
        assert!(card.topics.is_empty());
    }

    #[test]
    fn each_broken_part_of_a_card_is_named() {
        let mut no_text = card_value();
        no_text["text"] = json!("  ");
        assert!(check(&no_text).unwrap_err()[0].contains("no question text"));

        let mut one = card_value();
        one["options"] = json!([{"label": "CSV", "means": "plain"}]);
        assert!(check(&one).unwrap_err()[0].contains("two or more"));

        let mut blank = card_value();
        blank["options"][1]["label"] = json!("");
        assert!(check(&blank).unwrap_err()[0].contains("choice 2 has no label"));

        let mut kind = card_value();
        kind["kind"] = json!("money");
        assert!(check(&kind).unwrap_err()[0].contains("`money`"));

        let mut missing = card_value();
        missing.as_object_mut().unwrap().remove("kind");
        assert!(check(&missing).is_err());

        let mut wrong_type = card_value();
        wrong_type["options"] = json!("CSV");
        assert!(check(&wrong_type).unwrap_err()[0].contains("schema"));

        assert!(check(&json!("a question")).is_err());

        let mut all = card_value();
        all["text"] = json!("");
        all["options"] = json!([]);
        all["kind"] = json!("x");
        assert_eq!(check(&all).unwrap_err().len(), 3);
    }

    #[test]
    fn a_recommendation_outside_the_choices_is_none() {
        let mut value = card_value();
        value["recommended"] = json!(2);
        assert_eq!(check(&value).unwrap().recommended_index(), None);
        value["recommended"] = json!(-1);
        assert_eq!(check(&value).unwrap().recommended_index(), None);
        value["recommended"] = Value::Null;
        assert_eq!(check(&value).unwrap().recommended_index(), None);
    }

    #[test]
    fn the_letters_skip_the_keys_of_the_view() {
        assert_eq!(letter(0), Some('a'));
        assert_eq!(letter(16), Some('r'));
        assert_eq!(index_of_letter('q'), None);
        assert_eq!(index_of_letter('s'), None);
        assert_eq!(index_of_letter('t'), None);
        assert_eq!(index_of_letter('b'), Some(1));
        assert_eq!(letter(LETTERS.len()), None);
    }

    #[test]
    fn two_answers_are_the_same_by_choice_or_by_folded_text() {
        let card = check(&card_value()).unwrap();
        let csv = Reply::Choice(0);
        assert!(csv.same(&Reply::Text("  csv ".into()), &card));
        assert!(!csv.same(&Reply::Choice(1), &card));
        assert!(Reply::Text("Use  JSON".into()).same(&Reply::Text("use json".into()), &card));
        assert!(!Reply::Text("JSON".into()).same(&Reply::Choice(0), &card));
        let answer = Answer::new(&Reply::Text("xlsx".into()), &card, "user");
        assert_eq!(answer.choice, Some(1));
        assert_eq!(answer.text, "XLSX");
        assert_eq!(answer.show(), "XLSX (choice 2)");
        assert!(!answer.by_judge());
    }
}
