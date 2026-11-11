//! The tests of the app (`vision` with no arguments): the screens as text snapshots, the
//! colors, the update, the loader, the worker with a fake program, and one run against a demo
//! world with the built binary. No test needs a terminal.

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
#[path = "app/now.rs"]
mod now;
#[path = "app/snapshots.rs"]
mod snapshots;
#[path = "app/support.rs"]
mod support;
#[path = "app/update.rs"]
mod update;
#[path = "app/worker.rs"]
mod worker;
