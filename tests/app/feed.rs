//! The activity feed and the reader of the stream files: the join of the stream and the
//! ledger by time, the wrap of the text of the agent, no line for a tool call in any tab, no
//! line for what a turn gets or for the start and the end of its session, the counts of the
//! tabs, the bound, the follow and the scroll in a tab, text from outside with control
//! characters, and a stream file that grows while the reader follows it.

use std::io::Write;

use serde_json::json;
use vision::app::feed::{Batch, Feed, Filter, StreamLine, FILTERS, KEEP_STREAM};
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

/// A line of the ledger: a turn is of the kind Agents, a write of the kind Writes, and each
/// other word of the kind Steps.
fn moment(word: &str, text: &str) -> Moment {
    Moment {
        who: "vision".into(),
        word: word.into(),
        hue: Hue::Info,
        what: word.into(),
        text: text.into(),
        turn: None,
        filter: match word {
            "turn" => Filter::Agents,
            "write" => Filter::Writes,
            _ => Filter::Steps,
        },
    }
}

/// The words of the tool calls, which the feed never shows.
const TOOL_WORDS: [&str; 8] = ["read", "find", "edit", "run", "web", "task", "todo", "tool"];

fn words(feed: &Feed, width: usize) -> Vec<String> {
    words_in(feed, width, Filter::All)
}

fn words_in(feed: &Feed, width: usize, filter: Filter) -> Vec<String> {
    feed.lines(width, 0, filter)
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
    // The tool call gets no line: only its count stays.
    assert_eq!(
        words(&feed, 40),
        [
            "14:00:05 turn builder starts",
            "14:00:20 step between",
            "14:00:30 says Done.",
            "14:00:40 turn the turn ended",
        ]
    );
    assert_eq!(feed.stats[TURN].tools, 1);
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
fn the_text_of_the_agent_wraps_and_a_line_of_the_ledger_is_cut() {
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
    feed.set_moments(&[(Some(at(2)), moment("step", "moves from plan to build"))]);
    let lines = feed.lines(12, 0, Filter::All);
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        texts,
        ["one two", "three four", "five six", "seven", "moves from …"]
    );
    // The time and the word show on the first line of an item only.
    assert_eq!(lines[0].word, "says");
    assert_eq!(lines[1].word, "");
    assert_eq!(lines[4].word, "step");
    assert_eq!(lines[0].text_hue, Hue::Fg);
    assert_eq!(lines[4].text_hue, Hue::Dim);
}

#[test]
fn no_tab_shows_a_line_for_a_tool_call() {
    let mut feed = Feed::default();
    let calls = [
        ("Read", json!({"file_path": "a.py"})),
        ("Grep", json!({"pattern": "x"})),
        ("Edit", json!({"file_path": "a.py"})),
        ("Bash", json!({"command": "ls"})),
        ("WebFetch", json!({"url": "https://example.com"})),
        ("Task", json!({"description": "look"})),
        ("TodoWrite", json!({})),
        ("mcp__notes__search", json!({"query": "csv"})),
    ];
    for (i, (name, input)) in calls.into_iter().enumerate() {
        feed.add(&batch(i as i64, vec![tool(name, input)]));
    }
    feed.add(&batch(20, vec![says("Done.")]));
    assert_eq!(feed.len(), 1, "only the words of the agent stay");
    assert_eq!(feed.stats[TURN].tools, 8);
    for filter in FILTERS {
        for line in feed.lines(40, 0, filter) {
            assert!(
                !TOOL_WORDS.contains(&line.word.as_str()),
                "{filter:?}: {line:?}"
            );
        }
    }
}

#[test]
fn each_tab_counts_its_kind_and_shows_only_its_lines() {
    let mut feed = Feed::default();
    feed.add(&batch(10, vec![says("I add a test.")]));
    feed.set_moments(&[
        (Some(at(5)), moment("turn", "builder starts")),
        (Some(at(20)), moment("step", "moves from build to review")),
        (Some(at(30)), moment("write", "notes.write is confirmed")),
    ]);
    assert_eq!(feed.counts(), [4, 2, 0, 1, 1, 0, 0]);
    assert_eq!(
        words_in(&feed, 40, Filter::Agents),
        [
            "14:00:05 turn builder starts",
            "14:00:10 says I add a test."
        ]
    );
    assert_eq!(
        words_in(&feed, 40, Filter::Writes),
        ["14:00:30 write notes.write is confirmed"]
    );
    assert!(words_in(&feed, 40, Filter::You).is_empty());
    assert_eq!(words_in(&feed, 40, Filter::All).len(), 4);
}

#[test]
fn the_feed_keeps_a_bounded_number_of_items() {
    let mut feed = Feed::default();
    for i in 0..(KEEP_STREAM as i64 + 50) {
        feed.add(&batch(i, vec![says(&format!("line {i}"))]));
    }
    assert_eq!(feed.len(), KEEP_STREAM);
    let lines = feed.lines(40, 0, Filter::All);
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
fn a_long_turn_of_tool_calls_does_not_push_the_words_of_the_agent_out() {
    let mut feed = Feed::default();
    feed.add(&batch(0, vec![says("I start with the export.")]));
    for i in 0..(KEEP_STREAM as i64 * 2) {
        feed.add(&batch(
            i + 1,
            vec![tool("Read", json!({"file_path": "a.py"}))],
        ));
    }
    feed.add(&batch(9000, vec![says("The export is done.")]));
    assert_eq!(feed.len(), 2);
    assert_eq!(
        words(&feed, 40),
        [
            "14:00:00 says I start with the export.",
            "16:30:00 says The export is done."
        ]
    );
    assert_eq!(feed.stats[TURN].tools, KEEP_STREAM * 2);
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
fn a_scroll_inside_a_tab_counts_the_lines_of_that_tab() {
    let mut feed = Feed::default();
    for i in 0..30 {
        feed.add(&batch(i * 2, vec![says(&format!("line {i}"))]));
    }
    let moments: Vec<_> = (0..12)
        .map(|i| (Some(at(i * 2 + 1)), moment("step", &format!("step {i}"))))
        .collect();
    feed.set_moments(&moments);
    let steps = feed.lines(40, 0, Filter::Steps);
    assert_eq!(steps.len(), 12);
    // The view of 10 rows can go back 2 lines of the tab, not 32 of all of them.
    feed.up(100, steps.len(), 10);
    assert_eq!(feed.back, 2);
    let end = steps.len() - feed.back;
    let shown: Vec<&str> = steps[end.saturating_sub(10)..end]
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(shown.first(), Some(&"step 0"));
    assert_eq!(shown.last(), Some(&"step 9"));
    feed.end();
    assert!(feed.follow);
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
    for line in feed.lines(80, 0, Filter::All) {
        assert!(!line.text.chars().any(char::is_control), "{:?}", line.text);
    }
    for (_, _, text, _) in vision::app::feed::words_of(&tool(
        "Read",
        json!({"file_path": "evil\x1b]0;title\x07.py"}),
    )) {
        let cut = vision::app::text::cut(&text, 80);
        assert!(!cut.chars().any(char::is_control), "{cut:?}");
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
fn a_tool_with_no_word_of_its_own_shows_its_name_without_its_server_in_its_turn() {
    use vision::app::feed::words_of;
    let texts: Vec<(String, String)> = [
        tool("mcp__notes__search", json!({"query": "csv"})),
        tool("Skill", json!({})),
    ]
    .iter()
    .flat_map(words_of)
    .map(|(word, _, text, _)| (word, text))
    .collect();
    assert_eq!(
        texts,
        [
            ("tool".to_string(), "search: csv".to_string()),
            ("tool".to_string(), "Skill".to_string())
        ]
    );
}

#[test]
fn the_feed_drops_what_the_turn_gets_and_the_start_and_the_end_of_its_session() {
    use vision::app::feed::words_of;
    use vision::turn::ResultEvent;
    let gets = StreamEvent::User {
        texts: vec!["Turn 1. Read the facts of the turn and do the step that they name.".into()],
        tool_results: 0,
    };
    let start = StreamEvent::Init {
        session_id: Some("s".into()),
        tools: Vec::new(),
        permission_mode: None,
        mcp_servers: Vec::new(),
        model: Some("claude-opus-5-5".into()),
    };
    let ends = StreamEvent::Result(ResultEvent {
        subtype: "success".into(),
        structured_output: Some(json!({"kind": "result", "summary": "Refined."})),
        ..ResultEvent::default()
    });
    let failed = StreamEvent::Result(ResultEvent {
        subtype: "error_max_turns".into(),
        is_error: true,
        ..ResultEvent::default()
    });
    let mut feed = Feed::default();
    feed.add(&batch(1, vec![start.clone(), gets.clone()]));
    feed.add(&batch(2, vec![says("I read the facts.")]));
    // Many of these lines do not push the words of the agent out: they go before the bound.
    for i in 0..(KEEP_STREAM as i64 * 2) {
        feed.add(&batch(3 + i, vec![gets.clone(), ends.clone()]));
    }
    feed.add(&batch(9000, vec![failed.clone()]));
    assert_eq!(
        words(&feed, 60),
        [
            "14:00:02 says I read the facts.",
            "16:30:00 ends the session ended with an error (error_max_turns)",
        ]
    );
    assert_eq!(feed.counts(), [2, 1, 0, 0, 0, 1, 0]);
    assert_eq!(words_in(&feed, 60, Filter::Problems).len(), 1);

    // The events of the turn in the log keep each of them.
    let log: Vec<String> = [start, gets, ends, failed]
        .iter()
        .flat_map(words_of)
        .map(|(word, _, _, _)| word)
        .collect();
    assert_eq!(log, ["start", "gets", "ends", "ends"]);
}

#[test]
fn an_old_line_shows_the_time_of_the_file_only_for_the_last_event() {
    let mut feed = Feed::default();
    let ends = StreamEvent::Result(vision::turn::ResultEvent {
        subtype: "success".into(),
        ..vision::turn::ResultEvent::default()
    });
    let mut old = batch(500, vec![says("one"), says("two"), ends]);
    old.old = true;
    feed.add(&old);
    // The end of the session is the last event, and it gets no line: no line of the agent
    // takes the time of the file.
    assert_eq!(words(&feed, 40), ["says one", "says two"]);
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
