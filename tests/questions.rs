//! The cards, the Judge, and the answers of the person (section 8), driven through `vision
//! tick` and the commands with the fake `claude`. A work turn starts through the library, as
//! the pipeline will start it. Each Judge turn that the tick starts plays the scenario
//! `claude/fake-scenarios/judge.json` of the temporary home.

mod turn_support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};
use turn_support::{key, result_object, wait_for, Run, TestHome};
use vision::ledger::questions::QuestionRow;
use vision::memory::{Kind, Memory, NewDecision, NewPrinciple, Writer};
use vision::paths::TurnId;
use vision::turn::{Launched, TurnKind, TurnState};

const TICKET: &str = "DEMO-12";

/// A valid card. `n` makes its question unique.
fn card(n: usize, topics: &[&str]) -> Value {
    json!({
        "text": format!("Question {n}: must the export make CSV or XLSX?"),
        "context": "The export button has no format yet.",
        "example": "A person exports the March report.",
        "evidence": "src/export.rs:12",
        "kind": "product",
        "area": ["reports", "exports"],
        "topics": topics,
        "options": [{"label": "CSV", "means": "plain text"}, {"label": "XLSX", "means": "a sheet"}],
        "recommended": 0,
        "why": "Finance imports CSV."
    })
}

/// One entry of the output of the Judge.
fn entry(
    card: &str,
    outcome: &str,
    basis: &str,
    sources: &[&str],
    sure: bool,
    choice: Option<i64>,
) -> Value {
    json!({"card": card, "outcome": outcome, "basis": basis, "sources": sources, "sure": sure,
           "choice": choice, "topics": [], "reason": format!("The reason for {card}.")})
}

fn judge_result(entries: Vec<Value>) -> Value {
    json!([{"do": "init"}, {"do": "result", "structured_output": {"answers": entries}}])
}

/// The steps of the resumed asking turn: it reports its facts and ends.
fn resumed_steps() -> Value {
    json!([{"do": "init"}, {"do": "facts"}, {"do": "result", "structured_output": result_object("Planned.")}])
}

/// Writes the scenario of the Judge turns that the tick starts.
fn judge_scenario(home: &TestHome, runs: Value) {
    let dir = home.vision.join("claude/fake-scenarios");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("judge.json"), json!({ "turns": runs }).to_string()).unwrap();
}

/// Runs a work turn of the builder that ends with these cards, and waits for its end. The
/// second run of its scenario is the resumed turn.
fn ask(home: &TestHome, name: &str, cards: Vec<Value>, resumed: Value) -> Launched {
    let scenario = home.scenario(
        name,
        json!([
            [{"do": "init"}, {"do": "result", "structured_output":
                {"kind": "questions", "summary": "Cards.", "questions": cards}}],
            resumed
        ]),
    );
    let request = home.request(TICKET, "builder", "plan", TurnKind::Reader, &scenario);
    let launched = home.launch(&request);
    home.wait_ended(&launched);
    launched
}

fn tick(home: &TestHome) -> Run {
    let run = home.run(&["tick"]);
    run.expect_code(0);
    run
}

fn rows(home: &TestHome) -> Vec<QuestionRow> {
    let ledger = home.ledger();
    let mut out = Vec::new();
    for n in 1..100 {
        match ledger.question(&format!("Q-{n:04}")).unwrap() {
            Some(row) => out.push(row),
            None => break,
        }
    }
    out
}

fn row(home: &TestHome, id: &str) -> QuestionRow {
    home.ledger().question(id).unwrap().expect("the row")
}

fn states(home: &TestHome) -> Vec<String> {
    rows(home).into_iter().map(|r| r.state).collect()
}

/// Waits until a turn that a different process started has ended.
fn wait_turn(home: &TestHome, id: &str) -> TurnState {
    wait_for(
        Duration::from_secs(30),
        &format!("the turn {id} to end"),
        || home.state(TICKET, id).filter(TurnState::is_ended),
    )
}

/// The folder of a turn of the ticket.
fn turn_dir(home: &TestHome, id: &str) -> PathBuf {
    home.ctx()
        .paths
        .turn(&key(TICKET), &TurnId::parse(id).unwrap())
        .dir
}

fn judge_turns(home: &TestHome) -> Vec<String> {
    home.ledger()
        .turns_of_ticket(TICKET)
        .unwrap()
        .into_iter()
        .filter(|t| t.agent.as_deref() == Some("judge"))
        .map(|t| t.id)
        .collect()
}

/// Runs the tick that starts the Judge, waits for the Judge, and runs the tick that applies
/// its result. Gives the id of the Judge turn.
fn judge_round(home: &TestHome) -> String {
    tick(home);
    let judge = rows(home)[0].judge_turn.clone().expect("a Judge turn");
    wait_turn(home, &judge);
    tick(home);
    judge
}

fn decision_files(home: &TestHome) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(home.vision.join("memory/decisions"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    names.sort();
    names
}

fn memory_file(home: &TestHome, path: &str) -> String {
    std::fs::read_to_string(home.vision.join("memory").join(path)).unwrap()
}

fn git_author(dir: &Path, path: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "-1", "--format=%an", "--", path])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn json_of(run: &Run) -> Value {
    serde_json::from_str(&run.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {run:?}"))
}

/// A memory with the person's decision D-0001 and the principle P-001.
fn seed_memory(home: &TestHome) {
    let memory = Memory::open(&home.ctx().paths, "user").unwrap();
    let mut decision = NewDecision::answer(
        Writer::Person("user".into()),
        "Must the export make CSV or XLSX?",
        "CSV",
    );
    decision.kind = Some(Kind::Product);
    decision.area = vec!["exports".into()];
    decision.why = "Finance imports CSV.".into();
    memory.add_decision(&decision).unwrap();
    memory
        .add_principle(&NewPrinciple {
            agent: "judge".into(),
            kind: Some(Kind::Product),
            area: vec!["exports".into()],
            by: Writer::Person("user".into()),
            evidence: vec![],
            rule: "When a report is exported, make sure that the file is CSV.".into(),
            because: "Finance imports CSV.".into(),
            not_when: String::new(),
        })
        .unwrap();
}

fn principle_uses(home: &TestHome) -> String {
    memory_file(home, "principles/P-001.md")
        .lines()
        .find(|l| l.starts_with("uses:"))
        .unwrap_or("")
        .to_string()
}

#[test]
fn three_cards_get_rows_and_one_judge_turn_with_its_inputs() {
    let home = TestHome::new();
    home.edit_config(|doc| doc["user"] = toml_edit::value("Demo Person"));
    std::fs::create_dir_all(home.vision.join("tickets/DEMO-12")).unwrap();
    std::fs::write(home.vision.join("tickets/DEMO-12/refine.md"), "# Refine\n").unwrap();
    judge_scenario(
        &home,
        json!([[{"do": "init"}, {"do": "sleep", "seconds": 1},
                {"do": "result", "structured_output": {"answers": []}}]]),
    );
    let asking = ask(
        &home,
        "three",
        vec![card(1, &[]), card(2, &[]), card(3, &[])],
        resumed_steps(),
    );
    tick(&home);
    let all = rows(&home);
    assert_eq!(all.len(), 3);
    let judge = all[0]
        .judge_turn
        .clone()
        .expect("the cards have a Judge turn");
    for (n, r) in all.iter().enumerate() {
        assert_eq!(r.id, format!("Q-000{}", n + 1));
        assert_eq!(r.state, "judging");
        assert_eq!(r.ticket, TICKET);
        assert_eq!(r.turn, asking.id.to_string());
        assert_eq!(r.judge_turn.as_deref(), Some(judge.as_str()));
        let stored: Value = serde_json::from_str(&r.card).unwrap();
        assert_eq!(stored, card(n + 1, &[]), "the card is kept as it came");
    }
    assert!(judge.ends_with("-DEMO-12-judge-1"), "{judge}");

    let dir = turn_dir(&home, &judge);
    let launch: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("launch.json")).unwrap()).unwrap();
    assert_eq!(launch["request"]["agent"], "judge");
    assert_eq!(launch["request"]["kind"], "reader");
    assert_eq!(launch["request"]["step"], "judge");
    assert_eq!(launch["request"]["time_limit_secs"], 900);
    assert_eq!(launch["plan"]["schema"], "judge.schema.json");
    let memory = home.vision.join("memory");
    assert_eq!(
        launch["request"]["add_dirs"],
        json!([memory.display().to_string()])
    );
    let role = std::fs::read_to_string(dir.join("role.md")).unwrap();
    assert!(role.starts_with("# Judge"), "{role}");

    let workdir = PathBuf::from(launch["plan"]["workdir"].as_str().unwrap());
    assert_eq!(
        workdir,
        home.vision
            .join("tickets/DEMO-12/judge")
            .join(asking.id.to_string()),
        "a ticket with no clone gets a folder of the turn"
    );
    let facts: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("facts.json")).unwrap()).unwrap();
    assert_eq!(facts["agent"], "judge");
    assert_eq!(facts["person"], "Demo Person");
    assert_eq!(facts["memory"], memory.display().to_string());
    assert_eq!(facts["always_ask"][0], "security");
    assert_eq!(facts["asking"]["turn"], asking.id.to_string());
    assert_eq!(facts["asking"]["agent"], "builder");
    assert_eq!(facts["asking"]["step"], "plan");
    assert_eq!(
        facts["documents"]["refine"],
        format!(".vision/turns/{judge}/refine.md")
    );
    assert_eq!(facts["documents"]["plan"], Value::Null);
    assert_eq!(facts["documents"]["answers"], Value::Null);
    let cards_path = workdir.join(facts["cards"].as_str().unwrap());
    let cards: Value = serde_json::from_str(&std::fs::read_to_string(cards_path).unwrap()).unwrap();
    assert_eq!(cards.as_array().unwrap().len(), 3);
    assert_eq!(cards[2]["id"], "Q-0003");
    assert_eq!(cards[2]["text"], card(3, &[])["text"]);
    assert!(workdir
        .join(format!(".vision/turns/{judge}/refine.md"))
        .is_file());

    // A second tick while the Judge runs, and one after it ended, start no second Judge.
    tick(&home);
    wait_turn(&home, &judge);
    tick(&home);
    assert_eq!(judge_turns(&home), std::slice::from_ref(&judge));
    assert_eq!(
        states(&home),
        ["asked", "asked", "asked"],
        "a card with no entry goes to the person"
    );
    let needs = json_of(&home.run(&["needs", "--json"]));
    assert_eq!(needs["cards"].as_array().unwrap().len(), 3);
    assert_eq!(
        needs["cards"][0]["judge_reason"],
        "the Judge gave no outcome for this card"
    );
}

#[test]
fn the_rules_in_code_decide_each_outcome_of_the_judge() {
    let home = TestHome::new();
    seed_memory(&home);
    let cards = vec![
        card(1, &[]),
        card(2, &[]),
        card(3, &[]),
        card(4, &[]),
        card(5, &[]),
        card(6, &[]),
        card(7, &["money"]),
    ];
    let mut propose = entry("Q-0005", "propose", "none", &[], false, None);
    propose["text"] = json!("Use JSON");
    judge_scenario(
        &home,
        json!([judge_result(vec![
            entry(
                "Q-0001",
                "answer",
                "decision",
                &["D-0001", "P-001"],
                true,
                Some(0)
            ),
            entry("Q-0002", "answer", "decision", &["D-0099"], true, Some(0)),
            entry(
                "Q-0003",
                "answer",
                "inference",
                &["src/export.rs:12"],
                true,
                Some(1)
            ),
            entry("Q-0004", "answer", "inference", &[], false, Some(1)),
            propose,
            entry("Q-0006", "ask", "none", &[], false, None),
            entry("Q-0007", "answer", "principle", &["P-001"], true, Some(0)),
            entry("Q-0099", "answer", "inference", &[], true, Some(0)),
        ])]),
    );
    ask(&home, "seven", cards, resumed_steps());
    let judge = judge_round(&home);

    assert_eq!(
        states(&home),
        ["answered", "proposed", "answered", "proposed", "proposed", "asked", "asked"]
    );
    let all = rows(&home);
    let outcomes: Vec<&str> = all.iter().map(|r| r.outcome.as_deref().unwrap()).collect();
    assert_eq!(
        outcomes,
        ["answer", "propose", "answer", "propose", "propose", "ask", "ask"]
    );
    for r in &all {
        assert_eq!(r.judge_turn.as_deref(), Some(judge.as_str()));
    }
    let view = |id: &str| -> Value {
        serde_json::from_str(row(&home, id).proposal.as_deref().unwrap()).unwrap()
    };
    assert!(view("Q-0002")["rule"]
        .as_str()
        .unwrap()
        .contains("the source D-0099 does not exist"));
    assert!(view("Q-0004")["rule"]
        .as_str()
        .unwrap()
        .contains("not sure"));
    assert_eq!(view("Q-0005")["rule"], Value::Null);
    assert!(view("Q-0007")["rule"]
        .as_str()
        .unwrap()
        .contains("the topic `money`"));
    let q1 = row(&home, "Q-0001");
    assert_eq!(q1.basis.as_deref(), Some("decision"));
    assert_eq!(q1.sure, Some(true));
    assert_eq!(q1.sources.as_deref(), Some(r#"["D-0001","P-001"]"#));
    let answer: Value = serde_json::from_str(q1.answer.as_deref().unwrap()).unwrap();
    assert_eq!(answer["by"], "judge");
    assert_eq!(answer["text"], "CSV");
    assert_eq!(answer["why"], "The reason for Q-0001.");
    assert!(q1.answered.is_some());

    // Two decision files of the Judge, with the author judge.
    assert_eq!(
        decision_files(&home),
        ["D-0001.md", "D-0002.md", "D-0003.md"]
    );
    assert_eq!(q1.decision.as_deref(), Some("D-0002"));
    assert_eq!(row(&home, "Q-0003").decision.as_deref(), Some("D-0003"));
    let d2 = memory_file(&home, "decisions/D-0002.md");
    for line in [
        "by: judge",
        "basis: decision",
        "ticket: DEMO-12",
        "step: plan",
        "asked_by: builder",
        "kind: product",
        "area: [reports, exports]",
        "sources: [D-0001, P-001]",
        "## Why\nThe reason for Q-0001.",
        "## Answer\nCSV",
    ] {
        assert!(d2.contains(line), "{line:?} in {d2}");
    }
    let d3 = memory_file(&home, "decisions/D-0003.md");
    assert!(d3.contains("basis: inference"), "{d3}");
    assert!(d3.contains("sources: []"), "{d3}");
    assert!(d3.contains("Other evidence: src/export.rs:12"), "{d3}");
    assert!(d3.contains("## Answer\nXLSX"), "{d3}");
    let memory = home.vision.join("memory");
    assert_eq!(git_author(&memory, "decisions/D-0002.md"), "judge");
    assert_eq!(git_author(&memory, "decisions/D-0003.md"), "judge");
    assert_eq!(
        principle_uses(&home),
        "uses: {used: 1, agreed: 0, corrected: 0}",
        "a principle that a final answer names counts as used"
    );
    let log = std::fs::read_to_string(home.vision.join("log/vision.log")).unwrap();
    assert!(log.contains("unknown card(s) Q-0099"), "{log}");

    // What waits for the person.
    let needs = json_of(&home.run(&["needs", "--json"]));
    let ids: Vec<&str> = needs["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["Q-0002", "Q-0004", "Q-0005", "Q-0006", "Q-0007"]);
    let q5 = &needs["cards"][2];
    assert_eq!(q5["proposal"]["answer"], "Use JSON");
    assert_eq!(q5["proposal"]["choice"], Value::Null);
    assert_eq!(q5["proposal"]["reason"], "The reason for Q-0005.");
    assert_eq!(q5["choices"][1]["letter"], "b");
    assert_eq!(q5["recommended"], 1);
    let q2 = &needs["cards"][0];
    assert_eq!(q2["proposal"]["choice"], 1);
    assert_eq!(q2["proposal"]["sources"], json!(["D-0099"]));
    assert_eq!(q2["command"], "vision answer Q-0002 --choice 1");
    assert_eq!(needs["cards"][4]["proposal"], Value::Null);
    assert_eq!(needs["others"], json!([]));
    let snapshot: Value =
        serde_json::from_str(&std::fs::read_to_string(home.vision.join("state.json")).unwrap())
            .unwrap();
    assert_eq!(snapshot["needs"].as_array().unwrap().len(), 5);
    assert_eq!(snapshot["needs"][0]["kind"], "card");
    assert!(snapshot["needs"][0]["text"]
        .as_str()
        .unwrap()
        .contains("(the Judge proposes: CSV)"));

    let plain = home.run(&["needs"]);
    plain.expect_code(0);
    assert!(
        plain.stdout.starts_with("5 card(s) wait for you:"),
        "{plain:?}"
    );
    assert!(
        plain.stdout.contains("The Judge proposes: Use JSON"),
        "{plain:?}"
    );

    // The answers that the Judge gave alone.
    let answers = json_of(&home.run(&["answers", "--json"]));
    let ids: Vec<&str> = answers
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(
        ids.contains(&"Q-0001") && ids.contains(&"Q-0003"),
        "{ids:?}"
    );
    let a1 = answers
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "Q-0001")
        .unwrap();
    assert_eq!(a1["decision"], "D-0002");
    assert_eq!(a1["answer"], "CSV");
    assert_eq!(a1["basis"], "decision");
    assert_eq!(a1["sources"], json!(["D-0001", "P-001"]));
    assert_eq!(a1["ticket"], TICKET);
    assert_eq!(a1["corrected"], false);
    let table = home.run(&["answers", "--days", "1"]);
    table.expect_code(0);
    assert!(table.stdout.starts_with("ID "), "{table:?}");
    assert!(table.stdout.contains("D-0002"), "{table:?}");
    assert!(table.stdout.contains("vision correct ID"), "{table:?}");
}

#[test]
fn the_answers_of_the_person_go_back_to_the_asking_session() {
    let home = TestHome::new();
    seed_memory(&home);
    // The schema of a turn allows an empty label, but the test of a card in code does not.
    let mut bad = card(6, &[]);
    bad["options"][1]["label"] = json!(" ");
    let mut q2 = entry("Q-0002", "propose", "principle", &["P-001"], false, Some(0));
    q2["reason"] = json!("P-001 likely covers it.");
    judge_scenario(
        &home,
        json!([judge_result(vec![
            entry("Q-0001", "answer", "decision", &["D-0001"], true, Some(0)),
            q2,
            entry("Q-0003", "propose", "inference", &[], false, Some(0)),
            entry("Q-0004", "ask", "none", &[], false, None),
            entry("Q-0005", "ask", "none", &[], false, None),
        ])]),
    );
    let asking = ask(
        &home,
        "five",
        vec![
            card(1, &[]),
            card(2, &[]),
            card(3, &[]),
            card(4, &[]),
            card(5, &[]),
            bad,
        ],
        resumed_steps(),
    );
    judge_round(&home);
    assert_eq!(
        rows(&home).len(),
        5,
        "the card that is not valid gets no row"
    );

    // A card that the Judge answered, or that it still judges, is not for `answer`.
    let run = home.run(&["answer", "Q-0001", "--choice", "2"]);
    run.expect_code(1);
    assert!(run.stderr.contains("vision correct Q-0001"), "{run:?}");
    let run = home.run(&["answer", "Q-0009", "--choice", "1"]);
    run.expect_code(1);
    assert!(run.stderr.contains("there is no card Q-0009"), "{run:?}");
    let run = home.run(&["answer", "Q-0002", "--choice", "3"]);
    run.expect_code(1);
    assert!(run.stderr.contains("from 1 to 2"), "{run:?}");
    let run = home.run(&["answer", "Q-0002", "--choice", "0"]);
    run.expect_code(1);
    assert!(run.stderr.contains("counts from 1"), "{run:?}");

    // The proposal taken unchanged, a proposal changed, a text, and the needless mark.
    let run = home.run(&[
        "answer",
        "Q-0002",
        "--choice",
        "1",
        "--why",
        "Finance imports CSV.",
        "--when",
        "An export for an internal user.",
    ]);
    run.expect_code(0);
    assert!(
        run.stdout
            .starts_with("Saved the answer to Q-0002 as D-0003."),
        "{run:?}"
    );
    assert!(
        run.stdout.contains("when each card of its turn has one"),
        "{run:?}"
    );
    home.run(&["answer", "Q-0003", "--choice", "2"])
        .expect_code(0);
    home.run(&["answer", "Q-0004", "--text", "Use JSON"])
        .expect_code(0);
    let run = home.run(&["answer", "Q-0005", "--needless", "code"]);
    run.expect_code(0);
    assert!(run.stdout.contains("Marked Q-0005 as needless"), "{run:?}");
    assert!(
        run.stdout.contains("the next tick sends the answers back"),
        "{run:?}"
    );
    let run = home.run(&["answer", "Q-0005", "--text", "late"]);
    run.expect_code(1);
    assert!(run.stderr.contains("already marked as needless"), "{run:?}");

    let q2 = row(&home, "Q-0002");
    assert_eq!(q2.state, "answered");
    assert_eq!(q2.took_proposal, Some(true));
    assert_eq!(q2.decision.as_deref(), Some("D-0003"));
    let q3 = row(&home, "Q-0003");
    assert_eq!(q3.outcome.as_deref(), Some("propose"));
    assert_eq!(q3.took_proposal, Some(false), "a changed proposal");
    assert_eq!(row(&home, "Q-0004").took_proposal, None, "no proposal");
    let q5 = row(&home, "Q-0005");
    assert_eq!(q5.state, "needless");
    assert_eq!(q5.flag.as_deref(), Some("needless:code"));
    assert_eq!(q5.decision, None);
    assert_eq!(
        decision_files(&home),
        [
            "D-0001.md",
            "D-0002.md",
            "D-0003.md",
            "D-0004.md",
            "D-0005.md"
        ],
        "the needless card gets no decision file"
    );
    let d3 = memory_file(&home, "decisions/D-0003.md");
    for line in [
        "by: user",
        "basis: answer",
        "asked_by: builder",
        "## Answer\nCSV",
        "## Why\nFinance imports CSV.",
        "## Applies when\nAn export for an internal user.",
    ] {
        assert!(d3.contains(line), "{line:?} in {d3}");
    }
    assert!(memory_file(&home, "decisions/D-0005.md").contains("## Answer\nUse JSON"));
    assert_eq!(
        git_author(&home.vision.join("memory"), "decisions/D-0003.md"),
        "user"
    );
    assert_eq!(
        principle_uses(&home),
        "uses: {used: 0, agreed: 1, corrected: 0}",
        "the person took a proposal that named P-001"
    );
    let humans: Vec<(String, String, Option<String>)> = home
        .ledger()
        .conn()
        .prepare("SELECT kind, object, value FROM human ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        humans,
        [
            ("answer".into(), "Q-0002".into(), Some("CSV".into())),
            ("answer".into(), "Q-0003".into(), Some("XLSX".into())),
            ("answer".into(), "Q-0004".into(), Some("Use JSON".into())),
            ("needless".into(), "Q-0005".into(), Some("code".into())),
        ]
    );

    // The tick resumes the asking session with one message.
    tick(&home);
    let ledger = home.ledger();
    let resumed: Vec<_> = ledger
        .turns_of_ticket(TICKET)
        .unwrap()
        .into_iter()
        .filter(|t| t.resumes.as_deref() == Some(asking.session.as_str()))
        .collect();
    assert_eq!(resumed.len(), 1);
    let resumed = &resumed[0];
    assert_eq!(resumed.agent.as_deref(), Some("builder"));
    assert_eq!(resumed.step.as_deref(), Some("plan"));
    assert_eq!(resumed.kind.as_deref(), Some("reader"));
    wait_turn(&home, &resumed.id);
    let stream =
        std::fs::read_to_string(turn_dir(&home, &resumed.id).join("stream.jsonl")).unwrap();
    let facts = stream
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["subtype"] == "fake_facts")
        .expect("the resumed session read its facts");
    let message = facts["facts"]["message"].as_str().unwrap().to_string();
    for part in [
        "1. Question 1: must the export make CSV or XLSX?\n   Answer: CSV (choice 1)\n   Writer: judge\n   Sources: D-0001\n   Why: The reason for Q-0001.\n   Decision: D-0002",
        "2. Question 2: must the export make CSV or XLSX?\n   Answer: CSV (choice 1)\n   Writer: user (the person)\n   Sources: none\n   Why: Finance imports CSV.\n   Applies when: An export for an internal user.",
        "3. Question 3: must the export make CSV or XLSX?\n   Answer: XLSX (choice 2)\n   Writer: user (the person)",
        "4. Question 4: must the export make CSV or XLSX?\n   Answer: Use JSON\n",
        "5. Question 5: must the export make CSV or XLSX?\n   Answer: none. The person marked the question as needless: the code has the answer. Decide it yourself from the code and the files.",
        "Your card 6 \"Question 6: must the export make CSV or XLSX?\" was not valid",
    ] {
        assert!(message.contains(part), "{part:?} in {message}");
    }
    let answers_md =
        std::fs::read_to_string(home.vision.join("tickets/DEMO-12/answers.md")).unwrap();
    assert_eq!(
        answers_md.matches("\n## Q-").count() + usize::from(answers_md.starts_with("## Q-")),
        5
    );
    assert!(answers_md.contains("## Q-0001: Question 1"), "{answers_md}");
    assert!(
        answers_md.contains("- Writer: judge\n- Sources: D-0001\n"),
        "{answers_md}"
    );
    assert!(
        answers_md.contains("- Answer: none. The person marked the question as needless"),
        "{answers_md}"
    );

    // A second tick resumes nothing again, and collects the resumed turn.
    tick(&home);
    tick(&home);
    let count = home
        .ledger()
        .turns_of_ticket(TICKET)
        .unwrap()
        .into_iter()
        .filter(|t| t.resumes.as_deref() == Some(asking.session.as_str()))
        .count();
    assert_eq!(count, 1);
    let again = std::fs::read_to_string(home.vision.join("tickets/DEMO-12/answers.md")).unwrap();
    assert_eq!(again, answers_md);
    assert_eq!(
        home.ledger()
            .turn(&resumed.id)
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("done")
    );
}

#[test]
fn a_correction_supersedes_the_judge_and_reaches_the_ticket() {
    let home = TestHome::new();
    seed_memory(&home);
    judge_scenario(
        &home,
        json!([judge_result(vec![
            entry("Q-0001", "answer", "principle", &["P-001"], true, Some(0)),
            entry(
                "Q-0002",
                "answer",
                "decision",
                &["D-0001", "P-001"],
                true,
                Some(0)
            ),
            entry("Q-0003", "ask", "none", &[], false, None),
            entry("Q-0004", "answer", "inference", &[], true, Some(0)),
        ])]),
    );
    let resumed = json!([{"do": "init"}, {"do": "facts"}, {"do": "wait_input", "timeout": 20},
        {"do": "result", "structured_output": result_object("Planned.")}]);
    ask(
        &home,
        "correct",
        vec![card(1, &[]), card(2, &[]), card(3, &[]), card(4, &[])],
        resumed,
    );
    judge_round(&home);
    assert_eq!(states(&home), ["answered", "answered", "asked", "answered"]);
    assert_eq!(
        principle_uses(&home),
        "uses: {used: 2, agreed: 0, corrected: 0}"
    );
    let d_q1 = row(&home, "Q-0001").decision.unwrap();
    assert_eq!(d_q1, "D-0002");

    // Before the answers went back: a correction by the id of the decision file.
    let run = home.run(&[
        "correct",
        "D-0002",
        "XLSX",
        "--why",
        "The sheet keeps the formats.",
    ]);
    run.expect_code(0);
    assert!(
        run.stdout
            .contains("Corrected Q-0001: D-0005 replaces the decision D-0002"),
        "{run:?}"
    );
    assert!(
        run.stdout.contains("they carry the right answer"),
        "{run:?}"
    );
    let old = memory_file(&home, "decisions/D-0002.md");
    assert!(old.contains("status: superseded"), "{old}");
    let new = memory_file(&home, "decisions/D-0005.md");
    for line in [
        "by: user",
        "supersedes: D-0002",
        "## Answer\nXLSX",
        "## Why\nThe sheet keeps the formats.",
    ] {
        assert!(new.contains(line), "{line:?} in {new}");
    }
    let q1 = row(&home, "Q-0001");
    assert_eq!(q1.decision.as_deref(), Some("D-0005"));
    assert_eq!(q1.outcome.as_deref(), Some("answer"));
    let answer: Value = serde_json::from_str(q1.answer.as_deref().unwrap()).unwrap();
    assert_eq!(answer["by"], "user");
    assert_eq!(answer["text"], "XLSX");
    assert_eq!(
        principle_uses(&home),
        "uses: {used: 2, agreed: 0, corrected: 1}"
    );
    let corrections: i64 = home
        .ledger()
        .conn()
        .query_row(
            "SELECT count(*) FROM human WHERE kind = 'correct' AND object = 'Q-0001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(corrections, 1);
    let run = home.run(&["correct", "Q-0001", "CSV"]);
    run.expect_code(1);
    assert!(run.stderr.contains("already corrected"), "{run:?}");
    let run = home.run(&["correct", "Q-0003", "CSV"]);
    run.expect_code(1);
    assert!(
        run.stderr.contains("did not answer Q-0003 alone"),
        "{run:?}"
    );
    let run = home.run(&["correct", "Q-0002", " csv "]);
    run.expect_code(1);
    assert!(run.stderr.contains("nothing to correct"), "{run:?}");
    let run = home.run(&["correct", "P-001", "CSV"]);
    run.expect_code(1);
    assert!(run.stderr.contains("is a principle"), "{run:?}");

    // The answers carry the right answer.
    home.run(&["answer", "Q-0003", "--choice", "1"])
        .expect_code(0);
    tick(&home);
    let resumed = home
        .ledger()
        .last_turn(TICKET)
        .unwrap()
        .expect("the resumed turn");
    assert_eq!(resumed.agent.as_deref(), Some("builder"));
    let stream_path = turn_dir(&home, &resumed.id).join("stream.jsonl");
    let facts = wait_for(
        Duration::from_secs(20),
        "the facts of the resumed turn",
        || {
            std::fs::read_to_string(&stream_path)
                .ok()?
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .find(|v| v["subtype"] == "fake_facts")
        },
    );
    let message = facts["facts"]["message"].as_str().unwrap().to_string();
    assert!(
        message.contains("Answer: XLSX (choice 2)\n   Writer: user (the person)\n   Sources: none\n   Why: The sheet keeps the formats.\n   Decision: D-0005"),
        "{message}"
    );

    // While a turn of the ticket runs, the correction goes into it.
    let run = home.run(&["correct", "Q-0002", "XLSX"]);
    run.expect_code(0);
    assert!(
        run.stdout.contains(&format!(
            "Sent the correction to the running turn {}",
            resumed.id
        )),
        "{run:?}"
    );
    let stream = wait_for(
        Duration::from_secs(20),
        "the correction in the stream",
        || {
            std::fs::read_to_string(&stream_path)
                .ok()
                .filter(|t| t.contains("got: A correction from the person"))
        },
    );
    assert!(
        stream.contains("(Q-0002) is \\\"XLSX\\\", not \\\"CSV\\\" as the Judge said"),
        "{stream}"
    );
    wait_turn(&home, &resumed.id);
    tick(&home);

    // With no turn of the ticket, the correction waits for its next turn.
    let run = home.run(&["correct", "Q-0004", "XLSX"]);
    run.expect_code(0);
    assert!(run.stdout.contains("waits for its next turn"), "{run:?}");
    let inbox = home.vision.join("tickets/DEMO-12/inbox");
    let waiting: Vec<String> = std::fs::read_dir(&inbox)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    assert_eq!(waiting.len(), 1);
    assert!(waiting[0].contains("Q-0004"), "{waiting:?}");
    let answers_md =
        std::fs::read_to_string(home.vision.join("tickets/DEMO-12/answers.md")).unwrap();
    assert!(
        answers_md.contains("## Q-0004 corrected: Question 4: must the export make CSV or XLSX?\n\n- Answer: XLSX\n- Writer: user (the person)\n- Replaces: the answer D-0004 of the Judge"),
        "{answers_md}"
    );
    assert!(answers_md.contains("## Q-0002 corrected:"), "{answers_md}");
    assert!(
        !answers_md.contains("## Q-0001 corrected:"),
        "Q-0001 went back corrected: {answers_md}"
    );
    assert_eq!(
        principle_uses(&home),
        "uses: {used: 2, agreed: 0, corrected: 2}"
    );

    let answers = json_of(&home.run(&["answers", "--json"]));
    let corrected: Vec<bool> = answers
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["corrected"].as_bool().unwrap())
        .collect();
    assert_eq!(corrected, [true, true, true]);
    assert!(answers
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["correction"] == "XLSX" && a["answer"] == "CSV"));
}

#[test]
fn a_late_judge_is_killed_and_its_cards_go_to_the_person() {
    let home = TestHome::new();
    judge_scenario(&home, json!([[{"do": "init"}, {"do": "hang"}]]));
    ask(
        &home,
        "late",
        vec![card(1, &[]), card(2, &[])],
        resumed_steps(),
    );
    tick(&home);
    let judge = rows(&home)[0].judge_turn.clone().unwrap();
    let stream = turn_dir(&home, &judge).join("stream.jsonl");
    wait_for(Duration::from_secs(20), "the Judge to run", || {
        std::fs::read_to_string(&stream)
            .ok()
            .filter(|t| t.contains("\"init\""))
    });
    tick(&home);
    assert_eq!(
        states(&home),
        ["judging", "judging"],
        "the Judge still has time"
    );

    // Twenty minutes pass.
    home.ledger()
        .conn()
        .execute_batch("UPDATE questions SET created = '2026-01-01T00:00:00Z'")
        .unwrap();
    tick(&home);
    assert_eq!(states(&home), ["asked", "asked"]);
    let view: Value =
        serde_json::from_str(row(&home, "Q-0001").proposal.as_deref().unwrap()).unwrap();
    assert!(
        view["rule"]
            .as_str()
            .unwrap()
            .starts_with("rule 5: the Judge gave no outcome in 15 minutes"),
        "{view}"
    );
    let state = wait_turn(&home, &judge);
    assert_eq!(
        state.reason,
        Some(vision::turn::Reason::Killed),
        "{state:?}"
    );
    tick(&home);
    assert_eq!(
        home.ledger()
            .turn(&judge)
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("failed")
    );
    assert_eq!(
        states(&home),
        ["asked", "asked"],
        "the killed Judge changes nothing"
    );
    assert_eq!(judge_turns(&home).len(), 1);
}

#[test]
fn a_judge_that_fails_twice_sends_its_cards_to_the_person() {
    let home = TestHome::new();
    judge_scenario(
        &home,
        json!([[{"do": "init"}, {"do": "result", "structured_output": {"answers": "none"}}]]),
    );
    ask(&home, "fails", vec![card(1, &[])], resumed_steps());
    let first = judge_round(&home);
    let retry = row(&home, "Q-0001")
        .judge_turn
        .expect("the rows follow the automatic resume");
    assert_ne!(
        retry, first,
        "the collector resumed the Judge with continue"
    );
    assert_eq!(states(&home), ["judging"]);
    wait_turn(&home, &retry);
    tick(&home);
    assert_eq!(states(&home), ["asked"]);
    let view: Value =
        serde_json::from_str(row(&home, "Q-0001").proposal.as_deref().unwrap()).unwrap();
    assert!(view["rule"].as_str().unwrap().contains("failed"), "{view}");
}

#[test]
fn the_rows_follow_an_automatic_resume_of_the_judge_to_its_result() {
    let home = TestHome::new();
    judge_scenario(
        &home,
        json!([
            [{"do": "init"}, {"do": "result", "structured_output": {"kind": "result", "summary": "wrong"}}],
            judge_result(vec![entry("Q-0001", "answer", "inference", &[], true, Some(1))])
        ]),
    );
    ask(&home, "retry", vec![card(1, &[])], resumed_steps());
    judge_round(&home);
    let retry = row(&home, "Q-0001").judge_turn.unwrap();
    wait_turn(&home, &retry);
    tick(&home);
    let q1 = row(&home, "Q-0001");
    assert_eq!(q1.state, "answered");
    assert_eq!(q1.judge_turn.as_deref(), Some(retry.as_str()));
    assert_eq!(judge_turns(&home).len(), 2);
}

#[test]
fn answer_when_sure_false_makes_each_inference_a_proposal() {
    let home = TestHome::new();
    home.edit_config(|doc| doc["judge"]["answer_when_sure"] = toml_edit::value(false));
    judge_scenario(
        &home,
        json!([judge_result(vec![entry(
            "Q-0001",
            "answer",
            "inference",
            &[],
            true,
            Some(1)
        )])]),
    );
    ask(&home, "unsure", vec![card(1, &[])], resumed_steps());
    judge_round(&home);
    let q1 = row(&home, "Q-0001");
    assert_eq!(q1.state, "proposed");
    assert_eq!(q1.outcome.as_deref(), Some("propose"));
    let view: Value = serde_json::from_str(q1.proposal.as_deref().unwrap()).unwrap();
    assert!(view["rule"]
        .as_str()
        .unwrap()
        .contains("judge.answer_when_sure is false"));
    assert_eq!(decision_files(&home), Vec::<String>::new());
}

#[test]
fn a_turn_with_no_valid_card_is_a_failed_turn_and_gets_one_resume() {
    let home = TestHome::new();
    // The schema of a turn allows an empty text, but the test of a card in code does not.
    let mut bad = card(1, &[]);
    bad["text"] = json!("  ");
    let resumed = json!([{"do": "init"}, {"do": "facts"},
        {"do": "result", "structured_output": result_object("Fixed.")}]);
    let asking = ask(&home, "invalid", vec![bad], resumed);
    tick(&home);
    assert!(rows(&home).is_empty());
    let ledger = home.ledger();
    assert_eq!(
        ledger
            .turn(&asking.id.to_string())
            .unwrap()
            .unwrap()
            .status
            .as_deref(),
        Some("retried")
    );
    let retry = ledger.last_turn(TICKET).unwrap().unwrap();
    assert_eq!(retry.resumes.as_deref(), Some(asking.session.as_str()));
    let facts: Value = serde_json::from_str(
        &std::fs::read_to_string(turn_dir(&home, &retry.id).join("facts.json")).unwrap(),
    )
    .unwrap();
    let message = facts["message"].as_str().unwrap();
    assert!(message.starts_with("continue."), "{message}");
    assert!(
        message.contains("card 1: the card has no question text"),
        "{message}"
    );
    tick(&home);
    assert_eq!(
        home.ledger().turns_of_ticket(TICKET).unwrap().len(),
        2,
        "a second tick resumes nothing more"
    );
}

#[test]
fn two_ticks_in_a_row_and_a_tick_after_a_crash_create_nothing_twice() {
    let home = TestHome::new();
    judge_scenario(
        &home,
        json!([judge_result(vec![
            entry("Q-0001", "answer", "inference", &[], true, Some(0)),
            entry("Q-0002", "answer", "inference", &[], true, Some(1)),
        ])]),
    );
    let asking = ask(
        &home,
        "twice",
        vec![card(1, &[]), card(2, &[])],
        resumed_steps(),
    );
    tick(&home);
    tick(&home);
    assert_eq!(judge_turns(&home).len(), 1);
    assert_eq!(rows(&home).len(), 2);
    let judge = rows(&home)[0].judge_turn.clone().unwrap();
    wait_turn(&home, &judge);
    tick(&home);
    assert_eq!(decision_files(&home), ["D-0001.md", "D-0002.md"]);

    // The tick also resumed the session. A crash before its event: the event is gone.
    let resumes = |home: &TestHome| {
        home.ledger()
            .turns_of_ticket(TICKET)
            .unwrap()
            .into_iter()
            .filter(|t| t.resumes.as_deref() == Some(asking.session.as_str()))
            .count()
    };
    assert_eq!(resumes(&home), 1);
    home.ledger()
        .conn()
        .execute(
            "DELETE FROM events WHERE external_id = ?1",
            [format!("questions-resumed:{}", asking.id)],
        )
        .unwrap();
    // A crash after the decision file of Q-0002 and before its row: the row is in recording.
    home.ledger()
        .conn()
        .execute_batch(
            "UPDATE questions SET state = 'recording', decision = NULL, answered = '2026-01-01T00:00:00Z'
             WHERE id = 'Q-0002'",
        )
        .unwrap();
    tick(&home);
    tick(&home);
    assert_eq!(decision_files(&home), ["D-0001.md", "D-0002.md"]);
    let q2 = row(&home, "Q-0002");
    assert_eq!(q2.state, "answered");
    assert_eq!(q2.decision.as_deref(), Some("D-0002"));
    assert_eq!(
        resumes(&home),
        1,
        "the earlier resume is found, not repeated"
    );
    assert!(home
        .ledger()
        .questions_resumed(&asking.id.to_string())
        .unwrap());
    assert_eq!(judge_turns(&home).len(), 1);
}

#[test]
fn a_command_that_waits_for_the_tick_says_so() {
    let home = TestHome::new();
    judge_scenario(&home, json!([[{"do": "init"}, {"do": "hang"}]]));
    ask(&home, "early", vec![card(1, &[])], resumed_steps());
    tick(&home);
    let run = home.run(&["answer", "Q-0001", "--choice", "1"]);
    run.expect_code(1);
    assert!(run.stderr.contains("still judges Q-0001"), "{run:?}");
    let empty = home.run(&["needs"]);
    empty.expect_code(0);
    assert_eq!(empty.stdout, "Nothing waits for you.\n");
    let judge = rows(&home)[0].judge_turn.clone().unwrap();
    let ctx = home.ctx();
    vision::turn::kill_turn(&ctx, &key(TICKET), &TurnId::parse(&judge).unwrap()).unwrap();
    let none = home.run(&["answers"]);
    none.expect_code(0);
    assert_eq!(none.stdout, "The Judge gave no answer alone.\n");
}

#[test]
fn a_ticket_with_a_clone_gives_its_clone_to_the_judge() {
    let home = TestHome::new();
    let clone = home.scenarios.join("clone");
    std::fs::create_dir_all(&clone).unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&clone)
        .status()
        .unwrap();
    assert!(status.success());
    home.ledger()
        .conn()
        .execute(
            "INSERT INTO tickets (key, state, clone) VALUES ('DEMO-12', 'plan', ?1)",
            [clone.display().to_string()],
        )
        .unwrap();
    judge_scenario(&home, json!([judge_result(vec![])]));
    ask(&home, "clone", vec![card(1, &[])], resumed_steps());
    tick(&home);
    let judge = rows(&home)[0].judge_turn.clone().unwrap();
    let launch: Value = serde_json::from_str(
        &std::fs::read_to_string(turn_dir(&home, &judge).join("launch.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::canonicalize(launch["plan"]["workdir"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&clone).unwrap()
    );
    assert!(clone
        .join(format!(".vision/turns/{judge}/cards.json"))
        .is_file());
    let exclude = std::fs::read_to_string(clone.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|l| l == "/.vision/"), "{exclude}");
    wait_turn(&home, &judge);
    tick(&home);
}

#[test]
fn a_message_of_the_person_waits_for_the_work_turn_and_not_for_the_judge() {
    let home = TestHome::new();
    judge_scenario(
        &home,
        json!([[{"do": "init"}, {"do": "sleep", "seconds": 3},
        {"do": "result", "structured_output": {"answers": []}}]]),
    );
    ask(&home, "tell", vec![card(1, &[])], resumed_steps());
    tick(&home);
    let judge = rows(&home)[0].judge_turn.clone().unwrap();
    let run = home.run(&["tell", TICKET, "use the old export"]);
    run.expect_code(0);
    assert!(run.stdout.contains("waits for its next turn"), "{run:?}");
    wait_turn(&home, &judge);
    tick(&home);
    let judge_inbox = turn_dir(&home, &judge).join("inbox/sent");
    assert_eq!(
        std::fs::read_dir(&judge_inbox)
            .map(|d| d.count())
            .unwrap_or(0),
        0,
        "the Judge never got the message"
    );
    assert_eq!(
        std::fs::read_dir(home.vision.join("tickets/DEMO-12/inbox"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().is_file())
            .count(),
        1,
        "the message still waits for the next work turn"
    );
}
