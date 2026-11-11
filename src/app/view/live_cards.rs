//! The cards of the live view: the stop, the retry, the open of the session and the card after
//! it, and the keys of the live view. Each card says what happens before Enter does it. The
//! open card of a running turn also offers to stop the turn and open its session; while that
//! chain runs, the card shows its steps.

use super::canvas::{ink, Canvas};
use super::cards::{action_rows, show, Rows};
use super::chain_rows;
use super::theme::T;
use crate::app::chain::Owner;
use crate::app::model::{App, OpenCard, OpenedCard, RetryCard, StopCard};

pub fn stop(app: &App, card: &StopCard, c: &mut Canvas) {
    let mut rows = Rows::default();
    rows.text(3, format!("Stop {}?", card.key), ink(T.fg).bold());
    rows.blank();
    let turn = match &card.agent {
        Some(agent) => format!("The running turn of {agent} ends now. "),
        None => String::new(),
    };
    let branch = match &card.branch {
        Some(branch) => format!("the branch {branch}"),
        None => "its branch".to_string(),
    };
    rows.para(
        3,
        &format!(
            "{turn}The ticket keeps its step {} and {branch}, and its clone is free for another ticket. No new turn starts until you resume it.",
            card.step
        ),
        68,
        ink(T.dim),
    );
    rows.blank();
    rows.text(
        3,
        "Later, Enter on the stopped ticket starts it again at the same step.",
        ink(T.faint),
    );
    rows.blank();
    action_rows(
        app,
        &mut rows,
        &card.run,
        "stop",
        &format!("stops {}", card.key),
    );
    show(c, 76, &format!("Stop {}", card.key), T.bad, rows);
}

pub fn retry(app: &App, card: &RetryCard, c: &mut Canvas) {
    let mut rows = Rows::default();
    let head = if card.paused {
        format!("Continue {}?", card.key)
    } else {
        format!("Start the step {} of {} again?", card.step, card.key)
    };
    rows.text(3, head, ink(T.fg).bold());
    rows.blank();
    if let Some(reason) = &card.reason {
        rows.para(3, &format!("A rule stopped it: {reason}"), 68, ink(T.warn));
        rows.blank();
    }
    let agent = card.agent.as_deref().unwrap_or("the agent");
    let what = if card.paused {
        format!(
            "Vision clears the pause. The next turn of {agent} resumes the session that you worked in, and it gets a note that you worked there."
        )
    } else if card.held {
        "The turn of the step already gave a result that held, so no new turn starts. Vision runs the writes of the step again, the ones that waited.".to_string()
    } else {
        format!(
            "Vision clears the flag, and the step {} starts again with a new turn of {agent}. Its message says why the step stopped.",
            card.step
        )
    };
    rows.para(3, &what, 68, ink(T.dim));
    rows.blank();
    let go = if card.paused { "continue" } else { "retry" };
    action_rows(
        app,
        &mut rows,
        &card.run,
        go,
        &format!("retries {}", card.key),
    );
    show(c, 76, &format!("Retry {}", card.key), T.primary, rows);
}

pub fn open(app: &App, card: &OpenCard, c: &mut Canvas) {
    let mut rows = Rows::default();
    match &card.running {
        Some(agent) => {
            rows.text(3, format!("{agent} works now."), ink(T.fg).bold());
            rows.blank();
            rows.para(
                3,
                &format!(
                    "Press t to send {agent} a message: it reads your text at its next tool step. Press o to stop the turn of {agent} and open its session here: the turn that runs ends and is lost, and its session keeps what the agent did so far. Press x to stop the ticket and its turn."
                ),
                68,
                ink(T.dim),
            );
            rows.blank();
            let owner = Owner::TakeOver {
                key: card.key.clone(),
                agent: agent.clone(),
            };
            match app.chain_of(&owner) {
                Some(chain) => {
                    chain_rows::rows(app, chain, &mut rows, 68);
                    rows.blank();
                    if chain.running() {
                        rows.keys(&[("esc", "close this card; the action goes on")]);
                    } else {
                        rows.keys(&[("o", "try again"), ("esc", "close")]);
                    }
                }
                None => {
                    rows.keys(&[("t", "tell"), ("x", "stop"), ("esc", "close")]);
                    rows.keys(&[(
                        "o",
                        &format!("stop the turn of {agent} and open its session"),
                    )]);
                }
            }
            show(
                c,
                76,
                &format!("The session of {}", card.key),
                T.primary,
                rows,
            );
        }
        None => {
            let agent = card.last.as_deref().unwrap_or("the last agent");
            rows.text(3, format!("Open the session of {agent}?"), ink(T.fg).bold());
            rows.blank();
            rows.para(
                3,
                &format!(
                    "Vision pauses {}, so no new turn starts. The interactive session of the last turn of {agent} opens in this terminal. When you leave it, Vision comes back here, and you choose whether the ticket goes on.",
                    card.key
                ),
                68,
                ink(T.dim),
            );
            rows.blank();
            rows.keys(&[("enter", "open"), ("esc", "cancel")]);
            show(
                c,
                76,
                &format!("The session of {}", card.key),
                T.primary,
                rows,
            );
        }
    }
}

pub fn opened(app: &App, card: &OpenedCard, c: &mut Canvas) {
    if let Some(agent) = &card.took_over {
        return took_over(app, card, agent, c);
    }
    let mut rows = Rows::default();
    match (&card.error, card.code) {
        (None, _) => rows.text(3, "You left the session.", ink(T.fg).bold()),
        (Some(error), code) => {
            let how = match code {
                Some(code) => format!("The session ended with the exit code {code}."),
                None => "The session did not start, or a signal ended it.".to_string(),
            };
            rows.text(3, how, ink(T.warn).bold());
            rows.para(3, error, 68, ink(T.warn));
        }
    }
    rows.blank();
    rows.para(
        3,
        &format!(
            "{} stays paused until you continue it. Continue: Vision starts the step again, and the next turn resumes your session. Keep it paused: nothing moves until you press r.",
            card.key
        ),
        68,
        ink(T.dim),
    );
    rows.blank();
    match card.run {
        crate::app::model::Run::Idle => {
            rows.keys(&[("enter", "continue"), ("esc", "keep it paused")]);
        }
        _ => action_rows(
            app,
            &mut rows,
            &card.run,
            "continue",
            &format!("retries {}", card.key),
        ),
    }
    show(
        c,
        76,
        &format!("Back from the session of {}", card.key),
        T.primary,
        rows,
    );
}

/// The card after a session that the person took over from a running turn: the ticket is
/// stopped and paused.
fn took_over(app: &App, card: &OpenedCard, agent: &str, c: &mut Canvas) {
    let key = &card.key;
    let mut rows = Rows::default();
    match &card.error {
        None => rows.text(3, "You left the session.", ink(T.fg).bold()),
        Some(error) => {
            let how = match card.code {
                Some(code) => format!("The session ended with the exit code {code}."),
                None => "The session did not start, or a signal ended it.".to_string(),
            };
            rows.text(3, how, ink(T.warn).bold());
            rows.para(3, error, 68, ink(T.warn));
        }
    }
    rows.blank();
    rows.para(
        3,
        &format!(
            "{key} is stopped and paused. The turn of {agent} that ran is lost, and its session keeps what the agent did."
        ),
        68,
        ink(T.dim),
    );
    rows.blank();
    rows.para(
        3,
        &format!(
            "Continue: Vision starts {key} again at its step, then clears the pause, so that the next turn of {agent} resumes the session. Keep it stopped: nothing moves until you press n and continue it there."
        ),
        68,
        ink(T.dim),
    );
    rows.blank();
    match app.chain_of(&Owner::Continue(key.clone())) {
        Some(chain) => {
            chain_rows::rows(app, chain, &mut rows, 68);
            rows.blank();
            if chain.running() {
                rows.keys(&[("esc", "close this card; the action goes on")]);
            } else {
                rows.keys(&[("esc", "close")]);
            }
        }
        None => rows.keys(&[("enter", "continue"), ("esc", "keep it stopped")]),
    }
    show(
        c,
        76,
        &format!("Back from the session of {key}"),
        T.primary,
        rows,
    );
}

/// The keys of the live view.
pub fn help(c: &mut Canvas) {
    let list = [
        ("esc", "back to Home"),
        ("n", "answer what waits for this ticket"),
        ("t", "tell the agent something"),
        ("o", "open the session and type into it yourself"),
        ("l  p  d", "the log, the plan, the commits to push"),
        ("x", "stop the ticket"),
        ("r", "start the step again, or continue"),
        ("enter", "start a stopped ticket again"),
        ("↑↓  j k", "scroll the activity"),
        ("pgup pgdn", "scroll the activity by a page"),
        ("G  end", "back to the end of the activity"),
        ("?", "this list of keys"),
        ("q", "quit"),
    ];
    let mut rows = Rows::default();
    for (key, label) in list {
        rows.rows.push(vec![
            (3, key.to_string(), ink(T.fg).bold()),
            (16, label.to_string(), ink(T.dim)),
        ]);
    }
    rows.blank();
    rows.text(3, "esc closes this list", ink(T.faint));
    show(c, 70, "Keys", T.primary, rows);
}
