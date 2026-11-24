//! The refused requests of a review (design section 11.3), through `vision tick` with the demo
//! world and the fake `claude`. Two earlier rounds refused a request of `someone` near
//! `test_subtract.py:6`. A new comment at line 7 is the same request, so its third refusal
//! waits for the person: the Shepherd asks one card, the Judge cannot answer it, and the
//! answer of the person goes back to the same session.

mod pipeline_support;

use std::time::Duration;

use pipeline_support::{
    ends_with, result, runs_then, simple_scenarios, take_and_approve, World, KEY,
};
use serde_json::{json, Value};
use vision::pipeline::refusals::{self, Refusals};

const GREEN_BUILD: &str = "printf '\\n\\ndef subtract(a, b):\\n    \"\"\"Gives a minus b.\"\"\"\\n    return a - b\\n' >> calc.py && printf 'import unittest\\n\\nfrom calc import subtract\\n\\n\\nclass SubtractTest(unittest.TestCase):\\n    def test_subtract(self):\\n        self.assertEqual(subtract(5, 3), 2)\\n' > test_subtract.py && git add -A && git commit -q -m 'Add subtract'";

/// The counts of two earlier rounds: both refused the comment 90 of `someone`.
fn two_refusals() -> String {
    json!({
        "rounds": ["T-older-1", "T-older-2"],
        "requests": [{
            "kind": "inline", "ids": [90], "author": "someone",
            "path": "test_subtract.py", "line": 6, "count": 2,
            "request": "Test negative numbers too",
            "reasons": [
                {"turn": "T-older-1", "verdict": "low-priority", "reply": "Won't fix: subtract has no sign logic (net +1)"},
                {"turn": "T-older-2", "verdict": "incorrect", "reply": "Not a bug: test_subtract.py:8 covers it"}
            ],
            "updated": "2026-11-20T10:00:00Z"
        }]
    })
    .to_string()
}

/// The card that the Shepherd asks after the end fact `refusals` failed.
fn card() -> Value {
    json!({
        "text": "The comment 1 asks for a test of negative numbers. Fix it or keep refusing?",
        "context": "1. low-priority: subtract has no sign logic. 2. incorrect: test_subtract.py:8 covers it. Now: the same.",
        "example": "subtract(-1, 1) gives -2.",
        "evidence": "test_subtract.py:7",
        "kind": "code",
        "area": ["review"],
        "topics": [refusals::REFUSED_REVIEW, "comment:1"],
        "options": [
            {"label": refusals::FIX, "means": "the Shepherd adds the test"},
            {"label": refusals::KEEP, "means": "the refusal stands"}
        ],
        "recommended": 1,
        "why": "Two rounds found no sign logic to test."
    })
}

fn refused(reply: &str) -> String {
    json!([{"comment_id": 1, "verdict": "incorrect", "fix_commit": "", "reply": reply}]).to_string()
}

/// A world at the step `ready` with the counts of two refusals, the comment 1 of the third
/// round, and the scenarios of the Shepherd: refuse, then the card, then `last`.
fn third_refusal(last: Value) -> World {
    let world = World::new();
    simple_scenarios(&world);
    world.edit_config(|doc| doc["modes"]["review.reply"] = toml_edit::value("auto"));
    world.scenario(
        "builder",
        "build",
        json!([runs_then(
            GREEN_BUILD,
            result(
                "Built.",
                &[("pr.md", "Add subtract\n\nThe calculator can subtract.")]
            )
        )]),
    );
    world.scenario(
        "reviewer",
        "review",
        json!([ends_with(result(
            "Reviewed.",
            &[("review.md", "# Review\n\n## Findings\n\nNone.\n")]
        ))]),
    );
    world.scenario(
        "shepherd",
        "comments",
        json!([
            ends_with(result(
                "Refused the comment.",
                &[(
                    "verdicts.json",
                    &refused("Not a bug: the sign does not matter")
                )]
            )),
            ends_with(json!({"kind": "questions", "summary": "One card.", "questions": [card()]})),
            last
        ]),
    );
    // A sure answer of the Judge: the topic sends the card to the person anyway.
    world.scenario(
        "judge",
        "",
        json!([[{"do": "init"}, {"do": "result", "structured_output": {"answers": [
            {"card": "Q-0001", "outcome": "answer", "basis": "inference", "sources": [],
             "sure": true, "choice": 1, "topics": [], "reason": "The reasons hold."}
        ]}, "cost": 0.05}]]),
    );
    take_and_approve(&world);
    world.tick_to("ready");
    std::fs::write(
        world
            .vision
            .join("tickets")
            .join(KEY)
            .join(refusals::REFUSALS),
        two_refusals(),
    )
    .unwrap();
    world.world_cmd(&["ready", world.root(), "1"]);
    world.world_cmd(&[
        "comment",
        world.root(),
        "1",
        "Test negative numbers too",
        "--author",
        "someone",
        "--path",
        "test_subtract.py",
        "--line",
        "7",
    ]);
    world.tick_until("the card for the person", Duration::from_secs(90), |w| {
        let status = w.run(&["status", "--json"]);
        status.expect_code(0);
        status.json()["needs"]
            .as_array()
            .is_some_and(|n| n.iter().any(|n| n["kind"] == "card"))
    });
    world
}

/// The end facts of the turns of the step `comments`, oldest first.
fn comment_facts(world: &World) -> Vec<Option<String>> {
    world
        .turns()
        .into_iter()
        .filter(|t| t.0 == "comments")
        .map(|t| t.3)
        .collect()
}

fn counts(world: &World) -> Refusals {
    refusals::parse(world.doc(refusals::REFUSALS).as_deref())
}

/// The third refusal stops the round, the card goes to the person, and the count stays at 2
/// while the person decides.
fn assert_waits_for_the_person(world: &World) {
    let facts = comment_facts(world);
    assert_eq!(facts[0].as_deref(), Some("failed:refusals"), "{facts:?}");
    let found: Vec<String> = world
        .ledger()
        .events_of_ticket(KEY)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "end-fact")
        .filter_map(|e| e.detail)
        .filter(|d| d.contains("\"refusals\""))
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    for part in [
        "The comment 1 (someone, test_subtract.py:7)",
        "refuse for the 3rd time",
        "1. low-priority: Won't fix: subtract has no sign logic (net +1)",
        "2. incorrect: Not a bug: test_subtract.py:8 covers it",
        "[\\\"refused-review\\\", \\\"comment:1\\\"]",
    ] {
        assert!(found[0].contains(part), "no {part:?} in {}", found[0]);
    }
    let row = world
        .ledger()
        .question("Q-0001")
        .unwrap()
        .expect("the card");
    assert_eq!(row.outcome.as_deref(), Some("ask"), "{row:?}");
    assert_eq!(
        counts(world).requests[0].count,
        2,
        "a stopped round counts nothing"
    );
    assert_eq!(world.step().as_deref(), Some("comments"));
}

#[test]
fn the_third_refusal_asks_the_person_and_keep_starts_the_count_again() {
    let world = third_refusal(ends_with(result(
        "Kept the refusal.",
        &[(
            "verdicts.json",
            &refused("Not a bug: the person kept the refusal"),
        )],
    )));
    assert_waits_for_the_person(&world);

    world
        .run(&["answer", "Q-0001", "--choice", "2"])
        .expect_code(0);
    world.tick_to("ready-to-merge");
    let facts = comment_facts(&world);
    assert_eq!(facts.last().unwrap().as_deref(), Some("held"), "{facts:?}");
    let after = counts(&world);
    assert_eq!(after.requests.len(), 1, "{after:?}");
    let track = &after.requests[0];
    assert_eq!(track.count, 0, "the decision Keep starts the count again");
    assert!(track.reasons.is_empty(), "{track:?}");
    assert_eq!(track.ids, [90, 1], "the comment 1 joins the same request");
    assert_eq!(track.line, Some(7));
    // The reply of the kept refusal went to the forge.
    let prs = world.world_json("prs.json");
    let replies = prs["prs"][0]["comments"][0]["replies"].to_string();
    assert!(replies.contains("the person kept the refusal"), "{replies}");
}

#[test]
fn the_decision_fix_makes_the_shepherd_fix_the_request_and_ends_its_count() {
    let world = third_refusal(runs_then(
        "printf '\\n    def test_negative(self):\\n        self.assertEqual(subtract(-1, 1), -2)\\n' >> test_subtract.py && git commit -q -am 'Test negative numbers'",
        result(
            "Fixed the comment.",
            &[(
                "verdicts.json",
                r#"[{"comment_id": 1, "verdict": "valid", "fix_commit": "HEAD", "reply": "Added a test for negative numbers."}]"#,
            )],
        ),
    ));
    assert_waits_for_the_person(&world);

    world
        .run(&["answer", "Q-0001", "--choice", "1"])
        .expect_code(0);
    world.tick_to("ready-to-merge");
    let facts = comment_facts(&world);
    assert_eq!(facts.last().unwrap().as_deref(), Some("held"), "{facts:?}");
    assert!(
        counts(&world).requests.is_empty(),
        "a fix ends the count: {:?}",
        counts(&world)
    );
    let test = std::fs::read_to_string(world.clone().join("test_subtract.py")).unwrap();
    assert!(test.contains("test_negative"), "{test}");
}
