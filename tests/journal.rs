//! The journal of the turns (section 16): the entry of a turn, its index in a fake MemPalace,
//! the search, and the commands. No test reaches the real MemPalace: the server here is a fake
//! on a free localhost port.

mod common;
#[allow(dead_code)]
#[path = "journal_support/palace.rs"]
mod palace;
#[allow(dead_code)]
#[path = "jira_github_support/server.rs"]
mod server;

use common::{Home, Run};
use palace::{dead_url, fake_palace};
use serde_json::{json, Value};
use vision::ledger::questions::NewQuestion;
use vision::ledger::turns::{NewTurn, TurnEnd};
use vision::ledger::{Event, Ledger, TicketRow};
use vision::paths::{TicketKey, TurnId};
use vision::util::time;

const SECRET_VALUE: &str = "journal-fake-secret-5f6e7d8c";
const SECRET_ENV: (&str, &str) = ("DEMO_TOKEN_SOURCE", SECRET_VALUE);

fn edit_config(home: &Home, change: impl FnOnce(&mut toml_edit::DocumentMut)) {
    let path = home.path("config.toml");
    let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    change(&mut doc);
    std::fs::write(&path, doc.to_string()).unwrap();
}

/// A home with one secret, whose value comes from [`SECRET_ENV`].
fn home_with_secret() -> Home {
    let home = Home::init();
    edit_config(&home, |doc| {
        let mut secret = toml_edit::InlineTable::new();
        secret.insert("env", SECRET_ENV.0.into());
        doc["secrets"]["DEMO_TOKEN"] = toml_edit::value(secret);
    });
    home
}

fn run(home: &Home, args: &[&str]) -> Run {
    home.run_with(args, &[SECRET_ENV])
}

fn ledger(home: &Home) -> Ledger {
    Ledger::open(&home.path("ledger.sqlite")).unwrap()
}

/// The stream of a turn: a test that fails, a commit, and the same test that passes.
fn stream_with_a_fix() -> String {
    let call = |id: &str, command: &str| {
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}]}})
        .to_string()
    };
    let answer = |id: &str, content: &str, is_error: bool| {
        json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error}]}})
        .to_string()
    };
    [
        json!({"type": "system", "subtype": "init", "session_id": "s", "tools": [], "mcp_servers": []}).to_string(),
        json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "e1", "name": "Edit", "input": {"file_path": "/clone/calc.py"}}]}})
        .to_string(),
        call("b1", "python3 -m unittest test_calc"),
        answer(
            "b1",
            &format!("Exit code 1\nE\nERROR: test_subtract (test_calc.SubtractTest)\nNameError: name 'subtract' is not defined\ntoken {SECRET_VALUE}"),
            true,
        ),
        call("b2", "git add calc.py && git commit -m 'Add subtract'"),
        answer("b2", "[vision/DEMO-12 9f3e21c] Add subtract\n 1 file changed", false),
        call("b3", "python3 -m unittest test_calc"),
        answer("b3", "OK", false),
    ]
    .join("\n")
}

/// Adds a ticket row when it is missing, and one ended turn with its folder, its result, and
/// its stream. Gives the turn id.
#[allow(clippy::too_many_arguments)]
fn add_turn(
    home: &Home,
    key: &str,
    step: &str,
    n: u32,
    started: &str,
    ended: Option<&str>,
    result: Value,
    stream: &str,
) -> String {
    let ledger = ledger(home);
    let ticket = TicketKey::parse(key).unwrap();
    // A ticket with no step: the pipeline leaves it alone.
    if !ticket.is_reserved() && ledger.ticket(key).unwrap().is_none() {
        ledger
            .put_ticket(&TicketRow {
                key: key.into(),
                title: Some("Export the report".into()),
                ..TicketRow::default()
            })
            .unwrap();
    }
    let id = TurnId::new(time::parse_rfc3339(started).unwrap(), &ticket, step, n)
        .unwrap()
        .to_string();
    ledger
        .insert_turn(&NewTurn {
            id: &id,
            ticket: key,
            agent: "builder",
            step,
            kind: "worker",
            session: &format!("session-{id}"),
            resumes: None,
            model: "claude-opus-5-5",
            effort: "xhigh",
            started,
            role_hash: "role",
        })
        .unwrap();
    if let Some(ended) = ended {
        ledger
            .end_turn(
                &id,
                &TurnEnd {
                    status: "done".into(),
                    reason: "result".into(),
                    ended: ended.into(),
                    cost_usd: Some(0.5),
                    ..TurnEnd::default()
                },
            )
            .unwrap();
    }
    let turn = home.paths().turn(&ticket, &TurnId::parse(&id).unwrap());
    std::fs::create_dir_all(&turn.dir).unwrap();
    std::fs::write(&turn.result, result.to_string()).unwrap();
    std::fs::write(&turn.stream, stream).unwrap();
    id
}

fn minutes_ago(minutes: i64) -> String {
    time::rfc3339(time::now() - ::time::Duration::minutes(minutes))
}

fn result_with_notes() -> Value {
    json!({"kind": "result", "summary": format!("Added subtract to calc.py. The secret {SECRET_VALUE} stays out."),
    "notes": [
        {"kind": "problem", "text": "test_subtract failed with a NameError.", "files": ["test_calc.py"]},
        {"kind": "fix", "text": "Define subtract in calc.py.", "files": ["calc.py"], "commit": "9f3e21c"},
        {"kind": "command", "text": "python3 -m unittest test_calc runs one test file."}
    ]})
}

fn entry_text(home: &Home, key: &str, id: &str) -> String {
    home.read(&format!("journal/{key}/{id}.md"))
}

#[test]
fn rebuild_writes_one_entry_for_each_turn_of_a_ticket_and_masks_each_secret() {
    let home = home_with_secret();
    let build = add_turn(
        &home,
        "DEMO-12",
        "build",
        1,
        &minutes_ago(30),
        Some(&minutes_ago(20)),
        result_with_notes(),
        &stream_with_a_fix(),
    );
    // A card of the turn with the answer of the person.
    let mut l = ledger(&home);
    let ids = l
        .add_questions(
            &[NewQuestion {
                ticket: "DEMO-12",
                turn: &build,
                card: &json!({"text": "CSV or XLSX?", "options": [{"label": "CSV"}, {"label": "XLSX"}]}).to_string(),
                created: &minutes_ago(19),
            }],
            &Event::now("tick", "intake").ticket("DEMO-12"),
        )
        .unwrap();
    l.conn()
        .execute(
            "UPDATE questions SET state = 'answered', decision = 'D-0001', answer = ?2 WHERE id = ?1",
            rusqlite::params![
                ids[0],
                json!({"choice": 0, "text": "CSV", "by": "user", "why": "Finance imports CSV.", "when": ""}).to_string()
            ],
        )
        .unwrap();
    let plan = add_turn(
        &home,
        "DEMO-12",
        "plan",
        1,
        &minutes_ago(60),
        Some(&minutes_ago(50)),
        json!({"kind": "result", "summary": "Planned the export."}),
        "",
    );
    let curator = add_turn(
        &home,
        "curator",
        "curate",
        1,
        &minutes_ago(40),
        Some(&minutes_ago(39)),
        json!({"changes": []}),
        "",
    );
    let running = add_turn(
        &home,
        "DEMO-12",
        "review",
        1,
        &minutes_ago(5),
        None,
        json!(null),
        "",
    );

    let out = run(&home, &["journal", "rebuild"]);
    out.expect_code(0);
    assert!(
        out.stdout.starts_with("2 entr(y/ies) written, 0 unchanged, 2 turn(s) with no entry, 0 failed. The journal holds 2 entr(y/ies)"),
        "{out:?}"
    );
    assert!(!home.path(&format!("journal/curator/{curator}.md")).exists());
    assert!(!home.path(&format!("journal/DEMO-12/{running}.md")).exists());

    let text = entry_text(&home, "DEMO-12", &build);
    assert!(!text.contains(SECRET_VALUE), "the secret is masked: {text}");
    assert!(text.contains("[secret DEMO_TOKEN]"), "{text}");
    assert!(text.starts_with(&format!("---\nid: {build}\nticket: DEMO-12\nstep: build\nagent: builder\nmodel: claude-opus-5-5\n")), "{text}");
    assert!(text.contains("\noutcome: result\n"), "{text}");
    assert!(text.contains("\nfiles: [/clone/calc.py]\n"), "{text}");
    assert!(text.contains("\ncost: 0.5000\n"), "{text}");
    assert!(
        text.contains("\n## Summary\n\nAdded subtract to calc.py."),
        "{text}"
    );
    assert!(text.contains("\n## Notes\n\n- problem: test_subtract failed with a NameError. (files: test_calc.py)\n- fix: Define subtract in calc.py. (files: calc.py; commit: 9f3e21c)\n- command: python3 -m unittest test_calc runs one test file.\n"), "{text}");
    assert!(text.contains("\n## Errors and fixes\n\n- `python3 -m unittest test_calc` failed (exit 1): ERROR: test_subtract (test_calc.SubtractTest) | NameError: name 'subtract' is not defined Passed later, after the commit 9f3e21c.\n"), "{text}");
    assert!(text.contains(&format!("\n## Answers\n\n- {} (D-0001): CSV or XLSX? Answer: CSV (by user). Why: Finance imports CSV.\n", ids[0])), "{text}");
    assert!(
        text.contains("\n## Outcome\n\n- status done, reason result, result result\n"),
        "{text}"
    );
    assert!(!text.contains("## Gate results"), "{text}");

    let plan_text = entry_text(&home, "DEMO-12", &plan);
    assert!(!plan_text.contains("## Notes"), "{plan_text}");
    assert!(!plan_text.contains("## Errors and fixes"), "{plan_text}");

    // A second run writes the same files.
    let again = run(&home, &["journal", "rebuild"]);
    again.expect_code(0);
    assert!(
        again
            .stdout
            .starts_with("0 entr(y/ies) written, 2 unchanged"),
        "{again:?}"
    );
    assert_eq!(entry_text(&home, "DEMO-12", &build), text);

    // Show and the local search.
    let show = run(&home, &["journal", "show", &build]);
    show.expect_code(0);
    assert_eq!(show.stdout, text);
    run(&home, &["journal", "show", "20261116T143000Z-NONE-1-x-1"]).expect_code(1);
    let found = run(
        &home,
        &["journal", "search", "NameError subtract", "--json"],
    );
    found.expect_code(0);
    let found: Value = serde_json::from_str(&found.stdout).unwrap();
    assert_eq!(found["source"], "local");
    assert_eq!(found["hits"][0]["id"], json!(build));
    assert_eq!(found["hits"].as_array().unwrap().len(), 1, "{found}");
    let plain = run(&home, &["journal", "search", "builder"]);
    plain.expect_code(0);
    assert!(plain.stdout.contains("from the entry files"), "{plain:?}");
    // Both entries hold the word, and the newest comes first.
    let build_at = plain.stdout.find(&build).unwrap();
    let plan_at = plain.stdout.find(&plan).unwrap();
    assert!(build_at < plan_at, "{plain:?}");

    // With no value of the secret, no entry is written: the masking needs it.
    let bare = home.run(&["journal", "rebuild"]);
    assert_eq!(bare.code, 1, "{bare:?}");
    assert!(bare.stderr.contains("DEMO_TOKEN"), "{bare:?}");
}

#[test]
fn the_tick_writes_and_files_an_entry_and_a_rewrite_replaces_its_drawer() {
    let home = home_with_secret();
    let (server, palace) = fake_palace();
    let url = format!("{}/mcp", server.url);
    edit_config(&home, |doc| {
        doc["journal"]["mempalace_url"] = toml_edit::value(url.clone());
    });
    let id = add_turn(
        &home,
        "DEMO-12",
        "build",
        1,
        &minutes_ago(10),
        Some(&minutes_ago(2)),
        result_with_notes(),
        &stream_with_a_fix(),
    );
    run(&home, &["tick"]).expect_code(0);
    let path = home.path(&format!("journal/DEMO-12/{id}.md"));
    let text = std::fs::read_to_string(&path).unwrap();
    {
        let palace = palace.lock().unwrap();
        let deletes = palace.calls_of("mempalace_delete_by_source");
        let adds = palace.calls_of("mempalace_add_drawer");
        assert_eq!(deletes.len(), 1, "{:?}", palace.calls);
        assert_eq!(
            deletes[0],
            json!({"source_file": path.display().to_string(), "dry_run": false})
        );
        assert_eq!(adds.len(), 1);
        assert_eq!(adds[0]["wing"], "vision");
        assert_eq!(adds[0]["room"], "DEMO-12");
        assert_eq!(adds[0]["source_file"], json!(path.display().to_string()));
        assert_eq!(adds[0]["content"], json!(text));
        assert!(!adds[0]["content"].as_str().unwrap().contains(SECRET_VALUE));
        assert!(
            !adds[0]["content"]
                .as_str()
                .unwrap()
                .contains("\"type\": \"assistant\""),
            "no transcript goes to MemPalace"
        );
    }
    let row = ledger(&home).journal_row(&id).unwrap().unwrap();
    assert_eq!(row.filed_hash, row.hash);

    // A tick with no change files nothing.
    run(&home, &["tick"]).expect_code(0);
    assert_eq!(
        palace
            .lock()
            .unwrap()
            .calls_of("mempalace_add_drawer")
            .len(),
        1
    );

    // A new fact of the ledger rewrites the entry, and the new drawer replaces the old one.
    ledger(&home).set_end_fact(&id, "held").unwrap();
    run(&home, &["tick"]).expect_code(0);
    let new_text = std::fs::read_to_string(&path).unwrap();
    assert_ne!(new_text, text);
    assert!(new_text.contains("\noutcome: held\n"), "{new_text}");
    assert!(new_text.contains("\n- end facts: held\n"), "{new_text}");
    {
        let palace = palace.lock().unwrap();
        assert_eq!(palace.calls_of("mempalace_add_drawer").len(), 2);
        assert_eq!(palace.drawers.len(), 1, "one drawer for one source");
        assert_eq!(palace.drawers[0]["content"], json!(new_text));
    }

    // The search asks MemPalace with the candidates of the words too.
    let found = run(&home, &["journal", "search", "9f3e21c", "--json"]);
    found.expect_code(0);
    let found: Value = serde_json::from_str(&found.stdout).unwrap();
    assert_eq!(found["source"], "mempalace", "{found}");
    assert_eq!(found["hits"][0]["id"], json!(id));
    let searches = palace.lock().unwrap().calls_of("mempalace_search");
    assert_eq!(searches.last().unwrap()["candidate_strategy"], "union");
    assert_eq!(searches.last().unwrap()["wing"], "vision");

    // Reindex --all files each entry again, and still one drawer stays.
    let all = run(&home, &["journal", "reindex", "--all"]);
    all.expect_code(0);
    assert!(
        all.stdout
            .starts_with("1 entr(y/ies) filed in MemPalace, 0 still wait"),
        "{all:?}"
    );
    assert_eq!(palace.lock().unwrap().drawers.len(), 1);

    // The doctor asks only for the list of the tools: no write.
    let calls_before = palace.lock().unwrap().calls.len();
    let doctor = run(&home, &["doctor", "--json"]);
    let report: Value = serde_json::from_str(&doctor.stdout).unwrap();
    let journal = report["tests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["test"] == "journal")
        .unwrap()
        .clone();
    assert_eq!(journal["result"], "ok", "{journal}");
    assert!(
        journal["detail"]
            .as_str()
            .unwrap()
            .starts_with("1 entr(y/ies), 0 wait for the index. MemPalace answers at"),
        "{journal}"
    );
    assert_eq!(
        palace.lock().unwrap().calls.len(),
        calls_before,
        "no tool call"
    );
}

#[test]
fn a_mempalace_that_does_not_answer_leaves_the_files_and_a_later_reindex_files_them() {
    let home = home_with_secret();
    edit_config(&home, |doc| {
        doc["journal"]["mempalace_url"] = toml_edit::value(dead_url());
        doc["journal"]["timeout"] = toml_edit::value(2);
    });
    let id = add_turn(
        &home,
        "DEMO-12",
        "build",
        1,
        &minutes_ago(10),
        Some(&minutes_ago(2)),
        result_with_notes(),
        &stream_with_a_fix(),
    );
    run(&home, &["tick"]).expect_code(0);
    assert!(home.path(&format!("journal/DEMO-12/{id}.md")).is_file());
    let log = home.read("log/vision.log");
    assert_eq!(
        log.matches("wait for the index, because MemPalace does not answer")
            .count(),
        1,
        "one line for the tick: {log}"
    );
    assert!(ledger(&home)
        .journal_row(&id)
        .unwrap()
        .unwrap()
        .waits_for_index());

    let found = run(&home, &["journal", "search", "NameError"]);
    found.expect_code(0);
    assert!(
        found
            .stdout
            .starts_with("MemPalace did not answer, so the search read the entry files"),
        "{found:?}"
    );
    assert!(found.stdout.contains(&id), "{found:?}");

    let doctor = run(&home, &["doctor"]);
    let line = doctor
        .stdout
        .lines()
        .find(|l| l.contains(" journal "))
        .unwrap()
        .to_string();
    assert!(line.starts_with("warn  journal"), "{line}");
    assert!(
        line.contains("1 wait for the index. MemPalace does not answer"),
        "{line}"
    );
    run(&home, &["journal", "reindex"]).expect_code(1);

    let (server, palace) = fake_palace();
    edit_config(&home, |doc| {
        doc["journal"]["mempalace_url"] = toml_edit::value(format!("{}/mcp", server.url));
    });
    let filed = run(&home, &["journal", "reindex"]);
    filed.expect_code(0);
    assert!(
        filed
            .stdout
            .starts_with("1 entr(y/ies) filed in MemPalace, 0 still wait"),
        "{filed:?}"
    );
    assert_eq!(palace.lock().unwrap().drawers.len(), 1);
    assert!(!ledger(&home)
        .journal_row(&id)
        .unwrap()
        .unwrap()
        .waits_for_index());
}

#[test]
fn with_the_index_off_the_doctor_says_so_and_reindex_refuses() {
    let home = home_with_secret();
    let doctor = run(&home, &["doctor"]);
    let line = doctor
        .stdout
        .lines()
        .find(|l| l.contains(" journal "))
        .unwrap()
        .to_string();
    assert!(line.starts_with("ok "), "{line}");
    assert!(line.ends_with("0 entr(y/ies). The index of MemPalace is off (journal.mempalace_url is empty), so the search reads the files"), "{line}");
    let archive = doctor
        .stdout
        .lines()
        .find(|l| l.contains(" archive "))
        .unwrap()
        .to_string();
    assert!(archive.starts_with("ok "), "{archive}");
    assert!(
        archive.ends_with("off (archive.dir and archive.bucket are empty)"),
        "{archive}"
    );
    let refused = run(&home, &["journal", "reindex"]);
    refused.expect_code(1);
    assert!(refused.stderr.contains("the index is off"), "{refused:?}");
    let empty = run(&home, &["journal", "search", "anything"]);
    empty.expect_code(0);
    assert!(empty.stdout.starts_with("No entry fits"), "{empty:?}");
}

#[test]
fn a_tick_with_no_value_of_a_secret_writes_no_entry_and_says_so_once() {
    let home = home_with_secret();
    for n in 1..=2 {
        add_turn(
            &home,
            "DEMO-12",
            "build",
            n,
            &minutes_ago(10 + i64::from(n)),
            Some(&minutes_ago(2)),
            result_with_notes(),
            "",
        );
    }
    home.run(&["tick"]).expect_code(0);
    assert!(!home.path("journal/DEMO-12").exists());
    let log = home.read("log/vision.log");
    assert_eq!(
        log.matches("so no entry is written").count(),
        1,
        "one line for the tick: {log}"
    );
    // With the value, the next tick writes both.
    run(&home, &["tick"]).expect_code(0);
    assert_eq!(
        std::fs::read_dir(home.path("journal/DEMO-12"))
            .unwrap()
            .count(),
        2
    );
}
