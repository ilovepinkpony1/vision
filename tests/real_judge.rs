//! A real session for the Judge. The test is ignored, and it also needs
//! `VISION_REAL_CLAUDE=1`. Run it by hand:
//!
//! ```sh
//! VISION_REAL_CLAUDE=1 cargo test --test real_judge -- --ignored --nocapture
//! ```
//!
//! It uses the real `claude` of the `PATH`, the default login of the person
//! (`claude.config_dir = ""`), and the model `claude-haiku-5-5` for both agents.
//! It costs a few cents.
//!
//! The path: a real asking turn ends with one card. The memory holds a past answer of the
//! person that covers the card. The tick starts a real Judge turn, the Judge answers from
//! that decision, and the asking session goes on with the answer.

mod turn_support;

use std::time::Duration;

use serde_json::{json, Value};
use turn_support::{key, wait_for, TestHome};
use vision::turn::{self, TurnKind};

fn enabled() -> bool {
    if std::env::var("VISION_REAL_CLAUDE").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set VISION_REAL_CLAUDE=1 to run a real session");
    false
}

/// Runs one tick with the real home folder of the person and the real `claude`, because the
/// Judge turn that the tick starts needs the login of the person.
fn real_tick(home: &TestHome) {
    let real_home = std::env::var("HOME").expect("HOME is set");
    // The helper clears the environment, so this tick has no `USER`, as a system timer can
    // have none. Claude Code finds its login under the name of the user, and the supervisor
    // must fill that name in from the user database.
    let run = home.run_with(
        &["tick"],
        &[
            ("HOME", real_home.as_str()),
            ("VISION_CLAUDE_BIN", "claude"),
        ],
        None,
    );
    assert_eq!(run.code, 0, "{run:?}");
}

/// The end of the error files and of the stream of each turn of a ticket, for a failure text.
/// The temporary home is gone after a failure, so the text must carry the evidence.
fn turn_files(home: &TestHome, ticket: &str) -> String {
    fn tail(path: &std::path::Path, bytes: usize) -> String {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut start = text.len().saturating_sub(bytes);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text[start..].to_string()
    }
    let turns = home.vision.join("tickets").join(ticket).join("turns");
    let mut out = String::new();
    let mut dirs: Vec<_> = std::fs::read_dir(&turns)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    dirs.sort();
    for dir in dirs {
        out.push_str(&format!("\n=== {}\n", dir.display()));
        for (name, bytes) in [
            ("turn.json", 1500),
            ("stderr.log", 3000),
            ("supervisor.log", 1500),
            ("stream.jsonl", 2500),
        ] {
            out.push_str(&format!("--- {name}\n{}\n", tail(&dir.join(name), bytes)));
        }
    }
    out
}

#[test]
#[ignore = "runs the real claude: set VISION_REAL_CLAUDE=1 and pass --ignored"]
fn a_real_judge_answers_a_card_from_the_memory_and_the_asking_session_goes_on() {
    if !enabled() {
        return;
    }
    let home = TestHome::new();
    home.set_config("claude", "bin", "claude");
    home.set_config("claude", "config_dir", "");
    for agent in ["scout", "judge"] {
        home.run(&["model", "set", agent, "haiku", "--effort", "low"])
            .expect_code(0);
    }

    // The memory holds one past answer of the person that covers the question.
    let past = home.scenarios.join("past.jsonl");
    std::fs::write(
        &past,
        concat!(
            r#"{"question":"Must a report export make a CSV file or an XLSX file?","#,
            r#""options":["CSV","XLSX"],"answer":"CSV","#,
            r#""note":"The finance team imports CSV into their tool.","#,
            r#""ticket":"DEMO-7","kind":"product","area":["exports","reports"]}"#,
            "\n"
        ),
    )
    .unwrap();
    home.run(&["memory", "import", past.to_str().unwrap()])
        .expect_code(0);

    // An asking agent that asks exactly one card, and then reports the answer that it got.
    let role = home.scenarios.join("asker.md");
    std::fs::write(
        &role,
        "# Asker test\n\n\
         You are an agent in a test of Vision. Read the facts file that the first message names.\n\n\
         If its key `message` is null, end your turn with the kind `questions` and exactly one card. \
         The text of the card is `Must the report export of this ticket make a CSV file or an XLSX file?`. \
         Its context is `The ticket does not name the format.` \
         Its example is `A finance user exports the October report.` \
         Its options are `CSV` (means: a plain file that their import tool reads) and `XLSX` \
         (means: a formatted file that needs a new library). \
         Set `recommended` to null, the evidence to `the text of the ticket`, the kind to `product`, \
         and the area to `exports` and `reports`.\n\n\
         If the key `message` is not null, it holds the answer to your card. Then end your turn with \
         the kind `result` and a summary that starts with `format: ` and the answer.\n",
    )
    .unwrap();

    let mut request = turn::LaunchRequest::new(
        key("DEMO-12"),
        "scout",
        "refine",
        TurnKind::Reader,
        &home.work,
    );
    request.facts = json!({"task": {"untrusted": "Add an export button to the report page."}});
    request.role_file = Some(role);
    request.time_limit_secs = Some(300);
    let asked = home.launch(&request);
    let asking = asked.id.to_string();
    eprintln!("the asking turn {asking} in {}", asked.dir.display());

    let ctx = home.ctx();
    let ledger = home.ledger();
    let last_turn = wait_for(Duration::from_secs(420), "the resumed asking turn", || {
        real_tick(&home);
        std::thread::sleep(Duration::from_secs(3));
        let rows = ledger.questions_of_turn(&asking).unwrap();
        for row in &rows {
            assert!(
                row.state != "proposed" && row.state != "asked",
                "the Judge did not answer alone. The row: {row:#?}\n{}",
                turn_files(&home, "DEMO-12")
            );
        }
        if rows.is_empty() || !ledger.questions_resumed(&asking).unwrap() {
            return None;
        }
        // The newest turn of the asking agent that is not the asking turn itself.
        let (id, status): (String, String) = ledger
            .conn()
            .query_row(
                "SELECT id, status FROM turns WHERE ticket = 'DEMO-12' AND agent = 'scout' \
                 ORDER BY started DESC, id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        (id != asking && status != "running").then_some(id)
    });

    let rows = ledger.questions_of_turn(&asking).unwrap();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    let row = &rows[0];
    eprintln!("the question row: {row:#?}");
    assert_eq!(row.state, "answered", "{row:#?}");
    assert_eq!(row.outcome.as_deref(), Some("answer"), "{row:#?}");
    assert_eq!(row.basis.as_deref(), Some("decision"), "{row:#?}");
    let sources: Vec<String> = serde_json::from_str(row.sources.as_deref().unwrap()).unwrap();
    assert_eq!(sources, ["D-0001"], "{row:#?}");

    // The answer of the Judge is one decision file with the writer `judge`.
    let decision = row.decision.clone().expect("a decision file");
    let text = std::fs::read_to_string(
        home.vision
            .join("memory/decisions")
            .join(format!("{decision}.md")),
    )
    .unwrap();
    eprintln!("the decision file of the Judge:\n{text}");
    assert!(text.contains("by: judge"), "{text}");
    assert!(text.contains("sources: [D-0001]"), "{text}");
    assert!(text.contains("## Answer\nCSV"), "{text}");

    // The asking session went on with the answer.
    let outcome = turn::outcome(&ctx, &ledger, &last_turn.parse().unwrap()).unwrap();
    let result: Value = outcome.result.clone().expect("a result object");
    eprintln!("the result of the resumed turn: {result:#}");
    assert_eq!(result["kind"], "result", "{outcome:#?}");
    let summary = result["summary"].as_str().unwrap_or_default();
    assert!(summary.contains("CSV"), "{summary}");
}
