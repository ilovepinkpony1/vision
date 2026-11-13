//! The journal on the demo path (section 16): a turn of the pipeline leaves its entry, and the
//! next work turn gets `related.md` and can read the journal folder, never write it. The turns
//! play the fake `claude`.

mod pipeline_support;

use std::time::Duration;

use pipeline_support::*;
use serde_json::{json, Value};

/// The build of DEMO-12 with a test that fails, a commit, and the same test that passes.
fn build_with_a_fix() -> Value {
    json!([[
        {"do": "init"},
        {"do": "run", "command": "test -f ready.txt", "check": false},
        {"do": "run", "command": "touch ready.txt && git add ready.txt && git commit -m 'Mark ready'"},
        {"do": "run", "command": "test -f ready.txt"},
        {"do": "run", "command": BUILD_COMMAND},
        {"do": "result", "cost": 0.4, "structured_output": {
            "kind": "result", "summary": "Built subtract.",
            "documents": [{"name": "pr.md", "content": "Add subtract\n\nThe calculator can subtract."}],
            "notes": [
                {"kind": "dead-end", "text": "test -f ready.txt fails before the mark exists."},
                {"kind": "fact", "text": "calc.py holds each function of the calculator.", "files": ["calc.py"]}
            ]}}
    ]])
}

#[test]
fn a_turn_of_the_path_leaves_an_entry_and_the_next_turn_gets_related_entries() {
    let world = World::new();
    simple_scenarios(&world);
    world.scenario("builder", "build", build_with_a_fix());
    world.scenario(
        "reviewer",
        "review",
        json!([[{"do": "init"}, {"do": "args"},
            {"do": "result", "structured_output": {"kind": "result", "summary": "Reviewed."}}]]),
    );
    take_and_approve(&world);
    world.tick_to("review");

    let build = world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .find(|t| t.step.as_deref() == Some("build"))
        .unwrap();
    let entry = world.vision.join(format!("journal/{KEY}/{}.md", build.id));
    let text = std::fs::read_to_string(&entry).unwrap();
    assert!(text.contains("\nstep: build\nagent: builder\n"), "{text}");
    assert!(text.contains("\noutcome: held\n"), "{text}");
    let mark = git(world.clone(), &["log", "--format=%h", "--grep=Mark ready"]);
    assert!(
        text.contains(&format!("\n## Errors and fixes\n\n- `test -f ready.txt` failed (exit 1). Passed later, after the commit {mark}.\n")),
        "{text}"
    );
    assert!(text.contains("\n## Notes\n\n- dead-end: test -f ready.txt fails before the mark exists.\n- fact: calc.py holds each function of the calculator. (files: calc.py)\n"), "{text}");
    assert!(text.contains("\n- end facts: held\n"), "{text}");
    assert!(
        text.contains("\n- next: build -> review (next)\n"),
        "{text}"
    );
    assert!(text.contains("calc.py"), "the files of the diff: {text}");
    // The entries of the reader turns are there too.
    let refine = world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .find(|t| t.step.as_deref() == Some("refine"))
        .unwrap();
    assert!(world
        .vision
        .join(format!("journal/{KEY}/{}.md", refine.id))
        .is_file());

    // The next work turn: related.md with the build entry, and the journal as a folder that
    // it reads and never writes.
    world.tick_until("the review turn", Duration::from_secs(60), |w| {
        w.turns_of("review") > 0
    });
    world.settle();
    let review = world
        .ledger()
        .turns_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .find(|t| t.step.as_deref() == Some("review"))
        .unwrap();
    let turn_dir = world
        .vision
        .join(format!("tickets/{KEY}/turns/{}", review.id));
    let facts: Value =
        serde_json::from_str(&std::fs::read_to_string(turn_dir.join("facts.json")).unwrap())
            .unwrap();
    let journal = std::fs::canonicalize(world.vision.join("journal"))
        .unwrap()
        .display()
        .to_string();
    assert_eq!(facts["journal"], json!(journal));
    assert_eq!(
        facts["related"],
        json!(format!(".vision/turns/{}/related.md", review.id))
    );
    let related = std::fs::read_to_string(
        world
            .clone()
            .join(format!(".vision/turns/{}/related.md", review.id)),
    )
    .unwrap();
    assert!(
        related.starts_with("# Related journal entries\n"),
        "{related}"
    );
    assert!(
        related.contains(&format!("\n## {} ({KEY}, step build)\n", build.id)),
        "{related}"
    );
    assert!(
        related.contains("- dead-end: test -f ready.txt fails before the mark exists."),
        "{related}"
    );
    assert!(related.contains("### Errors and fixes"), "{related}");

    let launch: Value =
        serde_json::from_str(&std::fs::read_to_string(turn_dir.join("launch.json")).unwrap())
            .unwrap();
    let args: Vec<String> = serde_json::from_value(launch["plan"]["args"].clone()).unwrap();
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--add-dir" && w[1] == journal),
        "{args:?}"
    );
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(turn_dir.join("settings.json")).unwrap())
            .unwrap();
    let deny: Vec<String> =
        serde_json::from_value(settings["permissions"]["deny"].clone()).unwrap();
    assert!(deny.contains(&format!("Edit(/{journal}/**)")), "{deny:?}");
    assert!(deny.contains(&format!("Write(/{journal}/**)")), "{deny:?}");
    assert_eq!(
        settings["sandbox"]["filesystem"]["denyWrite"],
        json!([journal.clone()])
    );
}
