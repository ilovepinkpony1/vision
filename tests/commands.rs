mod common;

use common::Home;

#[test]
fn a_command_that_is_not_built_exits_with_code_3() {
    let home = Home::init();
    let lines: &[(&[&str], &str)] = &[
        (&["pick"], "pick"),
        (&["take", "DEMO-12"], "take"),
        (&["approve-plan", "DEMO-12", "ab12"], "approve-plan"),
        (&["approve", "DEMO-12", "git.push", "9f3e21c"], "approve"),
        (&["needs"], "needs"),
        (&["answer", "Q-1", "--choice", "1"], "answer"),
        (&["memory"], "memory"),
        (&["memory", "show", "D-0142"], "memory show"),
        (&["curate"], "curate"),
        (
            &["model", "eval", "judge", "--models", "opus"],
            "model eval",
        ),
        (&["promote", "git.push"], "promote"),
        (&["report", "--week"], "report"),
    ];
    for (args, name) in lines {
        let run = home.run(args);
        run.expect_code(3);
        assert_eq!(run.stderr, format!("not built yet: {name}\n"), "{args:?}");
        assert_eq!(run.stdout, "", "{args:?}");
    }
}

#[test]
fn a_usage_error_exits_with_code_1_not_2() {
    let home = Home::init();
    let run = home.run(&["take", "../escape"]);
    run.expect_code(1);
    assert!(run.stderr.contains("not a valid ticket key"), "{run:?}");
    home.run(&["no-such-command"]).expect_code(1);
    home.run(&["--help"]).expect_code(0);
}

#[test]
fn install_print_shows_the_timer_and_changes_nothing() {
    let home = Home::init();
    let run = home.run(&["install", "--print"]);
    run.expect_code(0);
    assert!(run.stdout.contains("changed nothing"), "{run:?}");
    assert!(
        run.stdout.contains(&home.vision.display().to_string()),
        "{run:?}"
    );
    assert!(run.stdout.contains("tick"), "{run:?}");
    if cfg!(target_os = "macos") {
        assert!(
            run.stdout
                .contains("<key>StartInterval</key>\n    <integer>60</integer>"),
            "{run:?}"
        );
        assert!(run.stdout.contains("launchctl bootstrap"), "{run:?}");
    } else {
        assert!(run.stdout.contains("OnUnitActiveSec=60s"), "{run:?}");
        assert!(
            run.stdout.contains("systemctl --user enable --now"),
            "{run:?}"
        );
    }
    let uninstall = home.run(&["uninstall", "--print"]);
    uninstall.expect_code(0);
    assert!(
        uninstall.stdout.contains("changed nothing"),
        "{uninstall:?}"
    );
    let made: Vec<_> = std::fs::read_dir(&home.user_home).unwrap().collect();
    assert!(made.is_empty(), "--print wrote into HOME: {made:?}");
}
