mod common;

use common::Home;
use vision::ledger::Ledger;

fn line_starts(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            Some(format!("{} {}", words.next()?, words.next()?))
        })
        .collect()
}

#[test]
fn doctor_on_a_fresh_home_passes_with_one_warning() {
    let home = Home::init();
    let run = home.run(&["doctor"]);
    run.expect_code(0);
    let starts = line_starts(&run.stdout);
    assert_eq!(
        starts[..8],
        [
            "ok home",
            "ok config",
            "ok ledger",
            "warn heartbeat",
            "ok git",
            "ok claude",
            "ok codex",
            "ok memory",
        ],
        "{run:?}"
    );
    assert!(
        run.stdout
            .contains("the timer is not installed. Run `vision install`"),
        "{run:?}"
    );
    assert!(
        run.stdout.contains(&home.fake_claude.display().to_string()),
        "{run:?}"
    );
}

#[test]
fn doctor_writes_each_result_to_the_health_table() {
    let home = Home::init();
    home.run(&["doctor"]).expect_code(0);
    let ledger = Ledger::open(&home.path("ledger.sqlite")).unwrap();
    let rows = ledger.latest_health().unwrap();
    let tests: Vec<&str> = rows.iter().map(|r| r.test.as_str()).collect();
    assert_eq!(
        tests,
        [
            "adapter",
            "archive",
            "claude",
            "codex",
            "config",
            "git",
            "heartbeat",
            "home",
            "journal",
            "ledger",
            "memory",
            "sessions"
        ]
    );
    assert!(rows.iter().all(|r| r.ok));
    let heartbeat = rows.iter().find(|r| r.test == "heartbeat").unwrap();
    assert!(heartbeat.detail.starts_with("warning: "), "{heartbeat:?}");
}

#[test]
fn doctor_names_the_author_of_the_commits_of_a_session() {
    let home = Home::init();
    let run = home.run(&["doctor"]);
    run.expect_code(0);
    assert!(run.stdout.contains("(vision) <"), "{run:?}");
    let config = home.path("config.toml");
    let mut doc: toml_edit::DocumentMut =
        std::fs::read_to_string(&config).unwrap().parse().unwrap();
    doc["git"]["name"] = toml_edit::value("app[bot]");
    doc["git"]["email"] = toml_edit::value("7+app[bot]@users.noreply.github.com");
    std::fs::write(&config, doc.to_string()).unwrap();
    let run = home.run(&["doctor"]);
    run.expect_code(0);
    assert!(
        run.stdout
            .contains("sessions commit as app[bot] <7+app[bot]@users.noreply.github.com>"),
        "{run:?}"
    );
}

#[test]
fn doctor_json_and_a_fresh_heartbeat() {
    let home = Home::init();
    home.run(&["tick"]).expect_code(0);
    let run = home.run(&["doctor", "--json"]);
    run.expect_code(0);
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(json["ok"], true);
    let heartbeat = json["tests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["test"] == "heartbeat")
        .unwrap();
    assert_eq!(heartbeat["result"], "ok");
}

#[test]
fn doctor_fails_without_the_claude_binary() {
    let home = Home::init();
    let missing = home.user_home.join("no-such-claude");
    let run = home.run_with(
        &["doctor"],
        &[("VISION_CLAUDE_BIN", missing.to_str().unwrap())],
    );
    run.expect_code(1);
    assert!(
        line_starts(&run.stdout).contains(&"fail claude".to_string()),
        "{run:?}"
    );
    assert!(run.stdout.contains("Install Claude Code"), "{run:?}");
}

#[test]
fn doctor_before_init_fails_and_says_what_to_do() {
    let home = Home::new();
    let run = home.run(&["doctor"]);
    run.expect_code(1);
    assert!(run.stdout.contains("fail  home"), "{run:?}");
    assert!(run.stdout.contains("vision init"), "{run:?}");
}

#[test]
fn doctor_names_the_plugins_and_fails_a_folder_with_none() {
    let home = Home::init();
    let run = home.run(&["doctor"]);
    assert!(
        !line_starts(&run.stdout)
            .iter()
            .any(|l| l.ends_with(" plugins")),
        "no plugin line with an empty list: {run:?}"
    );
    let plugin = home.user_home.join("plugins/mine");
    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name": "mine", "version": "1.0.0"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(plugin.join("skills/review")).unwrap();
    std::fs::write(
        plugin.join("skills/review/SKILL.md"),
        "---\nname: review\n---\n",
    )
    .unwrap();
    let empty = home.user_home.join("plugins/empty");
    std::fs::create_dir_all(&empty).unwrap();
    let gone = home.user_home.join("plugins/gone");
    let set = |dirs: &[&std::path::Path]| {
        let config = home.path("config.toml");
        let mut doc: toml_edit::DocumentMut =
            std::fs::read_to_string(&config).unwrap().parse().unwrap();
        let mut list = toml_edit::Array::new();
        for dir in dirs {
            list.push(dir.display().to_string());
        }
        doc["claude"]["plugins"] = toml_edit::value(list);
        std::fs::write(&config, doc.to_string()).unwrap();
    };

    set(&[&plugin]);
    let run = home.run(&["doctor"]);
    run.expect_code(0);
    assert!(
        line_starts(&run.stdout).contains(&"ok plugins".to_string()),
        "{run:?}"
    );
    assert!(run.stdout.contains("mine (1 skill) at "), "{run:?}");

    set(&[&plugin, &empty, &gone]);
    let run = home.run(&["doctor"]);
    run.expect_code(1);
    assert!(
        line_starts(&run.stdout).contains(&"fail plugins".to_string()),
        "{run:?}"
    );
    assert!(run.stdout.contains("holds no plugin"), "{run:?}");
    assert!(run.stdout.contains("does not exist"), "{run:?}");
    assert!(run.stdout.contains("Found: mine (1 skill)"), "{run:?}");
}

/// Runs git in `dir` with a neutral author.
fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let mut all = vec![
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.invalid",
    ];
    all.extend_from_slice(args);
    common::git(dir, &all)
}

#[test]
fn doctor_tests_each_clone_and_warns_about_a_name_that_one_clone_lacks() {
    let home = Home::init();
    let work = home.user_home.join("work");
    let main = work.join("app");
    std::fs::create_dir_all(&main).unwrap();
    git_in(&main, &["init", "-q", "-b", "main"]);
    git_in(&main, &["commit", "-q", "--allow-empty", "-m", "Start"]);
    let slot = work.join("app-b");
    git_in(
        &main,
        &["worktree", "add", "-q", "--detach", slot.to_str().unwrap()],
    );
    let broken = work.join("app-c");
    git_in(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            broken.to_str().unwrap(),
        ],
    );
    std::fs::remove_dir_all(main.join(".git/worktrees/app-c")).unwrap();
    let plain = work.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let missing = work.join("missing");
    let text = |path: &std::path::Path| path.display().to_string();
    let write = |clones: String, env: &str| {
        let config = home.path("config.toml");
        let mut doc: toml_edit::DocumentMut =
            std::fs::read_to_string(&config).unwrap().parse().unwrap();
        let entries: toml_edit::DocumentMut = format!("clones = [{clones}]\n").parse().unwrap();
        doc["clones"] = entries["clones"].clone();
        let fixed: toml_edit::DocumentMut = format!("env = {{ {env} }}\n").parse().unwrap();
        doc["claude"]["env"] = fixed["env"].clone();
        std::fs::write(&config, doc.to_string()).unwrap();
    };
    write(
        format!(
            "{:?}, {{ path = {:?}, env = {{ WORKSPACE = \"app-b\" }}, adapter = {{ app_url = \"http://localhost:3500/\", app_health_url = \"http://localhost:3500/health\" }} }}, {:?}, {:?}, {:?}",
            text(&main),
            text(&slot),
            text(&broken),
            text(&plain),
            text(&missing)
        ),
        "",
    );
    let run = home.run(&["doctor"]);
    run.expect_code(1);
    let lines: Vec<&str> = run
        .stdout
        .lines()
        .filter(|l| l.split_whitespace().nth(1) == Some("clone"))
        .collect();
    assert_eq!(lines.len(), 5, "one line for each clone: {run:?}");
    // The main checkout lacks the name that the worktree sets.
    assert!(lines[0].starts_with("warn  clone      ~/work/app. Its env lacks WORKSPACE (set by ~/work/app-b), and claude.env does not set it"), "{}", lines[0]);
    // The worktree names its main checkout and the names of its values.
    assert_eq!(
        lines[1],
        "ok    clone      ~/work/app-b, worktree of ~/work/app; env WORKSPACE; adapter app_health_url, app_url"
    );
    assert!(
        lines[2].starts_with("fail  clone      ~/work/app-c is a git worktree, but its git folder"),
        "{}",
        lines[2]
    );
    assert!(lines[2].contains("is gone"), "{}", lines[2]);
    assert!(
        lines[3]
            .starts_with("fail  clone      ~/work/plain is not the top folder of a git work tree"),
        "{}",
        lines[3]
    );
    assert!(
        lines[4].starts_with("fail  clone      ~/work/missing does not exist"),
        "{}",
        lines[4]
    );

    // With the name in claude.env, the main checkout has it too, and each line passes.
    write(
        format!(
            "{:?}, {{ path = {:?}, env = {{ WORKSPACE = \"app-b\" }} }}",
            text(&main),
            text(&slot)
        ),
        "WORKSPACE = \"app\"",
    );
    let run = home.run(&["doctor", "--json"]);
    run.expect_code(0);
    let json: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    let clones: Vec<&serde_json::Value> = json["tests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["test"] == "clone")
        .collect();
    assert_eq!(clones.len(), 2, "{json}");
    assert!(clones.iter().all(|t| t["result"] == "ok"), "{json}");
    assert_eq!(clones[0]["detail"], "~/work/app");
}
