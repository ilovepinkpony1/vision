//! The frame of the card of the things that wait (`mAsk`, `mPlan`, and `mWrite` of the design):
//! a title with the ticket and "2 of 3" when more than one thing waits, a body that scrolls,
//! and a foot that stays: the line that the person types in, the steps of a chain with their
//! marks, and the keys. Each kind of item builds its own body and foot.

use super::ask_card;
use super::canvas::{ink, Canvas};
use super::cards::Rows;
use super::chain_rows;
use super::header::spinner;
use super::other_cards;
use super::theme::T;
use super::write_card;
use crate::app::chain::Owner;
use crate::app::model::App;
use crate::app::needs::{NeedItem, NeedsCard};
use crate::app::text::cut;

/// The width of the card.
pub const WIDTH: i32 = 92;
/// The width of a text in the card, from its column 3.
pub const TEXT: usize = 86;

/// The parts of a card.
pub(super) struct Parts {
    pub(super) title: String,
    pub(super) body: Rows,
    pub(super) foot: Rows,
}

/// The rows of the body that the card has, and the rows that show at the size of the window.
pub fn body_size(app: &App) -> (usize, usize) {
    let Some(crate::app::model::Card::Need(card)) = &app.card else {
        return (0, 0);
    };
    let parts = parts(app, card);
    let (_, visible) = layout(i32::from(app.size.1), &parts);
    (parts.body.rows.len(), visible)
}

/// The height of the card and the rows of its body that show.
fn layout(h: i32, parts: &Parts) -> (i32, usize) {
    let foot = parts.foot.rows.len() as i32;
    let body = parts.body.rows.len() as i32;
    // The frame, a blank row under the title, a blank row between the body and the foot, and
    // a blank row at the end.
    let room = (h - 2 - 4 - foot - 1).max(1);
    let shown = body.min(room);
    (shown + foot + 5, shown as usize)
}

fn parts(app: &App, card: &NeedsCard) -> Parts {
    let Some(item) = card.item() else {
        let mut body = Rows::default();
        match &card.error {
            Some(error) => {
                body.text(
                    3,
                    "Vision could not read what waits for you:",
                    ink(T.bad).bold(),
                );
                body.para(3, error, TEXT, ink(T.bad));
            }
            None => body.rows.push(vec![
                (3, spinner(app).to_string(), ink(T.accent)),
                (5, "reads what waits for you".to_string(), ink(T.fg)),
            ]),
        }
        let mut foot = Rows::default();
        foot.keys(&[("esc", "close")]);
        return Parts {
            title: "What waits for you".into(),
            body,
            foot,
        };
    };
    let mut parts = match item {
        NeedItem::Question(view) => ask_card::parts(app, card, view),
        NeedItem::Plan(plan) => other_cards::plan(app, card, plan),
        NeedItem::Approval(write) => write_card::approval(app, card, write),
        NeedItem::FirstWrite(write) => write_card::first_write(app, card, write),
        NeedItem::Flag(flag) => other_cards::flag(app, card, flag),
        NeedItem::Paused(flag) => other_cards::paused(app, card, flag),
        NeedItem::Proposal(proposal) => other_cards::proposal(app, card, proposal),
    };
    let place = if card.len() > 1 {
        format!("{} of {} · ", card.at + 1, card.len())
    } else {
        String::new()
    };
    parts.title = match item.ticket() {
        Some(ticket) => format!("{ticket} · {place}{}", parts.title),
        None => format!("{place}{}", parts.title),
    };
    if let Some(note) = &card.note {
        let mut body = Rows::default();
        body.para(3, note, TEXT, ink(T.warn).bold());
        body.blank();
        body.rows.append(&mut parts.body.rows);
        parts.body = body;
    }
    let owner = Owner::Need(item.id());
    if let Some(chain) = app.chain_of(&owner) {
        let mut foot = Rows::default();
        chain_rows::rows(app, chain, &mut foot, TEXT);
        foot.blank();
        if chain.running() {
            foot.keys(&[("esc", "close this card; the action goes on")]);
        } else {
            foot.rows.append(&mut parts.foot.rows);
        }
        parts.foot = foot;
    }
    parts
}

pub fn draw(app: &App, card: &NeedsCard, c: &mut Canvas) {
    let parts = parts(app, card);
    let w = WIDTH.min(c.w - 2);
    let (h, visible) = layout(c.h, &parts);
    let h = h.min(c.h - 2);
    let x = (c.w - w) / 2;
    let y = (c.h - h) / 2;
    c.dim_all();
    c.wipe(x, y, w, h, T.panel);
    c.frame(x, y, w, h, T.warn);
    c.put(
        x + 2,
        y,
        &format!(" {} ", cut(&parts.title, (w - 6) as usize)),
        ink(T.fg).bold(),
    );
    let total = parts.body.rows.len();
    let top = card.scroll.min(total.saturating_sub(visible));
    put_rows(
        c,
        x,
        y + 2,
        w,
        &parts.body.rows[top..(top + visible).min(total)],
    );
    if top > 0 {
        c.right(x + w - 3, y, &format!(" ▲ {top} more "), ink(T.dim));
    }
    let below = total.saturating_sub(top + visible);
    let foot_y = y + 2 + visible as i32 + 1;
    if below > 0 {
        c.right(
            x + w - 3,
            foot_y - 1,
            &format!("▼ {below} more · ↑↓ scroll"),
            ink(T.dim),
        );
    }
    put_rows(c, x, foot_y, w, &parts.foot.rows);
}

fn put_rows(
    c: &mut Canvas,
    x: i32,
    y: i32,
    w: i32,
    rows: &[Vec<(i32, String, super::canvas::Ink)>],
) {
    for (i, row) in rows.iter().enumerate() {
        for (dx, text, style) in row {
            let room = (w - dx - 2).max(0) as usize;
            c.put(x + dx, y + i as i32, &cut(text, room), *style);
        }
    }
}
