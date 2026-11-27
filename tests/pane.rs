//! The turns in herdr panes (section 7.7), with `tests/bin/fake-herdr` as `herdr-app` and `hc`,
//! and the interactive mode of the fake `claude` (`tests/bin/fake-claude-pane`).

mod turn_support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use turn_support::{result_object, wait_for, TestHome};
use vision::turn::{Reason, TurnKind};

fn fake_herdr() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/bin/fake-herdr")
}

/// A home whose turns run in the fake Herdr: `herdr-app` and `hc` are links to the fake in the
/// folder of its state.
fn pane_home() -> (TestHome, PathBuf) {
    let home = TestHome::new();
    let state = home.scenarios.join("herdr");
    std::fs::create_dir_all(&state).unwrap();
    for name in ["herdr-app", "hc"] {
        std::os::unix::fs::symlink(fake_herdr(), state.join(name)).unwrap();
    }
    home.edit_config(|doc| {
        doc["herdr"]["enabled"] = toml_edit::value(true);
        doc["herdr"]["bin"] = toml_edit::value(state.join("hc").display().to_string());
        doc["herdr"]["app"] = toml_edit::value(state.join("herdr-app").display().to_string());
    });
    (home, state)
}

/// The calls of the fake, each with the name of its CLI first.
fn calls(state: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(state.join("calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The arguments of the calls `hc <command>`, after the command.
fn hc_calls(state: &Path, command: &str) -> Vec<Vec<String>> {
    calls(state)
        .into_iter()
        .filter(|c| c.len() >= 2 && c[0] == "hc" && c[1] == command)
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

fn prompts(state: &Path) -> Vec<String> {
    hc_calls(state, "send")
        .into_iter()
        .map(|c| c[1].clone())
        .collect()
}

/// The value of a label of a call `hc run`.
fn label<'a>(run: &'a [String], key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    run.iter().find_map(|a| a.strip_prefix(prefix.as_str()))
}

fn launch(home: &TestHome, replies: Value) -> vision::turn::Launched {
    let scenario = home.scenario("unused", json!([[]]));
    let mut request = home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario);
    request.facts["fake"]["pane"] = replies;
    home.launch(&request)
}

fn wait_first_prompt(launched: &vision::turn::Launched) {
    let hooks = launched.dir.join("hooks.jsonl");
    wait_for(Duration::from_secs(20), "the first prompt", || {
        std::fs::read_to_string(&hooks)
            .ok()
            .filter(|t| t.contains("UserPromptSubmit"))
    });
}

#[test]
fn a_turn_in_a_pane_runs_in_a_terminal_of_herdr_core_in_its_tab_and_ends_with_its_result() {
    let (home, state) = pane_home();
    let result = result_object("Refined in the pane.");
    let launched = launch(
        &home,
        json!([{"message": "I read the issue, and I start."}, {"message": result.to_string()}]),
    );
    let ended = home.wait_ended(&launched);
    assert_eq!(ended.reason, Some(Reason::Result), "{ended:?}");
    let pane = ended.pane.clone().expect("the turn names its pane");
    // The terminal has no parent, runs pane.sh, and has the labels of its tab. No space has the
    // name of the configuration, so the id of the new space is that name.
    let runs = hc_calls(&state, "run");
    assert_eq!(runs.len(), 1, "{runs:?}");
    let run = &runs[0];
    assert!(run.contains(&"--no-parent".to_string()), "{run:?}");
    assert!(run.last().unwrap().ends_with("/pane.sh"), "{run:?}");
    let tab = format!("vision-{}", launched.id);
    assert_eq!(label(run, "herdr.space"), Some("vision"));
    assert_eq!(label(run, "herdr.tab"), Some(tab.as_str()));
    assert_eq!(
        (
            pane.session.as_str(),
            pane.space.as_str(),
            pane.tab.as_str()
        ),
        ("vision", "vision", tab.as_str())
    );
    // The new space gets the name of the configuration, and the tab the ticket and the agent.
    assert_eq!(
        app_do(&state, "space.rename"),
        [json!({"id": "vision", "name": "vision"})]
    );
    assert_eq!(
        app_do(&state, "tab.rename"),
        [json!({"id": tab, "name": "DEMO-12 scout"})]
    );
    // The first answer was no result object, so Vision reminded the agent once, then ended
    // the session after the result. Each prompt went to the terminal of the turn.
    let typed = prompts(&state);
    assert_eq!(typed.len(), 2, "{typed:?}");
    assert!(
        typed[0].starts_with("Vision: your last message is not one JSON object."),
        "{typed:?}"
    );
    assert_eq!(typed[1], "/exit");
    assert!(hc_calls(&state, "send")
        .iter()
        .all(|c| c[0] == pane.terminal));
    // At the end, the terminal is gone from herdr-core, and its tab with it.
    assert!(hc_calls(&state, "rm").iter().any(|c| c[0] == pane.terminal));
    // The result object, the stream in the form of the headless stream, and the cost.
    let stored: Value = serde_json::from_str(&home.turn_file(&launched, "result.json")).unwrap();
    assert_eq!(stored, result);
    assert_eq!(home.events(&launched, "assistant", None).len(), 2);
    let users = home.events(&launched, "user", None);
    assert!(
        users[0]["message"]["content"]
            .as_str()
            .unwrap()
            .starts_with("Turn "),
        "{users:?}"
    );
    let results = home.events(&launched, "result", None);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["structured_output"], result);
    assert_eq!(results[0]["usage"]["output_tokens"], 8);
    assert_eq!(results[0]["total_cost_usd"], 0.1);
    assert_eq!(home.events(&launched, "system", Some("init")).len(), 1);
    // The settings hold the hooks, and the role holds the end of the turn in a pane.
    let settings: Value =
        serde_json::from_str(&home.turn_file(&launched, "settings.json")).unwrap();
    assert!(settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains(" turn-hook "));
    assert!(home
        .turn_file(&launched, "role.md")
        .contains("# The end of your turn"));
    // The collector reads it as any other turn.
    let collected = home.collect();
    assert_eq!(collected.len(), 1, "{collected:?}");
}

#[test]
fn a_message_of_the_person_in_the_pane_holds_the_reminder() {
    let (home, state) = pane_home();
    let result = result_object("Done with the rename.");
    let launched = launch(
        &home,
        json!([
            {"message": "I start.", "sleep": 1.5},
            {"message": "Sure, I renamed it as you asked."},
            {"message": result.to_string()}
        ]),
    );
    // The person types while the agent works on the first prompt.
    wait_first_prompt(&launched);
    let typed = std::fs::read_dir(&state)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "log"))
        .map(|p| p.with_extension("typed"))
        .expect("the terminal of the turn");
    std::fs::write(&typed, "Please also rename the method.\n").unwrap();
    let ended = home.wait_ended(&launched);
    assert_eq!(ended.reason, Some(Reason::Result), "{ended:?}");
    // One reminder after the first answer. The answer to the person got none.
    let reminders: Vec<String> = prompts(&state)
        .into_iter()
        .filter(|p| p.starts_with("Vision:"))
        .collect();
    assert_eq!(reminders.len(), 1, "{reminders:?}");
    let log = std::fs::read_to_string(home.vision.join("log/vision.log")).unwrap_or_default();
    assert!(log.contains("the person wrote in the pane"), "{log}");
}

#[test]
fn a_usage_limit_in_the_pane_ends_the_turn_with_its_message() {
    let (home, _state) = pane_home();
    let launched = launch(
        &home,
        json!([{"fail": "rate_limit", "message": "You've hit your limit · resets 3pm"}]),
    );
    let ended = home.wait_ended(&launched);
    assert_eq!(ended.reason, Some(Reason::Error), "{ended:?}");
    assert_eq!(
        ended.detail.as_deref(),
        Some("Claude hit a usage limit: You've hit your limit · resets 3pm")
    );
}

#[test]
fn herdr_app_opens_its_window_when_it_does_not_answer() {
    let (home, state) = pane_home();
    std::fs::write(state.join("down"), "").unwrap();
    let launched = launch(
        &home,
        json!([{"message": result_object("Done.").to_string()}]),
    );
    let ended = home.wait_ended(&launched);
    assert_eq!(ended.reason, Some(Reason::Result), "{ended:?}");
    let all = calls(&state);
    let opened = all
        .iter()
        .position(|c| *c == ["herdr-app"])
        .expect("herdr-app opens the window");
    let started = all
        .iter()
        .position(|c| c.len() > 1 && c[0] == "hc" && c[1] == "run")
        .unwrap();
    assert!(opened < started, "{all:?}");
    assert_eq!(
        label(&hc_calls(&state, "run")[0], "herdr.space"),
        Some("vision")
    );
}

#[test]
fn a_turn_runs_hidden_when_the_window_does_not_open_and_open_gives_it_its_tab() {
    let (home, state) = pane_home();
    std::fs::write(state.join("down"), "").unwrap();
    std::fs::write(state.join("no-window"), "").unwrap();
    // No reply: the session waits for its next prompt, so the turn runs on.
    let launched = launch(&home, json!([]));
    wait_first_prompt(&launched);
    let run = hc_calls(&state, "run").remove(0);
    assert_eq!(label(&run, "herdr.space"), None, "{run:?}");
    let log = std::fs::read_to_string(home.vision.join("log/vision.log")).unwrap_or_default();
    assert!(
        log.contains("runs hidden in herdr-core: Herdr.app did not open its window"),
        "{log}"
    );
    let pane = home
        .state("DEMO-12", &launched.id.to_string())
        .unwrap()
        .pane
        .unwrap();
    assert_eq!(pane.space, "", "{pane:?}");
    // Now the window opens: `open` gives the terminal its tab, names the tab, and shows it.
    std::fs::remove_file(state.join("no-window")).unwrap();
    home.run(&["open", "DEMO-12", "--herdr"]).expect_code(0);
    assert_eq!(
        hc_calls(&state, "label"),
        [vec![
            pane.terminal.clone(),
            "herdr.space=vision".to_string(),
            format!("herdr.tab={}", pane.tab)
        ]]
    );
    assert_eq!(
        app_do(&state, "tab.rename"),
        [json!({"id": pane.tab, "name": "DEMO-12 scout"})]
    );
    assert_eq!(app_do(&state, "tab.select"), [json!({"id": pane.tab})]);
    let ctx = home.ctx();
    assert!(vision::turn::kill_turn(&ctx, &launched.ticket, &launched.id).unwrap());
    home.wait_ended(&launched);
}

#[test]
fn the_judge_stays_headless() {
    let (home, state) = pane_home();
    let scenario = home.scenario(
        "judge",
        json!([[{"do": "init"}, {"do": "result", "structured_output": {"answers": []}}]]),
    );
    let request = home.request("DEMO-12", "judge", "questions", TurnKind::Reader, &scenario);
    let launched = home.launch(&request);
    home.wait_ended(&launched);
    let ended = home.state("DEMO-12", &launched.id.to_string()).unwrap();
    assert_eq!(ended.pane, None, "{ended:?}");
    assert!(calls(&state).is_empty(), "the Judge opens no tab");
}

#[test]
fn open_shows_the_running_pane_in_herdr_and_tell_types_into_it() {
    let (home, state) = pane_home();
    // No reply: the session waits for its next prompt, so the turn runs on.
    let launched = launch(&home, json!([]));
    wait_first_prompt(&launched);
    let open = home.run(&["open", "DEMO-12", "--herdr"]);
    open.expect_code(0);
    assert!(
        open.stdout.contains(
            "runs in the tab `DEMO-12 scout` of the Herdr space vision. Herdr.app shows it"
        ),
        "{}",
        open.stdout
    );
    let pane = home
        .state("DEMO-12", &launched.id.to_string())
        .unwrap()
        .pane
        .unwrap();
    assert_eq!(app_do(&state, "space.select"), [json!({"id": pane.space})]);
    assert_eq!(app_do(&state, "tab.select"), [json!({"id": pane.tab})]);
    assert!(
        hc_calls(&state, "label").is_empty(),
        "the tab exists already"
    );
    // Then the window comes to the front: herdr-app with no arguments.
    let all = calls(&state);
    let selected = all
        .iter()
        .rposition(|c| c.len() > 2 && c[2] == "tab.select")
        .unwrap();
    assert!(
        all[selected..].iter().any(|c| *c == ["herdr-app"]),
        "{all:?}"
    );
    // A message of the person goes into the pane, and the transcript shows it in the stream.
    home.run(&["tell", "DEMO-12", "Keep the old name too."])
        .expect_code(0);
    home.wait_stream(&launched, "Keep the old name too.");
    assert!(prompts(&state).contains(&"Keep the old name too.".to_string()));
    // A stop ends the session of the pane.
    let ctx = home.ctx();
    assert!(vision::turn::kill_turn(&ctx, &launched.ticket, &launched.id).unwrap());
    let ended = home.wait_ended(&launched);
    assert_eq!(ended.reason, Some(Reason::Killed), "{ended:?}");
    let pid: u32 = home
        .turn_file(&launched, "pane.pid")
        .trim()
        .parse()
        .unwrap();
    turn_support::wait_gone(pid);
}

#[test]
fn open_with_herdr_refuses_a_ticket_with_no_turn_in_a_pane() {
    let (home, _state) = pane_home();
    let run = home.run(&["open", "DEMO-99", "--herdr"]);
    assert_ne!(run.code, 0);
    assert!(
        run.stderr
            .contains("DEMO-99 has no turn in a herdr pane now"),
        "{}",
        run.stderr
    );
}

#[test]
fn open_with_no_running_turn_takes_over_the_session_in_the_tab_you() {
    let (home, state) = pane_home();
    let first = launch(
        &home,
        json!([{"message": result_object("One.").to_string()}]),
    );
    home.wait_ended(&first);
    home.collect();
    let open = home.run(&["open", "DEMO-12"]);
    open.expect_code(0);
    assert!(
        open.stdout
            .contains("DEMO-12 is paused, so no new turn starts for it. The session")
            && open
                .stdout
                .contains("opens in the tab `DEMO-12 you` of the Herdr space vision."),
        "{}",
        open.stdout
    );
    let runs = hc_calls(&state, "run");
    let run = runs.last().unwrap();
    let tab = label(run, "herdr.tab").unwrap().to_string();
    assert!(tab.starts_with("vision-DEMO-12-you-"), "{run:?}");
    let argv = &run[run.iter().position(|a| a == "--").unwrap() + 1..];
    assert_eq!(argv[..2], ["/bin/sh", "-c"], "{run:?}");
    assert!(
        argv[2].contains(&format!("--resume '{}'", first.session)),
        "{run:?}"
    );
    assert!(app_do(&state, "tab.rename").contains(&json!({"id": tab, "name": "DEMO-12 you"})));
    assert_eq!(app_do(&state, "tab.select"), [json!({"id": tab})]);
    assert_eq!(home.ledger().ticket_flags("DEMO-12").unwrap(), ["paused"]);
}

#[test]
fn each_folder_dialog_gets_down_and_then_enter_only_with_yes_selected() {
    let (home, state) = pane_home();
    let scenario = home.scenario("unused", json!([[]]));
    let mut request = home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario);
    request.facts["fake"]["dialogs"] = json!(3);
    request.facts["fake"]["pane"] =
        json!([{"message": result_object("After three folders.").to_string()}]);
    let launched = home.launch(&request);
    let ended = home.wait_ended(&launched);
    assert_eq!(ended.reason, Some(Reason::Result), "{ended:?}");
    let keys: Vec<String> = hc_calls(&state, "keys")
        .into_iter()
        .map(|c| c[1..].join(" "))
        .collect();
    assert_eq!(
        keys,
        ["down", "enter", "down", "enter", "down", "enter"],
        "{keys:?}"
    );
    let log = std::fs::read_to_string(home.vision.join("log/vision.log")).unwrap_or_default();
    assert_eq!(
        log.matches("trusted a folder in the dialog of Claude Code")
            .count(),
        3
    );
}

#[test]
fn each_turn_removes_its_terminal_and_the_next_turn_finds_the_space_by_its_name() {
    let (home, state) = pane_home();
    let first = launch(
        &home,
        json!([{"message": result_object("One.").to_string()}]),
    );
    let one = home.wait_ended(&first).pane.unwrap();
    let second = launch(
        &home,
        json!([{"message": result_object("Two.").to_string()}]),
    );
    let two = home.wait_ended(&second).pane.unwrap();
    assert_eq!(two.space, one.space, "the turns share the space");
    assert_ne!(two.tab, one.tab);
    let all = calls(&state);
    let removed = all
        .iter()
        .position(|c| c.len() > 2 && c[0] == "hc" && c[1] == "rm" && c[2] == one.terminal)
        .expect("the first turn removes its terminal");
    let started = all
        .iter()
        .rposition(|c| c.len() > 1 && c[0] == "hc" && c[1] == "run")
        .unwrap();
    assert!(removed < started, "{all:?}");
    assert_eq!(
        app_do(&state, "space.rename").len(),
        1,
        "the space gets its name once"
    );
}
