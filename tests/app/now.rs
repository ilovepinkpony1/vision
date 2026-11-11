//! The state function of the app: one case for each state of a ticket and each kind of need,
//! from the facts to the kind, the tone, the short line of Home, and the lines of the panel
//! NOW. No text names a command or a word of the code.

use vision::app::now::{now, Input, Kind, Pass, Tone};
use vision::executor::PrFacts;
use vision::pipeline::facts::{
    Checks, EntryFact, EntryState, LimitFact, StateFacts, TurnFact, WaitFacts,
};
use vision::pipeline::view::FlagView;
use vision::running::Running;
use vision::snapshot::{Need, NeedKind};

use super::support::*;

/// One case: a name, the facts, and what the function must give.
struct Case {
    name: &'static str,
    view: vision::pipeline::TicketView,
    facts: StateFacts,
    needs: Vec<Need>,
    running: Option<Running>,
    pass: Pass,
    kind: Kind,
    tone: Tone,
    short: &'static str,
    title: &'static str,
    lines: &'static [&'static str],
    /// True when the next pass acts on the ticket at once.
    ready: bool,
}

fn base(kind: &str, step: &str) -> (vision::pipeline::TicketView, StateFacts) {
    let mut v = view("DEMO-12", "Export the report", step, "");
    v.kind = Some(kind.into());
    let facts = StateFacts {
        key: "DEMO-12".into(),
        ..StateFacts::default()
    };
    (v, facts)
}

fn flag(name: &str, reason: &str) -> FlagView {
    FlagView {
        flag: name.into(),
        reason: reason.into(),
        since: None,
        command: String::new(),
    }
}

fn need(kind: NeedKind, id: &str, text: &str) -> Need {
    Need {
        kind,
        ticket: Some("DEMO-12".into()),
        id: id.into(),
        text: text.into(),
    }
}

fn case(name: &'static str, view: vision::pipeline::TicketView, facts: StateFacts) -> Case {
    Case {
        name,
        view,
        facts,
        needs: Vec::new(),
        running: None,
        pass: Pass::Idle,
        kind: Kind::Next,
        tone: Tone::Wait,
        short: "",
        title: "",
        lines: &[],
        ready: false,
    }
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();

    let (v, mut f) = base("turn", "build");
    f.work = Some(TurnFact {
        id: "t".into(),
        agent: "builder".into(),
        step: "build".into(),
        model: Some("claude-opus-5-5".into()),
        effort: Some("xhigh".into()),
        started: Some(minutes_ago(6)),
        n: 3,
    });
    out.push(Case {
        kind: Kind::Agent,
        tone: Tone::Work,
        short: "builder works · 6 min",
        title: "builder works",
        lines: &["step build · turn 3 of this ticket"],
        ..case("an agent works", v, f)
    });

    let (v, mut f) = base("turn", "plan");
    f.judge = Some(TurnFact {
        id: "j".into(),
        agent: "judge".into(),
        step: "plan".into(),
        started: Some(minutes_ago(1)),
        ..TurnFact::default()
    });
    f.judging_cards = 2;
    out.push(Case {
        kind: Kind::Judge,
        tone: Tone::Work,
        short: "the Judge reads 2 questions",
        title: "the Judge reads 2 questions",
        lines: &[
            "It answers from the memory when it is sure.",
            "It sends each other question to you.",
        ],
        ..case("the Judge reads", v, f)
    });

    let (v, f) = base("turn", "build");
    out.push(Case {
        running: Some(Running {
            what: "job".into(),
            name: "proof".into(),
            ticket: "DEMO-12".into(),
            started: minutes_ago(2),
            pid: 1,
        }),
        kind: Kind::Runs,
        tone: Tone::Work,
        short: "the job proof runs · 2:00",
        title: "the job proof runs",
        lines: &["A pass of the engine runs it now. No agent works on the ticket in this time."],
        ..case("a job runs inside a pass", v, f)
    });

    let (mut v, f) = base("turn", "plan");
    v.needs.insert("card".into(), 1);
    out.push(Case {
        needs: vec![need(
            NeedKind::Card,
            "Q-0002",
            "builder asks: Must the export keep the order of the columns?",
        )],
        kind: Kind::Needs,
        tone: Tone::Need,
        short: "needs you: 1 question",
        title: "1 question for you",
        lines: &[
            "builder asks: Must the export keep the order of the columns?",
            "The Judge could not answer it from the memory.",
        ],
        ..case("a question waits", v, f)
    });

    let (mut v, f) = base("human", "plan-ready");
    v.needs.insert("plan".into(), 1);
    out.push(Case {
        needs: vec![need(
            NeedKind::Plan,
            "3f2a1b4c5d6e",
            "the plan of DEMO-12 waits",
        )],
        kind: Kind::Needs,
        tone: Tone::Need,
        short: "needs you: the plan",
        title: "the plan waits for you",
        lines: &[
            "Read the plan, then approve it, so that the build can start.",
            "Press n to read it and approve it.",
        ],
        ..case("the plan waits", v, f)
    });

    let (mut v, f) = base("turn", "comments");
    v.needs.insert("operation".into(), 1);
    out.push(Case {
        needs: vec![need(
            NeedKind::Operation,
            "review.reply 1a2b3c4d5e6f",
            "review.reply at 1a2b3c4d5e6f waits for your approval",
        )],
        kind: Kind::Needs,
        tone: Tone::Need,
        short: "needs you: 1 write",
        title: "the write review.reply waits for you",
        lines: &[
            "It goes to the tracker or the forge only after your approval.",
            "review.reply at 1a2b3c4d5e6f waits for your approval",
        ],
        ..case("a write waits", v, f)
    });

    let (mut v, f) = base("turn", "build");
    v.flags
        .push(flag("needs-you", "the proof did not pass at 1a2b3c4d5e6f"));
    out.push(Case {
        kind: Kind::Needs,
        tone: Tone::Need,
        short: "needs you: the proof did not pass at 1a2b3c4d5e6f",
        title: "a rule stopped the ticket",
        lines: &[
            "the proof did not pass at 1a2b3c4d5e6f",
            "Fix the cause, then press r to start the step build again.",
        ],
        ..case("a rule stopped it", v, f)
    });

    let (mut v, mut f) = base("turn", "describe");
    v.flags.push(flag(
        "needs-you",
        "tracker.describe is in the mode shadow: nothing was sent",
    ));
    f.first_write = Some("tracker.describe".into());
    out.push(Case {
        kind: Kind::Needs,
        tone: Tone::Need,
        short: "needs you: the first write tracker.describe",
        title: "the first write tracker.describe waits for you",
        lines: &[
            "The mode of tracker.describe kept it back, so nothing was sent.",
            "Press n to read the write, and decide.",
        ],
        ..case("the first write waits", v, f)
    });

    let (mut v, f) = base("turn", "build");
    v.flags.push(flag("needs-you", "a budget"));
    out.push(Case {
        needs: vec![need(NeedKind::Card, "Q-1", "builder asks: Which one?")],
        kind: Kind::Needs,
        tone: Tone::Need,
        short: "needs you: 1 question, a budget",
        title: "2 things wait for you",
        lines: &["1 question, a budget", "Press n to see each one."],
        ..case("two needs", v, f)
    });

    let (mut v, mut f) = base("wait", "ci");
    v.pr = Some(7);
    f.wait = Some(WaitFacts {
        watch: Some("ci.state".into()),
        value: Some("none".into()),
        observed: Some(minutes_ago(3)),
        checks: Checks {
            passed: 3,
            running: 2,
            failed: 0,
        },
        ..WaitFacts::default()
    });
    out.push(Case {
        kind: Kind::Waits,
        tone: Tone::Wait,
        short: "waits for CI · 3 of 5 checks passed, 2 run, 0 failed",
        title: "waits for CI",
        lines: &[
            "The last commit: 3 of 5 checks passed, 2 run, 0 failed.",
            "Vision looked 3 min ago.",
        ],
        ..case("CI runs", v, f)
    });

    let (mut v, mut f) = base("wait", "ready");
    v.pr = Some(7);
    f.wait = Some(WaitFacts {
        watch: Some("pr.draft".into()),
        value: Some("true".into()),
        observed: Some(minutes_ago(0)),
        ..WaitFacts::default()
    });
    out.push(Case {
        kind: Kind::Waits,
        tone: Tone::Wait,
        short: "waits until the pull request #7 is ready for review",
        title: "waits for the pull request #7",
        lines: &[
            "the pull request #7 is a draft until somebody marks it ready for review.",
            "Vision looks again each minute.",
        ],
        ..case("a draft", v, f)
    });

    let (mut v, mut f) = base("wait", "in-review");
    v.pr = Some(7);
    f.wait = Some(WaitFacts {
        watch: Some("review.state".into()),
        value: Some("waiting".into()),
        observed: Some(minutes_ago(0)),
        pr: Some(PrFacts {
            number: 7,
            approved_by: vec!["a-reviewer".into()],
            comments_waiting: 2,
            ..PrFacts::default()
        }),
        ..WaitFacts::default()
    });
    out.push(Case {
        kind: Kind::Waits,
        tone: Tone::Wait,
        short: "waits for the review of the pull request #7",
        title: "waits for the review of the pull request #7",
        lines: &[
            "Approved by a-reviewer.",
            "2 comments wait for an answer.",
            "Vision looks again each minute.",
        ],
        ..case("a review", v, f)
    });

    let (v, mut f) = base("wait", "deployed");
    f.wait = Some(WaitFacts {
        watch: Some("envs.staging.holds".into()),
        value: Some("false".into()),
        ..WaitFacts::default()
    });
    out.push(Case {
        kind: Kind::Waits,
        tone: Tone::Wait,
        short: "waits for the env staging",
        title: "waits for the env staging",
        lines: &[
            "The value of envs.staging.holds is false now.",
            "Vision looks at the next pass.",
        ],
        ..case("an env", v, f)
    });

    let (v, mut f) = base("human", "plan-ready");
    f.action = Some("approve-plan".into());
    out.push(Case {
        kind: Kind::Waits,
        tone: Tone::Wait,
        short: "waits for your approval of the plan",
        title: "waits for your approval of the plan",
        lines: &["The step plan-ready waits for it. Nothing for it is ready yet."],
        ..case("a human step with no plan yet", v, f)
    });

    let (v, mut f) = base("turn", "build");
    f.uncertain = Some("git.push".into());
    out.push(Case {
        kind: Kind::Waits,
        tone: Tone::Wait,
        short: "waits for the check of git.push",
        title: "waits for the check of git.push",
        lines: &[
            "Nobody knows yet whether git.push reached the outside.",
            "Each pass asks again. Nothing else runs for the ticket until then.",
        ],
        ..case("an uncertain write", v, f)
    });

    let (v, mut f) = base("turn", "refine");
    f.agent = Some("scout".into());
    out.push(Case {
        pass: Pass::Runs,
        kind: Kind::Next,
        tone: Tone::Wait,
        short: "scout starts at the next pass",
        title: "scout starts at the next pass",
        lines: &["A pass runs now.", "No agent works, so this costs nothing."],
        ready: true,
        ..case("the next turn", v, f)
    });

    let (v, mut f) = base("turn", "review");
    f.agent = Some("reviewer".into());
    f.entry = Some(EntryFact {
        label: "job:second-review".into(),
        before: true,
        state: EntryState::Next,
    });
    out.push(Case {
        pass: Pass::Waits,
        kind: Kind::Next,
        tone: Tone::Wait,
        short: "the job second-review runs at the next pass, before reviewer starts",
        title: "the job second-review runs at the next pass, before reviewer starts",
        lines: &[
            "A pass waits for the job of the engine that runs now.",
            "No agent works, so this costs nothing.",
        ],
        ready: true,
        ..case("a job before a turn", v, f)
    });

    let (v, mut f) = base("turn", "build");
    f.agent = Some("builder".into());
    f.entry = Some(EntryFact {
        label: "git.push".into(),
        before: false,
        state: EntryState::Runs,
    });
    out.push(Case {
        pass: Pass::Paused,
        kind: Kind::Next,
        tone: Tone::Wait,
        short: "the write git.push runs at the next pass",
        title: "the write git.push runs at the next pass",
        lines: &[
            "The engine is paused. Press p on Home to start it again.",
            "No agent works, so this costs nothing.",
        ],
        ready: true,
        ..case("a write after a turn", v, f)
    });

    let (v, mut f) = base("turn", "build");
    f.agent = Some("builder".into());
    f.testing = true;
    out.push(Case {
        pass: Pass::Off,
        kind: Kind::Next,
        tone: Tone::Wait,
        short: "the next pass tests the work of builder",
        title: "the next pass tests the work of builder",
        lines: &[
            "Vision is off, so nothing starts. Press K on Home to turn it on.",
            "No agent works, so this costs nothing.",
        ],
        ready: true,
        ..case("the end facts of a turn", v, f)
    });

    let (v, mut f) = base("turn", "build");
    f.agent = Some("builder".into());
    f.held = true;
    out.push(Case {
        kind: Kind::Next,
        tone: Tone::Wait,
        short: "the next pass moves it to its next step",
        title: "the next pass moves it to its next step",
        lines: &[
            "The next pass starts within a minute.",
            "No agent works, so this costs nothing.",
        ],
        ready: true,
        ..case("a turn that held, with each write after it done", v, f)
    });

    let (v, mut f) = base("turn", "build");
    f.agent = Some("builder".into());
    f.held = true;
    f.entry = Some(EntryFact {
        label: "git.push".into(),
        before: false,
        state: EntryState::Waits,
    });
    out.push(Case {
        kind: Kind::Next,
        tone: Tone::Wait,
        short: "the write git.push waits for your approval",
        title: "the write git.push waits for your approval",
        lines: &[
            "The next pass starts within a minute.",
            "No agent works, so this costs nothing.",
        ],
        ..case("a write that waits is not ready", v, f)
    });

    // The mode shadow kept the write back, and no pass set the flag yet: the next pass does.
    let (v, mut f) = base("turn", "refine");
    f.agent = Some("scout".into());
    f.held = true;
    f.entry = Some(EntryFact {
        label: "tracker.describe".into(),
        before: false,
        state: EntryState::Other("shadowed".into()),
    });
    out.push(Case {
        kind: Kind::Next,
        tone: Tone::Wait,
        short: "the write tracker.describe is shadowed",
        title: "the write tracker.describe is shadowed",
        lines: &[
            "The next pass starts within a minute.",
            "No agent works, so this costs nothing.",
        ],
        ready: true,
        ..case("a write that a mode kept back is ready for its flag", v, f)
    });

    // The last turn asked questions, and no turn of the Judge runs: its work is not tested yet.
    let (v, mut f) = base("turn", "refine");
    f.agent = Some("scout".into());
    f.agents = vec!["scout".into()];
    f.testing = true;
    f.open_cards = 1;
    out.push(Case {
        kind: Kind::Next,
        tone: Tone::Wait,
        short: "the next pass goes on with the questions for the Judge",
        title: "the next pass goes on with the questions for the Judge",
        lines: &[
            "The next pass starts within a minute.",
            "No agent works, so this costs nothing.",
        ],
        ready: true,
        ..case("a turn that asked questions is not tested yet", v, f)
    });

    let (mut v, mut f) = base("turn", "build");
    v.flags.push(flag("waits-for-limit", "refused by limits"));
    f.limit = Some(LimitFact {
        window: "five_hour".into(),
        part: 0.64,
        stop: 0.6,
        resets: Some("2026-11-16T18:00:00Z".into()),
    });
    out.push(Case {
        kind: Kind::Limit,
        tone: Tone::Wait,
        short: "waits for the 5-hour limit · 64% of 60%",
        title: "waits for the 5-hour limit",
        lines: &[
            "The 5-hour window is at 64%, and new turns stop at 60%.",
            "It resets at 18:00.",
        ],
        ..case("a limit", v, f)
    });

    let (mut v, f) = base("turn", "review");
    v.flags.push(flag("stopped", "you stopped it"));
    out.push(Case {
        kind: Kind::Stopped,
        tone: Tone::Rest,
        short: "stopped · Enter starts it again",
        title: "stopped",
        lines: &[
            "You stopped it at the step review. Its branch stays in its clone.",
            "Press Enter to start it again.",
        ],
        ..case("stopped", v, f)
    });

    let (mut v, f) = base("turn", "build");
    v.flags.push(flag("paused", "you took over"));
    out.push(Case {
        needs: vec![need(
            NeedKind::Flag,
            "paused",
            "paused at the step build: x",
        )],
        kind: Kind::Paused,
        tone: Tone::Need,
        short: "paused · you took over its session",
        title: "you took over its session",
        lines: &[
            "No new turn starts while you work in the session.",
            "Press r to continue. The next turn resumes it.",
        ],
        ..case("paused", v, f)
    });

    let (v, mut f) = base("end", "done");
    f.finished = true;
    f.merged_pr = Some(7);
    out.push(Case {
        kind: Kind::Done,
        tone: Tone::Done,
        short: "done · pull request #7 merged",
        title: "done",
        lines: &[
            "The pull request #7 is merged.",
            "Nothing runs for it any more.",
        ],
        ..case("done", v, f)
    });
    out
}

#[test]
fn each_state_and_each_need_has_its_own_words() {
    let wall = clock(0.0).wall;
    for c in cases() {
        let got = now(&Input {
            view: &c.view,
            facts: &c.facts,
            needs: &c.needs,
            running: c.running.as_ref(),
            pass: c.pass,
            wall,
            offset_seconds: 0,
        });
        assert_eq!(got.kind, c.kind, "{}", c.name);
        assert_eq!(got.tone, c.tone, "{}", c.name);
        assert_eq!(got.short, c.short, "{}", c.name);
        assert_eq!(got.title, c.title, "{}", c.name);
        assert_eq!(got.lines, c.lines, "{}", c.name);
        assert_eq!(got.ready, c.ready, "{}", c.name);
        for text in std::iter::once(&got.short).chain(&got.lines) {
            assert!(!text.contains("vision "), "{}: {text}", c.name);
            assert!(!text.contains('`'), "{}: {text}", c.name);
            assert!(!text.contains("needs-you"), "{}: {text}", c.name);
            assert!(!text.contains("n't"), "{}: {text}", c.name);
        }
    }
}

#[test]
fn the_bar_needs_you_uses_the_same_words() {
    use vision::app::now::need_line;
    assert_eq!(
        need_line(&need(NeedKind::Plan, "3f2a", "the plan of DEMO-12 waits")),
        "the plan waits for your approval"
    );
    assert_eq!(
        need_line(&need(
            NeedKind::Operation,
            "git.push 1a2b3c4",
            "git.push at 1a2b3c4 waits"
        )),
        "the write git.push waits for your approval"
    );
    assert_eq!(
        need_line(&need(
            NeedKind::Flag,
            "needs-you",
            "needs-you at the step build: the proof did not pass"
        )),
        "a rule stopped it: the proof did not pass"
    );
    assert_eq!(
        need_line(&need(
            NeedKind::Flag,
            "paused",
            "paused at the step build: x"
        )),
        "paused: you took over its session"
    );
}
