//! The data and the state of the screen Models (`drawModels` of the design): one row for each
//! agent with its model, its effort, pinned or the default, the reason of the default, the
//! measured turns of each model with their mean cost, and the recommendation of the model table
//! of today. A change that is not pinned yet lives only here, until Enter pins it or Esc drops
//! it.

use std::collections::BTreeMap;

use super::model::Fetch;
use crate::models::SHORT_NAMES;

/// The effort levels, in their order.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// One agent of the model table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelLine {
    pub agent: String,
    /// The model and the effort that its next turn uses.
    pub model: String,
    pub effort: String,
    /// True when the person pinned the values.
    pub pinned: bool,
    /// When the person pinned them, RFC 3339 UTC.
    pub since: Option<String>,
    pub default_model: String,
    pub default_effort: String,
    /// Why this is the default.
    pub reason: String,
    /// The measured turns of each model that the agent used.
    pub evidence: Vec<Evidence>,
    /// The recommendation of the model table, in words.
    pub recommendation: String,
    /// The recommended model, when the evidence names one.
    pub recommended: Option<String>,
}

/// The measured turns of one model of an agent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evidence {
    pub model: String,
    pub turns: i64,
    pub cost_per_turn: f64,
}

impl ModelLine {
    /// The measured turns of a model.
    pub fn measured(&self, model: &str) -> Option<&Evidence> {
        self.evidence.iter().find(|e| e.model == model)
    }

    /// The models that the arrows go through: the models of the short names, then each other
    /// model that the agent uses or used.
    pub fn known_models(&self) -> Vec<String> {
        let mut out: Vec<String> = SHORT_NAMES
            .iter()
            .map(|(_, full)| full.to_string())
            .collect();
        for model in std::iter::once(&self.model).chain(self.evidence.iter().map(|e| &e.model)) {
            if !out.contains(model) {
                out.push(model.clone());
            }
        }
        out
    }
}

/// A change of one agent that is not pinned yet: the model and the effort that show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub model: String,
    pub effort: String,
}

/// The screen Models.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelsState {
    pub data: Fetch<Vec<ModelLine>>,
    /// The index of the selected agent.
    pub selected: usize,
    /// The changes that are not pinned yet, by agent.
    pub edits: BTreeMap<String, Edit>,
}

impl ModelsState {
    pub fn lines(&self) -> &[ModelLine] {
        self.data.data.as_deref().unwrap_or(&[])
    }

    /// The model and the effort that show for an agent: its change, else its values.
    pub fn shown(&self, line: &ModelLine) -> (String, String) {
        match self.edits.get(&line.agent) {
            Some(edit) => (edit.model.clone(), edit.effort.clone()),
            None => (line.model.clone(), line.effort.clone()),
        }
    }
}

/// The next item of a list after `now`, or the one before it, around the ends.
pub fn cycle(list: &[String], now: &str, by: i32) -> String {
    if list.is_empty() {
        return now.to_string();
    }
    let at = list.iter().position(|m| m == now).map_or(0, |i| i as i32);
    let len = list.len() as i32;
    list[(at + by).rem_euclid(len) as usize].clone()
}
