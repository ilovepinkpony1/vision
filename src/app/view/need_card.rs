//! The frame of the card of the things that wait (`mAsk`, `mPlan`, and `mWrite` of the design):
//! a title with the ticket and "2 of 3" when more than one thing waits, a head that stays (the
//! question row of a form), a body that scrolls, and a foot that stays: the line that the person
//! types in, the steps of a chain with their marks, and the keys. Each kind of item builds its
//! own head, body, and foot.
//!
//! [`place`] puts the card in the window for the draw, and [`hits`] gives the places of the
//! parts of a form that a click can hit, from the same placement.

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
use crate::app::needs::{NeedItem, NeedsCard, Target};
use crate::app::text::cut;

/// The width of the card.
pub const WIDTH: i32 = 92;
/// The width of a text in the card, from its column 3.
pub const TEXT: usize = 86;

/// The parts of a card.
#[derive(Default)]
pub(super) struct Parts {
    pub(super) title: String,
    /// The rows under the title that never scroll, such as the question row of a form.
    pub(super) head: Rows,
    pub(super) body: Rows,
    pub(super) foot: Rows,
    /// The parts that a click can hit.
    pub(super) hits: Vec<Hit>,
}

/// The rows of a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Head,
    Body,
    Foot,
}

/// A part of a card that a click can hit: its row in the head, the body, or the foot, and its
/// columns in the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub target: Target,
    pub area: Area,
    pub row: usize,
    pub x: i32,
    pub w: i32,
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

/// The rows of the head and the blank row under it.
fn head_rows(parts: &Parts) -> i32 {
    match parts.head.rows.len() as i32 {
        0 => 0,
        n => n + 1,
    }
}

/// The height of the card and the rows of its body that show.
fn layout(h: i32, parts: &Parts) -> (i32, usize) {
    let head = head_rows(parts);
    let foot = parts.foot.rows.len() as i32;
    let body = parts.body.rows.len() as i32;
    // The frame, a blank row under the title, a blank row between the body and the foot, and
    // a blank row at the end.
    let room = (h - 2 - 4 - foot - 1 - head).max(1);
    let shown = body.min(room);
    (shown + foot + 5 + head, shown as usize)
}

/// Where the card goes in a window of `w` by `h` cells: its frame, the first row of its head,
/// of its body, and of its foot, the first row of the body that shows, and the rows that show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub head_y: i32,
    pub body_y: i32,
    pub foot_y: i32,
    pub top: usize,
    pub visible: usize,
}

fn place(card: &NeedsCard, parts: &Parts, cw: i32, ch: i32) -> Place {
    let w = WIDTH.min(cw - 2);
    let (h, visible) = layout(ch, parts);
    let h = h.min(ch - 2);
    let x = (cw - w) / 2;
    let y = (ch - h) / 2;
    let total = parts.body.rows.len();
    let top = card.scroll.min(total.saturating_sub(visible));
    let head_y = y + 2;
    let body_y = head_y + head_rows(parts);
    Place {
        x,
        y,
        w,
        h,
        head_y,
        body_y,
        foot_y: body_y + visible as i32 + 1,
        top,
        visible,
    }
}

/// The parts of the form that shows which a click can hit, each with its column, its row, and
/// its width in the window. A part that the scroll hides is not in the list.
pub fn hits(app: &App) -> Vec<(Target, i32, i32, i32)> {
    let Some(crate::app::model::Card::Need(card)) = &app.card else {
        return Vec::new();
    };
    let parts = parts(app, card);
    let p = place(card, &parts, i32::from(app.size.0), i32::from(app.size.1));
    parts
        .hits
        .iter()
        .filter_map(|hit| {
            let y = match hit.area {
                Area::Head => p.head_y + hit.row as i32,
                Area::Body => {
                    if hit.row < p.top || hit.row >= p.top + p.visible {
                        return None;
                    }
                    p.body_y + (hit.row - p.top) as i32
                }
                Area::Foot => p.foot_y + hit.row as i32,
            };
            let w = hit.w.min(p.w - hit.x - 2);
            (w > 0).then_some((hit.target, p.x + hit.x, y, w))
        })
        .collect()
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
            ..Parts::default()
        };
    };
    let mut parts = match item {
        NeedItem::Questions(set) => ask_card::parts(app, card, set),
        NeedItem::Plan(plan) => other_cards::plan(app, card, plan),
        NeedItem::Approval(write) => write_card::approval(app, card, write),
        NeedItem::FirstWrite(write) => write_card::first_write(app, card, write),
        NeedItem::Flag(flag) => other_cards::flag(app, card, flag),
        NeedItem::Paused(flag) => other_cards::paused(app, card, flag),
        NeedItem::Proposal(proposal) => other_cards::proposal(app, card, proposal),
        NeedItem::Notice(notice) => other_cards::notice(app, card, notice),
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
        let added = body.rows.len();
        body.rows.append(&mut parts.body.rows);
        parts.body = body;
        for hit in parts.hits.iter_mut().filter(|h| h.area == Area::Body) {
            hit.row += added;
        }
    }
    let owner = Owner::Need(item.id());
    if let Some(chain) = app.chain_of(&owner) {
        let mut foot = Rows::default();
        chain_rows::rows(app, chain, &mut foot, TEXT);
        foot.blank();
        let added = foot.rows.len();
        if chain.running() {
            foot.keys(&[("esc", "close this card; the action goes on")]);
            parts.hits.retain(|h| h.area != Area::Foot);
        } else {
            foot.rows.append(&mut parts.foot.rows);
            for hit in parts.hits.iter_mut().filter(|h| h.area == Area::Foot) {
                hit.row += added;
            }
        }
        parts.foot = foot;
    }
    parts
}

pub fn draw(app: &App, card: &NeedsCard, c: &mut Canvas) {
    let parts = parts(app, card);
    let p = place(card, &parts, c.w, c.h);
    let Place { x, y, w, h, .. } = p;
    c.dim_all();
    c.wipe(x, y, w, h, T.panel);
    c.frame(x, y, w, h, T.warn);
    c.put(
        x + 2,
        y,
        &format!(" {} ", cut(&parts.title, (w - 6) as usize)),
        ink(T.fg).bold(),
    );
    put_rows(c, x, p.head_y, w, &parts.head.rows);
    let total = parts.body.rows.len();
    let top = p.top;
    put_rows(
        c,
        x,
        p.body_y,
        w,
        &parts.body.rows[top..(top + p.visible).min(total)],
    );
    if top > 0 {
        c.right(x + w - 3, y, &format!(" ▲ {top} more "), ink(T.dim));
    }
    let below = total.saturating_sub(top + p.visible);
    if below > 0 {
        c.right(
            x + w - 3,
            p.foot_y - 1,
            &format!("▼ {below} more · ↑↓ scroll"),
            ink(T.dim),
        );
    }
    put_rows(c, x, p.foot_y, w, &parts.foot.rows);
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
