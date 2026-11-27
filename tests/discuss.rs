//! `vision discuss KEY` (section 15.3): the own Claude Code session of the person about a
//! ticket. The demo world gives a real ticket that waits for the approval of its plan,
//! `tests/bin/fake-herdr` plays `herdr-app` and `hc`, and a small script plays the command of
//! `[discuss]`: it writes its arguments and its folder.

mod pipeline_support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pipeline_support::{simple_scenarios, World, KEY};
use serde_json::{json, Value};

fn fake_herdr() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/bin/fake-herdr")
}

/// The calls of the fake Herdr, each with the name of its CLI first.
fn calls(state: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(state.join("calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The arguments of each call `hc run`, after `run`.
fn runs(state: &Path) -> Vec<Vec<String>> {
    calls(state)
        .into_iter()
        .filter(|c| c.len() >= 2 && c[0] == "hc" && c[1] == "run")
        .map(|c| c[2..].to_vec())
        .collect()
}

/// The JSON objects of the calls `herdr-app do <command>`.
fn app_do(state: &Path, command: &str) -> Vec<Value> {
    calls(state)
        .into_iter()
        .filter(|c| c.len() == 4 && c[0] == "herdr-app" && c[1] == "do" && c[2] == command)
        .map(|c| serde_json::from_str(&c[3]).unwrap())
        .collect()
}

/// The value of the option `name` of a call.
fn option<'a>(call: &'a [String], name: &str) -> Option<&'a str> {
    let at = call.iter().position(|a| a == name)?;
    call.get(at + 1).map(String::as_str)
}

/// The fake Herdr as `[herdr]`, and a script as the command of `[discuss]`. Gives the folder
/// of the state of the fake and the folder where the script writes.
fn herdr_and_command(world: &World) -> (PathBuf, PathBuf) {
    let root = world.vision.parent().unwrap();
    let state = root.join("herdr");
    let out = root.join("discuss-out");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&out).unwrap();
    for name in ["herdr-app", "hc"] {
        std::os::unix::fs::symlink(fake_herdr(), state.join(name)).unwrap();
    }
    let script = root.join("discuss-command");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{out}/args.tmp' && mv '{out}/args.tmp' '{out}/args'\npwd > '{out}/pwd'\n",
            out = out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (hc, app, command) = (
        state.join("hc").display().to_string(),
        state.join("herdr-app").display().to_string(),
        script.display().to_string(),
    );
    world.edit_config(|doc| {
        doc["herdr"]["enabled"] = toml_edit::value(true);
        doc["herdr"]["bin"] = toml_edit::value(hc);
        doc["herdr"]["app"] = toml_edit::value(app);
        doc["discuss"]["command"] = toml_edit::value(command);
    });
    (state, out)
}

/// A ticket that waits for the approval of its plan: a thing that waits for the person.
fn waiting_world() -> World {
    let world = World::new();
    simple_scenarios(&world);
    world.run(&["take", KEY]).expect_code(0);
    world.tick_to("plan-ready");
    world
}

fn wait_for_file(path: &Path) -> String {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            return text;
        }
        assert!(
            Instant::now() < until,
            "{} did not appear in 20 seconds",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn discuss_writes_a_brief_and_opens_the_session_of_the_person_in_the_tab_discuss() {
    let world = waiting_world();
    let step = world.step();
    let flags = world.flags();
    let (state, out) = herdr_and_command(&world);
    let run = world.run_with(&["discuss", KEY, "--herdr"], &[("SHELL", "/bin/sh")]);
    run.expect_code(0);
    let said = "Your session about DEMO-12 opens in the tab `DEMO-12 discuss` of the Herdr space vision. Its brief is ";
    assert!(run.stdout.starts_with(said), "{run:?}");
    let brief = PathBuf::from(run.stdout[said.len()..].trim_end().trim_end_matches('.'));
    assert_eq!(
        brief.parent().unwrap(),
        world.vision.join("tickets/DEMO-12/discuss"),
        "{run:?}"
    );

    // One terminal in the space of the turns, in the clone of the ticket, with the shell of the
    // person: the command, then the shell again.
    let all = runs(&state);
    assert_eq!(all.len(), 1, "{all:?}");
    let call = &all[0];
    let tab = call
        .iter()
        .find_map(|a| a.strip_prefix("herdr.tab="))
        .unwrap()
        .to_string();
    assert!(tab.starts_with("vision-DEMO-12-discuss-"), "{call:?}");
    assert!(call.contains(&"herdr.space=vision".to_string()), "{call:?}");
    assert!(call.contains(&"--no-parent".to_string()), "{call:?}");
    assert_eq!(
        option(call, "--cwd"),
        Some(world.clone().to_str().unwrap()),
        "{call:?}"
    );
    let argv = &call[call.iter().position(|a| a == "--").unwrap() + 1..];
    assert_eq!(argv[..2], ["/bin/sh", "-lic"], "{call:?}");
    assert!(argv[2].ends_with("; exec '/bin/sh' -l"), "{call:?}");
    assert!(
        app_do(&state, "tab.rename").contains(&json!({"id": tab, "name": "DEMO-12 discuss"})),
        "{:?}",
        calls(&state)
    );
    assert_eq!(app_do(&state, "tab.select"), [json!({"id": tab})]);

    // The command got `--` and the first message, in the clone.
    let args = wait_for_file(&out.join("args"));
    let lines: Vec<&str> = args.lines().collect();
    assert_eq!(lines[0], "--", "{args}");
    assert_eq!(
        lines[1],
        format!(
            "Vision needs me on the ticket DEMO-12 at the step plan-ready. Read the brief from Vision first: {}. Then tell me in plain words what the problem is, what my options are, and which one you recommend. Do not change anything until I ask.",
            brief.display()
        ),
        "{args}"
    );
    let pwd = wait_for_file(&out.join("pwd"));
    assert_eq!(
        std::fs::canonicalize(pwd.trim()).unwrap(),
        std::fs::canonicalize(world.clone()).unwrap()
    );

    // The brief: what waits, the ticket, its history, and the rules of the session.
    let text = std::fs::read_to_string(&brief).unwrap();
    for part in [
        "# DEMO-12: a brief from Vision",
        "### The plan waits for an approval at the step plan-ready",
        "`vision approve-plan DEMO-12 ",
        "- Step: plan-ready",
        &format!(
            "- Clone: {}. This session runs there.",
            world.clone().display()
        ),
        "## The last events, newest last",
        "## The last turns, newest last",
        "builder at the step plan",
        "Its documents: ",
        "plan.md",
        "## Rules for this session",
    ] {
        assert!(text.contains(part), "the brief lacks {part:?}:\n{text}");
    }

    // The ticket stays as it is.
    assert_eq!(world.step(), step);
    assert_eq!(world.flags(), flags);
}

#[test]
fn discuss_with_herdr_fails_while_herdr_is_off_and_says_what_to_do() {
    let world = waiting_world();
    let run = world.run(&["discuss", KEY, "--herdr"]);
    run.expect_code(1);
    assert!(
        run.stderr.contains(
            "error: Your session about DEMO-12 opens only in a tab of Herdr.app, and Herdr is off in config.toml."
        ) && run
            .stderr
            .contains("run `vision discuss DEMO-12` in a terminal"),
        "{run:?}"
    );
    assert!(
        !world.vision.join("tickets/DEMO-12/discuss").exists(),
        "no brief without a session"
    );
}

#[test]
fn discuss_refuses_a_ticket_that_vision_does_not_know() {
    let world = World::new();
    let run = world.run(&["discuss", "DEMO-99"]);
    run.expect_code(1);
    assert!(
        run.stderr.contains("DEMO-99 is not a ticket of Vision"),
        "{run:?}"
    );
}
