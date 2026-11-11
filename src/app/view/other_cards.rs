//! The other cards of the things that wait: the plan (`mPlan` of the design) in a body that
//! scrolls with its short hash in the title; a rule that stopped the ticket, with its reason in
//! full and what each key does; a paused ticket; and a proposal of the Curator with the number
//! of its changes that wait.

use super::canvas::ink;
use super::cards::Rows;
use super::need_card::{Parts, TEXT};
use super::theme::T;
use crate::app::model::App;
use crate::app::needs::{FlagItem, NeedsCard, PlanItem, ProposalItem};
use crate::app::text::{count, wrap};
use crate::pipeline::SHORT_HASH;

pub(super) fn plan(app: &App, _card: &NeedsCard, item: &PlanItem) -> Parts {
    let mut body = Rows::default();
    for line in item.plan.text.lines() {
        let style = if line.starts_with('#') {
            ink(T.accent).bold()
        } else {
            ink(T.fg)
        };
        let parts = wrap(line, TEXT);
        if parts.is_empty() {
            body.blank();
        }
        for part in parts {
            body.text(3, part, style);
        }
    }
    if item.plan.edited {
        body.blank();
        body.para(
            3,
            "You edited the plan after the agent wrote it. The first text of the agent stays in plan.agent.md.",
            TEXT,
            ink(T.dim),
        );
    }
    let mut foot = Rows::default();
    let home = app
        .setup
        .as_ref()
        .map_or("the home folder", |s| s.home.as_str());
    foot.para(
        3,
        &format!(
            "To change the plan, edit {home}/tickets/{}/plan.md, and approve the new plan here. Vision has no way to send a plan back to its agent.",
            item.ticket
        ),
        TEXT,
        ink(T.faint),
    );
    foot.keys(&[
        ("↑↓", "scroll"),
        ("a", "approve"),
        ("x", "stop the ticket"),
        ("esc", "later"),
    ]);
    let hash = &item.plan.hash;
    Parts {
        title: format!(
            "the plan waits for your approval · {}",
            &hash[..SHORT_HASH.min(hash.len())]
        ),
        body,
        foot,
    }
}

pub(super) fn flag(_app: &App, _card: &NeedsCard, item: &FlagItem) -> Parts {
    let (key, step) = (&item.ticket, &item.step);
    let mut body = Rows::default();
    body.text(
        3,
        format!("A rule stopped {key} at the step {step}."),
        ink(T.fg).bold(),
    );
    body.blank();
    body.para(3, &item.reason, TEXT, ink(T.warn));
    body.blank();
    let mut foot = Rows::default();
    if item.stopped {
        body.para(
            3,
            &format!("{key} is also stopped. Enter opens the card that starts it again at the same step."),
            TEXT,
            ink(T.dim),
        );
        foot.keys(&[("enter", "start it again"), ("esc", "later")]);
    } else {
        let agent = item.agent.as_deref().unwrap_or("its agent");
        let retry = if item.held {
            format!("r runs the writes of the step {step} again. The turn of the step already gave a result that held, so no new turn starts.")
        } else {
            format!("r starts the step {step} again with a new turn of {agent}. Its message says why the step stopped.")
        };
        body.para(3, &retry, TEXT, ink(T.dim));
        let mut keys = vec![("r", "try the step again")];
        if let Some(last) = &item.last {
            body.para(
                3,
                &format!("o opens the session of {last} in this terminal. Vision pauses {key} while you work in it, and asks afterwards whether it goes on."),
                TEXT,
                ink(T.dim),
            );
            keys.push(("o", "open the session"));
        }
        body.para(
            3,
            &format!("x stops {key}. Its branch stays in its clone."),
            TEXT,
            ink(T.dim),
        );
        keys.push(("x", "stop the ticket"));
        keys.push(("esc", "later"));
        foot.keys(&keys);
    }
    Parts {
        title: "a rule stopped the ticket".into(),
        body,
        foot,
    }
}

pub(super) fn paused(_app: &App, _card: &NeedsCard, item: &FlagItem) -> Parts {
    let key = &item.ticket;
    let agent = item.agent.as_deref().unwrap_or("the agent");
    let mut body = Rows::default();
    body.text(
        3,
        format!("You took over the session of {key}."),
        ink(T.fg).bold(),
    );
    body.blank();
    body.para(
        3,
        "No new turn starts while the ticket is paused.",
        TEXT,
        ink(T.dim),
    );
    let what = if item.stopped {
        format!("{key} is also stopped. Continue starts it again at the step {}, then clears the pause. The next turn of {agent} resumes the session that you worked in.", item.step)
    } else {
        format!("Continue starts the step {} again, and the next turn of {agent} resumes the session that you worked in. It gets a note that you worked there.", item.step)
    };
    body.para(3, &what, TEXT, ink(T.dim));
    let mut foot = Rows::default();
    foot.keys(&[("enter", "continue"), ("esc", "later")]);
    Parts {
        title: "you took over its session".into(),
        body,
        foot,
    }
}

pub(super) fn proposal(_app: &App, _card: &NeedsCard, item: &ProposalItem) -> Parts {
    let mut body = Rows::default();
    body.text(
        3,
        format!(
            "The Curator proposes {} to the memory.",
            count(item.open, "change")
        ),
        ink(T.fg).bold(),
    );
    if !item.summary.is_empty() {
        body.blank();
        body.para(3, &item.summary, TEXT, ink(T.dim));
    }
    if item.replaying {
        body.blank();
        body.para(
            3,
            "Its replays still run, so the review shows their numbers when they end. You can review the changes now.",
            TEXT,
            ink(T.dim),
        );
    }
    body.blank();
    body.para(
        3,
        "Enter opens the review in this terminal. You accept or reject each change there, and Vision comes back here when you leave it.",
        TEXT,
        ink(T.dim),
    );
    let mut foot = Rows::default();
    foot.keys(&[("enter", "review"), ("esc", "later")]);
    Parts {
        title: format!("a proposal of the Curator · {}", item.name),
        body,
        foot,
    }
}
