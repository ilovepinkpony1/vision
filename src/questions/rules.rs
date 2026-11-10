//! The rules in code (section 8.3). A rule in code always wins over the output of the Judge.
//!
//! [`rule`] is pure: its inputs are the card, the entry of the Judge for it (or none), the
//! configuration `[judge]`, and a lookup that says whether a memory id exists and is active.
//! The rules run in this order:
//!
//! 0. A card with no entry goes to the person: `ask`.
//! 1. A topic of the card or of the entry in `judge.always_ask` makes the outcome `ask`.
//! 2. An `answer` with the basis `decision` or `principle` needs at least one source, and each
//!    source must exist and be active. If not, it becomes `propose`.
//! 3. An `answer` with the basis `inference` stays an answer only when `sure` is true and
//!    `judge.answer_when_sure` is true. If not, it becomes `propose`. An `answer` with the
//!    basis `none` has no basis at all, so it becomes `propose` too.
//! 4. An `answer` or a `propose` must name a valid choice or a text. If not, it becomes `ask`.
//!
//! Rule 5 (no outcome in `caps.judge_minutes`) is about time, so the tick applies it.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::card::{fold, Card, Reply};
use crate::config::Judge;

/// The outcome of one card (section 8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Answer,
    Propose,
    Ask,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Answer => "answer",
            Outcome::Propose => "propose",
            Outcome::Ask => "ask",
        }
    }

    pub fn parse(text: &str) -> Option<Outcome> {
        [Outcome::Answer, Outcome::Propose, Outcome::Ask]
            .into_iter()
            .find(|o| o.as_str() == text)
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The basis of an entry of the Judge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Basis {
    Decision,
    Principle,
    Inference,
    None,
}

impl Basis {
    pub fn as_str(self) -> &'static str {
        match self {
            Basis::Decision => "decision",
            Basis::Principle => "principle",
            Basis::Inference => "inference",
            Basis::None => "none",
        }
    }
}

/// One entry of `answers` in the output of the Judge (`agents/judge.schema.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The id of the card, such as `Q-0001`.
    pub card: String,
    pub outcome: Outcome,
    /// The index of the choice, from 0.
    #[serde(default)]
    pub choice: Option<i64>,
    #[serde(default)]
    pub text: Option<String>,
    pub basis: Basis,
    #[serde(default)]
    pub sources: Vec<String>,
    pub sure: bool,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub reason: String,
}

impl Entry {
    /// The answer that the entry names: a choice of the card, else a text. A choice outside
    /// the card does not count.
    pub fn reply(&self, card: &Card) -> Option<Reply> {
        let choice = self
            .choice
            .and_then(|i| usize::try_from(i).ok())
            .filter(|i| *i < card.options.len());
        if let Some(i) = choice {
            return Some(Reply::Choice(i));
        }
        self.text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| Reply::Text(t.to_string()))
    }
}

/// The final outcome of one card, and why the code changed the outcome of the Judge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ruling {
    pub outcome: Outcome,
    /// `None` when the outcome is the one that the Judge gave.
    pub reason: Option<String>,
}

/// The text of the rule for a card that has no entry.
pub const NO_ENTRY: &str = "the Judge gave no outcome for this card";

/// Applies the rules in code to the entry of the Judge for one card.
pub fn rule(
    card: &Card,
    entry: Option<&Entry>,
    judge: &Judge,
    active: &dyn Fn(&str) -> bool,
) -> Ruling {
    let Some(entry) = entry else {
        return Ruling {
            outcome: Outcome::Ask,
            reason: Some(NO_ENTRY.into()),
        };
    };

    // Rule 1.
    let sensitive = card.topics.iter().chain(entry.topics.iter()).find(|topic| {
        let topic = fold(topic);
        !topic.is_empty() && judge.always_ask.iter().any(|a| fold(a) == topic)
    });
    if let Some(topic) = sensitive {
        let reason = (entry.outcome != Outcome::Ask).then(|| {
            format!(
                "rule 1: the topic `{}` is in judge.always_ask, so the person answers",
                topic.trim()
            )
        });
        return Ruling {
            outcome: Outcome::Ask,
            reason,
        };
    }

    let mut outcome = entry.outcome;
    let mut reasons: Vec<String> = Vec::new();

    // Rules 2 and 3.
    if outcome == Outcome::Answer {
        let weaker = match entry.basis {
            Basis::Decision | Basis::Principle => {
                let sources: Vec<&str> = entry
                    .sources
                    .iter()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();
                if sources.is_empty() {
                    Some(format!(
                        "rule 2: an answer with the basis {} names no source",
                        entry.basis.as_str()
                    ))
                } else {
                    sources
                        .iter()
                        .find(|s| !active(s))
                        .map(|s| format!("rule 2: the source {s} does not exist or is not active"))
                }
            }
            Basis::Inference if !entry.sure => {
                Some("rule 3: the Judge is not sure of its inference".to_string())
            }
            Basis::Inference if !judge.answer_when_sure => Some(
                "rule 3: judge.answer_when_sure is false, so an inference is only a proposal"
                    .to_string(),
            ),
            Basis::Inference => None,
            Basis::None => Some("rule 3: an answer with the basis none has no basis".to_string()),
        };
        if let Some(reason) = weaker {
            outcome = Outcome::Propose;
            reasons.push(reason);
        }
    }

    // Rule 4.
    if matches!(outcome, Outcome::Answer | Outcome::Propose) && entry.reply(card).is_none() {
        let what = if outcome == Outcome::Answer {
            "answer"
        } else {
            "proposal"
        };
        reasons.push(format!(
            "rule 4: the {what} names no valid choice and no text"
        ));
        outcome = Outcome::Ask;
    }

    Ruling {
        outcome,
        reason: (!reasons.is_empty()).then(|| reasons.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::questions::card::Choice;

    fn card(topics: &[&str]) -> Card {
        Card {
            text: "CSV or XLSX?".into(),
            options: vec![
                Choice {
                    label: "CSV".into(),
                    means: "plain".into(),
                },
                Choice {
                    label: "XLSX".into(),
                    means: "sheet".into(),
                },
            ],
            kind: "product".into(),
            topics: topics.iter().map(|t| t.to_string()).collect(),
            ..Card::default()
        }
    }

    fn entry(outcome: Outcome, basis: Basis, sources: &[&str], sure: bool) -> Entry {
        Entry {
            card: "Q-0001".into(),
            outcome,
            choice: Some(0),
            text: None,
            basis,
            sources: sources.iter().map(|s| s.to_string()).collect(),
            sure,
            topics: Vec::new(),
            reason: "D-0001 covers it.".into(),
        }
    }

    fn judge() -> Judge {
        Judge::default()
    }

    /// D-0001 and P-001 are active. D-0002 exists but is superseded. Nothing else exists.
    fn active(id: &str) -> bool {
        matches!(id, "D-0001" | "P-001")
    }

    fn run(card: &Card, entry: Option<&Entry>, judge: &Judge) -> Ruling {
        rule(card, entry, judge, &active)
    }

    fn reason_of(ruling: &Ruling) -> &str {
        ruling.reason.as_deref().unwrap_or("")
    }

    #[test]
    fn a_card_with_no_entry_goes_to_the_person() {
        let ruling = run(&card(&[]), None, &judge());
        assert_eq!(ruling.outcome, Outcome::Ask);
        assert_eq!(reason_of(&ruling), NO_ENTRY);
    }

    #[test]
    fn an_answer_that_passes_each_rule_stays_an_answer() {
        for e in [
            entry(Outcome::Answer, Basis::Decision, &["D-0001"], false),
            entry(
                Outcome::Answer,
                Basis::Principle,
                &["P-001", "D-0001"],
                true,
            ),
            entry(Outcome::Answer, Basis::Inference, &[], true),
        ] {
            let ruling = run(&card(&[]), Some(&e), &judge());
            assert_eq!(ruling.outcome, Outcome::Answer, "{e:?}");
            assert_eq!(ruling.reason, None);
        }
    }

    #[test]
    fn a_proposal_and_an_ask_stay_as_the_judge_gave_them() {
        let propose = entry(Outcome::Propose, Basis::Inference, &[], false);
        assert_eq!(
            run(&card(&[]), Some(&propose), &judge()),
            Ruling {
                outcome: Outcome::Propose,
                reason: None
            }
        );
        let mut ask = entry(Outcome::Ask, Basis::None, &[], false);
        ask.choice = None;
        assert_eq!(
            run(&card(&[]), Some(&ask), &judge()),
            Ruling {
                outcome: Outcome::Ask,
                reason: None
            }
        );
    }

    #[test]
    fn rule_1_a_topic_of_the_card_or_of_the_answer_makes_it_ask() {
        let e = entry(Outcome::Answer, Basis::Decision, &["D-0001"], true);
        let ruling = run(&card(&["Money"]), Some(&e), &judge());
        assert_eq!(ruling.outcome, Outcome::Ask);
        assert!(reason_of(&ruling).starts_with("rule 1: the topic `Money`"));

        let mut e = entry(Outcome::Propose, Basis::Inference, &[], false);
        e.topics = vec![" deploy ".into()];
        let ruling = run(&card(&["exports"]), Some(&e), &judge());
        assert_eq!(ruling.outcome, Outcome::Ask);
        assert!(reason_of(&ruling).contains("`deploy`"));

        let mut ask = entry(Outcome::Ask, Basis::None, &[], false);
        ask.topics = vec!["security".into()];
        assert_eq!(run(&card(&[]), Some(&ask), &judge()).reason, None);

        let custom = Judge {
            always_ask: vec!["exports".into()],
            ..judge()
        };
        let e = entry(Outcome::Answer, Basis::Inference, &[], true);
        assert_eq!(
            run(&card(&["exports"]), Some(&e), &custom).outcome,
            Outcome::Ask
        );
        assert_eq!(
            run(&card(&["money"]), Some(&e), &custom).outcome,
            Outcome::Answer
        );
    }

    #[test]
    fn rule_2_a_missing_or_inactive_source_makes_a_proposal() {
        for (sources, needle) in [
            (vec![], "names no source"),
            (vec![" "], "names no source"),
            (vec!["D-0009"], "the source D-0009 does not exist"),
            (vec!["D-0001", "D-0002"], "the source D-0002 does not exist"),
            (vec!["not-an-id"], "the source not-an-id"),
        ] {
            for basis in [Basis::Decision, Basis::Principle] {
                let e = entry(Outcome::Answer, basis, &sources, true);
                let ruling = run(&card(&[]), Some(&e), &judge());
                assert_eq!(ruling.outcome, Outcome::Propose, "{sources:?}");
                assert!(reason_of(&ruling).starts_with("rule 2"), "{ruling:?}");
                assert!(reason_of(&ruling).contains(needle), "{ruling:?}");
            }
        }
        let propose = entry(Outcome::Propose, Basis::Decision, &["D-0009"], true);
        assert_eq!(
            run(&card(&[]), Some(&propose), &judge()).reason,
            None,
            "rule 2 tests answers only"
        );
    }

    #[test]
    fn rule_3_an_inference_stays_an_answer_only_when_sure_and_allowed() {
        let unsure = entry(Outcome::Answer, Basis::Inference, &[], false);
        let ruling = run(&card(&[]), Some(&unsure), &judge());
        assert_eq!(ruling.outcome, Outcome::Propose);
        assert!(reason_of(&ruling).contains("not sure"));

        let sure = entry(Outcome::Answer, Basis::Inference, &[], true);
        let off = Judge {
            answer_when_sure: false,
            ..judge()
        };
        let ruling = run(&card(&[]), Some(&sure), &off);
        assert_eq!(ruling.outcome, Outcome::Propose);
        assert!(reason_of(&ruling).contains("judge.answer_when_sure is false"));

        let decision = entry(Outcome::Answer, Basis::Decision, &["D-0001"], true);
        assert_eq!(
            run(&card(&[]), Some(&decision), &off).outcome,
            Outcome::Answer,
            "answer_when_sure is only about inference"
        );

        let none = entry(Outcome::Answer, Basis::None, &[], true);
        let ruling = run(&card(&[]), Some(&none), &judge());
        assert_eq!(ruling.outcome, Outcome::Propose);
        assert!(reason_of(&ruling).contains("basis none"));
    }

    #[test]
    fn rule_4_an_answer_or_a_proposal_needs_a_choice_or_a_text() {
        for outcome in [Outcome::Answer, Outcome::Propose] {
            let mut e = entry(outcome, Basis::Inference, &[], true);
            e.choice = None;
            let ruling = run(&card(&[]), Some(&e), &judge());
            assert_eq!(ruling.outcome, Outcome::Ask, "{outcome}");
            assert!(reason_of(&ruling).starts_with("rule 4"), "{ruling:?}");

            e.choice = Some(2);
            assert_eq!(
                run(&card(&[]), Some(&e), &judge()).outcome,
                Outcome::Ask,
                "a choice outside the card"
            );
            e.choice = Some(-1);
            assert_eq!(run(&card(&[]), Some(&e), &judge()).outcome, Outcome::Ask);

            e.text = Some("  JSON ".into());
            assert_eq!(
                run(&card(&[]), Some(&e), &judge()).outcome,
                outcome,
                "a text is enough"
            );
            e.text = Some("   ".into());
            assert_eq!(run(&card(&[]), Some(&e), &judge()).outcome, Outcome::Ask);
        }
    }

    #[test]
    fn rule_1_wins_over_each_later_rule() {
        let mut no_source_no_choice = entry(Outcome::Answer, Basis::Decision, &[], true);
        no_source_no_choice.choice = None;
        let mut unsure = entry(Outcome::Answer, Basis::Inference, &[], false);
        unsure.topics = vec!["credentials".into()];
        for (c, e) in [
            (card(&["security"]), no_source_no_choice.clone()),
            (card(&[]), unsure),
        ] {
            let ruling = run(&c, Some(&e), &judge());
            assert_eq!(ruling.outcome, Outcome::Ask);
            assert!(reason_of(&ruling).starts_with("rule 1"), "{ruling:?}");
            assert!(!reason_of(&ruling).contains("rule 2"), "{ruling:?}");
        }
    }

    #[test]
    fn rule_2_then_rule_4_makes_an_answer_ask_and_names_both() {
        let mut e = entry(Outcome::Answer, Basis::Principle, &["P-404"], true);
        e.choice = None;
        let ruling = run(&card(&[]), Some(&e), &judge());
        assert_eq!(ruling.outcome, Outcome::Ask);
        assert_eq!(
            reason_of(&ruling),
            "rule 2: the source P-404 does not exist or is not active; \
             rule 4: the proposal names no valid choice and no text"
        );
    }

    #[test]
    fn rule_3_then_rule_4_makes_an_answer_ask_and_names_both() {
        let mut e = entry(Outcome::Answer, Basis::Inference, &[], false);
        e.choice = Some(7);
        let ruling = run(&card(&[]), Some(&e), &judge());
        assert_eq!(ruling.outcome, Outcome::Ask);
        assert!(reason_of(&ruling).starts_with("rule 3"), "{ruling:?}");
        assert!(reason_of(&ruling).contains("; rule 4"), "{ruling:?}");
    }

    #[test]
    fn rule_2_passes_and_rule_4_still_applies() {
        let mut e = entry(Outcome::Answer, Basis::Decision, &["D-0001"], true);
        e.choice = None;
        let ruling = run(&card(&[]), Some(&e), &judge());
        assert_eq!(ruling.outcome, Outcome::Ask);
        assert_eq!(
            reason_of(&ruling),
            "rule 4: the answer names no valid choice and no text"
        );
    }

    #[test]
    fn an_entry_names_a_choice_before_a_text() {
        let c = card(&[]);
        let mut e = entry(Outcome::Answer, Basis::Inference, &[], true);
        e.choice = Some(1);
        e.text = Some("CSV".into());
        assert_eq!(e.reply(&c), Some(Reply::Choice(1)));
        e.choice = Some(5);
        assert_eq!(e.reply(&c), Some(Reply::Text("CSV".into())));
    }

    #[test]
    fn an_entry_reads_from_the_output_of_the_judge() {
        let value = serde_json::json!({"card": "Q-0001", "outcome": "propose", "basis": "none",
            "sure": false, "reason": "Likely CSV.", "choice": null});
        let e: Entry = serde_json::from_value(value).unwrap();
        assert_eq!(e.outcome, Outcome::Propose);
        assert_eq!(e.choice, None);
        assert!(e.sources.is_empty());
        assert_eq!(Outcome::parse("ask"), Some(Outcome::Ask));
        assert_eq!(Outcome::parse("maybe"), None);
    }
}
