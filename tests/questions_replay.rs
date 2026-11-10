//! The replay of the Judge through the library, against a second memory folder, with the fake
//! `claude`. A replay writes no decision file, changes no row, and counts no use.

mod turn_support;

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use turn_support::{key, TestHome};
use vision::config::Effort;
use vision::memory::{Kind, Memory, MemoryId, NewDecision, NewPrinciple, Writer};
use vision::questions::{replay, Outcome, ReplayOptions};

const ASKING: &str = "20261110T100000Z-DEMO-12-plan-1";

fn card(n: usize) -> Value {
    json!({"text": format!("Question {n}: CSV or XLSX?"), "context": "The export button.",
           "example": "The March report.", "evidence": "src/export.rs:12", "kind": "product",
           "area": ["exports"], "recommended": 0, "why": "Finance imports CSV.",
           "options": [{"label": "CSV", "means": "plain"}, {"label": "XLSX", "means": "sheet"}]})
}

fn person_decision(memory: &Memory, n: usize, answer: &str) -> String {
    let mut new = NewDecision::answer(
        Writer::Person("user".into()),
        &format!("Question {n}: CSV or XLSX?"),
        answer,
    );
    new.kind = Some(Kind::Product);
    new.ticket = Some("DEMO-12".into());
    memory.add_decision(&new).unwrap().id.to_string()
}

fn entry(
    card: &str,
    basis: &str,
    sources: &[&str],
    choice: Option<i64>,
    text: Option<&str>,
) -> Value {
    let mut value = json!({"card": card, "outcome": "answer", "basis": basis, "sources": sources,
           "sure": true, "choice": choice, "reason": format!("The reason for {card}.")});
    if let Some(text) = text {
        value["text"] = json!(text);
    }
    value
}

/// Rows that the person answered (Q-0001 to Q-0004), a needless card (Q-0005), and an answer
/// of the Judge (Q-0006). The home memory has the principle P-001 retired; the second folder
/// has it active.
fn setup(home: &TestHome) -> std::path::PathBuf {
    let memory = Memory::open(&home.ctx().paths, "user").unwrap();
    let decisions = [
        person_decision(&memory, 1, "CSV"),
        person_decision(&memory, 2, "XLSX"),
        person_decision(&memory, 3, "Use JSON"),
        person_decision(&memory, 4, "CSV"),
    ];
    let principle = memory
        .add_principle(&NewPrinciple {
            agent: "judge".into(),
            kind: Some(Kind::Product),
            area: vec!["exports".into()],
            by: Writer::Person("user".into()),
            evidence: vec![],
            rule: "When a report is exported, make sure that it is CSV.".into(),
            because: "Finance imports CSV.".into(),
            not_when: String::new(),
        })
        .unwrap();
    let second = home.scenarios.join("proposal memory");
    for folder in ["decisions", "principles"] {
        std::fs::create_dir_all(second.join(folder)).unwrap();
        for e in std::fs::read_dir(home.vision.join("memory").join(folder)).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), second.join(folder).join(e.file_name())).unwrap();
        }
    }
    memory
        .retire(
            MemoryId::Principle(principle.id),
            "Only in the proposal.",
            memory.person(),
        )
        .unwrap();

    let ledger = home.ledger();
    ledger
        .conn()
        .execute(
            "INSERT INTO turns (id, ticket, agent, step, status) VALUES (?1, 'DEMO-12', 'builder', 'plan', 'done')",
            [ASKING],
        )
        .unwrap();
    let answers = [
        (1, Some(0), "CSV", "user", Some(decisions[0].as_str())),
        (2, Some(1), "XLSX", "user", Some(decisions[1].as_str())),
        (3, None, "Use JSON", "user", Some(decisions[2].as_str())),
        (4, Some(0), "CSV", "user", Some(decisions[3].as_str())),
        (6, Some(0), "CSV", "judge", None),
    ];
    for (n, choice, text, by, decision) in answers {
        let answer = json!({"choice": choice, "text": text, "by": by, "why": "", "when": ""});
        ledger
            .conn()
            .execute(
                "INSERT INTO questions (id, ticket, turn, card, state, outcome, answer, decision, created, answered)
                 VALUES (?1, 'DEMO-12', ?2, ?3, 'answered', 'ask', ?4, ?5, '2026-11-10T10:00:00Z', ?6)",
                rusqlite::params![
                    format!("Q-000{n}"),
                    ASKING,
                    card(n).to_string(),
                    answer.to_string(),
                    decision,
                    format!("2026-11-10T10:0{n}:00Z")
                ],
            )
            .unwrap();
    }
    ledger
        .conn()
        .execute(
            "INSERT INTO questions (id, ticket, turn, card, state, flag, created, answered)
             VALUES ('Q-0005', 'DEMO-12', ?1, ?2, 'needless', 'needless:code', '2026-11-10T10:00:00Z', '2026-11-10T10:05:00Z')",
            rusqlite::params![ASKING, card(5).to_string()],
        )
        .unwrap();
    second
}

fn judge_scenario(home: &TestHome, runs: Value) {
    let dir = home.vision.join("claude/fake-scenarios");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("judge.json"), json!({ "turns": runs }).to_string()).unwrap();
}

fn snapshot_rows(home: &TestHome) -> Vec<String> {
    home.ledger()
        .conn()
        .prepare("SELECT id || state || coalesce(answer, '') || coalesce(decision, '') FROM questions ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn commits(dir: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_replay_compares_the_judge_with_the_person_against_a_second_memory() {
    let home = TestHome::new();
    let second = setup(&home);
    judge_scenario(
        &home,
        json!([[{"do": "init"}, {"do": "result", "cost": 0.25, "structured_output": {"answers": [
            entry("Q-0001", "principle", &["P-001"], Some(0), None),
            entry("Q-0002", "decision", &["D-0002"], Some(1), None),
            entry("Q-0003", "inference", &[], None, Some("  use JSON ")),
            entry("Q-0004", "inference", &[], Some(1), None),
        ]}}]]),
    );
    let rows_before = snapshot_rows(&home);
    let commits_before = commits(&home.vision.join("memory"));
    let ctx = home.ctx();
    let ledger = home.ledger();
    let report = replay(
        &ctx,
        &ledger,
        &ReplayOptions {
            n: 10,
            memory: second.clone(),
            model: Some("opus".into()),
            effort: Some(Effort::Low),
            key: key("eval"),
            wait: Some(Duration::from_secs(30)),
        },
    )
    .unwrap();

    let ids: Vec<&str> = report.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(
        ids,
        ["Q-0004", "Q-0003", "Q-0002", "Q-0001"],
        "newest first, no needless card, no answer of the Judge"
    );
    let item = |id: &str| report.items.iter().find(|i| i.id == id).unwrap();
    assert_eq!(
        item("Q-0001").outcome,
        Outcome::Answer,
        "P-001 is active in the second folder"
    );
    assert!(item("Q-0001").agree);
    assert_eq!(item("Q-0002").outcome, Outcome::Propose);
    assert!(
        item("Q-0002").rule.as_deref().unwrap().contains("D-0002"),
        "the decision of the replayed answer is hidden from the Judge"
    );
    assert!(item("Q-0002").agree);
    assert_eq!(item("Q-0003").outcome, Outcome::Answer);
    assert!(
        item("Q-0003").agree,
        "the same text after trimming and case folding"
    );
    assert_eq!(item("Q-0004").outcome, Outcome::Answer);
    assert!(!item("Q-0004").agree);
    assert_eq!(item("Q-0004").person, "CSV");
    assert_eq!(item("Q-0004").judge.as_deref(), Some("XLSX"));
    assert_eq!(item("Q-0004").judge_choice, Some(2));
    assert_eq!(
        (
            report.agree,
            report.disagree,
            report.to_person,
            report.proposals_agree
        ),
        (2, 1, 1, 1)
    );
    assert_eq!(report.cost_usd, 0.25);
    assert!(report.seconds > 0.0);
    assert_eq!(report.reason.as_deref(), Some("result"));
    assert!(!report.timed_out);
    assert_eq!(report.model, "claude-opus-5-5");
    assert_eq!(report.effort, Effort::Low);
    assert!(report.turn.contains("-eval-replay-"), "{}", report.turn);

    // The Judge read a copy without the replayed decisions.
    assert!(report.memory.join("principles/P-001.md").is_file());
    for d in ["D-0001", "D-0002", "D-0003", "D-0004"] {
        assert!(
            !report.memory.join(format!("decisions/{d}.md")).exists(),
            "{d}"
        );
    }
    let facts: Value = serde_json::from_str(
        &std::fs::read_to_string(
            home.vision
                .join("tickets/eval/turns")
                .join(&report.turn)
                .join("facts.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(facts["memory"], report.memory.display().to_string());
    assert_eq!(facts["asking"], Value::Null);
    assert_eq!(facts["documents"]["answers"], Value::Null);

    // Nothing changed.
    assert_eq!(snapshot_rows(&home), rows_before);
    assert_eq!(commits(&home.vision.join("memory")), commits_before);
    let p1 = std::fs::read_to_string(second.join("principles/P-001.md")).unwrap();
    assert!(
        p1.contains("uses: {used: 0, agreed: 0, corrected: 0}"),
        "{p1}"
    );
    assert_eq!(
        std::fs::read_dir(home.vision.join("memory/decisions"))
            .unwrap()
            .count(),
        5,
        "four decisions and .gitkeep"
    );
}

#[test]
fn a_replay_that_waits_too_long_kills_its_turn_and_sends_each_card_to_the_person() {
    let home = TestHome::new();
    let second = setup(&home);
    judge_scenario(&home, json!([[{"do": "init"}, {"do": "hang"}]]));
    let ctx = home.ctx();
    let ledger = home.ledger();
    let report = replay(
        &ctx,
        &ledger,
        &ReplayOptions {
            n: 2,
            memory: second,
            model: None,
            effort: None,
            key: key("curator"),
            wait: Some(Duration::from_secs(1)),
        },
    )
    .unwrap();
    assert!(report.timed_out);
    assert_eq!(report.items.len(), 2);
    assert_eq!((report.agree, report.disagree, report.to_person), (0, 0, 2));
    assert_eq!(report.reason.as_deref(), Some("killed"));
    assert_eq!(report.model, "claude-fable-5-2");
}

#[test]
fn a_replay_needs_a_reserved_key_a_memory_folder_and_answers() {
    let home = TestHome::new();
    let ctx = home.ctx();
    let ledger = home.ledger();
    let options = |key_text: &str, memory: &Path| ReplayOptions {
        n: 5,
        memory: memory.to_path_buf(),
        model: None,
        effort: None,
        key: key(key_text),
        wait: Some(Duration::from_secs(1)),
    };
    let memory = home.vision.join("memory");
    let err = replay(&ctx, &ledger, &options("DEMO-12", &memory)).unwrap_err();
    assert!(err.to_string().contains("reserved key"), "{err}");
    let err = replay(&ctx, &ledger, &options("eval", &home.scenarios)).unwrap_err();
    assert!(err.to_string().contains("not a memory folder"), "{err}");
    let err = replay(&ctx, &ledger, &options("eval", &memory)).unwrap_err();
    assert!(err.to_string().contains("nothing to replay"), "{err}");
}
