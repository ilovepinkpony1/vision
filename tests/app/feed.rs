//! The activity feed and the reader of the stream files: the join of the stream and the
//! ledger by time, the wrap of the text of the agent and the cut of a tool line, the bound, the
//! follow and the scroll, text from outside with control characters, and a stream file that
//! grows while the reader follows it.

use std::io::Write;

use serde_json::json;
use vision::app::feed::{Batch, Feed, StreamLine, KEEP_STREAM};
use vision::app::model::Hue;
use vision::app::moments::Moment;
use vision::app::tail::Reader;
use vision::paths::{Paths, TicketKey};
use vision::turn::{StreamEvent, ToolCall};

use super::live_support::{at, TURN};

fn says(text: &str) -> StreamEvent {
    StreamEvent::Assistant {
        texts: vec![text.into()],
        tools: Vec::new(),
    }
}

fn tool(name: &str, input: serde_json::Value) -> StreamEvent {
    StreamEvent::Assistant {
        texts: Vec::new(),
        tools: vec![ToolCall {
            name: name.into(),
            input,
        }],
    }
}

fn batch(seconds: i64, events: Vec<StreamEvent>) -> Batch {
    Batch {
        turn: TURN.into(),
        events: events
            .into_iter()
            .map(|event| StreamLine {
                event,
                output: None,
            })
            .collect(),
        old: false,
        at: at(seconds),
        started: Some(at(0)),
    }
}

fn moment(word: &str, text: &str) -> Moment {
    Moment {
        who: "vision".into(),
        word: word.into(),
        hue: Hue::Info,
        what: word.into(),
        text: text.into(),
        turn: None,
    }
}

fn words(feed: &Feed, width: usize) -> Vec<String> {
    feed.lines(width, 0)
        .iter()
        .map(|l| {
            format!("{} {} {}", l.time, l.word, l.text)
                .trim()
                .to_string()
        })
        .collect()
}

#[test]
fn the_feed_joins_the_stream_and_the_ledger_by_time() {
    let mut feed = Feed::default();
    feed.add(&batch(10, vec![tool("Read", json!({"file_path": "a.py"}))]));
    feed.add(&batch(30, vec![says("Done.")]));
    let changed = feed.set_moments(&[
        (Some(at(5)), moment("turn", "builder starts")),
        (Some(at(20)), moment("step", "between")),
        (Some(at(40)), moment("turn", "the turn ended")),
    ]);
    assert!(changed);
    assert!(!feed.set_moments(&[
        (Some(at(5)), moment("turn", "builder starts")),
        (Some(at(20)), moment("step", "between")),
        (Some(at(40)), moment("turn", "the turn ended")),
    ]));
    assert_eq!(
        words(&feed, 40),
        [
            "14:00:05 turn builder starts",
            "14:00:10 read a.py",
            "14:00:20 step between",
            "14:00:30 says Done.",
            "14:00:40 turn the turn ended",
        ]
    );
}

#[test]
fn old_lines_sort_after_the_start_of_their_turn_and_show_no_time() {
    let mut feed = Feed::default();
    feed.set_moments(&[
        (Some(at(0)), moment("turn", "builder starts")),
        (Some(at(600)), moment("turn", "the turn ended")),
    ]);
    let mut old = batch(500, vec![says("one"), says("two"), says("three")]);
    old.old = true;
    feed.add(&old);
    assert_eq!(
        words(&feed, 40),
        [
            "14:00:00 turn builder starts",
            "says one",
            "says two",
            "14:08:20 says three",
            "14:10:00 turn the turn ended",
        ]
    );
}

#[test]
fn the_text_of_the_agent_wraps_and_a_tool_line_is_cut() {
    let mut feed = Feed::default();
    feed.add(&batch(
        1,
        vec![
            says("one two three four five six seven"),
            tool(
                "Bash",
                json!({"command": "python3 -m unittest discover -q tests"}),
            ),
        ],
    ));
    let lines = feed.lines(12, 0);
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        texts,
        ["one two", "three four", "five six", "seven", "python3 -m …"]
    );
    // The time and the word show on the first line of an item only.
    assert_eq!(lines[0].word, "says");
    assert_eq!(lines[1].word, "");
    assert_eq!(lines[4].word, "run");
    assert_eq!(lines[0].text_hue, Hue::Fg);
    assert_eq!(lines[4].text_hue, Hue::Dim);
}

#[test]
fn the_feed_keeps_a_bounded_number_of_items() {
    let mut feed = Feed::default();
    for i in 0..(KEEP_STREAM as i64 + 50) {
        feed.add(&batch(i, vec![says(&format!("line {i}"))]));
    }
    assert_eq!(feed.len(), KEEP_STREAM);
    let lines = feed.lines(40, 0);
    assert_eq!(lines.first().unwrap().text, "line 50");
    assert_eq!(
        lines.last().unwrap().text,
        format!("line {}", KEEP_STREAM + 49)
    );
    let moments: Vec<_> = (0..500)
        .map(|i| (Some(at(i)), moment("step", &format!("m {i}"))))
        .collect();
    feed.set_moments(&moments);
    assert_eq!(feed.len(), KEEP_STREAM + vision::app::feed::KEEP_LEDGER);
}

#[test]
fn a_scroll_stops_the_follow_and_g_goes_back_to_the_end() {
    let mut feed = Feed::default();
    for i in 0..30 {
        feed.add(&batch(i, vec![says(&format!("line {i}"))]));
    }
    assert!(feed.follow);
    feed.up(3, 30, 10);
    assert!(!feed.follow);
    assert_eq!(feed.back, 3);
    feed.up(100, 30, 10);
    assert_eq!(feed.back, 20, "the first line stays the top");
    // New lines keep the same lines in view.
    feed.keep_place(2);
    assert_eq!(feed.back, 22);
    feed.down(5);
    assert_eq!(feed.back, 17);
    feed.end();
    assert!(feed.follow);
    assert_eq!(feed.back, 0);
    feed.keep_place(4);
    assert_eq!(feed.back, 0, "a feed that follows stays at the end");
    feed.up(1, 30, 10);
    feed.down(1);
    assert!(feed.follow, "a scroll down to the end follows again");
    // A feed that fits its view does not scroll.
    let mut short = Feed::default();
    short.add(&batch(1, vec![says("only")]));
    short.up(1, 1, 10);
    assert!(short.follow);
}

#[test]
fn text_from_outside_loses_each_control_character() {
    let mut feed = Feed::default();
    feed.add(&batch(
        1,
        vec![
            says("red \x1b[31mtext\x07 and\r\nmore"),
            tool("Read", json!({"file_path": "evil\x1b]0;title\x07.py"})),
        ],
    ));
    for line in feed.lines(80, 0) {
        assert!(!line.text.chars().any(char::is_control), "{:?}", line.text);
    }
}

#[test]
fn the_stats_count_the_tool_calls_and_the_output_tokens() {
    let mut feed = Feed::default();
    let mut b = batch(
        1,
        vec![tool("Read", json!({})), tool("StructuredOutput", json!({}))],
    );
    b.events[0].output = Some(("m1".into(), 120));
    b.events[1].output = Some(("m1".into(), 150));
    feed.add(&b);
    let mut c = batch(2, vec![tool("Bash", json!({"command": "ls"}))]);
    c.events[0].output = Some(("m2".into(), 1000));
    feed.add(&c);
    let stats = &feed.stats[TURN];
    assert_eq!(
        stats.tools, 2,
        "the tool of the result object does not count"
    );
    assert_eq!(stats.output_tokens(), Some(1150));
}

#[test]
fn the_tools_of_a_session_have_their_words() {
    use vision::app::feed::tool_word;
    let word = |name: &str| tool_word(name).map(|(word, _)| word);
    assert_eq!(word("Read"), Some("read"));
    assert_eq!(word("Grep"), Some("find"));
    assert_eq!(word("Glob"), Some("find"));
    assert_eq!(word("Edit"), Some("edit"));
    assert_eq!(word("Write"), Some("edit"));
    assert_eq!(word("Bash"), Some("run"));
    assert_eq!(word("WebFetch"), Some("web"));
    assert_eq!(word("Task"), Some("task"));
    assert_eq!(word("mcp__notes__search"), None);
    assert_eq!(word("Skill"), None);
}

#[test]
fn a_tool_with_no_word_of_its_own_shows_its_name_without_its_server() {
    let mut feed = Feed::default();
    feed.add(&batch(
        1,
        vec![
            tool("mcp__notes__search", json!({"query": "csv"})),
            tool("Skill", json!({})),
        ],
    ));
    assert_eq!(
        words(&feed, 40),
        ["14:00:01 tool search: csv", "14:00:01 tool Skill"]
    );
}

#[test]
fn the_text_that_starts_a_turn_takes_one_line_and_a_later_text_wraps() {
    let gets = |text: &str| StreamEvent::User {
        texts: vec![text.into()],
        tool_results: 0,
    };
    let mut feed = Feed::default();
    feed.add(&batch(
        1,
        vec![gets(
            "Turn 1. Read the facts of the turn and do the step that they name.",
        )],
    ));
    feed.add(&batch(2, vec![says("I read it.")]));
    feed.add(&batch(
        3,
        vec![gets(
            "Use the old export, and keep the name of the column as it is.",
        )],
    ));
    let lines = words(&feed, 30);
    assert_eq!(lines[0], "14:00:01 gets Turn 1. Read the facts of the…");
    assert_eq!(lines[1], "14:00:02 says I read it.");
    assert_eq!(lines[2], "14:00:03 gets Use the old export, and keep");
    assert_eq!(lines[3], "the name of the column as it");
}

#[test]
fn the_reader_gives_each_event_of_a_growing_file_one_time() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path());
    let key = TicketKey::parse("DEMO-12").unwrap();
    let old_turn = "20261116T140000Z-DEMO-12-plan-1";
    let new_turn = TURN;
    let line = |text: &str| {
        format!(
            "{}\n",
            json!({"type": "assistant", "message": {"id": "m", "content": [{"type": "text", "text": text}], "usage": {"output_tokens": 7}}})
        )
    };
    let turns = paths.turns_dir(&key);
    let stream = |turn: &str| turns.join(turn).join("stream.jsonl");
    std::fs::create_dir_all(stream(old_turn).parent().unwrap()).unwrap();
    std::fs::write(stream(old_turn), line("before")).unwrap();
    let mut reader = Reader::new(paths, key);
    let now = at(0);

    // The first read gives the lines that were there, as old lines.
    let first = reader.read(now);
    assert_eq!(first.len(), 1);
    assert!(first[0].old);
    assert_eq!(first[0].events.len(), 1);
    assert_eq!(first[0].events[0].output, Some(("m".into(), 7)));
    assert!(reader.read(now).is_empty(), "no line twice");

    // A new turn appears, and its file grows: a complete line, then a line in two parts.
    std::fs::create_dir_all(stream(new_turn).parent().unwrap()).unwrap();
    std::fs::write(stream(new_turn), line("first")).unwrap();
    let second = reader.read(now);
    assert_eq!(second.len(), 1);
    assert!(!second[0].old, "a turn that came later has new lines");
    assert_eq!(second[0].turn, new_turn);
    let whole = line("second");
    let (a, b) = whole.split_at(20);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(stream(new_turn))
        .unwrap();
    file.write_all(a.as_bytes()).unwrap();
    file.flush().unwrap();
    assert!(reader.read(now).is_empty(), "half a line waits");
    file.write_all(b.as_bytes()).unwrap();
    file.write_all(b"not json\n").unwrap();
    file.flush().unwrap();
    let third = reader.read(now);
    assert_eq!(third.len(), 1);
    assert_eq!(
        third[0].events.len(),
        1,
        "a line that is not an event is left out"
    );
    match &third[0].events[0].event {
        StreamEvent::Assistant { texts, .. } => assert_eq!(texts, &["second".to_string()]),
        other => panic!("{other:?}"),
    }
    assert!(reader.read(now).is_empty());
}
