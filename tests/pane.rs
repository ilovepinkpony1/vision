//! The turns in herdr panes (section 7.7), with `tests/bin/fake-herdr` as the herdr CLI and the
//! interactive mode of the fake `claude` (`tests/bin/fake-claude-pane`).

mod turn_support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use turn_support::{result_object, wait_for, TestHome};
use vision::turn::{Reason, TurnKind};

fn fake_herdr() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/bin/fake-herdr")
}

/// A home whose turns run in panes of the fake herdr. The session name is the folder of the
/// state of the fake.
fn pane_home() -> (TestHome, PathBuf) {
    let home = TestHome::new();
    let state = home.scenarios.join("herdr");
    std::fs::create_dir_all(&state).unwrap();
    home.edit_config(|doc| {
        doc["herdr"]["enabled"] = toml_edit::value(true);
        doc["herdr"]["bin"] = toml_edit::value(fake_herdr().display().to_string());
        doc["herdr"]["session"] = toml_edit::value(state.display().to_string());
        doc["herdr"]["app"] = toml_edit::value("/usr/bin/false");
    });
    (home, state)
}

/// The calls of the fake herdr.
fn calls(state: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(state.join("calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn prompts(state: &Path) -> Vec<String> {
    calls(state)
        .into_iter()
        .filter(|c| c.len() >= 4 && c[0] == "agent" && c[1] == "prompt")
        .map(|c| c[3].clone())
        .collect()
}

fn launch(home: &TestHome, replies: Value) -> vision::turn::Launched {
    let scenario = home.scenario("unused", json!([[]]));
    let mut request = home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario);
    request.facts["fake"]["pane"] = replies;
    home.launch(&request)
}

#[test]
fn a_turn_in_a_pane_runs_in_a_tab_of_the_ticket_and_ends_with_its_result_object() {
    let (home, state) = pane_home();
    let result = result_object("Refined in the pane.");
    let launched = launch(
        &home,
        json!([{"message": "I read the issue, and I start."}, {"message": result.to_string()}]),
    );
    let ended = home.wait_ended(&launched);
    assert_eq!(ended.reason, Some(Reason::Result), "{ended:?}");
    let pane = ended.pane.clone().expect("the turn names its pane");
    assert_eq!(pane.session, state.display().to_string());
    // The ticket gets its workspace, and the tab of the turn has the name of the agent.
    let all = calls(&state);
    assert!(
        all.iter()
            .any(|c| c[..2] == ["workspace", "create"] && c.contains(&"DEMO-12".to_string())),
        "{all:?}"
    );
    assert!(all
        .iter()
        .any(|c| c[..2] == ["tab", "rename"] && c[2] == pane.tab && c[3] == "scout"));
    assert!(all
        .iter()
        .any(|c| c[..2] == ["pane", "run"] && c[2] == pane.pane && c[3].ends_with("pane.sh'")));
    // The first answer was no result object, so Vision reminded the agent once, then ended
    // the session after the result.
    let typed = prompts(&state);
    assert_eq!(typed.len(), 2, "{typed:?}");
    assert!(
        typed[0].starts_with("Vision: your last message is not one JSON object."),
        "{typed:?}"
    );
    assert_eq!(typed[1], "/exit");
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
    let hooks = launched.dir.join("hooks.jsonl");
    wait_for(Duration::from_secs(20), "the first prompt", || {
        std::fs::read_to_string(&hooks)
            .ok()
            .filter(|t| t.contains("UserPromptSubmit"))
    });
    let typed = std::fs::read_dir(&state)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "log"))
        .map(|p| p.with_extension("typed"))
        .expect("the pane of the turn");
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
fn a_herdr_session_that_does_not_start_ends_the_turn_with_what_to_do() {
    let (home, state) = pane_home();
    std::fs::write(state.join("down"), "").unwrap();
    let launched = launch(&home, json!([]));
    let ended = home.wait_ended(&launched);
    assert_eq!(ended.reason, Some(Reason::Error), "{ended:?}");
    let detail = ended.detail.unwrap_or_default();
    assert!(
        detail.contains("Herdr.app did not open a window on the session"),
        "{detail}"
    );
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

/// A stand-in for the program of Herdr.app: it logs its arguments and lists no window.
fn fake_app(state: &Path) -> PathBuf {
    let path = state.join("fake-app");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n[ \"$2\" = list ] && echo '[]'\nexit 0\n",
            state.join("app.log").display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

#[test]
fn open_shows_the_running_pane_in_herdr_and_tell_types_into_it() {
    let (home, state) = pane_home();
    let app = fake_app(&state);
    home.edit_config(|doc| doc["herdr"]["app"] = toml_edit::value(app.display().to_string()));
    // No reply: the session waits for its next prompt, so the turn runs on.
    let launched = launch(&home, json!([]));
    let hooks = launched.dir.join("hooks.jsonl");
    wait_for(Duration::from_secs(20), "the first prompt", || {
        std::fs::read_to_string(&hooks)
            .ok()
            .filter(|t| t.contains("UserPromptSubmit"))
    });
    let open = home.run(&["open", "DEMO-12", "--herdr"]);
    open.expect_code(0);
    assert!(
        open.stdout.contains("Herdr.app shows it"),
        "{}",
        open.stdout
    );
    let all = calls(&state);
    assert!(
        all.iter().any(|c| c[..2] == ["workspace", "focus"]),
        "{all:?}"
    );
    assert!(all.iter().any(|c| c[..2] == ["tab", "focus"]), "{all:?}");
    let log = std::fs::read_to_string(state.join("app.log")).unwrap();
    assert!(log.contains("window list --json"), "{log}");
    assert!(
        log.contains(&format!("window new --session {} --json", state.display())),
        "with no window of the ticket, Herdr.app opens one: {log}"
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
