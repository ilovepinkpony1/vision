//! The tests of the app (`vision` with no arguments): the screens and the cards as text
//! snapshots, the colors, the update, the loader and its picture, the worker with a fake
//! program, and runs against a demo world with the built binary: the live view, the cards of
//! the things that wait with their chains, and the reads of Models and Stats. No test needs a
//! terminal.

#[path = "pipeline_support/mod.rs"]
mod pipeline_support;

#[path = "app/boot.rs"]
mod boot;
#[path = "app/colors.rs"]
mod colors;
#[path = "app/e2e.rs"]
mod e2e;
#[path = "app/feed.rs"]
mod feed;
#[path = "app/input.rs"]
mod input;
#[path = "app/live.rs"]
mod live;
#[path = "app/live_colors.rs"]
mod live_colors;
#[path = "app/live_support.rs"]
mod live_support;
#[path = "app/live_update.rs"]
mod live_update;
#[path = "app/need_cards.rs"]
mod need_cards;
#[path = "app/need_support.rs"]
mod need_support;
#[path = "app/need_update.rs"]
mod need_update;
#[path = "app/needs_e2e.rs"]
mod needs_e2e;
#[path = "app/now.rs"]
mod now;
#[path = "app/picture.rs"]
mod picture;
#[path = "app/screens.rs"]
mod screens;
#[path = "app/screens_data.rs"]
mod screens_data;
#[path = "app/snapshots.rs"]
mod snapshots;
#[path = "app/support.rs"]
mod support;
#[path = "app/take_over.rs"]
mod take_over;
#[path = "app/update.rs"]
mod update;
#[path = "app/worker.rs"]
mod worker;
