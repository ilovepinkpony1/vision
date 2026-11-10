//! The store of the memory through the library: decisions, principles, ids, uses, hand edits,
//! the safety of a failed write, and the history.

mod common;

use std::collections::BTreeSet;
use std::fs;

use common::{git, Home};
use vision::memory::Memory;
use vision::memory::{
    Basis, DecisionId, Filter, Invalid, Kind, MemoryId, NewDecision, NewPrinciple, PrincipleId,
    SourceState, Status, UseKind, Uses, Writer,
};

fn user() -> Writer {
    Writer::Person("user".into())
}

fn setup() -> (Home, Memory) {
    let home = Home::init();
    let memory = Memory::open(&home.paths(), "user").unwrap();
    (home, memory)
}

fn answer(question: &str, answer: &str) -> NewDecision {
    let mut new = NewDecision::answer(user(), question, answer);
    new.ticket = Some("DEMO-12".into());
    new.step = Some("plan".into());
    new.asked_by = Some("builder".into());
    new.kind = Some(Kind::Product);
    new.area = vec!["reports".into(), "exports".into()];
    new.choices = vec!["CSV".into(), "XLSX".into()];
    new.why = "The finance team imports CSV.".into();
    new
}

fn rule(agent: &str, text: &str) -> NewPrinciple {
    NewPrinciple {
        agent: agent.into(),
        kind: Some(Kind::Product),
        area: vec!["exports".into()],
        by: user(),
        evidence: vec![],
        rule: text.into(),
        because: "A reason.".into(),
        not_when: String::new(),
    }
}

fn commits(home: &Home) -> usize {
    git(&home.path("memory"), &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap()
}

fn last_author(home: &Home) -> String {
    git(&home.path("memory"), &["log", "-1", "--format=%an <%ae>"])
}

#[test]
fn a_decision_of_each_writer_is_one_commit_by_its_writer() {
    let (home, memory) = setup();
    let d = memory
        .add_decision(&answer(
            "Must the Export button make a CSV file or an XLSX file?",
            "CSV",
        ))
        .unwrap();
    assert_eq!(d.id.to_string(), "D-0001");
    assert_eq!(d.status, Status::Active);
    assert_eq!(d.area, ["reports", "exports"]);
    assert_eq!(d.choices, ["CSV", "XLSX"]);
    assert_eq!(last_author(&home), "user <user@localhost>");
    let text = home.read("memory/decisions/D-0001.md");
    assert!(text.contains("\narea: [reports, exports]\n"), "{text}");
    assert!(text.contains("\nasked_by: builder\n"), "{text}");

    let p = memory
        .add_principle(&rule("judge", "When X, make sure that Y."))
        .unwrap();
    let mut judged = answer("Which separator does the CSV use?", "A comma");
    judged.by = Writer::Judge;
    judged.basis = Basis::Principle;
    judged.sources = vec![p.id.into()];
    let j = memory.add_decision(&judged).unwrap();
    assert_eq!(j.by, Writer::Judge);
    assert_eq!(j.sources, [MemoryId::Principle(p.id)]);
    assert_eq!(last_author(&home), "judge <judge@localhost>");
    assert_eq!(commits(&home), 4);
    assert_eq!(git(&home.path("memory"), &["status", "--porcelain"]), "");
}

#[test]
fn a_wrong_decision_changes_nothing() {
    let (home, memory) = setup();
    let mut bad = answer("Q?", "A");
    bad.by = Writer::Judge;
    let err = memory.add_decision(&bad).unwrap_err().to_string();
    assert!(err.contains("not answer"), "{err}");
    let mut bad = answer("Q?", "A");
    bad.basis = Basis::Inference;
    assert!(memory.add_decision(&bad).is_err());
    let mut bad = answer("Q?", "A");
    bad.by = Writer::Judge;
    bad.basis = Basis::Decision;
    bad.sources = vec![MemoryId::parse("D-0042").unwrap()];
    let err = memory.add_decision(&bad).unwrap_err();
    let invalid = err.downcast_ref::<Invalid>().expect("an Invalid error");
    assert!(invalid.problems[0].reason.contains("D-0042"), "{invalid}");
    assert!(memory.add_decision(&answer(" ", "A")).is_err());
    assert_eq!(commits(&home), 1);
    assert!(!home.path("memory/decisions/D-0001.md").exists());
}

#[test]
fn supersede_marks_the_old_file_in_one_commit() {
    let (home, memory) = setup();
    let old = memory
        .add_decision(&answer("Which format?", "XLSX"))
        .unwrap();
    let before = commits(&home);
    let new = memory
        .supersede(old.id, &answer("Which format?", "CSV"))
        .unwrap();
    assert_eq!(commits(&home), before + 1);
    assert_eq!(new.supersedes, Some(old.id));
    assert_eq!(memory.decision(old.id).unwrap().status, Status::Superseded);
    let err = memory
        .supersede(old.id, &answer("Which format?", "TSV"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("superseded"), "{err}");
}

#[test]
fn a_person_writes_active_principles_and_the_curator_candidates() {
    let (_home, memory) = setup();
    let p = memory
        .add_principle(&rule("judge", "When X, make sure that Y."))
        .unwrap();
    assert_eq!(p.status, Status::Active);
    let mut c = rule("judge", "When A, make sure that B.");
    c.by = Writer::Curator;
    assert_eq!(memory.add_principle(&c).unwrap().status, Status::Candidate);
    let mut j = rule("judge", "When A, make sure that B.");
    j.by = Writer::Judge;
    assert!(memory.add_principle(&j).is_err());
    assert!(memory.add_principle(&rule("Not An Agent", "R")).is_err());
}

#[test]
fn evidence_must_be_decisions_of_a_person() {
    let (_home, memory) = setup();
    let person = memory.add_decision(&answer("Q one?", "A")).unwrap();
    let mut judged = answer("Q two?", "B");
    judged.by = Writer::Judge;
    judged.basis = Basis::Inference;
    let judge = memory.add_decision(&judged).unwrap();
    let mut p = rule("judge", "When X, make sure that Y.");
    p.evidence = vec![person.id, judge.id];
    let err = memory.add_principle(&p).unwrap_err().to_string();
    assert!(err.contains("an answer of the Judge"), "{err}");
    p.evidence = vec![person.id];
    assert_eq!(memory.add_principle(&p).unwrap().evidence, [person.id]);
}

#[test]
fn the_principles_of_an_agent_are_its_own_and_those_for_all() {
    let (_home, memory) = setup();
    let own = memory
        .add_principle(&rule("judge", "When J, make sure that J."))
        .unwrap();
    let all = memory
        .add_principle(&rule("all", "When A, make sure that A."))
        .unwrap();
    memory
        .add_principle(&rule("builder", "When B, make sure that B."))
        .unwrap();
    let retired = memory
        .add_principle(&rule("judge", "When R, make sure that R."))
        .unwrap();
    memory
        .retire(retired.id.into(), "No longer true.", &user())
        .unwrap();
    let mut candidate = rule("judge", "When C, make sure that C.");
    candidate.by = Writer::Curator;
    memory.add_principle(&candidate).unwrap();

    let ids: Vec<PrincipleId> = memory
        .principles_for("judge")
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, [own.id, all.id]);
    let file = memory.principles_file("judge").unwrap();
    assert!(
        file.text.starts_with("# Principles for judge\n"),
        "{}",
        file.text
    );
    assert!(
        file.text
            .contains("## P-001\n\nWhen J, make sure that J.\n"),
        "{}",
        file.text
    );
    assert!(file.text.contains("## P-002\n"), "{}", file.text);
    assert!(!file.text.contains("When R"), "{}", file.text);
    assert!(!file.text.contains("When C"), "{}", file.text);
    assert_eq!(file.loaded.len(), 2);
    assert_eq!(
        file.loaded[0].version,
        memory.principle(own.id).unwrap().version
    );
    assert!(file.problems.is_empty());
    let none = memory.principles_file("scout").unwrap();
    assert_eq!(none.loaded.len(), 1, "only the principle for all");
}

#[test]
fn source_state_serves_the_judge_rules() {
    let (home, memory) = setup();
    let d = memory.add_decision(&answer("Q?", "A")).unwrap();
    let p = memory
        .add_principle(&rule("judge", "When X, make sure that Y."))
        .unwrap();
    memory.retire(p.id.into(), "Wrong.", &user()).unwrap();
    assert_eq!(memory.source_state("D-0001"), SourceState::Active);
    assert!(memory.is_active(&d.id.to_string()));
    assert_eq!(
        memory.source_state("P-001"),
        SourceState::Inactive(Status::Retired)
    );
    assert!(!memory.is_active("P-001"));
    assert_eq!(memory.source_state("D-0099"), SourceState::Missing);
    assert_eq!(memory.source_state("nonsense"), SourceState::Missing);
    fs::write(home.path("memory/decisions/D-0002.md"), "---\nid: [\n---\n").unwrap();
    assert_eq!(memory.source_state("D-0002"), SourceState::Broken);
    assert!(memory.exists("D-0002"));
}

#[test]
fn uses_are_counted_in_one_commit() {
    let (home, memory) = setup();
    let a = memory
        .add_principle(&rule("judge", "When A, make sure that A."))
        .unwrap();
    let b = memory
        .add_principle(&rule("judge", "When B, make sure that B."))
        .unwrap();
    let before = commits(&home);
    memory
        .count_uses(
            &[
                (a.id, UseKind::Used),
                (a.id, UseKind::Used),
                (a.id, UseKind::Agreed),
                (b.id, UseKind::Corrected),
            ],
            &Writer::Judge,
        )
        .unwrap();
    assert_eq!(commits(&home), before + 1);
    assert_eq!(
        memory.principle(a.id).unwrap().uses,
        Uses {
            used: 2,
            agreed: 1,
            corrected: 0
        }
    );
    assert_eq!(
        memory.principle(b.id).unwrap().uses,
        Uses {
            used: 0,
            agreed: 0,
            corrected: 1
        }
    );
    assert!(home
        .read("memory/principles/P-001.md")
        .contains("uses: {used: 2, agreed: 1, corrected: 0}"));
}

#[test]
fn ids_never_repeat_across_writers_at_the_same_time() {
    let (home, _memory) = setup();
    let paths = home.paths();
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let paths = paths.clone();
            std::thread::spawn(move || {
                let memory = Memory::open(&paths, "user").unwrap();
                (0..5)
                    .map(|i| {
                        memory
                            .add_decision(&answer(&format!("Question {t}-{i}?"), "A"))
                            .unwrap()
                            .id
                    })
                    .collect::<Vec<DecisionId>>()
            })
        })
        .collect();
    let ids: Vec<DecisionId> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    let unique: BTreeSet<DecisionId> = ids.iter().copied().collect();
    assert_eq!(unique.len(), 20, "{ids:?}");
    assert_eq!(unique.iter().map(|id| id.number()).max(), Some(20));
    assert_eq!(commits(&home), 21);
    let memory = Memory::open(&paths, "user").unwrap();
    assert!(memory.lint().unwrap().is_clean());
}

#[test]
fn the_id_of_a_removed_file_is_never_given_again() {
    let (home, memory) = setup();
    memory.add_decision(&answer("One?", "A")).unwrap();
    memory.add_decision(&answer("Two?", "A")).unwrap();
    let dir = home.path("memory");
    git(&dir, &["rm", "-q", "decisions/D-0002.md"]);
    git(
        &dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-q",
            "-m",
            "remove",
        ],
    );
    assert_eq!(
        memory
            .add_decision(&answer("Three?", "A"))
            .unwrap()
            .id
            .number(),
        3
    );
}

#[test]
fn a_hand_edit_is_committed_as_the_person_before_a_change() {
    let (home, memory) = setup();
    let p = memory
        .add_principle(&rule("judge", "When X, make sure that Y."))
        .unwrap();
    let path = home.path("memory/principles/P-001.md");
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("## Not when\n", "## Not when\nA customer asks.\n");
    fs::write(&path, &edited).unwrap();
    let before = commits(&home);
    memory
        .count_uses(&[(p.id, UseKind::Used)], &Writer::Judge)
        .unwrap();
    assert_eq!(commits(&home), before + 2);
    let log = git(&home.path("memory"), &["log", "-2", "--format=%an %s"]);
    assert_eq!(
        log,
        "judge Count the uses of P-001\nuser Hand edit of principles/P-001.md"
    );
    assert_eq!(memory.principle(p.id).unwrap().not_when, "A customer asks.");

    // A hand edit that breaks the file stops the change, and nothing is committed.
    fs::write(&path, "---\nid: P-001\nstatus: [\n---\n").unwrap();
    let before = commits(&home);
    let err = memory
        .count_uses(&[(p.id, UseKind::Used)], &Writer::Judge)
        .unwrap_err();
    assert!(format!("{err:#}").contains("P-001"), "{err:#}");
    assert_eq!(commits(&home), before);

    let report = memory.commit_hand_edits().unwrap();
    assert!(report.committed.is_empty());
    assert_eq!(report.left.len(), 1, "{report:?}");

    // The person fixes the broken file with an edit: one commit of the person, with no step
    // of its own for the broken text.
    let broken = fs::read_to_string(&path).unwrap();
    let committed = git(&home.path("memory"), &["show", "HEAD:principles/P-001.md"]) + "\n";
    let fixed = committed.replace("A customer asks.", "A customer asks for XLSX.");
    let before = commits(&home);
    assert!(memory
        .replace_text(p.id.into(), &broken, &fixed, &user())
        .unwrap());
    assert_eq!(commits(&home), before + 1);
    assert_eq!(
        git(&home.path("memory"), &["log", "-1", "--format=%an %s"]),
        "user Edit P-001"
    );
    // A text that is the committed text again needs no commit.
    fs::write(&path, &broken).unwrap();
    assert!(memory
        .replace_text(p.id.into(), &broken, &fixed, &user())
        .unwrap());
    assert_eq!(commits(&home), before + 1);
    assert_eq!(git(&home.path("memory"), &["status", "--porcelain"]), "");
}

#[test]
fn a_failed_commit_leaves_no_half_change() {
    let (home, memory) = setup();
    memory.add_decision(&answer("One?", "A")).unwrap();
    let lock = home.path("memory/.git/index.lock");
    fs::write(&lock, "").unwrap();
    let err = memory
        .supersede(DecisionId::new(1).unwrap(), &answer("One?", "B"))
        .unwrap_err();
    assert!(format!("{err:#}").contains("index.lock"), "{err:#}");
    fs::remove_file(&lock).unwrap();
    assert!(!home.path("memory/decisions/D-0002.md").exists());
    assert_eq!(
        memory.decision(DecisionId::new(1).unwrap()).unwrap().status,
        Status::Active
    );
    assert_eq!(git(&home.path("memory"), &["status", "--porcelain"]), "");
}

#[test]
fn the_store_reads_and_writes_only_main() {
    let (home, _memory) = setup();
    git(&home.path("memory"), &["checkout", "-q", "-b", "other"]);
    let err = Memory::open(&home.paths(), "user").unwrap_err().to_string();
    assert!(err.contains("checkout main"), "{err}");
    git(&home.path("memory"), &["checkout", "-q", "main"]);
    assert!(
        Memory::open(&home.paths(), "judge").is_err(),
        "a person cannot be named judge"
    );
    let missing = Home::new();
    let err = Memory::open(&missing.paths(), "user")
        .unwrap_err()
        .to_string();
    assert!(err.contains("vision init"), "{err}");
}

#[test]
fn a_list_filters_by_each_field() {
    let (_home, memory) = setup();
    memory
        .add_decision(&answer("Must the export be CSV?", "CSV"))
        .unwrap();
    let mut other = answer("Which page size?", "A4");
    other.kind = Some(Kind::Scope);
    other.area = vec!["printing".into()];
    other.asked_by = Some("scout".into());
    other.choices = vec!["A4".into(), "Letter".into()];
    other.why = "Most readers print on A4.".into();
    memory.add_decision(&other).unwrap();
    memory
        .add_principle(&rule(
            "judge",
            "When X, make sure that the CSV has a header.",
        ))
        .unwrap();
    let count = |filter: Filter| memory.list(&filter).unwrap().files.len();
    assert_eq!(count(Filter::default()), 3);
    assert_eq!(
        count(Filter {
            text: Some("csv".into()),
            ..Filter::default()
        }),
        2
    );
    assert_eq!(
        count(Filter {
            kind: Some(Kind::Scope),
            ..Filter::default()
        }),
        1
    );
    assert_eq!(
        count(Filter {
            area: Some("printing".into()),
            ..Filter::default()
        }),
        1
    );
    assert_eq!(
        count(Filter {
            agent: Some("scout".into()),
            ..Filter::default()
        }),
        1
    );
    assert_eq!(
        count(Filter {
            writer: Some("user".into()),
            ..Filter::default()
        }),
        3
    );
    assert_eq!(
        count(Filter {
            status: Some(Status::Retired),
            ..Filter::default()
        }),
        0
    );
}

#[test]
fn a_revert_is_a_new_commit_and_refuses_to_break_a_file() {
    let (home, memory) = setup();
    let d = memory.add_decision(&answer("One?", "A")).unwrap();
    let mut p = rule("judge", "When X, make sure that Y.");
    p.evidence = vec![d.id];
    let p = memory.add_principle(&p).unwrap();
    memory.retire(p.id.into(), "Wrong.", &user()).unwrap();
    let retire = memory.log(Some(1)).unwrap().remove(0);
    let made = memory.revert(&retire.short, &user()).unwrap();
    assert_eq!(made.subject, format!("Revert \"{}\"", retire.subject));
    assert_eq!(made.author, "user");
    assert_eq!(memory.principle(p.id).unwrap().status, Status::Active);

    // Undoing the decision would leave the evidence of P-001 without its file.
    let add = memory
        .log(None)
        .unwrap()
        .into_iter()
        .find(|c| c.subject.starts_with("Add D-0001"))
        .unwrap();
    let before = commits(&home);
    let err = memory.revert(&add.sha, &user()).unwrap_err().to_string();
    assert!(err.contains("invalid"), "{err}");
    assert_eq!(commits(&home), before);
    assert!(home.path("memory/decisions/D-0001.md").exists());
    assert_eq!(git(&home.path("memory"), &["status", "--porcelain"]), "");
    assert!(memory.revert("no-such-commit", &user()).is_err());
}
