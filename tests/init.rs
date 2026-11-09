mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use common::{git, Home};
use vision::config::Config;
use vision::ledger::{Ledger, SCHEMA_VERSION};

#[test]
fn init_makes_the_tree_of_section_3() {
    let home = Home::new();
    let run = home.run(&["init"]);
    run.expect_code(0);
    assert!(run.stdout.contains("Set up Vision at"), "{run:?}");
    for dir in [
        "log",
        "claude",
        "sandbox",
        "skills",
        "adapters",
        "tickets",
        "memory/decisions",
        "memory/principles",
    ] {
        assert!(home.path(dir).is_dir(), "{dir}/ is missing");
    }
    assert_eq!(home.read("memory/ARCHIVE.md"), "");
}

#[test]
fn init_writes_the_default_config_with_its_comments() {
    let home = Home::init();
    let text = home.read("config.toml");
    for comment in [
        "# the name of the adapter",
        "# minutes before the supervisor kills a turn",
        "# verb -> hold | shadow | ask | auto",
        "# [agents.judge]",
    ] {
        assert!(
            text.contains(comment),
            "the comment {comment:?} is missing:\n{text}"
        );
    }
    let config = Config::parse(&text).unwrap();
    let claude_dir = home.path("claude").display().to_string();
    assert_eq!(
        config.claude.config_dir.as_deref(),
        Some(claude_dir.as_str())
    );
    let mut defaults = config.clone();
    defaults.claude.config_dir = None;
    assert_eq!(defaults, Config::default());
}

#[test]
fn init_creates_the_ledger_at_the_schema_version() {
    let home = Home::init();
    let ledger = Ledger::open(&home.path("ledger.sqlite")).unwrap();
    assert_eq!(ledger.schema_version().unwrap(), SCHEMA_VERSION);
    let events = ledger.recent_events(None, 10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "init");
}

#[test]
fn init_makes_the_memory_a_git_repo_with_one_commit() {
    let home = Home::init();
    let memory = home.path("memory");
    assert_eq!(git(&memory, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(git(&memory, &["log", "-1", "--format=%an"]), "vision");
    let files = git(&memory, &["ls-files"]);
    assert_eq!(
        files.lines().collect::<Vec<_>>(),
        ["ARCHIVE.md", "decisions/.gitkeep", "principles/.gitkeep"]
    );
    assert_eq!(git(&memory, &["status", "--porcelain"]), "");
}

#[test]
fn a_second_init_changes_nothing_and_says_so() {
    let home = Home::init();
    let before = files_with_times(&home.vision);
    let head = git(&home.path("memory"), &["rev-parse", "HEAD"]);

    let run = home.run(&["init"]);
    run.expect_code(0);
    assert!(
        run.stdout.contains("already set up") && run.stdout.contains("Nothing changed"),
        "{run:?}"
    );
    assert_eq!(files_with_times(&home.vision), before);
    assert_eq!(git(&home.path("memory"), &["rev-parse", "HEAD"]), head);
}

#[test]
fn init_makes_only_a_missing_part() {
    let home = Home::init();
    std::fs::remove_dir(home.path("tickets")).unwrap();
    let config = home.read("config.toml");
    let run = home.run(&["init"]);
    run.expect_code(0);
    assert!(run.stdout.contains("made tickets/"), "{run:?}");
    assert!(!run.stdout.contains("config.toml"), "{run:?}");
    assert_eq!(home.read("config.toml"), config);
}

/// Each file outside the git folder of the memory, with its size and its time of change.
fn files_with_times(root: &Path) -> BTreeMap<String, (u64, SystemTime)> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.ends_with(".git") {
                continue;
            }
            let meta = std::fs::metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                files.insert(name, (meta.len(), meta.modified().unwrap()));
            }
        }
    }
    files
}
