//! One real session of Claude Code. Each test here is ignored, and it also does nothing unless
//! `VISION_REAL_CLAUDE=1` is set. Run it by hand:
//!
//! ```text
//! VISION_REAL_CLAUDE=1 cargo test --test real_claude -- --ignored --nocapture
//! ```
//!
//! It uses a temporary `VISION_HOME`, the `claude` of `PATH` with the default login of the
//! person (`claude.config_dir = ""`), and the model `claude-haiku-5-5`. It costs a few cents.

mod turn_support;

use std::time::Duration;

use serde_json::{json, Value};
use turn_support::{key, wait_for, TestHome};
use vision::config::Effort;
use vision::turn::{self, Reason, Told, TurnKind, TurnState, TurnStatus};

fn enabled() -> bool {
    if std::env::var("VISION_REAL_CLAUDE").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set VISION_REAL_CLAUDE=1 to run a real session");
    false
}

#[test]
#[ignore = "runs the real claude: set VISION_REAL_CLAUDE=1 and pass --ignored"]
fn a_real_reader_turn_reads_a_file_and_gets_a_second_message() {
    if !enabled() {
        return;
    }
    let home = TestHome::new();
    home.set_config("claude", "bin", "claude");
    home.set_config("claude", "config_dir", "");
    std::fs::write(home.work.join("notes.txt"), "The secret word is lantern.\n").unwrap();
    std::fs::write(home.work.join("other.txt"), "Nothing to see here.\n").unwrap();
    let role = home.scenarios.join("reader-test.md");
    std::fs::write(
        &role,
        "# Reader test\n\n\
         You are a reader in a test of Vision. Read the facts file that the first message names. \
         Then read the file notes.txt of the working folder, and then the file other.txt. \
         End your turn with the kind `result` and a summary that names the secret word of notes.txt. \
         If the person sends you a message during the turn, end the summary with `ack: ` and the message.\n",
    )
    .unwrap();

    let mut request = vision::turn::LaunchRequest::new(
        key("DEMO-12"),
        "scout",
        "refine",
        TurnKind::Reader,
        &home.work,
    );
    request.facts = json!({"task": {"untrusted": "Find the secret word in notes.txt."}});
    request.role_file = Some(role);
    request.model = Some("claude-haiku-5-5".into());
    request.effort = Some(Effort::Low);
    request.time_limit_secs = Some(300);
    let launched = home.launch(&request);
    eprintln!("turn {} in {}", launched.id, launched.dir.display());

    std::thread::sleep(Duration::from_secs(3));
    let ctx = home.ctx();
    let ledger = home.ledger();
    let told = turn::tell(
        &ctx,
        &ledger,
        &launched.ticket,
        "Please also say: banana.",
        "user",
    )
    .unwrap();
    assert!(
        matches!(told, Told::Running(_)),
        "the turn ended before the second message: {told:?}. See {}",
        launched.dir.display()
    );

    let ticket = launched.ticket.to_string();
    let id = launched.id.to_string();
    let state = wait_for(Duration::from_secs(300), "the real turn to end", || {
        home.state(&ticket, &id).filter(TurnState::is_ended)
    });
    eprintln!("turn.json: {state:?}");
    assert_eq!(
        state.reason,
        Some(Reason::Result),
        "see {}",
        launched.dir.display()
    );

    let stream = home.turn_file(&launched, "stream.jsonl");
    assert!(
        stream.contains("Please also say: banana."),
        "the second message is not in the stream: {}",
        launched.dir.display()
    );
    let init = home.events(&launched, "system", Some("init"));
    let tools: Vec<String> = serde_json::from_value(init[0]["tools"].clone()).unwrap();
    eprintln!("init tools: {tools:?}");

    home.collect();
    let outcome = turn::outcome(&ctx, &ledger, &launched.id).unwrap();
    eprintln!(
        "outcome: {}",
        serde_json::to_string_pretty(&outcome).unwrap()
    );
    assert_eq!(outcome.status, TurnStatus::Done);
    let result = outcome.result.clone().expect("a result object");
    let schema: Value = serde_json::from_str(include_str!("../agents/turn.schema.json")).unwrap();
    assert_eq!(
        turn::schema_problems(&schema, &result),
        Vec::<String>::new()
    );
    assert_eq!(result["kind"], "result");
    let summary = result["summary"].as_str().unwrap_or("").to_lowercase();
    assert!(summary.contains("lantern"), "{summary}");
    assert!(outcome.cost_usd.is_some_and(|c| c > 0.0), "{outcome:?}");
    assert!(outcome.usage.output > 0, "{outcome:?}");
}
