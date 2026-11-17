//! The reads of the app against a temporary home with finished tickets. The local data and the
//! data of the live view are the same as the reads of all tickets gave them, the costs of all
//! tickets come from one read, the events of a timeline read once, and the loader reads again
//! only after a change: a write of the ledger, a file of a ticket, the file `off`, the
//! configuration, and the refs of the memory.

use rusqlite::params;
use serde_json::json;
use time::OffsetDateTime;
use vision::app::live::{StepLine, TicketData};
use vision::app::load;
use vision::app::load_ticket::{self, EventLines};
use vision::app::loader::State;
use vision::app::model::{Held, Local, Week};
use vision::app::msg::Msg;
use vision::app::needs::Draft;
use vision::ctx::Ctx;
use vision::ledger::turns::{NewTurn, TurnEnd, Window};
use vision::ledger::Event;
use vision::ledger::Ledger;
use vision::memory::{CuratorChange, Memory, Op};
use vision::paths::{Paths, TicketKey};
use vision::pipeline::commands::{diff_of, measures_at, report_at, report_of, timeline};
use vision::pipeline::state::{flags_of, Counts, NEEDS_YOU, PAUSED, STOPPED};
use vision::snapshot::{collect_needs, NeedKind};
use vision::status::StatusView;
use vision::util::time::rfc3339;

use super::fifo::Fifo;
use super::pipeline_support::{simple_scenarios, World, KEY};

/// The finished tickets of the home: the first half finished a day ago, the rest a month ago.
const FINISHED: usize = 6;
/// A ticket in progress with no clone.
const OTHER: &str = "DEMO-3000";

/// A home with DEMO-12 at the end of its plan step, the finished tickets, and the ticket
/// `OTHER`. Each one has a copy of the rows of DEMO-12.
fn home() -> World {
    let world = World::new();
    simple_scenarios(&world);
    world.run(&["take", KEY]).expect_code(0);
    world.tick_to("plan-ready");
    let ledger = world.ledger();
    let now = OffsetDateTime::now_utc();
    for n in 0..FINISHED {
        let days = if n < FINISHED / 2 { 1 } else { 30 };
        let at = rfc3339(now - std::time::Duration::from_secs(days * 24 * 3600));
        copy_ticket(&ledger, &format!("DEMO-{}", 2000 + n), Some(&at));
    }
    copy_ticket(&ledger, OTHER, None);
    world
}

/// Copies DEMO-12 and each of its rows as the ticket `to`, with no clone. A finished copy has
/// the time of its end step. A text key of a row gets the new key, and an integer key a new
/// number.
fn copy_ticket(ledger: &Ledger, to: &str, finished: Option<&str>) {
    let conn = ledger.conn();
    let row = ledger.ticket(KEY).unwrap().unwrap();
    let mut counts: serde_json::Value =
        serde_json::from_str(row.counts.as_deref().unwrap_or("{}")).unwrap();
    if let Some(at) = finished {
        counts["finished"] = json!(at);
    }
    conn.execute(
        "INSERT INTO tickets (key, state, flags, clone, branch, pr, head, taken, updated, counts, title)
         SELECT ?1, state, flags, NULL, branch, pr, head, taken, updated, ?2, title FROM tickets WHERE key = ?3",
        params![to, counts.to_string(), KEY],
    )
    .unwrap();
    let tables: [(&str, &[&str]); 7] = [
        ("turns", &["id"]),
        ("events", &["external_id"]),
        ("spans", &[]),
        ("questions", &["id"]),
        ("human", &[]),
        ("operations", &["id", "idem"]),
        ("checks", &[]),
    ];
    for (table, keys) in tables {
        let mut stmt = conn
            .prepare("SELECT name, type, pk FROM pragma_table_info(?1)")
            .unwrap();
        let columns: Vec<(String, String, i64)> = stmt
            .query_map([table], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .filter(|(_, kind, pk)| !(*pk == 1 && kind == "INTEGER"))
            .collect();
        let names: Vec<&str> = columns.iter().map(|(name, _, _)| name.as_str()).collect();
        let values: Vec<String> = names
            .iter()
            .map(|name| match *name {
                "ticket" => "?1".to_string(),
                "id" if table == "turns" => "replace(id, ?2, ?1)".to_string(),
                name if keys.contains(&name) => format!("{name} || '-' || ?1"),
                name => name.to_string(),
            })
            .collect();
        conn.execute(
            &format!(
                "INSERT INTO {table} ({}) SELECT {} FROM {table} WHERE ticket = ?2",
                names.join(", "),
                values.join(", ")
            ),
            params![to, KEY],
        )
        .unwrap();
    }
}

/// Compares a read of the new way with the read of the old way. The old read runs again after
/// the new one when they differ, so that the test holds when a second or a minute ends between
/// them.
fn same_as_old<T: PartialEq + std::fmt::Debug>(
    mut new: impl FnMut() -> T,
    old: impl Fn() -> T,
    what: &str,
) {
    let before = old();
    let got = new();
    if got == before {
        return;
    }
    assert_eq!(got, old(), "{what}");
}

/// The local data as the app read it before: the status view and the facts of each ticket,
/// then only the tickets in progress.
fn old_local(paths: &Paths, wall: OffsetDateTime) -> Local {
    let ctx = Ctx::new(paths.clone());
    let view = StatusView::read(&ctx).unwrap();
    let ledger = ctx.ledger().unwrap();
    let rows = ledger.tickets().unwrap();
    let since = rfc3339(wall - std::time::Duration::from_secs(7 * 24 * 3600));
    let mut facts = vision::pipeline::facts::state_facts(&ctx, &ledger).unwrap();
    let mut tickets = Vec::new();
    let mut agents: Vec<String> = Vec::new();
    for ticket in view.tickets {
        let Some(row) = rows.iter().find(|r| r.key == ticket.key) else {
            continue;
        };
        if row.state.as_deref().is_none_or(str::is_empty) || Counts::of(row).finished.is_some() {
            continue;
        }
        let flags = flags_of(row);
        if let Some(works) = &ticket.works {
            if !agents.contains(&works.agent) {
                agents.push(works.agent.clone());
            }
        }
        let at = facts.iter().position(|f| f.key == ticket.key);
        tickets.push(Held {
            facts: at.map(|i| facts.swap_remove(i)).unwrap_or_default(),
            stopped: flags.iter().any(|f| f == STOPPED),
            needs_you: ticket
                .needs
                .keys()
                .any(|k| k != vision::pipeline::view::NOTICE)
                || flags.iter().any(|f| f == NEEDS_YOU || f == PAUSED),
            taken: row.taken.clone(),
            view: ticket,
        });
    }
    let finished = rows
        .iter()
        .filter(|row| {
            Counts::of(row)
                .finished
                .is_some_and(|at| at.as_str() >= since.as_str())
        })
        .count();
    let report = report_of(&ledger, Some(&since)).unwrap();
    let total = &report.total;
    let turns = vision::turn::running_turns(&ctx, &ledger).unwrap().len();
    let now = rfc3339(wall);
    let reset = |window| {
        ledger
            .active_reading(window, &now)
            .unwrap()
            .and_then(|r| r.resets)
    };
    Local {
        off: view.off,
        limits: view.limits,
        needs: view.needs,
        tickets,
        turns,
        agents,
        week: Week {
            cost_usd: total.cost_usd,
            finished,
            without_you: total.cards_by_memory + total.cards_by_judge,
            questions: total.cards_by_memory + total.cards_by_judge + total.cards_to_person,
        },
        timer: vision::install::is_installed(paths),
        running: view.running,
        resets: [reset(Window::FiveHour), reset(Window::SevenDay)],
    }
}

/// The data of the live view as the app read it before: the row of the ticket from the rows of
/// all tickets, its measures from the report of all tickets, and its timeline from each event.
/// The steps of the adapter, with their kinds, come from `steps`. The team and the jobs are
/// left out: the same code reads them both ways.
fn old_ticket(paths: &Paths, key: &str, steps: &[(String, String)]) -> TicketData {
    let ctx = Ctx::new(paths.clone());
    let ledger = ctx.ledger().unwrap();
    let ticket_key = TicketKey::parse(key).unwrap();
    let needs = collect_needs(&ctx, &ledger).unwrap();
    let view = vision::pipeline::status_rows(&ctx, &ledger, &needs)
        .unwrap()
        .into_iter()
        .find(|r| r.key == key)
        .unwrap();
    let facts = vision::pipeline::facts::state_facts_of(&ctx, &ledger, key)
        .unwrap()
        .unwrap_or_default();
    let measures = report_of(&ledger, None)
        .unwrap()
        .tickets
        .get(key)
        .cloned()
        .unwrap_or_default();
    let steps = steps
        .iter()
        .map(|(id, kind)| StepLine {
            id: id.clone(),
            kind: kind.clone(),
            entries: facts.entries.get(id).copied().unwrap_or(0),
            seconds: measures.step_seconds.get(id).copied().unwrap_or(0),
            cost_usd: measures.step_cost_usd.get(id).copied().unwrap_or(0.0),
        })
        .collect();
    let unpushed = if view.clone.is_some() && !facts.finished {
        diff_of(&ctx, &ticket_key, false)
            .ok()
            .map(|d| d.commits.len())
    } else {
        None
    };
    let plan = std::fs::read(paths.ticket_doc(&ticket_key, "plan.md"))
        .ok()
        .map(|bytes| {
            vision::pipeline::plan_hash(&bytes)[..vision::pipeline::SHORT_HASH].to_string()
        });
    TicketData {
        taken: ledger.ticket(key).unwrap().and_then(|r| r.taken),
        needs: needs
            .into_iter()
            .filter(|n| n.ticket.as_deref() == Some(key))
            .collect(),
        view,
        facts,
        steps,
        team: Vec::new(),
        timeline: timeline(&ledger, &ticket_key).unwrap(),
        measures,
        unpushed,
        plan,
        jobs: Vec::new(),
    }
}

/// The data with no team and no jobs, as [`old_ticket`] gives it.
fn masked(mut data: TicketData) -> TicketData {
    data.team.clear();
    data.jobs.clear();
    data
}

fn local_of(msg: Msg) -> Local {
    match msg {
        Msg::Local(Ok(local)) => local,
        other => panic!("no local data: {other:?}"),
    }
}

fn ticket_of(msg: Msg) -> TicketData {
    match msg {
        Msg::Ticket { data, .. } => (*data).unwrap(),
        other => panic!("no data of the live view: {other:?}"),
    }
}

#[test]
fn the_local_data_and_the_costs_are_the_same_as_the_reads_of_all_tickets() {
    let world = home();
    let paths = world.ctx().paths;
    let wall = OffsetDateTime::now_utc();
    same_as_old(
        || load::local(&paths, wall).unwrap(),
        || old_local(&paths, wall),
        "the read of the app",
    );
    let mut state = State::new(paths.clone());
    same_as_old(
        || local_of(state.local(wall, true)),
        || old_local(&paths, wall),
        "the read of the loader",
    );
    let local = load::local(&paths, wall).unwrap();
    let keys: Vec<&str> = local.tickets.iter().map(|h| h.view.key.as_str()).collect();
    assert_eq!(keys, [KEY, OTHER], "a finished ticket is not in progress");
    assert_eq!(
        local.week.finished,
        FINISHED / 2,
        "the tickets of the last 7 days"
    );

    // The costs of all tickets in one read are the costs of each ticket.
    let ledger = world.ledger();
    let costs = ledger.spent_by_ticket().unwrap();
    let rows = ledger.tickets().unwrap();
    assert_eq!(rows.len(), FINISHED + 2);
    for row in &rows {
        let one = ledger.spent_on_ticket(&row.key).unwrap();
        let all = costs.get(&row.key).copied().unwrap_or(0.0);
        assert!((one - all).abs() < 1e-9, "{}: {one} {all}", row.key);
        assert!(one > 0.0, "{} has the turns of DEMO-12", row.key);
    }

    // The measures of one ticket are those of the report of all tickets, for a reserved key too.
    let now = OffsetDateTime::now_utc();
    let report = report_at(&ledger, None, now).unwrap();
    for key in rows.iter().map(|r| r.key.as_str()).chain(["curator"]) {
        assert_eq!(
            measures_at(&ledger, key, now).unwrap(),
            report.tickets.get(key).cloned().unwrap_or_default(),
            "{key}"
        );
    }
}

#[test]
fn the_data_of_the_live_view_is_the_same_as_the_read_of_all_tickets() {
    let world = home();
    let paths = world.ctx().paths;
    let first = load_ticket::ticket(&paths, KEY).unwrap();
    let steps: Vec<(String, String)> = first
        .steps
        .iter()
        .map(|s| (s.id.clone(), s.kind.clone()))
        .collect();
    assert!(!steps.is_empty());
    assert!(first.unpushed.is_some(), "DEMO-12 holds a clone");
    assert!(!first.team.is_empty() && !first.timeline.is_empty());
    // The ticket with a clone, one in progress with none, and a finished one.
    for key in [KEY, OTHER, "DEMO-2000"] {
        same_as_old(
            || masked(load_ticket::ticket(&paths, key).unwrap()),
            || old_ticket(&paths, key, &steps),
            key,
        );
    }
    let mut state = State::new(paths.clone());
    for key in [KEY, OTHER, KEY] {
        same_as_old(
            || masked(ticket_of(state.ticket(key, true))),
            || old_ticket(&paths, key, &steps),
            key,
        );
    }
    // A ticket that is not there is an error.
    match state.ticket("DEMO-999", true) {
        Msg::Ticket { data, .. } => {
            let error = (*data).unwrap_err();
            assert!(error.contains("no ticket DEMO-999"), "{error}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_event_lines_read_each_event_once_and_give_the_same_timeline() {
    let world = home();
    let ledger = world.ledger();
    let key = TicketKey::parse(KEY).unwrap();
    let mut events = EventLines::new(KEY);
    events.update(&ledger, false).unwrap();
    let all = timeline(&ledger, &key).unwrap();
    assert_eq!(events.timeline(&ledger).unwrap(), all);
    let read = events.read();
    assert!(read > 0);

    // New events: one now, one at the time of the first line, one of another ticket, and the
    // start of a turn, which has no line.
    let first = all[0].at.clone();
    let now = vision::util::time::now_rfc3339();
    let new = [
        (now.clone(), KEY, "note"),
        (first.clone(), KEY, "note"),
        (now.clone(), OTHER, "note"),
        (now.clone(), KEY, "turn-started"),
    ];
    for (n, (at, ticket, kind)) in new.into_iter().enumerate() {
        let mut event = Event::now("user", kind)
            .ticket(ticket)
            .detail(json!({ "n": n }));
        event.at = at;
        ledger.add_event(&event).unwrap();
        events.update(&ledger, false).unwrap();
        assert_eq!(
            events.timeline(&ledger).unwrap(),
            timeline(&ledger, &key).unwrap(),
            "after event {n}"
        );
    }
    assert_eq!(events.read(), read + 3, "only the new events of the ticket");

    // A turn that ends in the second of an event: its line comes first, as in the sort of the
    // whole timeline.
    let id = "20261116T150000Z-DEMO-12-build-9";
    ledger
        .insert_turn(&NewTurn {
            id,
            ticket: KEY,
            agent: "builder",
            step: "build",
            kind: "build",
            session: "s-9",
            resumes: None,
            model: "claude-fable-5-2",
            effort: "high",
            started: &first,
            role_hash: "",
        })
        .unwrap();
    ledger
        .end_turn(
            id,
            &TurnEnd {
                status: "done".into(),
                reason: "result".into(),
                ended: now.clone(),
                session: None,
                cost_usd: Some(0.25),
                input: 1,
                cache_creation: 0,
                cache_read: 0,
                output: 1,
                denials: 0,
                five_hour: None,
                seven_day: None,
            },
        )
        .unwrap();
    events.update(&ledger, false).unwrap();
    let merged = events.timeline(&ledger).unwrap();
    assert_eq!(merged, timeline(&ledger, &key).unwrap());
    let ended = merged
        .iter()
        .position(|l| l.source == "turn" && l.kind == "ended" && l.at == now)
        .unwrap();
    assert_eq!(
        merged[ended + 1].at,
        now,
        "an event of the same second follows"
    );

    // An event that leaves the ledger: only the check of the count reads each event again.
    ledger
        .conn()
        .execute(
            "DELETE FROM events WHERE id = (SELECT min(id) FROM events WHERE ticket = ?1)",
            [KEY],
        )
        .unwrap();
    events.update(&ledger, false).unwrap();
    assert_ne!(
        events.timeline(&ledger).unwrap(),
        timeline(&ledger, &key).unwrap()
    );
    events.update(&ledger, true).unwrap();
    assert_eq!(
        events.timeline(&ledger).unwrap(),
        timeline(&ledger, &key).unwrap()
    );
    assert_eq!(events.read(), read + 2);
}

#[test]
fn the_loader_reads_again_only_after_a_change() {
    let world = home();
    let paths = world.ctx().paths;
    let wall = OffsetDateTime::now_utc();
    let mut state = State::new(paths.clone());
    let reads = |state: &State| state.reads().local;

    // The first look reads. Its check did not name the tickets that the read shows, so it
    // has no stamps of their folders from before the read: the next look reads once more.
    // After it, a look with no change gives a small message.
    local_of(state.local(wall, false));
    assert_eq!(reads(&state), 1);
    local_of(state.local(wall, false));
    assert_eq!(reads(&state), 2);
    assert_eq!(state.local(wall, false), Msg::LocalSame);
    assert_eq!(reads(&state), 2);
    // A look of an update with no data reads whatever the check says.
    local_of(state.local(wall, true));
    assert_eq!(reads(&state), 3);

    // A write of another connection to the ledger.
    world
        .ledger()
        .add_event(&Event::now("user", "note").ticket(OTHER))
        .unwrap();
    local_of(state.local(wall, false));
    assert_eq!(reads(&state), 4);
    assert_eq!(state.local(wall, false), Msg::LocalSame);

    // A file of a ticket in progress, such as its plan.
    let other = TicketKey::parse(OTHER).unwrap();
    std::fs::create_dir_all(paths.ticket_dir(&other)).unwrap();
    std::fs::write(paths.ticket_doc(&other, "plan.md"), "# Plan\n").unwrap();
    local_of(state.local(wall, false));
    assert_eq!(reads(&state), 5);
    assert_eq!(state.local(wall, false), Msg::LocalSame);

    // A file that no screen reads changes nothing.
    std::fs::write(paths.home().join("notes.txt"), "x").unwrap();
    assert_eq!(state.local(wall, false), Msg::LocalSame);

    // The file off.
    std::fs::write(paths.off(), "").unwrap();
    assert!(local_of(state.local(wall, false)).off);
    assert_eq!(state.local(wall, false), Msg::LocalSame);

    // The configuration: a new context reads it once.
    let contexts = state.reads().config;
    world.edit_config(|doc| doc["limits"]["five_hour"] = toml_edit::value(0.5));
    let local = local_of(state.local(wall, false));
    assert_eq!(state.reads().config, contexts + 1);
    let before = load::local(&paths, wall).unwrap();
    assert_eq!(local.limits, before.limits);
    assert_eq!(state.local(wall, false), Msg::LocalSame);
    assert_eq!(state.reads().config, contexts + 1);

    // The live view: the same rules for one ticket.
    let data = ticket_of(state.ticket(KEY, false));
    assert_eq!(state.reads().ticket, 1);
    assert_eq!(
        state.ticket(KEY, false),
        Msg::TicketSame { key: KEY.into() }
    );
    ticket_of(state.ticket(KEY, true));
    assert_eq!(state.reads().ticket, 2);
    // A write to the ledger: its line shows.
    world
        .ledger()
        .add_event(
            &Event::now("user", "note")
                .ticket(KEY)
                .detail(json!({ "text": "a new line" })),
        )
        .unwrap();
    let after = ticket_of(state.ticket(KEY, false));
    assert_eq!(after.timeline.len(), data.timeline.len() + 1);
    assert!(after.timeline.iter().any(|l| l.text.contains("a new line")));
    assert_eq!(
        state.ticket(KEY, false),
        Msg::TicketSame { key: KEY.into() }
    );
    // A write of the plan of the ticket: its new hash shows.
    let key = TicketKey::parse(KEY).unwrap();
    let plan = paths.ticket_doc(&key, "plan.md");
    let text = std::fs::read_to_string(&plan).unwrap();
    std::fs::write(&plan, format!("{text}\nOne more line.\n")).unwrap();
    let changed = ticket_of(state.ticket(KEY, false));
    assert_ne!(changed.plan, after.plan);
    assert_eq!(
        state.ticket(KEY, false),
        Msg::TicketSame { key: KEY.into() }
    );
    // A write to another ticket in the ledger changes the data version: the view reads again,
    // and its data stays the same.
    world
        .ledger()
        .add_event(&Event::now("user", "note").ticket(OTHER))
        .unwrap();
    let same = ticket_of(state.ticket(KEY, false));
    assert_eq!(masked(same), masked(changed));
    // Another ticket reads at once.
    ticket_of(state.ticket(OTHER, false));
    assert_eq!(state.reads().ticket, 6);
    // After the view closed, the loader keeps nothing of it: the next view reads whole.
    state.live_closed();
    ticket_of(state.ticket(OTHER, false));
    assert_eq!(state.reads().ticket, 7);
    assert_eq!(
        state.ticket(OTHER, false),
        Msg::TicketSame { key: OTHER.into() }
    );
    // Home still sees the writes of the ledger that came meanwhile.
    local_of(state.local(wall, false));
}

#[test]
fn a_write_of_the_files_of_the_question_form_changes_no_check() {
    let world = home();
    let paths = world.ctx().paths;
    let wall = OffsetDateTime::now_utc();
    let key = TicketKey::parse(KEY).unwrap();
    let mut state = State::new(paths.clone());
    local_of(state.local(wall, false));
    local_of(state.local(wall, false));
    assert_eq!(state.local(wall, false), Msg::LocalSame);
    ticket_of(state.ticket(KEY, false));
    let same = Msg::TicketSame { key: KEY.into() };
    assert_eq!(state.ticket(KEY, false), same);
    // A draft of the form, a write of the drafts file that has not ended, and the file of a
    // send: only the form reads them, so Home and the live view read nothing again.
    let drafts = [("Q-0001".to_string(), Draft::Needless("code".into()))];
    vision::app::drafts::save(&paths, KEY, &drafts).unwrap();
    let temp = paths.ticket_doc(&key, &format!(".{}.1-1.tmp", vision::app::drafts::FILE));
    std::fs::write(&temp, "{").unwrap();
    let send = paths.ticket_doc(&key, vision::app::drafts::SEND_FILE);
    std::fs::write(&send, "{}").unwrap();
    assert_eq!(state.local(wall, false), Msg::LocalSame);
    assert_eq!(state.ticket(KEY, false), same);
    vision::app::drafts::save(&paths, KEY, &[]).unwrap();
    std::fs::remove_file(&temp).unwrap();
    std::fs::remove_file(&send).unwrap();
    assert_eq!(state.local(wall, false), Msg::LocalSame);
    assert_eq!(state.ticket(KEY, false), same);
    assert_eq!(state.reads().local, 2);
    assert_eq!(state.reads().ticket, 1);
    // Another file of the folder still counts.
    std::fs::write(paths.ticket_doc(&key, "notes.md"), "x").unwrap();
    local_of(state.local(wall, false));
    ticket_of(state.ticket(KEY, false));
}

#[test]
fn a_role_file_of_the_adapter_that_goes_away_shows_at_the_next_look() {
    let world = home();
    let paths = world.ctx().paths;
    // The private adapter defines an agent, with its role file in the adapter folder.
    world.write_adapter(|text| {
        format!("{text}\n[agents.lane]\nmodel = \"claude-sonnet-5-5\"\neffort = \"high\"\n")
    });
    let role = paths.home().join("adapters/demo/roles/lane.md");
    std::fs::create_dir_all(role.parent().unwrap()).unwrap();
    std::fs::write(&role, "You read the diff.\n").unwrap();
    let loads = |paths: &Paths| {
        let ctx = Ctx::new(paths.clone());
        vision::adapter::load_with(ctx.config().unwrap(), paths).is_ok()
    };
    assert!(loads(&paths));
    let mut state = State::new(paths.clone());
    let same = Msg::TicketSame { key: KEY.into() };
    assert!(!ticket_of(state.ticket(KEY, false)).steps.is_empty());
    assert_eq!(state.ticket(KEY, false), same);
    // The role file goes away: a command no longer loads the adapter, and the live view shows
    // no path, as a read with a new context does.
    std::fs::remove_file(&role).unwrap();
    assert!(!loads(&paths));
    let next = state.ticket(KEY, false);
    assert_ne!(next, same, "the loader keeps the adapter of before");
    let data = ticket_of(next);
    assert!(data.steps.is_empty(), "{:?}", data.steps);
    assert!(load_ticket::ticket(&paths, KEY).unwrap().steps.is_empty());
    // The role file comes back, and so does the path.
    std::fs::write(&role, "You read the diff.\n").unwrap();
    assert!(!ticket_of(state.ticket(KEY, false)).steps.is_empty());
    assert_eq!(state.ticket(KEY, false), same);
}

/// The home of [`home`], where the plan of `OTHER`, which waits for its approval, is a FIFO.
/// Each read of Home opens it once, after the read began, and the check only looks at its
/// stamp. At its first open, `during` runs: in the middle of the first read.
fn home_with_plan_fifo(during: impl FnOnce(&Paths) + Send + 'static) -> (World, Fifo) {
    let world = home();
    let paths = world.ctx().paths;
    let other = TicketKey::parse(OTHER).unwrap();
    std::fs::create_dir_all(paths.ticket_dir(&other)).unwrap();
    let mut during = Some(during);
    let at = paths.clone();
    let fifo = Fifo::serve(&paths.ticket_doc(&other, "plan.md"), move |n| {
        if n == 1 {
            if let Some(during) = during.take() {
                during(&at);
            }
        }
    });
    (world, fifo)
}

#[test]
fn a_write_of_the_ledger_in_the_middle_of_a_read_of_new_tickets_shows_at_the_next_look() {
    let (world, _plan) = home_with_plan_fifo(|paths| {
        Ledger::open(&paths.ledger())
            .unwrap()
            .conn()
            .execute(
                "UPDATE tickets SET flags = '[\"stopped\"]' WHERE key = ?1",
                [OTHER],
            )
            .unwrap();
    });
    let paths = world.ctx().paths;
    let wall = OffsetDateTime::now_utc();
    let stopped = |local: &Local| {
        local
            .tickets
            .iter()
            .find(|h| h.view.key == OTHER)
            .map(|h| h.stopped)
    };
    let mut state = State::new(paths.clone());
    // The first look knows no ticket yet, so the tickets of its read are new to it. Another
    // connection stops OTHER after the read began.
    let first = local_of(state.local(wall, false));
    assert_eq!(
        stopped(&first),
        Some(false),
        "the read began before the stop"
    );
    // The read did not see the stop, so the next look reads again and shows it.
    let next = state.local(wall, false);
    assert_ne!(next, Msg::LocalSame, "the stop counts as seen");
    assert_eq!(stopped(&local_of(next)), Some(true));
}

#[test]
fn a_write_of_a_file_in_the_middle_of_a_read_of_new_tickets_shows_at_the_next_look() {
    let key = TicketKey::parse(KEY).unwrap();
    let new_plan = "# Plan\n\nA new plan with more lines than the plan before.\n";
    let (world, _plan) = home_with_plan_fifo(move |paths| {
        std::fs::write(paths.ticket_doc(&key, "plan.md"), new_plan).unwrap();
    });
    let paths = world.ctx().paths;
    let wall = OffsetDateTime::now_utc();
    let key = TicketKey::parse(KEY).unwrap();
    let hash_of =
        |text: &[u8]| vision::pipeline::plan_hash(text)[..vision::pipeline::SHORT_HASH].to_string();
    let old_hash = hash_of(&std::fs::read(paths.ticket_doc(&key, "plan.md")).unwrap());
    let plan_of = |local: &Local| {
        local
            .needs
            .iter()
            .find(|n| n.kind == NeedKind::Plan && n.ticket.as_deref() == Some(KEY))
            .map(|n| n.id.clone())
    };
    let mut state = State::new(paths.clone());
    // The read of the first look reads the plan of DEMO-12 before the plan of OTHER, and the
    // plan of DEMO-12 changes while the read waits at the plan of OTHER.
    let first = local_of(state.local(wall, false));
    assert_eq!(
        plan_of(&first),
        Some(old_hash),
        "the read began before the write"
    );
    let next = state.local(wall, false);
    assert_ne!(next, Msg::LocalSame, "the new plan counts as seen");
    assert_eq!(plan_of(&local_of(next)), Some(hash_of(new_plan.as_bytes())));
}

#[test]
fn the_proposals_of_the_memory_show_after_a_change_of_its_refs() {
    let world = World::new();
    let ctx = world.ctx();
    let paths = ctx.paths.clone();
    let wall = OffsetDateTime::now_utc();
    let mut state = State::new(paths.clone());
    let proposals = |local: &Local| {
        local
            .needs
            .iter()
            .filter(|n| n.kind == NeedKind::Proposal)
            .count()
    };
    let local = local_of(state.local(wall, false));
    assert_eq!(proposals(&local), 0);
    assert_eq!(state.reads().memory, 1);
    assert_eq!(state.local(wall, false), Msg::LocalSame);
    assert_eq!(state.reads().memory, 1, "no git command without a change");

    // The Curator makes a proposal: the refs of the memory change, and the next look shows it.
    let memory = Memory::from_ctx(&ctx).unwrap();
    let built = memory
        .build_proposal(
            "Ask about the export",
            &[CuratorChange {
                op: Op::Question,
                path: None,
                content: None,
                removes: vec![],
                target: None,
                question: Some("Do internal users ever need XLSX?".into()),
                evidence: vec![],
                reason: "No decision has a case for XLSX.".into(),
            }],
            vision::util::time::now().date(),
        )
        .unwrap();
    assert!(built.proposal.is_some());
    let local = local_of(state.local(wall, false));
    assert_eq!(proposals(&local), 1, "{:?}", local.needs);
    assert_eq!(state.reads().memory, 2);
    assert_eq!(local, load::local(&paths, wall).unwrap());
    assert_eq!(state.local(wall, false), Msg::LocalSame);
    assert_eq!(state.reads().memory, 2);

    // After a pass, an action, or a session ended, the next look reads the memory again. The
    // same list gives no new read of the data.
    state.memory_changed();
    assert_eq!(state.local(wall, false), Msg::LocalSame);
    assert_eq!(state.reads().memory, 3);
    assert_eq!(state.reads().local, 2);
}
