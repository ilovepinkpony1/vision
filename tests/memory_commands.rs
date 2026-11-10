//! The commands of section 9.2, run as the built binary in a temporary home.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::{git, Home};
use vision::ledger::memory_use::MemoryUse;
use vision::ledger::Ledger;
use vision::memory::{Basis, Kind, Memory, MemoryId, NewDecision, NewPrinciple, Writer};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/memory")
        .join(name)
}

fn commits(home: &Home) -> usize {
    git(&home.path("memory"), &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap()
}

/// A fake editor: a shell script that runs `body` with the file in `$1`.
fn editor(home: &Home, name: &str, body: &str) -> String {
    let path = home.user_home.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -e\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

#[test]
fn teach_writes_an_active_principle_split_at_because() {
    let home = Home::init();
    let run = home.run(&[
        "memory",
        "teach",
        "judge",
        "When an internal user exports a report, make sure that the file is CSV, because the finance team imports CSV",
    ]);
    run.expect_code(0);
    assert!(
        run.stdout
            .starts_with("Taught P-001 to judge. It is active at once.\n"),
        "{run:?}"
    );
    let text = home.read("memory/principles/P-001.md");
    assert_eq!(
        text,
        "---\nid: P-001\nagent: judge\nkind: null\narea: []\nstatus: active\nby: user\nevidence: []\nuses: {used: 0, agreed: 0, corrected: 0}\n---\n\
         ## Rule\nWhen an internal user exports a report, make sure that the file is CSV.\n\n\
         ## Because\nThe finance team imports CSV.\n\n## Not when\n"
    );
    let memory = home.path("memory");
    assert_eq!(
        git(&memory, &["log", "-1", "--format=%an <%ae>"]),
        "user <user@localhost>"
    );
    assert_eq!(git(&memory, &["status", "--porcelain"]), "");

    home.run(&["memory", "teach", "all", "Never print a secret"])
        .expect_code(0);
    assert!(home
        .read("memory/principles/P-002.md")
        .contains("## Rule\nNever print a secret.\n\n## Because\n\n"));
    let run = home.run(&["memory", "teach", "robot", "Do X"]);
    run.expect_code(1);
    assert!(
        run.stderr.contains("not an agent of Vision") && run.stderr.contains("judge"),
        "{run:?}"
    );
    assert!(!home.path("memory/principles/P-003.md").exists());
}

#[test]
fn the_list_filters_searches_and_prints_json() {
    let home = Home::init();
    let run = home.run(&["memory"]);
    run.expect_code(0);
    assert!(run.stdout.starts_with("The memory is empty."), "{run:?}");
    home.run(&[
        "memory",
        "import",
        fixture("answers.jsonl").to_str().unwrap(),
    ])
    .expect_code(0);
    home.run(&[
        "memory",
        "teach",
        "judge",
        "When a report is exported, make sure that it is CSV",
    ])
    .expect_code(0);

    let run = home.run(&["memory"]);
    run.expect_code(0);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines[0], "ID      STATUS  KIND     AREA             BY      AGENT  TEXT",
        "{run:?}"
    );
    assert!(
        lines[1].starts_with(
            "D-0001  active  product  reports,exports  import  -      Must the Export button"
        ),
        "{run:?}"
    );
    assert_eq!(lines.len(), 6, "{run:?}");
    assert!(
        lines[5]
            .starts_with("P-001   active  -        -                user    judge  When a report"),
        "{run:?}"
    );

    let ids = |args: &[&str]| -> Vec<String> {
        let run = home.run(args);
        run.expect_code(0);
        run.stdout
            .lines()
            .skip(1)
            .map(|l| l.split_whitespace().next().unwrap().to_string())
            .collect()
    };
    assert_eq!(ids(&["memory", "--kind", "product"]), ["D-0001"]);
    assert_eq!(ids(&["memory", "--area", "reports"]), ["D-0001", "D-0002"]);
    assert_eq!(ids(&["memory", "csv"]), ["D-0001", "P-001"]);
    assert_eq!(
        ids(&["memory", "finance", "--status", "active"]),
        ["D-0001"]
    );
    let run = home.run(&["memory", "--status", "retired"]);
    run.expect_code(0);
    assert_eq!(run.stdout, "No file matches.\n");

    let run = home.run(&["memory", "--json", "--kind", "scope"]);
    run.expect_code(0);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    let files = value["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["id"], "D-0002");
    assert_eq!(files[0]["type"], "decision");
    assert_eq!(files[0]["choices"], serde_json::json!(["A4", "Letter"]));
    assert_eq!(value["problems"], serde_json::json!([]));
}

#[test]
fn show_names_the_evidence_the_sources_and_the_uses() {
    let home = Home::init();
    let memory = Memory::open(&home.paths(), "user").unwrap();
    let mut new = NewDecision::answer(
        Writer::Person("user".into()),
        "Must the export be CSV?",
        "CSV",
    );
    new.ticket = Some("DEMO-12".into());
    let d = memory.add_decision(&new).unwrap();
    let p = memory
        .add_principle(&NewPrinciple {
            agent: "judge".into(),
            kind: Some(Kind::Product),
            area: vec!["exports".into()],
            by: Writer::Person("user".into()),
            evidence: vec![d.id],
            rule: "When a report is exported, make sure that it is CSV.".into(),
            because: "Finance imports CSV.".into(),
            not_when: String::new(),
        })
        .unwrap();
    let mut judged = NewDecision::answer(Writer::Judge, "Must the summary be CSV?", "CSV");
    judged.basis = Basis::Principle;
    judged.sources = vec![p.id.into()];
    memory.add_decision(&judged).unwrap();
    let ledger = Ledger::open(&home.path("ledger.sqlite")).unwrap();
    ledger
        .add_memory_use(&MemoryUse {
            turn: "20261116T143000Z-DEMO-12-plan-1".into(),
            file: "P-001".into(),
            version: Some(p.version.clone()),
            applied: true,
            how: Some("chose CSV".into()),
        })
        .unwrap();

    let run = home.run(&["memory", "show", "P-001"]);
    run.expect_code(0);
    assert!(run.stdout.starts_with("---\nid: P-001\n"), "{run:?}");
    assert!(
        run.stdout
            .contains("evidence   D-0001  active  user   Must the export be CSV?"),
        "{run:?}"
    );
    assert!(
        run.stdout
            .contains("source of  D-0002  active  judge  Must the summary be CSV?"),
        "{run:?}"
    );
    assert!(
        run.stdout.contains("Uses: used 0, agreed 0, corrected 0."),
        "{run:?}"
    );
    assert!(
        run.stdout
            .contains("20261116T143000Z-DEMO-12-plan-1  applied  chose CSV"),
        "{run:?}"
    );

    let run = home.run(&["memory", "show", "D-0001", "--json"]);
    run.expect_code(0);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(value["file"]["question"], "Must the export be CSV?");
    assert_eq!(value["references"][0]["role"], "evidence for");
    assert_eq!(value["references"][0]["id"], "P-001");

    let run = home.run(&["memory", "show", "D-0099"]);
    run.expect_code(1);
    assert!(run.stderr.contains("no D-0099"), "{run:?}");
    let run = home.run(&["memory", "show", "D-1"]);
    run.expect_code(1);
    assert!(run.stderr.contains("D-0142"), "{run:?}");
}

#[test]
fn edit_commits_a_valid_change_and_keeps_an_invalid_one_out() {
    let home = Home::init();
    home.run(&["memory", "teach", "judge", "When X, make sure that Y"])
        .expect_code(0);
    let path = home.path("memory/principles/P-001.md");
    let start = commits(&home);

    let good = editor(&home, "good.sh", "printf 'A customer asks.\\n' >> \"$1\"");
    let run = home.run_with(&["memory", "edit", "P-001"], &[("EDITOR", &good)]);
    run.expect_code(0);
    assert!(
        run.stdout.starts_with("Saved P-001 in the commit "),
        "{run:?}"
    );
    assert!(fs::read_to_string(&path)
        .unwrap()
        .ends_with("## Not when\nA customer asks.\n"));
    assert_eq!(commits(&home), start + 1);
    assert_eq!(
        git(&home.path("memory"), &["log", "-1", "--format=%an %s"]),
        "user Edit P-001"
    );

    let valid = fs::read_to_string(&path).unwrap();
    let bad = editor(
        &home,
        "bad.sh",
        "sed 's/^status: active$/status: maybe/' \"$1\" > \"$1.new\"\nmv \"$1.new\" \"$1\"",
    );
    let run = home.run_with(&["memory", "edit", "P-001"], &[("EDITOR", &bad)]);
    run.expect_code(1);
    assert!(
        run.stderr
            .contains("not valid, so the file stays as it was"),
        "{run:?}"
    );
    assert!(
        run.stderr
            .contains("principles/P-001.md:6: `status` is maybe"),
        "{run:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), valid);
    assert_eq!(commits(&home), start + 1);

    // The next edit opens the text that was not valid, so the person can fix it.
    let fix = editor(&home, "fix.sh", "grep -q 'status: maybe' \"$1\"\nsed 's/^status: maybe$/status: active/; s/^A customer asks.$/A customer asks for XLSX./' \"$1\" > \"$1.new\"\nmv \"$1.new\" \"$1\"");
    let run = home.run_with(&["memory", "edit", "P-001"], &[("EDITOR", &fix)]);
    run.expect_code(0);
    assert!(
        run.stderr.contains("Opening your last text of P-001"),
        "{run:?}"
    );
    assert!(fs::read_to_string(&path)
        .unwrap()
        .ends_with("A customer asks for XLSX.\n"));

    let run = home.run_with(&["memory", "edit", "P-001"], &[("EDITOR", "true")]);
    run.expect_code(0);
    assert_eq!(run.stdout, "No change to P-001.\n");
    let run = home.run_with(&["memory", "edit", "P-001"], &[("EDITOR", "false")]);
    run.expect_code(1);
    assert!(run.stderr.contains("did not change"), "{run:?}");
    let run = home.run(&["memory", "edit", "P-001"]);
    run.expect_code(1);
    assert!(run.stderr.contains("set EDITOR"), "{run:?}");
    assert_eq!(git(&home.path("memory"), &["status", "--porcelain"]), "");
}

#[test]
fn retire_keeps_the_file_and_its_reason() {
    let home = Home::init();
    home.run(&["memory", "teach", "judge", "When X, make sure that Y"])
        .expect_code(0);
    let run = home.run(&["memory", "retire", "P-001", "The person corrected it twice"]);
    run.expect_code(0);
    assert_eq!(
        run.stdout,
        "Retired P-001. The file stays, and no turn loads it.\n"
    );
    let text = home.read("memory/principles/P-001.md");
    assert!(text.contains("\nstatus: retired\n"), "{text}");
    assert!(
        text.ends_with("## Retired\nThe person corrected it twice\n"),
        "{text}"
    );
    let run = home.run(&["memory", "retire", "P-001", "again"]);
    run.expect_code(0);
    assert_eq!(run.stdout, "P-001 is already retired.\n");
    let memory = Memory::open(&home.paths(), "user").unwrap();
    assert!(memory.principles_for("judge").unwrap().is_empty());
    let run = home.run(&["memory", "retire", "P-009", "why"]);
    run.expect_code(1);
    assert!(run.stderr.contains("no P-009"), "{run:?}");
}

#[test]
fn lint_prints_one_line_for_each_problem_and_exits_with_1() {
    let home = Home::init();
    home.run(&[
        "memory",
        "import",
        fixture("answers.jsonl").to_str().unwrap(),
    ])
    .expect_code(0);
    let run = home.run(&["memory", "lint"]);
    run.expect_code(0);
    assert!(
        run.stdout.contains("4 files in ") && run.stdout.ends_with(": no problem.\n"),
        "{run:?}"
    );

    let memory = home.path("memory");
    fs::write(memory.join("decisions/notes.md"), "free notes\n").unwrap();
    fs::write(
        memory.join("principles/P-001.md"),
        "---\nid: P-002\nagent: judge\nkind: product\nstatus: active\nby: user\nevidence: [D-0001, D-0042]\n---\n## Rule\nR\n",
    )
    .unwrap();
    fs::write(
        memory.join("decisions/D-0005.md"),
        home.read("memory/decisions/D-0001.md")
            .replace("id: D-0001", "id: D-0005")
            .replace("by: import", "by: judge")
            .replace("## Answer\nCSV\n", ""),
    )
    .unwrap();
    let run = home.run(&["memory", "lint"]);
    run.expect_code(1);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines[..lines.len() - 1],
        [
            "decisions/D-0005.md:1: the required section `## Answer` is missing",
            "decisions/notes.md:1: the file name is not an id such as D-0142.md. Rename it, or move it out of the memory",
            "principles/P-001.md:2: `id` is P-002, but the file name says P-001. Rename the file or fix the id",
            "principles/P-001.md:7: `evidence` names D-0001, which has the writer import. A candidate or active principle needs decisions of a person as evidence",
            "principles/P-001.md:7: `evidence` names D-0042, but the memory has no file decisions/D-0042.md",
        ],
        "{run:?}"
    );
    assert!(lines.last().unwrap().contains("5 problems"), "{run:?}");
    let run = home.run(&["memory", "lint", "--json"]);
    run.expect_code(1);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(value["problems"].as_array().unwrap().len(), 5);
    assert_eq!(value["problems"][0]["line"], 1);
    // A list still shows the valid files, and warns about the others.
    let run = home.run(&["memory"]);
    run.expect_code(0);
    assert!(
        run.stderr.contains("warning: decisions/D-0005.md:1:"),
        "{run:?}"
    );
}

#[test]
fn an_import_run_two_times_adds_nothing_the_second_time() {
    let home = Home::init();
    let file = fixture("answers.jsonl");
    let file = file.to_str().unwrap();
    let start = commits(&home);
    let run = home.run(&["memory", "import", file]);
    run.expect_code(0);
    assert!(
        run.stdout
            .starts_with("Read 8 lines: 4 added, 3 skipped, 1 already there.\n"),
        "{run:?}"
    );
    assert!(
        run.stderr.contains("line 4: skipped: it has no question"),
        "{run:?}"
    );
    assert!(
        run.stderr.contains("line 5: skipped: it has no answer"),
        "{run:?}"
    );
    assert!(
        run.stderr.contains("line 6: skipped: the line is not JSON"),
        "{run:?}"
    );
    assert!(
        run.stderr.contains("warning: line 7: `kind` is `privacy`"),
        "{run:?}"
    );
    assert_eq!(commits(&home), start + 1, "one commit for the import");
    let memory = home.path("memory");
    assert_eq!(
        git(&memory, &["log", "-1", "--format=%an %s"]),
        "import Import 4 decisions from answers.jsonl"
    );
    let d1 = home.read("memory/decisions/D-0001.md");
    assert!(
        d1.contains("\nby: import\nbasis: answer\nticket: DEMO-12\n"),
        "{d1}"
    );
    assert!(
        d1.contains("\nat: 2026-10-01T10:00:00Z\n") && d1.contains("\nimport_hash: "),
        "{d1}"
    );
    assert!(d1.contains("## Question\nMust the Export button make a CSV file or an XLSX file?\nChoices: CSV | XLSX\n"), "{d1}");
    assert!(
        d1.contains("## Why\nThe finance team imports CSV into their tool.\n"),
        "{d1}"
    );
    let d2 = home.read("memory/decisions/D-0002.md");
    assert!(
        d2.contains("\nat: 2026-10-02T00:00:00Z\n") && d2.contains("Choices: A4 | Letter"),
        "{d2}"
    );
    let d4 = home.read("memory/decisions/D-0004.md");
    assert!(
        d4.contains("\nticket: \"16\"\n") && d4.contains("\nkind: code\n"),
        "{d4}"
    );

    let run = home.run(&["memory", "import", file]);
    run.expect_code(0);
    assert!(
        run.stdout
            .starts_with("Read 8 lines: 0 added, 3 skipped, 5 already there.\n"),
        "{run:?}"
    );
    assert_eq!(commits(&home), start + 1);
    assert!(!home.path("memory/decisions/D-0005.md").exists());
    let run = home.run(&["memory", "import", "/no/such/file.jsonl"]);
    run.expect_code(1);
    assert!(
        run.stderr.contains("could not read /no/such/file.jsonl"),
        "{run:?}"
    );
}

#[test]
fn log_shows_the_history_and_revert_undoes_one_commit() {
    let home = Home::init();
    home.run(&["memory", "teach", "judge", "When X, make sure that Y"])
        .expect_code(0);
    home.run(&["memory", "retire", "P-001", "Wrong"])
        .expect_code(0);
    let run = home.run(&["memory", "log"]);
    run.expect_code(0);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert!(lines[0].starts_with("COMMIT   TIME"), "{run:?}");
    assert!(lines[1].ends_with("user    Retire P-001: Wrong"), "{run:?}");
    assert!(lines[3].ends_with("vision  Start the memory"), "{run:?}");
    let run = home.run(&["memory", "log", "--json"]);
    run.expect_code(0);
    let commits_json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    let retire = commits_json[0]["short"].as_str().unwrap().to_string();
    assert_eq!(commits_json[0]["author"], "user");

    let run = home.run(&["memory", "revert", &retire]);
    run.expect_code(0);
    assert!(
        run.stdout.contains("Revert \"Retire P-001: Wrong\""),
        "{run:?}"
    );
    assert!(home
        .read("memory/principles/P-001.md")
        .contains("\nstatus: active\n"));
    assert_eq!(commits(&home), 4);
    assert_eq!(
        git(&home.path("memory"), &["log", "-1", "--format=%an"]),
        "user"
    );
    let run = home.run(&["memory", "revert", "abc1234"]);
    run.expect_code(1);
    assert!(run.stderr.contains("no commit `abc1234`"), "{run:?}");
    let first = git(
        &home.path("memory"),
        &["rev-list", "--max-parents=0", "HEAD"],
    );
    let run = home.run(&["memory", "revert", &first]);
    run.expect_code(1);
    assert!(run.stderr.contains("first commit"), "{run:?}");
}

#[test]
fn review_is_not_built_yet_and_a_home_needs_init() {
    let home = Home::init();
    let run = home.run(&["memory", "review"]);
    run.expect_code(3);
    assert_eq!(run.stderr, "not built yet: memory review\n");
    let fresh = Home::new();
    let run = fresh.run(&["memory"]);
    run.expect_code(1);
    assert!(run.stderr.contains("vision init"), "{run:?}");
    // An id must have its form.
    let run = home.run(&["memory", "retire", "P-7", "why"]);
    run.expect_code(1);
    assert!(run.stderr.contains("P-007"), "{run:?}");
    assert!(MemoryId::parse("P-007").is_ok());
}

#[test]
fn processes_that_teach_at_the_same_time_get_different_ids() {
    let home = Home::init();
    std::thread::scope(|scope| {
        let runs: Vec<_> = (0..6)
            .map(|i| {
                let home = &home;
                scope.spawn(move || {
                    home.run(&["memory", "teach", "all", &format!("Rule number {i}")])
                })
            })
            .collect();
        for run in runs {
            run.join().unwrap().expect_code(0);
        }
    });
    let mut names: Vec<String> = fs::read_dir(home.path("memory/principles"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["P-001.md", "P-002.md", "P-003.md", "P-004.md", "P-005.md", "P-006.md"]
    );
    assert_eq!(commits(&home), 7);
    home.run(&["memory", "lint"]).expect_code(0);
}
