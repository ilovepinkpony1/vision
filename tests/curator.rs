//! The Curator (section 9.4) with the fake `claude`: the signals, the Curator turn, the
//! proposal branch with its archive and its replays, the review, and the tick. Each Curator
//! turn plays `claude/fake-scenarios/curator.json` of the temporary home, and each replay of
//! the Judge plays `claude/fake-scenarios/judge.json`.

mod turn_support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};
use turn_support::{wait_for, Run, TestHome};
use vision::curator::records::{
    self, Applied, ProposalRecord, EVENT_APPLIED, EVENT_PROPOSAL, HUMAN_REJECT, OPEN,
};
use vision::curator::signals::{self, Mark};
use vision::memory::{Basis, CuratorChange, Kind, Memory, NewDecision, NewPrinciple, Op, Writer};
use vision::paths::TicketKey;
use vision::turn::TurnState;

const ASKING: &str = "20261110T100000Z-DEMO-12-plan-1";

fn user() -> Writer {
    Writer::Person("user".into())
}

fn memory(home: &TestHome) -> Memory {
    Memory::open(&home.ctx().paths, "user").unwrap()
}

fn memory_dir(home: &TestHome) -> PathBuf {
    home.vision.join("memory")
}

/// Runs git in a folder and gives its output.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn add_decision(memory: &Memory, ticket: &str, question: &str, by: Writer, why: &str) -> String {
    let mut new = NewDecision::answer(by.clone(), question, "CSV");
    if by == Writer::Judge {
        new.basis = Basis::Inference;
    }
    new.ticket = Some(ticket.into());
    new.kind = Some(Kind::Product);
    new.area = vec!["exports".into()];
    new.choices = vec!["CSV".into(), "XLSX".into()];
    new.why = why.into();
    memory.add_decision(&new).unwrap().id.to_string()
}

/// Decisions of the person on two tickets (D-0001 on DEMO-12, D-0002 on DEMO-13), one of the
/// Judge (D-0003), one with no reason (D-0004), and four principles of the person.
fn seed(home: &TestHome) {
    let memory = memory(home);
    let why = "The finance team imports CSV.";
    add_decision(&memory, "DEMO-12", "Must the export be CSV?", user(), why);
    add_decision(
        &memory,
        "DEMO-13",
        "Must the summary export be CSV?",
        user(),
        why,
    );
    add_decision(
        &memory,
        "DEMO-14",
        "Must the audit export be CSV?",
        Writer::Judge,
        "The code writes CSV.",
    );
    add_decision(
        &memory,
        "DEMO-12",
        "Must the log export be CSV?",
        user(),
        "",
    );
    for (agent, kind, area, rule) in [
        (
            "judge",
            Kind::Product,
            vec!["exports"],
            "When a report is exported, make sure that the file is CSV.",
        ),
        (
            "all",
            Kind::Code,
            vec![],
            "When a file is written, make sure that it is UTF-8.",
        ),
        (
            "all",
            Kind::Code,
            vec![],
            "When text is saved, make sure that it is UTF-8.",
        ),
        (
            "builder",
            Kind::Code,
            vec![],
            "When a test fails, make sure that the fix comes with a test.",
        ),
    ] {
        memory
            .add_principle(&NewPrinciple {
                agent: agent.into(),
                kind: Some(kind),
                area: area.into_iter().map(String::from).collect(),
                by: user(),
                evidence: vec![],
                rule: rule.into(),
                because: "It worked so far.".into(),
                not_when: String::new(),
            })
            .unwrap();
    }
}

fn card(n: usize) -> Value {
    json!({"text": format!("Question {n}: CSV or XLSX?"), "context": "The export button.",
           "example": "The March report.", "evidence": "src/export.rs:12", "kind": "product",
           "area": ["exports"], "recommended": 0, "why": "Finance imports CSV.",
           "options": [{"label": "CSV", "means": "plain"}, {"label": "XLSX", "means": "sheet"}]})
}

/// The cards Q-0001 to Q-0003 that the person answered with CSV, with their decisions. Their
/// asking turn already got its answers, so that a tick resumes nothing.
fn answered_cards(home: &TestHome) {
    let ledger = home.ledger();
    ledger
        .conn()
        .execute(
            "INSERT INTO turns (id, ticket, agent, step, status) VALUES (?1, 'DEMO-12', 'builder', 'plan', 'done')",
            [ASKING],
        )
        .unwrap();
    ledger
        .add_event(
            &vision::ledger::Event::now("tick", "questions-resumed")
                .external_id(&vision::ledger::questions::resume_mark(ASKING)),
        )
        .unwrap();
    for (n, decision) in [(1, "D-0001"), (2, "D-0002"), (3, "D-0004")] {
        let answer = json!({"choice": 0, "text": "CSV", "by": "user", "why": "", "when": ""});
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
}

fn entry(card: &str, basis: &str, sources: &[&str], outcome: &str, choice: Option<i64>) -> Value {
    json!({"card": card, "outcome": outcome, "basis": basis, "sources": sources, "sure": true,
           "choice": choice, "reason": format!("The reason for {card}.")})
}

/// The Judge of the replays: with the memory of today it answers Q-0001 wrong; with a memory
/// where P-005 is active it answers Q-0001 right from that principle. Each fake session ends
/// right after its result, so that no test waits for the grace of the supervisor.
fn judge_scenario(home: &TestHome) {
    let today = json!({"answers": [
        entry("Q-0001", "inference", &[], "answer", Some(1)),
        entry("Q-0002", "inference", &[], "answer", Some(0)),
        entry("Q-0003", "none", &[], "ask", None),
    ]});
    let proposed = json!({"answers": [
        entry("Q-0001", "principle", &["P-005"], "answer", Some(0)),
        entry("Q-0002", "inference", &[], "answer", Some(0)),
        entry("Q-0003", "none", &[], "ask", None),
    ]});
    write_scenario(
        home,
        "judge",
        json!([[{"do": "init"}, {"do": "result", "cost": 0.2, "structured_output": today,
                 "when": [{"file": "principles/P-005.md", "contains": "status: active",
                           "set": {"structured_output": proposed}}]},
                {"do": "exit", "code": 0}]]),
    );
}

fn write_scenario(home: &TestHome, agent: &str, runs: Value) {
    let dir = home.vision.join("claude/fake-scenarios");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{agent}.json")),
        json!({ "turns": runs }).to_string(),
    )
    .unwrap();
}

fn fixture_output() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/curator/output.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn curator_scenario(home: &TestHome, output: Value, sleep: f64) {
    write_scenario(
        home,
        "curator",
        json!([[{"do": "init"}, {"do": "sleep", "seconds": sleep},
                {"do": "result", "cost": 0.4, "structured_output": output},
                {"do": "exit", "code": 0}]]),
    );
}

fn proposals(home: &TestHome) -> Vec<String> {
    let out = git(
        &memory_dir(home),
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/proposal/",
        ],
    );
    out.lines().map(str::to_string).collect()
}

fn curator_turns(home: &TestHome) -> Vec<String> {
    home.ledger()
        .turns_of_ticket("curator")
        .unwrap()
        .into_iter()
        .filter(|t| t.agent.as_deref() == Some("curator"))
        .map(|t| t.id)
        .collect()
}

fn turn_dir(home: &TestHome, id: &str) -> PathBuf {
    home.ctx()
        .paths
        .turn(
            &TicketKey::parse("curator").unwrap(),
            &vision::paths::TurnId::parse(id).unwrap(),
        )
        .dir
}

fn wait_curator_turn(home: &TestHome, id: &str) {
    let status = turn_dir(home, id).join("turn.json");
    wait_for(Duration::from_secs(30), "the Curator turn to end", || {
        TurnState::read(&status)
            .ok()
            .flatten()
            .filter(TurnState::is_ended)
    });
}

fn tick(home: &TestHome) -> Run {
    let run = home.run(&["tick"]);
    run.expect_code(0);
    run
}

fn show(home: &TestHome, rev_path: &str) -> String {
    git(&memory_dir(home), &["show", rev_path])
}

fn main_file(home: &TestHome, path: &str) -> String {
    std::fs::read_to_string(memory_dir(home).join(path)).unwrap()
}

/// A fake editor: a script that appends a line to the end of the file, the `## Not when`
/// section of a principle. The path has spaces, so it is quoted for the shell.
fn editor(home: &TestHome) -> String {
    let path = home.scenarios.join("editor.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\nprintf 'A customer asks for a sheet.\\n' >> \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!("'{}'", path.display())
}

#[test]
fn curate_builds_a_proposal_with_its_replays_and_the_review_brings_it_to_main() {
    let home = TestHome::new();
    seed(&home);
    answered_cards(&home);
    judge_scenario(&home);
    curator_scenario(&home, fixture_output(), 0.0);
    let dir = memory_dir(&home);
    let head = git(&dir, &["rev-parse", "main"]);
    let main_before: Vec<String> = ["principles/P-001.md", "principles/P-003.md", "ARCHIVE.md"]
        .iter()
        .map(|p| main_file(&home, p))
        .collect();

    let run = home.run(&["curate"]);
    run.expect_code(0);
    let out = &run.stdout;
    assert!(out.contains("Started the Curator turn"), "{run:?}");
    let names = proposals(&home);
    assert_eq!(names.len(), 1, "{run:?}");
    let name = names[0].clone();
    assert!(
        out.contains(&format!("It proposes 5 change(s) in {name}.")),
        "{run:?}"
    );
    assert!(
        out.contains("Dropped 1 change(s) of the Curator:"),
        "{run:?}"
    );
    assert!(
        out.contains("change 6 of the Curator (add principles/NEW-2.md): a new principle needs at least 2 active decisions of the person from 2 different tickets as evidence, but it names 2 from 1 ticket(s)"),
        "{run:?}"
    );
    assert!(
        out.contains("Replay of 3 answers: today 1 right, 1 wrong, 1 to the person; proposed 2 right, 0 wrong, 1 to the person; 1 card(s) changed side."),
        "{run:?}"
    );
    assert!(out.contains("Run `vision memory review`"), "{run:?}");

    // The Curator turn: a reader of the agent curator, the step curate, under the key curator,
    // with the signals in its session view and the memory as an extra folder.
    let turns = curator_turns(&home);
    assert_eq!(turns.len(), 1);
    let turn = turns[0].clone();
    assert!(turn.contains("-curator-curate-"), "{turn}");
    let launch: Value = serde_json::from_str(
        &std::fs::read_to_string(turn_dir(&home, &turn).join("launch.json")).unwrap(),
    )
    .unwrap();
    let request = &launch["request"];
    assert_eq!(request["agent"], "curator");
    assert_eq!(request["step"], "curate");
    assert_eq!(request["kind"], "reader");
    assert_eq!(request["ticket"], "curator");
    assert_eq!(request["add_dirs"], json!([dir.display().to_string()]));
    assert_eq!(launch["plan"]["schema"], "curator.schema.json");
    let workdir = PathBuf::from(request["workdir"].as_str().unwrap());
    assert!(
        workdir.starts_with(home.vision.join("tickets/curator/curate")),
        "{workdir:?}"
    );
    let facts: Value = serde_json::from_str(
        &std::fs::read_to_string(turn_dir(&home, &turn).join("facts.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(facts["memory"], dir.display().to_string());
    assert_eq!(facts["person"], "user");
    assert_eq!(
        facts["signals"],
        format!(".vision/turns/{turn}/signals.json")
    );
    let signals: Value = serde_json::from_str(
        &std::fs::read_to_string(workdir.join(format!(".vision/turns/{turn}/signals.json")))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(signals["since"], Value::Null);
    assert_eq!(signals["new_decisions"].as_array().unwrap().len(), 4);
    assert_eq!(signals["no_reason"][0]["id"], "D-0004");
    assert!(
        std::fs::read_to_string(turn_dir(&home, &turn).join("role.md"))
            .unwrap()
            .contains("# Curator")
    );

    // The proposal branch: the candidate with the next id, the retired files that stay, and
    // the archive. The main branch did not move.
    assert_eq!(git(&dir, &["rev-parse", "main"]), head);
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");
    let after: Vec<String> = ["principles/P-001.md", "principles/P-003.md", "ARCHIVE.md"]
        .iter()
        .map(|p| main_file(&home, p))
        .collect();
    assert_eq!(after, main_before);
    let added = show(&home, &format!("{name}:principles/P-005.md"));
    assert!(
        added.contains("\nstatus: candidate\n") && added.contains("\nby: curator\n"),
        "{added}"
    );
    let merged = show(&home, &format!("{name}:principles/P-003.md"));
    assert!(
        merged.contains("\nstatus: retired\n") && merged.contains("Merged into P-002."),
        "{merged}"
    );
    let retired = show(&home, &format!("{name}:principles/P-004.md"));
    assert!(retired.contains("\nstatus: retired\n"), "{retired}");
    let archive = show(&home, &format!("{name}:ARCHIVE.md"));
    for n in [2, 3, 4] {
        assert!(
            archive.contains(&format!("## {name} change {n}\n")),
            "{archive}"
        );
    }
    assert!(
        archive.contains("> When a report is exported, make sure that the file is CSV."),
        "{archive}"
    );

    // The ledger keeps the summary, the dropped change, and both replays with the proposal.
    let ledger = home.ledger();
    let record: ProposalRecord = records::read(&ledger, EVENT_PROPOSAL, &name)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, OPEN);
    assert_eq!(record.turn, turn);
    assert_eq!(record.summary, "Tidy the export rules, and ask about XLSX.");
    assert_eq!(record.dropped.len(), 1);
    let replays = record.replays.clone().unwrap();
    assert_eq!(replays.cards, 3);
    assert_eq!(replays.today.as_ref().unwrap().agree, 1);
    assert_eq!(replays.proposed.as_ref().unwrap().agree, 2);
    assert!(!replays.today_stored);
    let applied: Applied = records::read(&ledger, EVENT_APPLIED, &turn)
        .unwrap()
        .unwrap();
    assert_eq!(applied.proposal.as_deref(), Some(name.as_str()));
    assert_eq!((applied.changes, applied.kept), (6, 5));

    // An open proposal is a need of `vision status` and `vision prompt`.
    let status: Value = serde_json::from_str(&home.run(&["status", "--json"]).stdout).unwrap();
    let needs = status["needs"].as_array().unwrap();
    assert!(
        needs
            .iter()
            .any(|n| n["kind"] == "proposal" && n["id"] == name.as_str()),
        "{status:#}"
    );
    let needs: Value = serde_json::from_str(&home.run(&["needs", "--json"]).stdout).unwrap();
    assert_eq!(needs["others"][0]["kind"], "proposal", "{needs:#}");
    assert_eq!(
        needs["others"][0]["command"],
        format!("vision memory review --proposal {name}")
    );
    tick(&home);
    let prompt = home.run(&["prompt"]);
    assert!(prompt.stdout.starts_with("V:1 "), "{prompt:?}");
    assert_eq!(
        curator_turns(&home).len(),
        1,
        "the tick starts no second run"
    );

    // `memory review --json`: the changes with their evidence and diffs, the dropped change,
    // and the replay numbers with the card that changed side.
    let run = home.run(&["memory", "review", "--json"]);
    run.expect_code(0);
    let views: Value = serde_json::from_str(&run.stdout).unwrap();
    let view = &views[0];
    assert_eq!(view["name"], name.as_str());
    assert_eq!(view["open"], 5);
    assert_eq!(view["turn"], turn.as_str());
    assert_eq!(view["changes"].as_array().unwrap().len(), 5);
    assert_eq!(view["changes"][0]["op"], "add");
    assert_eq!(view["changes"][0]["evidence"][0]["id"], "D-0001");
    assert_eq!(
        view["changes"][0]["evidence"][0]["question"],
        "Must the export be CSV?"
    );
    assert_eq!(view["changes"][0]["evidence"][1]["ticket"], "DEMO-13");
    assert!(view["changes"][1]["diffs"][0]["diff"]
        .as_str()
        .unwrap()
        .contains("+When an internal user exports a report"));
    assert_eq!(view["dropped"][0]["index"], 6);
    assert_eq!(view["replays"]["today"]["agree"], 1);
    assert_eq!(view["replays"]["proposed"]["agree"], 2);
    assert_eq!(view["changed"][0]["id"], "Q-0001");
    assert_eq!(view["changed"][0]["today"], "XLSX");
    assert_eq!(view["changed"][0]["proposed"], "CSV");
    assert_eq!(view["changed"][0]["today_side"], "wrong");
    assert_eq!(view["changed"][0]["proposed_side"], "right");
    // Without a terminal, the review prints the list and changes nothing.
    let text = home.run(&["memory", "review"]);
    text.expect_code(0);
    assert!(text.stdout.contains("Cards that changed side:"), "{text:?}");
    assert!(
        text.stdout.contains("Q-0001: Question 1: CSV or XLSX?"),
        "{text:?}"
    );
    assert!(
        text.stdout
            .contains("Change 1 of 5: add principles/P-005.md"),
        "{text:?}"
    );
    assert_eq!(git(&dir, &["rev-parse", "main"]), head);

    // Accept change 1 without a terminal: the candidate becomes active with the person as `by`.
    let run = home.run(&["memory", "review", "--accept", "1"]);
    run.expect_code(0);
    assert!(
        run.stdout
            .contains("Accepted change 1: add principles/P-005.md in the commit"),
        "{run:?}"
    );
    let p5 = main_file(&home, "principles/P-005.md");
    assert!(
        p5.contains("\nstatus: active\n") && p5.contains("\nby: user\n"),
        "{p5}"
    );

    // The interactive review: edit then accept change 2, reject change 3 with a reason, skip
    // change 4, and answer the question of change 5.
    let ctx = home.ctx();
    let mut input = std::io::Cursor::new(
        "e\nr\nThey differ in scope.\ns\na\nNever so far.\n"
            .as_bytes()
            .to_vec(),
    );
    let mut out: Vec<u8> = Vec::new();
    vision::curator::review_lines(&ctx, &mut input, &mut out, Some(&editor(&home)), None).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("Accepted your text of change 2"), "{out}");
    assert!(out.contains("Rejected change 3."), "{out}");
    assert!(out.contains("Accepted change 5."), "{out}");
    assert!(!out.contains("Closed"), "change 4 is still open: {out}");
    let p1 = main_file(&home, "principles/P-001.md");
    assert!(
        p1.contains("When an internal user exports a report"),
        "{p1}"
    );
    assert!(
        p1.contains("A customer asks for a sheet."),
        "the edited text: {p1}"
    );
    assert!(
        main_file(&home, "principles/P-003.md").contains("\nstatus: active\n"),
        "the rejected merge did not reach main"
    );

    // The last open change closes the proposal and deletes its branch.
    let run = home.run(&["memory", "review", "--accept", "all"]);
    run.expect_code(0);
    assert!(
        run.stdout
            .contains("Accepted change 4: retire principles/P-004.md"),
        "{run:?}"
    );
    assert!(run.stdout.contains(&format!("Closed {name}")), "{run:?}");
    assert!(proposals(&home).is_empty());
    assert!(main_file(&home, "principles/P-004.md").contains("\nstatus: retired\n"));
    let record: ProposalRecord = records::read(&ledger, EVENT_PROPOSAL, &name)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, "closed");
    let run = home.run(&["memory", "review", "--json"]);
    assert_eq!(run.stdout.trim(), "[]");

    // Main has one commit of the person for each accepted change, and the archive of each.
    let log = git(&dir, &["log", "--format=%an|%s", &format!("{head}..main")]);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 3, "{log}");
    assert!(
        lines.iter().all(|l| l.starts_with("user|Accept change ")),
        "{log}"
    );
    assert!(lines[0].contains(&format!("change 4 of {name}")), "{log}");
    let archive = main_file(&home, "ARCHIVE.md");
    assert!(
        archive.contains(&format!("## {name} change 2\n")),
        "{archive}"
    );
    assert!(
        archive.contains(&format!("## {name} change 4\n")),
        "{archive}"
    );
    assert!(
        !archive.contains(&format!("## {name} change 3\n")),
        "{archive}"
    );
    home.run(&["memory", "lint"]).expect_code(0);

    // The rejected change and the answered question reach the next signals.
    let rows = ledger.human_rows_after(0, HUMAN_REJECT).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1.reason.as_deref(), Some("They differ in scope."));
    let (_, last) = records::last_good(&ledger).unwrap().unwrap();
    let next = signals::collect(
        &ctx,
        &ledger,
        &memory(&home),
        Some(&last.mark),
        vision::util::time::now(),
    )
    .unwrap();
    assert_eq!(next.signals.rejected.len(), 1, "{:#?}", next.signals);
    assert_eq!(next.signals.rejected[0].why, "They differ in scope.");
    assert_eq!(next.signals.rejected[0].change["op"], "merge");
    assert_eq!(next.signals.answered_questions[0].answer, "Never so far.");
    assert!(next.signals.new_decisions.is_empty());

    // A second run of the same day gets a new name, because main names the first one.
    let run = home.run(&["curate"]);
    run.expect_code(0);
    let names = proposals(&home);
    assert_eq!(names.len(), 1, "{run:?}");
    assert_ne!(
        names[0], name,
        "the closed name is not given again: {run:?}"
    );
    assert!(
        run.stdout.contains("P-004 is already retired"),
        "the store drops what the checks could not know: {run:?}"
    );
    let second: ProposalRecord = records::read(&ledger, EVENT_PROPOSAL, &names[0])
        .unwrap()
        .unwrap();
    assert_ne!(second.turn, turn);
}

#[test]
fn curate_with_empty_signals_starts_no_turn() {
    let home = TestHome::new();
    let run = home.run(&["curate"]);
    run.expect_code(0);
    assert!(
        run.stdout
            .contains("No signal, so no Curator turn started."),
        "{run:?}"
    );
    assert!(run.stdout.contains("Nothing waits for review."), "{run:?}");
    assert!(curator_turns(&home).is_empty());
    let run = home.run(&["memory", "review"]);
    run.expect_code(0);
    assert_eq!(run.stdout, "Nothing waits for review.\n");
}

#[test]
fn a_curator_run_with_only_invalid_changes_makes_no_proposal() {
    let home = TestHome::new();
    seed(&home);
    let output = json!({"summary": "Two bad ideas.", "changes": [
        {"op": "retire", "target": "P-042", "reason": "No such file."},
        {"op": "edit", "path": "principles/P-001.md", "content": "not a memory file", "reason": "Broken."},
    ]});
    curator_scenario(&home, output, 0.0);
    let run = home.run(&["curate"]);
    run.expect_code(0);
    assert!(
        run.stdout
            .contains("Each change of the Curator was dropped."),
        "{run:?}"
    );
    assert!(
        run.stdout
            .contains("`P-042`, which is not a file of the memory"),
        "{run:?}"
    );
    assert!(
        run.stdout.contains("the content is not a memory file"),
        "{run:?}"
    );
    assert!(proposals(&home).is_empty());
    // The run ended well, so the next signals start after it.
    let run = home.run(&["curate"]);
    run.expect_code(0);
    assert!(
        run.stdout.contains("No signal since the last Curator run"),
        "{run:?}"
    );
}

/// Changes one file of a branch by hand, as a person with git would.
fn hand_edit_branch(home: &TestHome, branch: &str, path: &str, text: &str) {
    let dir = memory_dir(home);
    let tree = home.scenarios.join("hand tree");
    git(
        &dir,
        &["worktree", "add", "-q", tree.to_str().unwrap(), branch],
    );
    std::fs::write(tree.join(path), text).unwrap();
    let status = Command::new("git")
        .arg("-C")
        .arg(&tree)
        .args([
            "-c",
            "user.name=someone",
            "-c",
            "user.email=someone@localhost",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(["commit", "-q", "-a", "-m", "hand edit"])
        .status()
        .unwrap();
    assert!(status.success());
    git(
        &dir,
        &["worktree", "remove", "--force", tree.to_str().unwrap()],
    );
}

#[test]
fn a_proposal_that_loses_a_line_is_rejected_and_main_stays() {
    let home = TestHome::new();
    seed(&home);
    let memory = memory(&home);
    let dir = memory_dir(&home);
    let head = git(&dir, &["rev-parse", "main"]);
    let content = main_file(&home, "principles/P-001.md").replace(
        "When a report is exported, make sure that the file is CSV.",
        "When an internal user exports a report, make sure that the file is CSV.",
    );
    let built = memory
        .build_proposal(
            "Narrow P-001.",
            &[CuratorChange {
                op: Op::Edit,
                path: Some("principles/P-001.md".into()),
                content: Some(content),
                removes: vec![],
                target: None,
                question: None,
                evidence: vec![],
                reason: "Too wide.".into(),
            }],
            vision::util::time::now().date(),
        )
        .unwrap();
    let name = built.proposal.unwrap().name;
    // A hand edit drops the archive entry, so the old line of the rule is lost.
    hand_edit_branch(&home, &name, "ARCHIVE.md", "");
    let ctx = home.ctx();
    let ledger = home.ledger();
    let record = ProposalRecord {
        proposal: name.clone(),
        turn: "a-turn".into(),
        summary: "Narrow P-001.".into(),
        state: OPEN.into(),
        ..ProposalRecord::default()
    };
    let done = vision::curator::runs::finish(&ctx, &ledger, &memory, record).unwrap();
    assert_eq!(done.state, "rejected");
    assert_eq!(done.lost.len(), 1, "{:?}", done.lost);
    assert!(
        done.lost[0].contains("principles/P-001.md")
            && done.lost[0].contains("When a report is exported"),
        "{:?}",
        done.lost
    );
    assert!(done.replays.is_none(), "a rejected proposal gets no replay");
    assert!(proposals(&home).is_empty(), "its branch is deleted");
    assert_eq!(git(&dir, &["rev-parse", "main"]), head);
    let stored: ProposalRecord = records::read(&ledger, EVENT_PROPOSAL, &name)
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, "rejected");
}

#[test]
fn an_edited_change_passes_the_lint_and_keeps_each_line() {
    let home = TestHome::new();
    seed(&home);
    let memory = memory(&home);
    let dir = memory_dir(&home);
    let head = git(&dir, &["rev-parse", "main"]);
    let original = main_file(&home, "principles/P-001.md");
    let narrower = original.replace(
        "When a report is exported, make sure that the file is CSV.",
        "When an internal user exports a report, make sure that the file is CSV.",
    );
    let built = memory
        .build_proposal(
            "Narrow P-001.",
            &[CuratorChange {
                op: Op::Edit,
                path: Some("principles/P-001.md".into()),
                content: Some(narrower.clone()),
                removes: vec![],
                target: None,
                question: None,
                evidence: vec![],
                reason: "Too wide.".into(),
            }],
            vision::util::time::now().date(),
        )
        .unwrap();
    let name = built.proposal.unwrap().name;
    let tip = git(&dir, &["rev-parse", &name]);

    // A text that fails the lint changes nothing.
    let err = memory
        .edit_change(
            &name,
            1,
            &narrower.replace("status: active", "status: maybe"),
        )
        .unwrap_err();
    assert!(
        err.downcast_ref::<vision::memory::Invalid>().is_some(),
        "{err:#}"
    );
    assert_eq!(git(&dir, &["rev-parse", &name]), tip);

    // A valid text that also drops the reason: the archive entry of the change holds it.
    let edited = narrower.replace("It worked so far.", "The finance team imports CSV.");
    assert!(memory.edit_change(&name, 1, &edited).unwrap());
    assert!(
        !memory.edit_change(&name, 1, &edited).unwrap(),
        "the same text"
    );
    assert_eq!(
        show(&home, &format!("{name}:principles/P-001.md")),
        edited.trim_end()
    );
    let archive = show(&home, &format!("{name}:ARCHIVE.md"));
    assert_eq!(
        archive.matches(&format!("## {name} change 1")).count(),
        1,
        "{archive}"
    );
    assert!(archive.contains("> It worked so far."), "{archive}");
    assert!(
        archive.contains("> When a report is exported, make sure that the file is CSV."),
        "{archive}"
    );
    assert!(memory.proposal_lost_lines(&name).unwrap().is_empty());
    let view = memory.proposal(&name).unwrap();
    assert_eq!(view.file.changes[0].note.as_deref(), Some("edited by user"));
    assert_eq!(git(&dir, &["log", "-1", "--format=%an", &name]), "user");
    assert_eq!(git(&dir, &["rev-parse", "main"]), head, "main did not move");
}

#[test]
fn the_signals_start_at_the_mark_of_the_last_good_run() {
    let home = TestHome::new();
    seed(&home);
    let ledger = home.ledger();
    ledger
        .conn()
        .execute_batch(
            "INSERT INTO turns (id, ticket, agent, step, status) VALUES ('t1', 'DEMO-12', 'builder', 'plan', 'done');
             INSERT INTO questions (id, ticket, turn, card, state, outcome, proposal, answer, took_proposal, decision, answered)
             VALUES ('Q-0001', 'DEMO-12', 't1', '{\"text\": \"CSV or XLSX?\", \"options\": [{\"label\": \"CSV\"}, {\"label\": \"XLSX\"}]}',
                     'answered', 'propose', '{\"judge\": {\"card\": \"Q-0001\", \"outcome\": \"propose\", \"choice\": 1, \"basis\": \"inference\", \"sure\": false}}',
                     '{\"choice\": 0, \"text\": \"CSV\", \"by\": \"user\", \"why\": \"Finance.\"}', 0, 'D-0001', '2026-01-10T10:00:00Z');
             INSERT INTO questions (id, ticket, turn, card, state, flag, answered)
             VALUES ('Q-0002', 'DEMO-12', 't1', '{\"text\": \"Which folder?\"}', 'needless', 'needless:code', '2026-01-10T10:01:00Z');
             INSERT INTO human (at, ticket, kind, object, value, reason) VALUES ('2026-01-10T10:02:00Z', 'DEMO-12', 'correct', 'Q-0001', 'CSV', 'The finance team.');
             INSERT INTO review_rows (round, comment_id, reviewer, verdict, fix) VALUES ('DEMO-12:1', 7, 'someone', 'valid', 1);",
        )
        .unwrap();
    let ticket = home.vision.join("tickets/DEMO-12");
    std::fs::create_dir_all(&ticket).unwrap();
    std::fs::write(ticket.join("plan.md"), "Step 1.\nStep 2.\n").unwrap();
    std::fs::write(ticket.join("approved-plan.md"), "Step 1.\nStep 3.\n").unwrap();
    let ctx = home.ctx();
    let memory = memory(&home);
    let now = vision::util::time::now();
    let first = signals::collect(&ctx, &ledger, &memory, None, now).unwrap();
    let s = &first.signals;
    assert_eq!(s.new_decisions.len(), 4);
    assert_eq!(s.new_decisions[2].by, "judge");
    assert_eq!(s.no_reason.len(), 1);
    assert_eq!(s.changed_proposals.len(), 1);
    assert_eq!(s.changed_proposals[0].proposal, "XLSX");
    assert_eq!(s.changed_proposals[0].answer, "CSV");
    assert_eq!(s.corrections.len(), 1);
    assert_eq!(s.corrections[0].judge, "XLSX");
    assert_eq!(s.corrections[0].why, "The finance team.");
    assert_eq!(s.needless[0].reason, "code");
    assert_eq!(s.plans[0].ticket, "DEMO-12");
    assert_eq!(s.plans[0].removed, json!({"untrusted": "Step 2."}));
    assert_eq!(s.plans[0].added, json!({"untrusted": "Step 3."}));
    assert_eq!(s.reviews[0].reviewer, json!({"untrusted": "someone"}));
    assert!(!s.overlaps.is_empty(), "P-001 and the decisions of exports");
    assert_eq!(first.mark.human_id, 1);
    assert_eq!(first.mark.review_rowid, 1);

    // After the mark: only what came later.
    let mark = Mark {
        at: vision::util::time::rfc3339(now + ::time::Duration::seconds(5)),
        ..first.mark.clone()
    };
    let later = vision::util::time::now() + ::time::Duration::seconds(10);
    let again = signals::collect(&ctx, &ledger, &memory, Some(&mark), later).unwrap();
    assert!(again.signals.is_empty(), "{:#?}", again.signals);
    let new = add_decision(
        &memory,
        "DEMO-15",
        "Must the invoice export be CSV?",
        user(),
        "",
    );
    let third = signals::collect(&ctx, &ledger, &memory, Some(&mark), later).unwrap();
    let ids: Vec<&str> = third
        .signals
        .new_decisions
        .iter()
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(ids, [new.as_str()]);
    assert_eq!(third.signals.no_reason.len(), 1);
    assert!(
        third
            .signals
            .overlaps
            .iter()
            .all(|o| o.files.contains(&new)),
        "only pairs with the new file: {:#?}",
        third.signals.overlaps
    );
}

#[test]
fn the_tick_starts_a_curator_turn_after_a_merge_and_never_two_at_once() {
    let home = TestHome::new();
    seed(&home);
    curator_scenario(
        &home,
        json!({"summary": "Nothing to change.", "changes": []}),
        1.5,
    );
    tick(&home);
    assert!(curator_turns(&home).is_empty(), "no merge and no week yet");

    let ledger = home.ledger();
    assert!(records::record_merged(&ledger, "DEMO-12", 7, Some("abc1234")).unwrap());
    tick(&home);
    let turns = curator_turns(&home);
    assert_eq!(turns.len(), 1, "the merge starts a run");
    let started: records::Started = records::read(&ledger, records::EVENT_STARTED, &turns[0])
        .unwrap()
        .unwrap();
    assert_eq!(started.why, "DEMO-12 merged");

    // A second merge while the turn runs starts no second turn, and neither does curate.
    assert!(records::record_merged(&ledger, "DEMO-13", 8, None).unwrap());
    tick(&home);
    assert_eq!(curator_turns(&home).len(), 1);
    let run = home.run(&["curate"]);
    run.expect_code(0);
    assert!(
        run.stdout.contains("A Curator turn already runs"),
        "{run:?}"
    );
    assert!(
        run.stdout.contains("The Curator proposed no change."),
        "{run:?}"
    );
    assert_eq!(curator_turns(&home).len(), 1);
    let applied: Applied = records::read(&ledger, EVENT_APPLIED, &turns[0])
        .unwrap()
        .unwrap();
    assert_eq!(
        applied.note.as_deref(),
        Some("the Curator proposed no change")
    );

    // The merge of DEMO-13 is still newer than the last start, but nothing happened since.
    tick(&home);
    assert_eq!(curator_turns(&home).len(), 1, "empty signals start no turn");
    // A new decision is a signal: the due run starts now.
    add_decision(
        &memory(&home),
        "DEMO-13",
        "Must the export have a header?",
        user(),
        "Finance.",
    );
    tick(&home);
    let turns = curator_turns(&home);
    assert_eq!(turns.len(), 2);
    wait_curator_turn(&home, &turns[1]);
    tick(&home);
    let applied: Applied = records::read(&ledger, EVENT_APPLIED, &turns[1])
        .unwrap()
        .unwrap();
    assert_eq!(applied.summary, "Nothing to change.");
    tick(&home);
    assert_eq!(curator_turns(&home).len(), 2);
}

#[test]
fn the_tick_starts_a_curator_turn_seven_days_after_the_last_good_run() {
    let home = TestHome::new();
    seed(&home);
    curator_scenario(
        &home,
        json!({"summary": "Nothing to change.", "changes": []}),
        0.0,
    );
    let ledger = home.ledger();
    let head = git(&memory_dir(&home), &["rev-parse", "main"]);
    let good_run = |turn: &str, days: i64| {
        let at = vision::util::time::now() - ::time::Duration::days(days);
        records::add(
            &ledger,
            "tick",
            EVENT_APPLIED,
            turn,
            &Applied {
                turn: turn.into(),
                mark: Mark {
                    at: vision::util::time::rfc3339(at),
                    memory_head: Some(head.clone()),
                    human_id: 0,
                    review_rowid: 0,
                },
                ..Applied::default()
            },
        )
        .unwrap();
    };
    good_run("old-run", 2);
    add_decision(
        &memory(&home),
        "DEMO-15",
        "Must the invoice export be CSV?",
        user(),
        "Finance.",
    );
    tick(&home);
    assert!(curator_turns(&home).is_empty(), "2 days are not a week");

    // The newest good run counts, and it was 8 days ago.
    good_run("older-run", 8);
    tick(&home);
    let turns = curator_turns(&home);
    assert_eq!(turns.len(), 1, "a week passed and there are signals");
    let started: records::Started = records::read(&ledger, records::EVENT_STARTED, &turns[0])
        .unwrap()
        .unwrap();
    assert_eq!(started.why, "7 days passed since the last good run");
    assert_eq!(started.counts.get("new_decisions"), Some(&1));
    wait_curator_turn(&home, &turns[0]);
    tick(&home);
    assert!(records::read::<Applied>(&ledger, EVENT_APPLIED, &turns[0])
        .unwrap()
        .is_some());
    tick(&home);
    assert_eq!(curator_turns(&home).len(), 1, "the next week starts now");
}
