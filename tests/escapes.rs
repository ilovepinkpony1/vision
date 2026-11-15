//! The escape check (design section 11.6) through `vision tick`, against the fake GitHub with
//! state of tests/jira_github_support: after our gate cr passed, the first review of a review bot
//! is read once, and each comment of a counted priority that no finding of ours matches is an
//! escape: a row of the ledger, a row of `findings`, a note in the journal entry of the turn
//! whose gate passed, and the numbers of `vision report`.
//!
//! Nothing here reaches the network: `github_api` points at the fake server, and the read verb
//! runs in the built-in adapter of the binary.

mod common;
mod jira_github_support;

use std::sync::{Arc, Mutex};

use common::Home;
use jira_github_support::server::call;
use jira_github_support::world::{install, World};
use jira_github_support::{git, FakeServer};
use serde_json::{json, Value};
use toml_edit::DocumentMut;
use vision::ledger::pipeline::FindingRow;
use vision::ledger::review::GateRoundRow;
use vision::ledger::turns::NewTurn;
use vision::ledger::{Ledger, TicketRow};
use vision::paths::{TicketKey, TurnId};

const KEY: &str = "DEMO-12";
const BRANCH: &str = "vision/DEMO-12";
const FORGE: &str = "forge-escape-token-6f5e4d3c2b1a";
const BOT: &str = "review-bot[bot]";
const PASS_TURN: &str = "20261115T090000Z-DEMO-12-review-1";

struct Setup {
    home: Home,
    server: FakeServer,
    world: Arc<Mutex<World>>,
    _repos: tempfile::TempDir,
}

impl Setup {
    /// A home with the adapter jira-github of the repo, the fake GitHub with the pull request #7
    /// of the branch, `[escapes]` for the review bot, and a ticket whose gate cr passed an hour
    /// ago with two findings of ours.
    fn new() -> Setup {
        let repos = tempfile::tempdir().unwrap();
        let remote = repos.path().join("remote.git");
        let work = repos.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(
            &["init", "--bare", "--quiet", &remote.display().to_string()],
            repos.path(),
        );
        git(&["init", "--quiet"], &work);
        std::fs::write(work.join("app.rb"), "puts 1\n").unwrap();
        git(&["add", "-A"], &work);
        git(&["commit", "--quiet", "-m", "start"], &work);
        let remote_text = remote.display().to_string();
        git(
            &["push", "--quiet", &remote_text, "HEAD:refs/heads/main"],
            &work,
        );
        git(
            &[
                "push",
                "--quiet",
                &remote_text,
                &format!("HEAD:refs/heads/{BRANCH}"),
            ],
            &work,
        );
        let mut world = World::new(&remote, None, Some(FORGE), None);
        assert_eq!(world.open_pull(BRANCH), 7);
        let world = Arc::new(Mutex::new(world));
        let server = FakeServer::start();
        install(&server, Arc::clone(&world));

        let home = Home::init();
        let path = home.path("config.toml");
        let mut doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        doc["project"] = toml_edit::value("jira-github");
        let tables = format!(
            "[adapter]\ngithub_repo = \"acme/widgets\"\ngithub_api = \"{}\"\nhttp_timeout = 20\n\n[secrets]\nFORGE_TOKEN = {{ env = \"VISION_TEST_FORGE\" }}\n\n[escapes]\nreviewers = [\"{BOT}\"]\nevery = 600\n",
            server.url
        )
        .parse::<DocumentMut>()
        .unwrap();
        for (name, item) in tables.iter() {
            doc[name] = item.clone();
        }
        std::fs::write(&path, doc.to_string()).unwrap();

        let ledger = Ledger::open(&home.path("ledger.sqlite")).unwrap();
        ledger
            .put_ticket(&TicketRow {
                key: KEY.into(),
                state: Some("in-review".into()),
                // A stopped ticket moves no step, so the tick only runs the escape check.
                flags: Some("[\"stopped\"]".into()),
                branch: Some(BRANCH.into()),
                pr: Some(7),
                counts: Some("{}".into()),
                ..TicketRow::default()
            })
            .unwrap();
        ledger
            .insert_turn(&NewTurn {
                id: PASS_TURN,
                ticket: KEY,
                agent: "reviewer",
                step: "review",
                kind: "worker",
                session: "s-1",
                resumes: None,
                model: "claude-opus-5-5",
                effort: "max",
                started: "2026-11-15T09:00:00Z",
                role_hash: "r",
            })
            .unwrap();
        ledger.set_turn_status(PASS_TURN, "done").unwrap();
        ledger.set_end_fact(PASS_TURN, "held").unwrap();
        let hour_ago =
            vision::util::time::rfc3339(vision::util::time::now() - ::time::Duration::hours(1));
        ledger
            .add_gate_round(&GateRoundRow {
                turn: PASS_TURN.into(),
                ticket: KEY.into(),
                step: "review".into(),
                gate: "cr".into(),
                doc: "cr.md".into(),
                round: 1,
                head: Some("abc123".into()),
                passed: true,
                failing: Some(0),
                at: hour_ago,
                ..GateRoundRow::default()
            })
            .unwrap();
        for (path, line) in [("app/models/export.rb", 40), ("app/views/show.erb", 3)] {
            ledger
                .add_finding(&FindingRow {
                    ticket: KEY.into(),
                    round: 1,
                    fingerprint: format!("{path}#x"),
                    source: "cr".into(),
                    verdict: "kept".into(),
                    path: Some(path.into()),
                    line: Some(line),
                    title: Some("A finding of ours".into()),
                    turn: Some(PASS_TURN.into()),
                    ..FindingRow::default()
                })
                .unwrap();
        }
        Setup {
            home,
            server,
            world,
            _repos: repos,
        }
    }

    fn tick(&self) {
        self.home
            .run_with(&["tick"], &[("VISION_TEST_FORGE", FORGE)])
            .expect_code(0);
    }

    fn team(&self, action: &str, body: Value) -> Value {
        let (status, answer) = call(
            self.server.port,
            "POST",
            &format!("/_control/{action}"),
            Some(&body),
            &[],
        );
        assert!((200..300).contains(&status), "{action}: {status} {answer}");
        answer
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.home.path("ledger.sqlite")).unwrap()
    }

    /// The calls of the adapter that read the reviews of the pull request.
    fn review_reads(&self) -> usize {
        self.world
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|e| {
                e.get("path").and_then(Value::as_str) == Some("/repos/acme/widgets/pulls/7/reviews")
            })
            .count()
    }
}

#[test]
fn a_comment_of_the_review_bot_that_no_finding_of_ours_matches_is_an_escape() {
    let s = Setup::new();
    // No review yet: the look is recorded, and the next tick waits `every` seconds.
    s.tick();
    let ledger = s.ledger();
    let checks = ledger.escape_checks_of(KEY).unwrap();
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert!(checks[0].looked.is_some() && checks[0].checked.is_none());
    assert_eq!(s.review_reads(), 1);
    s.tick();
    assert_eq!(s.review_reads(), 1, "no second look within `every`");

    // The bot reviews with four inline comments, and a person comments too.
    let review = s.team(
        "review",
        json!({"number": 7, "login": BOT, "state": "COMMENTED", "body": "Two things."}),
    );
    let id = review["id"].as_i64().unwrap();
    for (path, line, body) in [
        (
            "app/models/export.rb",
            42,
            "**[P1] A retry sends the report twice**\n\nThe job runs again.",
        ),
        (
            "app/models/user.rb",
            7,
            "**[P0] The nil user breaks the export with forge-escape-token-6f5e4d3c2b1a**\n\nDetails.",
        ),
        ("app/views/show.erb", 90, "**[P2] The label is long**"),
        ("app/models/user.rb", 30, "**[P3] A style nit**"),
    ] {
        s.team(
            "comment",
            json!({"number": 7, "login": BOT, "review": id, "path": path, "line": line, "body": body}),
        );
    }
    s.team(
        "comment",
        json!({"number": 7, "login": "lead", "path": "app/a.rb", "line": 1, "body": "**[P0] not a bot**"}),
    );
    // The look is not due yet: a ledger edit makes it due, as the time of `every` does.
    ledger
        .conn()
        .execute(
            "UPDATE escape_checks SET looked = '2026-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
    s.tick();

    let checks = ledger.escape_checks_of(KEY).unwrap();
    let check = &checks[0];
    assert_eq!(check.review_id, Some(id));
    assert!(check.checked.is_some());
    assert_eq!(
        (check.comments, check.counted, check.escapes),
        (4, 3, 2),
        "P3 is not counted, and the comment at line 42 is 2 lines from our finding"
    );
    let escapes = ledger.escapes(Some(KEY), None).unwrap();
    let places: Vec<(String, i64, String)> = escapes
        .iter()
        .map(|e| {
            (
                e.path.clone().unwrap(),
                e.line.unwrap(),
                e.priority.clone().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        places,
        [
            ("app/models/user.rb".to_string(), 7, "0".to_string()),
            ("app/views/show.erb".to_string(), 90, "2".to_string())
        ]
    );
    let title = escapes[0].title.as_deref().unwrap();
    assert!(
        title.starts_with("[P0] The nil user breaks the export"),
        "{title}"
    );
    assert!(!title.contains(FORGE), "the title is masked: {title}");
    assert!(escapes[0]
        .url
        .as_deref()
        .unwrap()
        .contains("/pull/7#discussion_r"));
    let rows: Vec<FindingRow> = ledger
        .findings_of(KEY)
        .unwrap()
        .into_iter()
        .filter(|f| f.source == "escape")
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].turn.as_deref(), Some(PASS_TURN));
    assert_eq!(rows[0].priority.as_deref(), Some("0"));

    // The journal entry of the turn whose gate passed has a note for each escape.
    let entry = std::fs::read_to_string(s.home.paths().journal_entry(
        &TicketKey::parse(KEY).unwrap(),
        &TurnId::parse(PASS_TURN).unwrap(),
    ))
    .unwrap();
    assert!(
        entry.contains("- problem: The review missed a finding (priority 0) that review-bot[bot] found at app/models/user.rb:7: [P0] The nil user breaks the export"),
        "{entry}"
    );
    assert!(entry.contains("(files: app/views/show.erb)"), "{entry}");
    assert!(!entry.contains(FORGE), "{entry}");

    // The check reads the review once: more ticks read nothing more.
    let reads = s.review_reads();
    s.tick();
    assert_eq!(s.review_reads(), reads);
    assert_eq!(ledger.escapes(Some(KEY), None).unwrap().len(), 2);

    // vision report gives the numbers, and vision report KEY lists the escapes.
    let report = s.home.run(&["report"]).expect_code(0).stdout.clone();
    assert!(
        report.contains(
            "Review escapes: 1 checked PR(s), 2 escape(s) (by priority 0: 1, 1: 0, 2: 1), 2.00 per PR"
        ),
        "{report}"
    );
    let json: Value = serde_json::from_str(&s.home.run(&["report", "--json"]).stdout).unwrap();
    assert_eq!(json["escapes"]["checked_prs"], 1);
    assert_eq!(json["escapes"]["by_priority"]["2"], 1);
    let ticket = s.home.run(&["report", KEY]).expect_code(0).stdout.clone();
    assert!(
        ticket.contains("review-bot[bot] reviewed #7 after the pass of 20261115T090000Z-DEMO-12-review-1: 4 comment(s), 3 counted, 2 escaped"),
        "{ticket}"
    );
    assert!(
        ticket.contains("P0  app/models/user.rb:7  [P0] The nil user"),
        "{ticket}"
    );
    assert!(
        ticket.contains("P2  app/views/show.erb:90  [P2] The label is long"),
        "{ticket}"
    );
    let ticket_json: Value =
        serde_json::from_str(&s.home.run(&["report", KEY, "--json"]).stdout).unwrap();
    assert_eq!(ticket_json["escape_rows"].as_array().unwrap().len(), 2);
    assert_eq!(ticket_json["escapes"]["escapes"], 2);
}

#[test]
fn with_no_reviewer_the_check_reads_nothing() {
    let s = Setup::new();
    let path = s.home.path("config.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace(&format!("reviewers = [\"{BOT}\"]"), "reviewers = []");
    std::fs::write(&path, text).unwrap();
    s.tick();
    assert_eq!(s.review_reads(), 0);
    assert!(s.ledger().escape_checks_of(KEY).unwrap().is_empty());
}
