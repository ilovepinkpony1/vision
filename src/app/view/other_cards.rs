//! The other cards of the things that wait: the plan (`mPlan` of the design) in a body that
//! scrolls with its short hash in the title; a rule that stopped the ticket, with its reason in
//! full and what each key does; a paused ticket; a proposal of the Curator with the number of
//! its changes that wait; and a notice of a gate with its list of issues.

use super::canvas::ink;
use super::cards::Rows;
use super::need_card::{Parts, DISCUSS, TEXT};
use super::theme::T;
use crate::app::model::App;
use crate::app::msg::Key;
use crate::app::needs::{FlagItem, NeedsCard, PlanItem, ProposalItem};
use crate::app::text::{count, wrap};
use crate::ledger::notices::NoticeRow;
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
    // `a` approves the plan, so a click on it arms first. `x` only opens the stop card.
    foot.keys_with(
        &[
            ("↑↓", "scroll"),
            ("a", "approve"),
            ("x", "stop the ticket"),
            DISCUSS,
            ("esc", "later"),
        ],
        &[Key::Char('a')],
        app.armed_act(),
    );
    let hash = &item.plan.hash;
    Parts {
        title: format!(
            "the plan waits for your approval · {}",
            &hash[..SHORT_HASH.min(hash.len())]
        ),
        body,
        foot,
        ..Parts::default()
    }
}

pub(super) fn flag(app: &App, _card: &NeedsCard, item: &FlagItem) -> Parts {
    let (key, step) = (&item.ticket, &item.step);
    // A late wait keeps watching under its flag, unless the ticket is also stopped.
    let late = !item.stopped && crate::pipeline::decide::is_late(&item.reason);
    let mut body = Rows::default();
    let first = if late {
        format!("{key} waited at the step {step} longer than its limit.")
    } else {
        format!("A rule stopped {key} at the step {step}.")
    };
    body.text(3, first, ink(T.fg).bold());
    body.blank();
    // What happened and what to do first; the raw reason stays below as the details.
    let friendly = crate::friendly::explain(&item.reason);
    body.para(3, &friendly.headline, TEXT, ink(T.warn));
    if let Some(action) = &friendly.action {
        body.para(3, action, TEXT, ink(T.fg));
    }
    if let Some(details) = &friendly.details {
        body.blank();
        body.para(3, &format!("Details: {details}"), TEXT, ink(T.faint));
    }
    body.blank();
    let mut foot = Rows::default();
    let discuss = format!("C opens your own Claude Code session about {key} in a new tab of Herdr, with a brief of what Vision knows. The ticket stays as it is.");
    if item.stopped {
        body.para(
            3,
            &format!("{key} is also stopped. Enter opens the card that continues it at the step {step}, or at a step that you pick."),
            TEXT,
            ink(T.dim),
        );
        body.para(3, &discuss, TEXT, ink(T.dim));
        foot.keys(&[("enter", "continue"), DISCUSS, ("esc", "later")]);
    } else {
        let agent = item.agent.as_deref().unwrap_or("its agent");
        let retry = if late {
            format!("r starts the step {step} again with a new limit.")
        } else if item.act {
            format!("r runs the writes of the step {step} again. The step runs no turn, so no agent starts.")
        } else if item.held {
            format!("r runs the writes of the step {step} again. The turn of the step already gave a result that held, so no new turn starts.")
        } else if let Some(doc) = &item.check {
            format!("r reads {doc} again. The step runs no turn, so no agent starts, and the same flag comes back unless {doc} changed.")
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
        body.para(3, &discuss, TEXT, ink(T.dim));
        keys.push(("x", "stop the ticket"));
        keys.push(DISCUSS);
        keys.push(("esc", "later"));
        // `r` starts the step again, so a click on it arms first. `o` and `x` only open a card.
        foot.keys_with(&keys, &[Key::Char('r')], app.armed_act());
    }
    let title = if late {
        "the wait passed its limit, and Vision keeps watching"
    } else {
        "a rule stopped the ticket"
    };
    Parts {
        title: title.into(),
        body,
        foot,
        ..Parts::default()
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
        format!("{key} is also stopped. Enter opens the card that continues it at the step {}, or at a step that you pick. At its step, the next turn of {agent} resumes the session that you worked in.", item.step)
    } else {
        format!("Enter opens the card that continues it at the step {}, or at a step that you pick. At its step, the next turn of {agent} resumes the session that you worked in, with a note that you worked there.", item.step)
    };
    body.para(3, &what, TEXT, ink(T.dim));
    let mut foot = Rows::default();
    // Enter only opens the continue card, whose Enter acts, so a click here acts at once.
    foot.keys(&[("enter", "continue"), DISCUSS, ("esc", "later")]);
    Parts {
        title: "you took over its session".into(),
        body,
        foot,
        ..Parts::default()
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
        ..Parts::default()
    }
}

pub(super) fn notice(app: &App, _card: &NeedsCard, item: &NoticeRow) -> Parts {
    let mut body = Rows::default();
    let n = item.issues.len();
    // A notice of the gate cr names the findings that keep coming back after their fix.
    let head = if item.gate == crate::pipeline::rounds::CR_SOURCE {
        format!(
            "{} of the report {} of the step {} {} coming back after the fix, round after round:",
            count(n, "finding"),
            item.doc,
            item.step,
            if n == 1 { "keeps" } else { "keep" }
        )
    } else {
        format!(
            "The gate {} of the step {} passed, so the ticket went on. Its report {} has {} that {} not block:",
            item.gate,
            item.step,
            item.doc,
            count(n, "issue"),
            if n == 1 { "does" } else { "do" }
        )
    };
    body.para(3, &head, TEXT, ink(T.fg).bold());
    body.blank();
    for issue in &item.issues {
        let head = format!("{}  {}  ", issue.id, issue.severity);
        let room = TEXT.saturating_sub(head.chars().count()).max(20);
        for (i, part) in wrap(&issue.title, room).into_iter().enumerate() {
            if i == 0 {
                body.rows.push(vec![
                    (3, head.clone(), ink(T.warn)),
                    (3 + head.chars().count() as i32, part, ink(T.fg)),
                ]);
            } else {
                body.text(3 + head.chars().count() as i32, part, ink(T.fg));
            }
        }
    }
    if let Some(note) = &item.note {
        let mut chars = note.chars();
        let note: String = chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
        body.blank();
        body.para(3, &format!("{note}."), TEXT, ink(T.dim));
    }
    body.blank();
    body.para(
        3,
        &format!(
            "A notice never stops a ticket. It stays until you mark it read. The turn was {}.",
            item.turn
        ),
        TEXT,
        ink(T.dim),
    );
    let mut foot = Rows::default();
    foot.keys_with(
        &[("enter", "mark it read"), DISCUSS, ("esc", "later")],
        &[Key::Enter],
        app.armed_act(),
    );
    Parts {
        title: format!("a notice · {}", item.name()),
        body,
        foot,
        ..Parts::default()
    }
}
