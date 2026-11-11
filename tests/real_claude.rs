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

/// Runs `git` in the working folder of the test.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=someone",
            "-c",
            "user.email=someone@localhost",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A worker must be able to commit: each `clean` end fact and each fix depends on it. A real
/// session showed that Claude Code asks for an approval for some forms of a commit command also
/// in the sandbox, and a turn of Vision has nobody who can approve. This test runs four forms
/// in one real worker turn, prints which ones ran, and asserts the forms that the role files
/// teach.
#[test]
#[ignore = "runs the real claude: set VISION_REAL_CLAUDE=1 and pass --ignored"]
fn a_real_worker_turn_commits_with_the_forms_that_the_roles_teach() {
    if !enabled() {
        return;
    }
    let home = TestHome::new();
    home.set_config("claude", "bin", "claude");
    home.set_config("claude", "config_dir", "");
    git(&home.work, &["init", "-q"]);
    std::fs::write(home.work.join("base.txt"), "base\n").unwrap();
    git(&home.work, &["add", "base.txt"]);
    git(&home.work, &["commit", "-q", "-m", "Base"]);
    let role = home.scenarios.join("worker-test.md");
    std::fs::write(
        &role,
        "# Worker test\n\n\
         You are a worker in a test of Vision. The test finds out which commit commands a session can run. \
         Read the facts file that the first message names. Then run each command below as its own Bash call, \
         in this order, exactly as it is written. Do not change a command, do not repeat one, and do not \
         replace a command that is denied or that fails. Go on with the next one.\n\n\
         Command 1:\n\n```\nprintf one > a.txt && git add a.txt && git commit -q -m \"Add a\"\n```\n\n\
         Command 2:\n\n```\nprintf two > b.txt && git add b.txt && git commit -q -m \"$(cat <<'EOF'\nAdd b\n\nThe second line of the message.\n\nSigned-off-by: Some One <someone@localhost>\nEOF\n)\"\n```\n\n\
         Command 3:\n\n```\nprintf three > c.txt && git add c.txt && git -c user.name=demo -c user.email=demo@localhost commit -q -m \"Add c\"\n```\n\n\
         Command 4:\n\n```\nprintf four > d.txt && git add d.txt && git commit -q -m \"Add d\n\nSigned-off-by: Some One <someone@localhost>\"\n```\n\n\
         End your turn with the kind `result`. The summary has one line for each command: \
         `1: ran`, `1: denied`, or `1: failed` with the error text.\n",
    )
    .unwrap();

    let mut request = vision::turn::LaunchRequest::new(
        key("DEMO-12"),
        "builder",
        "build",
        TurnKind::Worker,
        &home.work,
    );
    request.facts = json!({"task": {"untrusted": "Run the four commands of your role file."}});
    request.role_file = Some(role);
    request.model = Some("claude-haiku-5-5".into());
    request.effort = Some(Effort::Low);
    request.time_limit_secs = Some(300);
    let launched = home.launch(&request);
    eprintln!("turn {} in {}", launched.id, launched.dir.display());
    let ticket = launched.ticket.to_string();
    let id = launched.id.to_string();
    let state = wait_for(Duration::from_secs(300), "the real turn to end", || {
        home.state(&ticket, &id).filter(TurnState::is_ended)
    });
    eprintln!("turn.json: {state:?}");

    // The facts: each Bash call with its result, and the commits that exist.
    let stream = home.turn_file(&launched, "stream.jsonl");
    let mut results = Vec::new();
    for line in stream.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        for part in event["message"]["content"].as_array().into_iter().flatten() {
            match part["type"].as_str() {
                Some("tool_use") if part["name"] == "Bash" => {
                    eprintln!("BASH: {}", part["input"]);
                }
                Some("tool_result") => {
                    let text = part["content"].to_string();
                    let end = text
                        .char_indices()
                        .nth(600)
                        .map_or(text.len(), |(at, _)| at);
                    eprintln!("RESULT: {}", &text[..end]);
                    results.push(text);
                }
                _ => {}
            }
        }
    }
    let log = git(&home.work, &["log", "--format=%s"]);
    eprintln!("the commits:\n{log}");
    home.collect();
    let ctx = home.ctx();
    let ledger = home.ledger();
    let outcome = turn::outcome(&ctx, &ledger, &launched.id).unwrap();
    eprintln!(
        "summary: {}",
        outcome
            .result
            .as_ref()
            .map_or(Value::Null, |r| r["summary"].clone())
    );
    assert_eq!(state.reason, Some(Reason::Result));
    let subjects: Vec<&str> = log.lines().collect();
    for subject in ["Add a", "Add b", "Add d"] {
        assert!(
            subjects.contains(&subject),
            "a worker cannot make the commit `{subject}`: {log}"
        );
    }
    // The git configuration of a worker names an empty global ignore file, so that git does
    // not look for one in the home folder of the person, which the sandbox does not let it read.
    assert!(
        !results.iter().any(|r| r.contains("unable to access")),
        "a git command warns about a file that it cannot read: {results:#?}"
    );
}
