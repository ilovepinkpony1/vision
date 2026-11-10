//! The launch API, the supervisor, and the collector, driven through the library and the
//! built binary with the fake `claude`.

mod turn_support;

use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use turn_support::{alive, key, result_object, wait_for, wait_gone, TestHome};
use vision::paths::TurnId;
use vision::turn::{self, Reason, Refusal, TurnKind, TurnStatus, ViewFile};

const FAR_FUTURE: i64 = 4102444800; // 2100-01-01T00:00:00Z

fn pid_of(event: &Value, field: &str) -> u32 {
    event[field].as_u64().expect("a pid") as u32
}

#[test]
fn a_reader_turn_runs_from_launch_to_a_collected_row() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "reader",
        json!([[
            {"do": "init"},
            {"do": "text", "text": "I read the issue."},
            {"do": "tool", "name": "Read", "input": {"file_path": "notes.txt"}},
            {"do": "partial", "text": "a line in two parts"},
            {"do": "rate_limit", "window": "five_hour", "utilization": 0.25, "resetsAt": FAR_FUTURE},
            {"do": "rate_limit", "window": "seven_day", "utilization": 0.5, "resetsAt": FAR_FUTURE},
            {"do": "result", "structured_output": result_object("Refined."), "cost": 0.12,
             "usage": {"input_tokens": 10, "cache_creation_input_tokens": 20,
                       "cache_read_input_tokens": 30, "output_tokens": 40}, "denials": 1}
        ]]),
    );
    let issue = home.scenarios.join("issue.json");
    std::fs::write(&issue, r#"{"title": "Export the report"}"#).unwrap();
    let mut request = home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario);
    request.expects = vec!["doc:refine.md".into()];
    request.view = vec![ViewFile {
        from: issue,
        to: "turns/{turn}/issue.json".into(),
    }];
    request.facts["ticket"] = json!({"issue": ".vision/turns/{turn}/issue.json"});
    let launched = home.launch(&request);
    let id = launched.id.to_string();
    assert!(id.ends_with("-DEMO-12-refine-1"), "{id}");

    let ledger = home.ledger();
    let row = ledger
        .turn(&id)
        .unwrap()
        .expect("the row is written at launch");
    assert_eq!(row.agent.as_deref(), Some("scout"));
    assert_eq!(row.kind.as_deref(), Some("reader"));
    assert_eq!(row.model.as_deref(), Some("claude-fable-5-2"));
    assert_eq!(row.effort.as_deref(), Some("high"));
    assert_eq!(row.session.as_deref(), Some(launched.session.as_str()));
    assert_eq!(row.role_hash.as_deref().map(str::len), Some(64));

    let state = home.wait_ended(&launched);
    assert_eq!(state.reason, Some(Reason::Result), "{state:?}");
    assert_eq!(state.exit_code, Some(0));

    let view = home.work.join(".vision/turns").join(&id);
    let facts: Value =
        serde_json::from_str(&std::fs::read_to_string(view.join("facts.json")).unwrap()).unwrap();
    assert_eq!(facts["turn"], id);
    assert_eq!(facts["ticket"]["key"], "DEMO-12");
    assert_eq!(
        facts["ticket"]["issue"],
        format!(".vision/turns/{id}/issue.json")
    );
    assert_eq!(facts["expects"], json!(["doc:refine.md"]));
    assert_eq!(
        home.turn_file(&launched, "facts.json"),
        std::fs::read_to_string(view.join("facts.json")).unwrap()
    );
    assert!(view.join("issue.json").is_file());

    let collected = home.collect();
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].status, TurnStatus::Done);
    let row = ledger.turn(&id).unwrap().unwrap();
    assert_eq!(row.status.as_deref(), Some("done"));
    assert_eq!(row.reason.as_deref(), Some("result"));
    assert_eq!(row.cost_usd, Some(0.12));
    assert_eq!(
        (row.input, row.cache_creation, row.cache_read, row.output),
        (Some(10), Some(20), Some(30), Some(40))
    );
    assert_eq!(row.denials, Some(1));
    assert_eq!(row.five_hour, Some(0.25));
    assert_eq!(row.seven_day, Some(0.5));
    assert_eq!(
        row.five_hour_resets.as_deref(),
        Some("2100-01-01T00:00:00Z")
    );
    assert!(row.ended.is_some());

    let result: Value = serde_json::from_str(&home.turn_file(&launched, "result.json")).unwrap();
    assert_eq!(result, result_object("Refined."));
    let outcome = turn::outcome(&home.ctx(), &ledger, &launched.id).unwrap();
    assert_eq!(outcome.status, TurnStatus::Done);
    assert_eq!(outcome.usage.output, 40);
    assert_eq!(outcome.result.unwrap()["summary"], "Refined.");

    let assistant = home.events(&launched, "assistant", None);
    assert!(
        assistant
            .iter()
            .any(|e| e["message"]["content"][0]["text"] == "a line in two parts"),
        "a line written in two parts is one line of the stream"
    );
    let kinds: Vec<String> = ledger
        .recent_events(Some("DEMO-12"), 10)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, ["turn-ended", "turn-started"]);
    let init = &home.events(&launched, "system", Some("init"))[0];
    wait_gone(pid_of(init, "fake_pid"));
    assert!(home.collect().is_empty(), "a turn is collected once");
}

#[test]
fn a_worker_turn_gets_the_sandbox_and_the_worker_git_configuration() {
    let home = TestHome::new();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&home.work)
        .status()
        .unwrap();
    assert!(status.success());
    home.edit_config(|doc| {
        let mut list = toml_edit::Array::new();
        list.push("/opt/tool chain");
        doc["sandbox"]["allow_read"] = toml_edit::value(list);
        doc["user"] = toml_edit::value("Demo Person");
    });
    let scenario = home.scenario(
        "worker",
        json!([[
            {"do": "init"},
            {"do": "args"},
            {"do": "env", "show": ["GIT_CONFIG_GLOBAL", "CLAUDE_CONFIG_DIR"]},
            {"do": "result", "structured_output": result_object("Built.")}
        ]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "builder", "build", TurnKind::Worker, &scenario));
    let state = home.wait_ended(&launched);
    assert_eq!(state.reason, Some(Reason::Result), "{state:?}");

    let gitconfig = home.vision.join("worker.gitconfig");
    let settings: Value =
        serde_json::from_str(&home.turn_file(&launched, "settings.json")).unwrap();
    assert_eq!(
        settings,
        json!({"sandbox": {"enabled": true, "autoAllowBashIfSandboxed": true,
            "allowUnsandboxedCommands": false, "failIfUnavailable": true,
            "filesystem": {"denyRead": ["~/"], "allowRead": [
                home.work.display().to_string(), "/opt/tool chain", gitconfig.display().to_string()]},
            "network": {"allowedDomains": ["localhost", "127.0.0.1"], "allowLocalBinding": true,
                "allowUnixSockets": []}}})
    );
    let git_text = std::fs::read_to_string(&gitconfig).unwrap();
    assert!(
        git_text.contains("name = \"Demo Person (vision)\""),
        "{git_text}"
    );
    assert!(
        git_text.contains("email = demo-person@noreply.invalid"),
        "{git_text}"
    );

    let env = &home.events(&launched, "system", Some("fake_env"))[0];
    assert_eq!(
        env["values"]["GIT_CONFIG_GLOBAL"],
        gitconfig.display().to_string()
    );
    assert_eq!(
        env["values"]["CLAUDE_CONFIG_DIR"],
        home.vision.join("claude").display().to_string()
    );

    let args = &home.events(&launched, "system", Some("fake_args"))[0];
    let argv: Vec<String> = serde_json::from_value(args["argv"].clone()).unwrap();
    let value_of = |flag: &str| {
        let at = argv
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("{flag} missing: {argv:?}"));
        argv[at + 1].clone()
    };
    assert_eq!(argv[0], "-p");
    assert_eq!(value_of("--model"), "claude-opus-5-5");
    assert_eq!(value_of("--effort"), "xhigh");
    assert_eq!(value_of("--tools"), "Read,Grep,Glob,Task,Edit,Write,Bash");
    assert_eq!(value_of("--permission-mode"), "acceptEdits");
    assert_eq!(value_of("--permission-prompts"), "none");
    assert_eq!(value_of("--max-budget-usd"), "25");
    assert_eq!(value_of("--session-id"), launched.session);
    assert_eq!(
        value_of("--settings"),
        launched.dir.join("settings.json").display().to_string()
    );
    for flag in [
        "--restricted",
        "--strict-mcp-config",
        "--verbose",
        "--replay-user-messages",
    ] {
        assert!(argv.iter().any(|a| a == flag), "{flag} missing: {argv:?}");
    }
    assert!(!argv.iter().any(|a| a == "--resume"));
    let schema: Value = serde_json::from_str(&value_of("--json-schema")).unwrap();
    assert_eq!(schema["required"], json!(["kind", "summary"]));
    let role = std::fs::read_to_string(value_of("--append-system-prompt-file")).unwrap();
    assert!(role.starts_with("# Rules for each work turn"), "{role}");
    assert!(role.contains("\n# Builder\n"), "{role}");
    let cwd = std::fs::canonicalize(args["cwd"].as_str().unwrap()).unwrap();
    assert_eq!(cwd, std::fs::canonicalize(&home.work).unwrap());

    let exclude = std::fs::read_to_string(home.work.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|l| l == "/.vision/"), "{exclude}");
    let porcelain = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&home.work)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&porcelain.stdout),
        "",
        "the view never shows as a change"
    );
}

#[test]
fn a_reader_gets_no_sandbox_and_a_judge_gets_only_its_role_file() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "judge",
        json!([[{"do": "init"}, {"do": "result", "structured_output": {"answers": []}}]]),
    );
    let memory = home.vision.join("memory");
    let mut request = home.request("DEMO-12", "judge", "judge", TurnKind::Reader, &scenario);
    request.add_dirs = vec![memory.clone()];
    let launched = home.launch(&request);
    home.wait_ended(&launched);
    let settings: Value =
        serde_json::from_str(&home.turn_file(&launched, "settings.json")).unwrap();
    assert!(settings.get("sandbox").is_none(), "{settings}");
    assert_eq!(settings["permissions"]["deny"][0], "Bash");
    let role = home.turn_file(&launched, "role.md");
    assert!(role.starts_with("# Judge"), "{role}");
    let plan: Value = serde_json::from_str(&home.turn_file(&launched, "launch.json")).unwrap();
    let args: Vec<String> = serde_json::from_value(plan["plan"]["args"].clone()).unwrap();
    let schema_at = args.iter().position(|a| a == "--json-schema").unwrap();
    assert!(args[schema_at + 1].contains("\"answers\""));
    let add_at = args.iter().position(|a| a == "--add-dir").unwrap();
    assert_eq!(args[add_at + 1], memory.display().to_string());
    assert_eq!(plan["plan"]["contract"]["permission_mode"], "dontAsk");
    home.collect();
    let outcome = turn::outcome(&home.ctx(), &home.ledger(), &launched.id).unwrap();
    assert_eq!(
        outcome.status,
        TurnStatus::Done,
        "the Judge result fits judge.schema.json"
    );
}

#[test]
fn a_contract_mismatch_kills_the_group() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "contract",
        json!([[
            {"do": "spawn_child"},
            {"do": "init", "tools": ["Read", "Grep", "Glob", "Task", "Bash"]},
            {"do": "hang"}
        ]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    let state = home.wait_ended(&launched);
    assert_eq!(state.reason, Some(Reason::Contract), "{state:?}");
    assert!(
        state
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("extra: [Bash]"),
        "{state:?}"
    );
    let child = &home.events(&launched, "system", Some("fake_child"))[0];
    let init = &home.events(&launched, "system", Some("init"))[0];
    wait_gone(pid_of(child, "pid"));
    wait_gone(pid_of(init, "fake_pid"));
    let collected = home.collect();
    assert_eq!(
        collected[0].status,
        TurnStatus::Failed,
        "a contract mismatch is not resumed"
    );
    assert_eq!(collected[0].retry, None);
}

#[test]
fn the_time_limit_kills_the_whole_group() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "hang",
        json!([[{"do": "init"}, {"do": "spawn_child"}, {"do": "hang"}]]),
    );
    let mut request = home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario);
    request.time_limit_secs = Some(2);
    let started = Instant::now();
    let launched = home.launch(&request);
    let state = home.wait_ended(&launched);
    assert_eq!(state.reason, Some(Reason::Timeout), "{state:?}");
    assert!(started.elapsed() >= Duration::from_secs(2));
    let child = &home.events(&launched, "system", Some("fake_child"))[0];
    let init = &home.events(&launched, "system", Some("init"))[0];
    wait_gone(pid_of(child, "pid"));
    wait_gone(pid_of(init, "fake_pid"));
    let collected = home.collect();
    assert_eq!(collected[0].status, TurnStatus::Failed);
    assert_eq!(collected[0].reason, Reason::Timeout);
}

#[test]
fn a_turn_with_questions_is_resumed_in_the_same_session() {
    let home = TestHome::new();
    let card = json!({"text": "CSV or XLSX?", "context": "The export button.", "evidence": "src/export.rs",
        "kind": "product", "options": [{"label": "CSV", "means": "plain"}, {"label": "XLSX", "means": "sheet"}]});
    let scenario = home.scenario(
        "questions",
        json!([
            [{"do": "init"}, {"do": "result", "structured_output": {"kind": "questions", "summary": "One card.", "questions": [card]}}],
            [{"do": "init"}, {"do": "args"}, {"do": "result", "structured_output": result_object("Planned.")}]
        ]),
    );
    let first =
        home.launch(&home.request("DEMO-12", "builder", "plan", TurnKind::Reader, &scenario));
    home.wait_ended(&first);
    home.collect();
    let ctx = home.ctx();
    let ledger = home.ledger();
    let outcome = turn::outcome(&ctx, &ledger, &first.id).unwrap();
    assert_eq!(outcome.status, TurnStatus::Done);
    assert_eq!(outcome.result.as_ref().unwrap()["kind"], "questions");

    let second = turn::resume(&ctx, &ledger, &first.id, "Answers: CSV, from the person").unwrap();
    assert_eq!(second.session, first.session);
    assert!(
        second.id.to_string().ends_with("-DEMO-12-plan-2"),
        "{}",
        second.id
    );
    home.wait_ended(&second);
    home.collect();
    let row = ledger.turn(&second.id.to_string()).unwrap().unwrap();
    assert_eq!(row.resumes.as_deref(), Some(first.session.as_str()));
    assert_eq!(row.status.as_deref(), Some("done"));
    let args = &home.events(&second, "system", Some("fake_args"))[0];
    let argv: Vec<String> = serde_json::from_value(args["argv"].clone()).unwrap();
    let at = argv.iter().position(|a| a == "--resume").expect("--resume");
    assert_eq!(argv[at + 1], first.session);
    assert!(!argv.iter().any(|a| a == "--session-id"));
    let init = &home.events(&second, "system", Some("init"))[0];
    assert_eq!(init["session_id"], first.session.as_str());
    let facts: Value = serde_json::from_str(&home.turn_file(&second, "facts.json")).unwrap();
    assert_eq!(facts["message"], "Answers: CSV, from the person");
}

#[test]
fn a_dead_supervisor_is_a_crash_and_its_session_is_stopped_and_resumed() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "crash",
        json!([
            [{"do": "init"}, {"do": "spawn_child"}, {"do": "hang"}],
            [{"do": "init"}, {"do": "result", "structured_output": result_object("Recovered.")}]
        ]),
    );
    let first =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    let child_event = wait_for(Duration::from_secs(20), "the child of the fake", || {
        home.events(&first, "system", Some("fake_child"))
            .into_iter()
            .next()
    });
    let child = pid_of(&child_event, "pid");
    let fake = pid_of(&home.events(&first, "system", Some("init"))[0], "fake_pid");
    let state = home.state("DEMO-12", &first.id.to_string()).unwrap();
    let supervisor = state.pid.expect("the supervisor pid");
    assert!(alive(supervisor));
    // SAFETY: the pid is the supervisor that this test started.
    unsafe {
        libc::kill(supervisor as i32, libc::SIGKILL);
    }
    wait_gone(supervisor);
    assert!(
        alive(fake),
        "the session outlives a killed supervisor until the next tick"
    );

    let collected = home.collect();
    assert_eq!(collected.len(), 1, "{collected:?}");
    assert_eq!(collected[0].reason, Reason::Crash);
    assert_eq!(collected[0].status, TurnStatus::Retried);
    wait_gone(fake);
    wait_gone(child);
    let ledger = home.ledger();
    let retry_id = collected[0].retry.clone().expect("an automatic resume");
    let outcome = turn::outcome(&home.ctx(), &ledger, &first.id).unwrap();
    assert_eq!(outcome.retried_by.as_deref(), Some(retry_id.as_str()));
    assert_eq!(outcome.reason, Some(Reason::Crash));

    let retry = TurnId::parse(&retry_id).unwrap();
    let retry_dir = home.ctx().paths.turn(&key("DEMO-12"), &retry).dir;
    let facts: Value =
        serde_json::from_str(&std::fs::read_to_string(retry_dir.join("facts.json")).unwrap())
            .unwrap();
    assert_eq!(facts["message"], "continue");
    wait_for(Duration::from_secs(20), "the resume to end", || {
        home.state("DEMO-12", &retry_id).filter(|s| s.is_ended())
    });
    let collected = home.collect();
    assert_eq!(collected[0].status, TurnStatus::Done);
    let row = ledger.turn(&retry_id).unwrap().unwrap();
    assert_eq!(row.resumes.as_deref(), Some(first.session.as_str()));
}

#[test]
fn no_valid_result_gets_one_continue_and_a_second_failure_is_final() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "invalid",
        json!([
            [{"do": "init"}, {"do": "result", "structured_output": null}],
            [{"do": "init"}, {"do": "result", "structured_output": {"kind": "done"}}]
        ]),
    );
    let first =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    home.wait_ended(&first);
    let collected = home.collect();
    assert_eq!(collected[0].status, TurnStatus::Retried);
    assert_eq!(collected[0].reason, Reason::Result);
    let retry = collected[0].retry.clone().unwrap();
    wait_for(Duration::from_secs(20), "the resume to end", || {
        home.state("DEMO-12", &retry).filter(|s| s.is_ended())
    });
    let collected = home.collect();
    assert_eq!(collected.len(), 1);
    assert_eq!(
        collected[0].status,
        TurnStatus::Failed,
        "the second failure is final"
    );
    assert_eq!(collected[0].retry, None);
    let ledger = home.ledger();
    assert_eq!(
        ledger.turns_of_ticket("DEMO-12").unwrap().len(),
        2,
        "no third turn"
    );
    let events = ledger.recent_events(Some("DEMO-12"), 1).unwrap();
    let detail: Value = serde_json::from_str(events[0].detail.as_deref().unwrap()).unwrap();
    assert!(
        detail["no_retry"]
            .as_str()
            .unwrap()
            .contains("second failure"),
        "{detail}"
    );
    assert!(
        detail["problems"][0]
            .as_str()
            .unwrap()
            .contains("lacks `summary`"),
        "{detail}"
    );
}

#[test]
fn a_session_that_fails_at_once_is_resumed_once_and_can_recover() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "error",
        json!([
            [{"do": "init"}, {"do": "exit", "code": 3, "stderr": "API error: overloaded"}],
            [{"do": "init"}, {"do": "result", "structured_output": result_object("Recovered.")}]
        ]),
    );
    let first =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    let state = home.wait_ended(&first);
    assert_eq!(state.reason, Some(Reason::Error));
    assert_eq!(state.exit_code, Some(3));
    let detail = state.detail.unwrap();
    assert!(detail.contains("exited with code 3"), "{detail}");
    assert!(detail.contains("API error: overloaded"), "{detail}");
    let collected = home.collect();
    assert_eq!(collected[0].status, TurnStatus::Retried);
    let retry = collected[0].retry.clone().unwrap();
    wait_for(Duration::from_secs(20), "the resume to end", || {
        home.state("DEMO-12", &retry).filter(|s| s.is_ended())
    });
    assert_eq!(home.collect()[0].status, TurnStatus::Done);
}

#[test]
fn the_environment_of_a_session_is_an_allow_list() {
    let home = TestHome::new();
    home.edit_config(|doc| {
        let mut list = toml_edit::Array::new();
        list.push("EXTRA_PASS");
        doc["claude"]["env_pass"] = toml_edit::value(list);
    });
    let scenario = home.scenario(
        "env",
        json!([[{"do": "init"}, {"do": "env", "show": ["EXTRA_PASS"]},
                {"do": "result", "structured_output": result_object("ok")}]]),
    );
    let ctx = home.ctx();
    let ledger = home.ledger();
    let prepared = turn::prepare(
        &ctx,
        &ledger,
        &home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario),
    )
    .unwrap();
    assert_eq!(prepared.supervisor_pid, None);
    // The supervisor runs in the foreground here, with secrets in its own environment.
    let run = home.run_with(
        &["turn-run", &prepared.id.to_string()],
        &[
            ("FORGE_TOKEN", "secret-forge"),
            ("GH_TOKEN", "secret-gh"),
            ("EXTRA_PASS", "passed"),
            ("LC_ALL", "C"),
            ("TERM", "xterm"),
        ],
        None,
    );
    run.expect_code(0);
    let env = &home.events(&prepared, "system", Some("fake_env"))[0];
    let names: Vec<String> = serde_json::from_value(env["names"].clone()).unwrap();
    for present in [
        "PATH",
        "HOME",
        "LC_ALL",
        "TERM",
        "EXTRA_PASS",
        "CLAUDE_CONFIG_DIR",
    ] {
        assert!(
            names.iter().any(|n| n == present),
            "{present} is missing: {names:?}"
        );
    }
    for absent in [
        "FORGE_TOKEN",
        "GH_TOKEN",
        "VISION_HOME",
        "VISION_CLAUDE_BIN",
    ] {
        assert!(
            !names.iter().any(|n| n == absent),
            "{absent} reached the session: {names:?}"
        );
    }
    assert_eq!(env["values"]["EXTRA_PASS"], "passed");
    let again = home.run(&["turn-run", &prepared.id.to_string()]);
    again.expect_code(1);
    assert!(again.stderr.contains("already ended"), "{again:?}");
}

#[test]
fn each_rule_refuses_a_new_turn_and_names_itself() {
    let home = TestHome::new();
    let ctx = home.ctx();
    let ledger = home.ledger();
    let refusal = |ticket: &str, kind: TurnKind| {
        let ctx = home.ctx();
        turn::refusal(&ctx, &ledger, &key(ticket), kind).unwrap()
    };
    assert_eq!(refusal("DEMO-12", TurnKind::Reader), None);

    home.run(&["off"]).expect_code(0);
    let scenario = home.scenario("never", json!([[{"do": "init"}]]));
    let err = turn::launch(
        &ctx,
        &ledger,
        &home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario),
    )
    .unwrap_err();
    let refused = err.downcast_ref::<Refusal>().expect("a refusal");
    assert_eq!(refused.rule(), "off");
    assert!(err.to_string().contains("vision on"), "{err}");
    assert!(
        ledger.turns_of_ticket("DEMO-12").unwrap().is_empty(),
        "a refused turn writes nothing"
    );
    assert!(
        !home.vision.join("tickets/DEMO-12/turns").exists()
            || std::fs::read_dir(home.vision.join("tickets/DEMO-12/turns"))
                .unwrap()
                .next()
                .is_none()
    );
    home.run(&["on"]).expect_code(0);

    home.edit_config(|doc| doc["budget"]["reader"] = toml_edit::value(0));
    assert_eq!(
        refusal("DEMO-12", TurnKind::Reader).unwrap().rule(),
        "budget.reader"
    );
    assert_eq!(refusal("DEMO-12", TurnKind::Worker), None);
    home.edit_config(|doc| doc["budget"]["reader"] = toml_edit::value(3));

    let now = vision::util::time::now_rfc3339();
    let insert = |id: &str, ticket: &str, cost: f64| {
        ledger
            .conn()
            .execute(
                "INSERT INTO turns (id, ticket, started, ended, status, cost_usd) VALUES (?1, ?2, ?3, ?3, 'done', ?4)",
                rusqlite::params![id, ticket, now, cost],
            )
            .unwrap();
    };
    insert("t-ticket", "DEMO-12", 100.0);
    let ticket = refusal("DEMO-12", TurnKind::Reader).unwrap();
    assert_eq!(ticket.rule(), "budget.ticket");
    assert!(ticket.to_string().contains("$100.00 of $100"), "{ticket}");
    assert_eq!(refusal("DEMO-13", TurnKind::Reader), None);
    insert("t-curator", "curator", 150.0);
    assert_eq!(
        refusal("curator", TurnKind::Reader),
        None,
        "a reserved key has no ticket budget"
    );
    insert("t-week", "DEMO-14", 60.0);
    let week = refusal("DEMO-15", TurnKind::Reader).unwrap();
    assert_eq!(week.rule(), "budget.week");
    ledger.conn().execute("DELETE FROM turns", []).unwrap();

    let reading = |id: &str, column: &str, part: f64, resets: &str| {
        let sql = format!(
            "INSERT INTO turns (id, ticket, started, ended, status, {column}, {column}_resets) VALUES (?1, 'DEMO-9', ?2, ?2, 'done', ?3, ?4)"
        );
        ledger
            .conn()
            .execute(&sql, rusqlite::params![id, now, part, resets])
            .unwrap();
    };
    reading("t-five", "five_hour", 0.7, "2100-01-01T00:00:00Z");
    let limit = refusal("DEMO-12", TurnKind::Reader).unwrap();
    assert_eq!(limit.rule(), "limits.five_hour");
    assert!(limit.is_limit());
    assert!(
        limit
            .to_string()
            .contains("at 70%, and new turns stop at 60%"),
        "{limit}"
    );
    assert!(
        limit.to_string().contains("resets at 2100-01-01T00:00:00Z"),
        "{limit}"
    );
    ledger.conn().execute("DELETE FROM turns", []).unwrap();
    reading("t-seven", "seven_day", 0.9, "2100-01-01T00:00:00Z");
    assert_eq!(
        refusal("DEMO-12", TurnKind::Reader).unwrap().rule(),
        "limits.seven_day"
    );
}

#[test]
fn a_limit_reading_after_its_reset_time_does_not_block() {
    let home = TestHome::new();
    let ledger = home.ledger();
    let now = vision::util::time::now_rfc3339();
    ledger
        .conn()
        .execute(
            "INSERT INTO turns (id, ticket, started, ended, status, five_hour, five_hour_resets, seven_day, seven_day_resets)
             VALUES ('t-old', 'DEMO-9', ?1, ?1, 'done', 0.99, '2020-01-01T00:00:00Z', 0.99, '2020-01-01T00:00:00Z')",
            [now],
        )
        .unwrap();
    assert_eq!(
        turn::refusal(&home.ctx(), &ledger, &key("DEMO-12"), TurnKind::Reader).unwrap(),
        None
    );
    let scenario = home.scenario(
        "after-reset",
        json!([[{"do": "init"}, {"do": "result", "structured_output": result_object("ok")}]]),
    );
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    assert_eq!(home.wait_ended(&launched).reason, Some(Reason::Result));
}

#[test]
fn a_missing_role_file_is_named_and_a_home_role_file_wins() {
    let home = TestHome::new();
    let scenario = home.scenario(
        "roles",
        json!([[{"do": "init"}, {"do": "result", "structured_output": result_object("ok")}]]),
    );
    let ctx = home.ctx();
    let ledger = home.ledger();
    let err = turn::launch(
        &ctx,
        &ledger,
        &home.request("DEMO-12", "planner", "plan", TurnKind::Reader, &scenario),
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("agents/planner.md does not exist"),
        "{err:#}"
    );
    assert!(ledger.turns_of_ticket("DEMO-12").unwrap().is_empty());

    let agents = home.vision.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("scout.md"), "# Scout of this home\n").unwrap();
    let launched =
        home.launch(&home.request("DEMO-12", "scout", "refine", TurnKind::Reader, &scenario));
    let role = home.turn_file(&launched, "role.md");
    assert!(role.starts_with("# Rules for each work turn"), "{role}");
    assert!(role.ends_with("# Scout of this home\n"), "{role}");
    home.wait_ended(&launched);
}
