//! The proposals of the Curator through the library: a build with each kind of change, the
//! main branch that does not move, a part accepted and the rest rejected, and the rule "no line
//! is lost" against a hand edit of the branch.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use common::{git, Home};
use time::macros::date;
use vision::memory::{
    Basis, ChangeState, CuratorChange, Kind, Memory, NewDecision, NewPrinciple, Op, Status, Writer,
};

const NAME: &str = "proposal/2026-11-16-1";

fn user() -> Writer {
    Writer::Person("user".into())
}

/// A memory with three decisions (two of the person, one of the Judge) and four principles.
fn setup() -> (Home, Memory) {
    let home = Home::init();
    let memory = Memory::open(&home.paths(), "user").unwrap();
    for (ticket, question, by) in [
        ("DEMO-12", "Must the export be CSV?", user()),
        ("DEMO-13", "Must the summary export be CSV?", user()),
        ("DEMO-14", "Must the log export be CSV?", Writer::Judge),
    ] {
        let mut new = NewDecision::answer(by.clone(), question, "CSV");
        if by == Writer::Judge {
            new.basis = Basis::Inference;
        }
        new.ticket = Some(ticket.into());
        new.kind = Some(Kind::Product);
        new.area = vec!["exports".into()];
        new.why = "The finance team imports CSV.".into();
        memory.add_decision(&new).unwrap();
    }
    for (agent, rule) in [
        (
            "judge",
            "When a report is exported, make sure that the file is CSV.",
        ),
        ("all", "When a file is written, make sure that it is UTF-8."),
        ("all", "When text is saved, make sure that it is UTF-8."),
        (
            "builder",
            "When a test fails, make sure that the fix comes with a test.",
        ),
    ] {
        memory
            .add_principle(&NewPrinciple {
                agent: agent.into(),
                kind: Some(Kind::Code),
                area: vec![],
                by: user(),
                evidence: vec![],
                rule: rule.into(),
                because: "It worked so far.".into(),
                not_when: String::new(),
            })
            .unwrap();
    }
    (home, memory)
}

fn principle_text(id: &str, status: &str, by: &str, evidence: &str, rule: &str) -> String {
    format!(
        "---\nid: {id}\nagent: judge\nkind: product\narea: [exports]\nstatus: {status}\nby: {by}\nevidence: [{evidence}]\nuses: {{used: 0, agreed: 0, corrected: 0}}\n---\n## Rule\n{rule}\n\n## Because\nThe finance team imports CSV.\n\n## Not when\n"
    )
}

fn change(op: Op, reason: &str) -> CuratorChange {
    CuratorChange {
        op,
        path: None,
        content: None,
        removes: vec![],
        target: None,
        question: None,
        evidence: vec![],
        reason: reason.into(),
    }
}

/// Each kind of change, and three changes that the build must skip.
fn changes() -> Vec<CuratorChange> {
    vec![
        CuratorChange {
            path: Some("principles/NEW-1.md".into()),
            content: Some(principle_text("NEW-1", "active", "user", "D-0001, D-0002", "When an export is for finance, make sure that it is CSV.")),
            evidence: vec!["D-0001".into(), "D-0002".into()],
            ..change(Op::Add, "Two tickets of the person chose CSV for finance.")
        },
        CuratorChange {
            path: Some("principles/P-001.md".into()),
            content: Some(
                principle_text("P-001", "active", "user", "", "When a report is exported for an internal user, make sure that the file is CSV.")
                    .replace("agent: judge\nkind: product\narea: [exports]", "agent: judge\nkind: code\narea: []"),
            ),
            ..change(Op::Edit, "The rule was too wide.")
        },
        CuratorChange {
            path: Some("principles/P-002.md".into()),
            content: Some(
                principle_text("P-002", "active", "user", "", "When a file or a text is written, make sure that it is UTF-8.")
                    .replace("agent: judge", "agent: all"),
            ),
            removes: vec!["principles/P-003.md".into()],
            ..change(Op::Merge, "P-002 and P-003 say the same thing.")
        },
        CuratorChange {
            target: Some("P-004".into()),
            ..change(Op::Retire, "The person corrected it twice.")
        },
        CuratorChange {
            question: Some("Do internal users ever need XLSX?".into()),
            ..change(Op::Question, "D-0001 has no case for XLSX.")
        },
        CuratorChange {
            path: Some("principles/NEW-2.md".into()),
            content: Some(principle_text("NEW-2", "candidate", "curator", "D-0003", "When logs are exported, make sure that they are CSV.")),
            ..change(Op::Add, "The Judge chose CSV.")
        },
        CuratorChange {
            path: Some("principles/P-001.md".into()),
            content: Some(principle_text("P-001", "active", "user", "", "Another text.")),
            ..change(Op::Edit, "A second edit of the same file.")
        },
        CuratorChange {
            target: Some("P-099".into()),
            ..change(Op::Retire, "No such file.")
        },
    ]
}

/// The text of each file of the work tree of `main`.
fn files(home: &Home) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for folder in ["decisions", "principles"] {
        for entry in fs::read_dir(home.path(&format!("memory/{folder}"))).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            out.insert(
                format!("{folder}/{name}"),
                fs::read_to_string(entry.path()).unwrap(),
            );
        }
    }
    out.insert("ARCHIVE.md".into(), home.read("memory/ARCHIVE.md"));
    out
}

fn show(dir: &Path, rev_path: &str) -> String {
    git(dir, &["show", rev_path])
}

/// Changes one file of a branch by hand, as a person with git would.
fn hand_edit_branch(home: &Home, branch: &str, path: &str, edit: impl Fn(&str) -> String) {
    let memory = home.path("memory");
    let tree = home.path("hand-tree");
    git(
        &memory,
        &["worktree", "add", "-q", tree.to_str().unwrap(), branch],
    );
    let file = tree.join(path);
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, edit(&text)).unwrap();
    let status = Command::new("git")
        .arg("-C")
        .arg(&tree)
        .args([
            "-c",
            "user.name=someone",
            "-c",
            "user.email=someone@localhost",
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
        &memory,
        &["worktree", "remove", "--force", tree.to_str().unwrap()],
    );
}

#[test]
fn a_proposal_with_each_kind_of_change_leaves_main_alone() {
    let (home, memory) = setup();
    let dir = home.path("memory");
    let head = git(&dir, &["rev-parse", "main"]);
    let before = files(&home);

    let built = memory
        .build_proposal("Tidy the export rules", &changes(), date!(2026 - 11 - 16))
        .unwrap();
    let proposal = built.proposal.expect("a proposal");
    assert_eq!(proposal.name, NAME);
    assert_eq!(proposal.base, head);
    let ops: Vec<Op> = proposal.changes.iter().map(|c| c.op).collect();
    assert_eq!(
        ops,
        [Op::Add, Op::Edit, Op::Merge, Op::Retire, Op::Question]
    );
    let skipped: Vec<usize> = built.skipped.iter().map(|s| s.index).collect();
    assert_eq!(skipped, [5, 6, 7], "{:#?}", built.skipped);
    assert!(
        built.skipped[0].reason.contains("an answer of the Judge"),
        "{:#?}",
        built.skipped
    );
    assert!(
        built.skipped[1].reason.contains("change 2"),
        "{:#?}",
        built.skipped
    );
    assert!(
        built.skipped[2].reason.contains("P-099"),
        "{:#?}",
        built.skipped
    );

    // The main branch, its files, and its work tree did not change.
    assert_eq!(git(&dir, &["rev-parse", "main"]), head);
    assert_eq!(files(&home), before);
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");
    assert_eq!(git(&dir, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(
        git(&dir, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );

    // The branch has the new candidate with the next free id, the retired files, and the archive.
    let added = show(&dir, &format!("{NAME}:principles/P-005.md"));
    assert!(
        added.contains("\nid: P-005\n")
            && added.contains("\nstatus: candidate\n")
            && added.contains("\nby: curator\n"),
        "{added}"
    );
    let merged = show(&dir, &format!("{NAME}:principles/P-003.md"));
    assert!(
        merged.contains("\nstatus: retired\n") && merged.contains("## Retired\nMerged into P-002."),
        "{merged}"
    );
    let archive = show(&dir, &format!("{NAME}:ARCHIVE.md"));
    assert!(
        archive.contains(&format!("## {NAME} change 2\n")),
        "{archive}"
    );
    assert!(
        archive.contains(&format!(
            "- Commit: {head}\n- Reason: The rule was too wide.\n"
        )),
        "{archive}"
    );
    assert!(
        archive.contains("> When a report is exported, make sure that the file is CSV.\n"),
        "{archive}"
    );
    assert!(
        archive.contains(&format!("## {NAME} change 3\n"))
            && archive.contains("From principles/P-003.md:\n\n> status: active\n"),
        "{archive}"
    );
    assert!(
        archive.contains(&format!("## {NAME} change 4\n")),
        "{archive}"
    );
    assert!(
        !archive.contains(&format!("## {NAME} change 1\n")),
        "an add removes no line: {archive}"
    );
    assert_eq!(git(&dir, &["log", "-1", "--format=%an", NAME]), "curator");
    assert!(memory.proposal_lost_lines(NAME).unwrap().is_empty());

    let listed = memory.proposals().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].open, listed[0].accepted, listed[0].rejected),
        (5, 0, 0)
    );
    let view = memory.proposal(NAME).unwrap();
    assert!(view.lost.is_empty());
    assert!(
        view.changes[1].diffs[0]
            .diff
            .contains("-When a report is exported, make sure that the file is CSV."),
        "{:?}",
        view.changes[1]
    );
    assert!(view.changes[4].diffs.is_empty());

    // Accept the add, the edit, and the question.
    let a1 = memory.accept_change(NAME, 1).unwrap();
    assert_eq!(a1.path.as_deref(), Some("principles/P-005.md"));
    let a2 = memory.accept_change(NAME, 2).unwrap();
    assert!(a2.commit.is_some());
    let a5 = memory.accept_change(NAME, 5).unwrap();
    assert_eq!(a5.commit, None);
    let p5 = memory
        .principle(vision::memory::PrincipleId::new(5).unwrap())
        .unwrap();
    assert_eq!(p5.status, Status::Active, "an accepted candidate is active");
    assert_eq!(p5.by, user());
    assert!(memory
        .principle(vision::memory::PrincipleId::new(1).unwrap())
        .unwrap()
        .rule
        .contains("internal user"));
    let main_archive = home.read("memory/ARCHIVE.md");
    assert!(
        main_archive.contains(&format!("## {NAME} change 2\n")),
        "{main_archive}"
    );
    assert!(
        !main_archive.contains(&format!("## {NAME} change 3\n")),
        "{main_archive}"
    );
    assert_eq!(
        git(&dir, &["log", "-1", "--format=%an %s", "main"]),
        format!("user Accept change 2 of {NAME}: edit principles/P-001.md")
    );
    assert!(
        memory.lint().unwrap().is_clean(),
        "{:?}",
        memory.lint().unwrap()
    );

    // Reject the rest. The files of the merge and the retirement stay as they were.
    assert_eq!(memory.reject_all(NAME, Some("Not now.")).unwrap(), 2);
    assert_eq!(
        files(&home)["principles/P-003.md"],
        before["principles/P-003.md"]
    );
    assert_eq!(
        files(&home)["principles/P-004.md"],
        before["principles/P-004.md"]
    );
    let view = memory.proposal(NAME).unwrap();
    let states: Vec<ChangeState> = view.file.changes.iter().map(|c| c.state).collect();
    use ChangeState::{Accepted, Rejected};
    assert_eq!(states, [Accepted, Accepted, Rejected, Rejected, Accepted]);
    assert_eq!(view.file.changes[2].note.as_deref(), Some("Not now."));
    let err = memory.accept_change(NAME, 3).unwrap_err().to_string();
    assert!(err.contains("already rejected"), "{err}");

    memory.delete_proposal(NAME).unwrap();
    assert!(memory.proposals().unwrap().is_empty());
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");
}

#[test]
fn a_hand_edit_that_loses_a_line_blocks_the_proposal() {
    let (home, memory) = setup();
    let dir = home.path("memory");
    let built = memory
        .build_proposal("One edit", &changes()[1..2], date!(2026 - 11 - 16))
        .unwrap();
    assert!(built.proposal.is_some());
    assert!(memory.proposal_lost_lines(NAME).unwrap().is_empty());
    // Someone drops the old reason of P-001 from the archive of the branch.
    hand_edit_branch(&home, NAME, "ARCHIVE.md", |text| {
        text.replace("> It worked so far.\n", "")
    });
    let lost = memory.proposal_lost_lines(NAME).unwrap();
    assert_eq!(lost.len(), 1, "{lost:?}");
    assert_eq!(lost[0].path, "principles/P-001.md");
    assert_eq!(lost[0].line, 15);
    assert_eq!(lost[0].text, "It worked so far.");
    // Someone also drops a line of the file itself that the proposal did not touch.
    hand_edit_branch(&home, NAME, "principles/P-001.md", |text| {
        text.replace("uses: {used: 0, agreed: 0, corrected: 0}\n", "")
    });
    let lost = memory.proposal_lost_lines(NAME).unwrap();
    assert_eq!(lost.len(), 2, "{lost:?}");
    assert_eq!(lost[0].text, "uses: {used: 0, agreed: 0, corrected: 0}");
    let head = git(&dir, &["rev-parse", "main"]);
    let err = memory.accept_change(NAME, 1).unwrap_err().to_string();
    assert!(
        err.contains("loses lines") && err.contains("It worked so far."),
        "{err}"
    );
    assert_eq!(git(&dir, &["rev-parse", "main"]), head);
    let err = memory.delete_proposal(NAME).unwrap_err().to_string();
    assert!(err.contains("open changes"), "{err}");
    memory.reject_change(NAME, 1, None).unwrap();
    memory.delete_proposal(NAME).unwrap();
}

#[test]
fn a_change_of_a_file_that_main_changed_since_is_refused() {
    let (_home, memory) = setup();
    memory
        .build_proposal("One edit", &changes()[1..2], date!(2026 - 11 - 16))
        .unwrap();
    memory
        .retire(
            vision::memory::MemoryId::parse("P-001").unwrap(),
            "Wrong.",
            &user(),
        )
        .unwrap();
    let err = memory.accept_change(NAME, 1).unwrap_err().to_string();
    assert!(err.contains("changed on main"), "{err}");
    let p1 = memory
        .principle(vision::memory::PrincipleId::new(1).unwrap())
        .unwrap();
    assert_eq!(p1.status, Status::Retired);
}

#[test]
fn an_added_file_gets_a_new_id_when_main_took_its_id() {
    let (home, memory) = setup();
    let dir = home.path("memory");
    memory
        .build_proposal("One add", &changes()[0..1], date!(2026 - 11 - 16))
        .unwrap();
    // Someone writes P-005 on main by hand and commits it with git.
    fs::write(
        home.path("memory/principles/P-005.md"),
        principle_text("P-005", "active", "user", "", "When X, make sure that Y."),
    )
    .unwrap();
    git(&dir, &["add", "principles/P-005.md"]);
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
            "by hand",
        ],
    );
    let accepted = memory.accept_change(NAME, 1).unwrap();
    assert_eq!(accepted.path.as_deref(), Some("principles/P-006.md"));
    let p6 = memory
        .principle(vision::memory::PrincipleId::new(6).unwrap())
        .unwrap();
    assert_eq!(p6.status, Status::Active);
    assert!(p6.rule.contains("for finance"));
    let view = memory.proposal(NAME).unwrap();
    assert_eq!(
        view.file.changes[0].note.as_deref(),
        Some("accepted as principles/P-006.md")
    );
    // A second proposal of the same day gets the next number.
    let second = memory
        .build_proposal("Ask", &changes()[4..5], date!(2026 - 11 - 16))
        .unwrap();
    assert_eq!(second.proposal.unwrap().name, "proposal/2026-11-16-2");
    let all = memory.accept_all("proposal/2026-11-16-2").unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].1.is_ok());
}

#[test]
fn a_proposal_with_no_valid_change_makes_no_branch() {
    let (home, memory) = setup();
    let bad = [changes()[5].clone(), changes()[7].clone()];
    let built = memory
        .build_proposal("Nothing", &bad, date!(2026 - 11 - 16))
        .unwrap();
    assert!(built.proposal.is_none());
    assert_eq!(built.skipped.len(), 2);
    assert_eq!(
        git(&home.path("memory"), &["branch", "--list", "proposal/*"]),
        ""
    );
    assert!(memory.proposals().unwrap().is_empty());
}
