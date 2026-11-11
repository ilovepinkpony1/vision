//! ADF to Markdown and Markdown to ADF, for each element, and the round trips.

use serde_json::{json, Value};
use vision::adapters::jira_github::adf::{to_adf, to_markdown};

fn doc(blocks: Vec<Value>) -> Value {
    json!({"type": "doc", "version": 1, "content": blocks})
}

fn para(inline: Vec<Value>) -> Value {
    json!({"type": "paragraph", "content": inline})
}

/// A text node with marks: a name such as "strong", or a mark object.
fn text(value: &str, marks: Vec<Value>) -> Value {
    let mut node = json!({"type": "text", "text": value});
    if !marks.is_empty() {
        let marks: Vec<Value> = marks
            .into_iter()
            .map(|m| match m {
                Value::String(name) => json!({ "type": name }),
                other => other,
            })
            .collect();
        node["marks"] = Value::Array(marks);
    }
    node
}

fn t(value: &str) -> Value {
    text(value, vec![])
}

fn link(href: &str) -> Value {
    json!({"type": "link", "attrs": {"href": href}})
}

fn item(blocks: Vec<Value>) -> Value {
    json!({"type": "listItem", "content": blocks})
}

/// Each element: the ADF document and its Markdown text. Each pair converts both ways.
fn elements() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "paragraph",
            doc(vec![para(vec![t("One line.")])]),
            "One line.",
        ),
        (
            "two paragraphs",
            doc(vec![para(vec![t("One.")]), para(vec![t("Two.")])]),
            "One.\n\nTwo.",
        ),
        (
            "hard break",
            doc(vec![para(vec![
                t("a"),
                json!({"type": "hardBreak"}),
                t("b"),
            ])]),
            "a\nb",
        ),
        (
            "heading",
            doc(vec![
                json!({"type": "heading", "attrs": {"level": 2}, "content": [t("Why")]}),
            ]),
            "## Why",
        ),
        (
            "heading 6",
            doc(vec![
                json!({"type": "heading", "attrs": {"level": 6}, "content": [t("Deep")]}),
            ]),
            "###### Deep",
        ),
        (
            "bold",
            doc(vec![para(vec![
                t("a "),
                text("bold", vec![json!("strong")]),
                t(" b"),
            ])]),
            "a **bold** b",
        ),
        (
            "italic",
            doc(vec![para(vec![
                t("an "),
                text("em", vec![json!("em")]),
                t(" word"),
            ])]),
            "an *em* word",
        ),
        (
            "bold italic",
            doc(vec![para(vec![text(
                "both",
                vec![json!("strong"), json!("em")],
            )])]),
            "**_both_**",
        ),
        (
            "strike",
            doc(vec![para(vec![text("gone", vec![json!("strike")])])]),
            "~~gone~~",
        ),
        (
            "inline code",
            doc(vec![para(vec![
                t("run "),
                text("make test", vec![json!("code")]),
            ])]),
            "run `make test`",
        ),
        (
            "code with a backtick",
            doc(vec![para(vec![text("a`b", vec![json!("code")])])]),
            "``a`b``",
        ),
        (
            "link",
            doc(vec![para(vec![
                t("see "),
                text("the docs", vec![link("https://example.com/docs")]),
            ])]),
            "see [the docs](https://example.com/docs)",
        ),
        (
            "code link",
            doc(vec![para(vec![text(
                "cfg",
                vec![link("https://example.com/c"), json!("code")],
            )])]),
            "[`cfg`](https://example.com/c)",
        ),
        (
            "bullet list",
            doc(vec![json!({"type": "bulletList", "content": [
                item(vec![para(vec![t("one")])]), item(vec![para(vec![t("two")])])]})]),
            "- one\n- two",
        ),
        (
            "numbered list",
            doc(vec![json!({"type": "orderedList", "content": [
                item(vec![para(vec![t("one")])]), item(vec![para(vec![t("two")])])]})]),
            "1. one\n2. two",
        ),
        (
            "numbered from 3",
            doc(vec![json!({"type": "orderedList", "attrs": {"order": 3},
                            "content": [item(vec![para(vec![t("three")])])]})]),
            "3. three",
        ),
        (
            "nested list",
            doc(vec![json!({"type": "bulletList", "content": [
                item(vec![para(vec![t("top")]),
                          json!({"type": "orderedList", "content": [item(vec![para(vec![t("inner")])])]})]),
                item(vec![para(vec![t("next")])]),
            ]})]),
            "- top\n  1. inner\n- next",
        ),
        (
            "code block",
            doc(vec![
                json!({"type": "codeBlock", "attrs": {"language": "python"},
                            "content": [t("x = 1\n\nprint(x)")]}),
            ]),
            "```python\nx = 1\n\nprint(x)\n```",
        ),
        (
            "code block with a fence inside",
            doc(vec![
                json!({"type": "codeBlock", "content": [t("```\nin\n```")]}),
            ]),
            "````\n```\nin\n```\n````",
        ),
        (
            "quote",
            doc(vec![
                json!({"type": "blockquote", "content": [para(vec![t("said")])]}),
            ]),
            "> said",
        ),
        (
            "rule",
            doc(vec![
                para(vec![t("a")]),
                json!({"type": "rule"}),
                para(vec![t("b")]),
            ]),
            "a\n\n---\n\nb",
        ),
        (
            "escaped markers",
            doc(vec![para(vec![t(
                "2 * 3 = 6 and [x] and `y` and snake_case_name",
            )])]),
            "2 \\* 3 = 6 and \\[x\\] and \\`y\\` and snake_case_name",
        ),
        (
            "line that looks like a list",
            doc(vec![para(vec![t("- not a list")])]),
            "\\- not a list",
        ),
        (
            "line that looks like a heading",
            doc(vec![para(vec![t("# not a heading")])]),
            "\\# not a heading",
        ),
        (
            "line that looks like a number",
            doc(vec![para(vec![t("1. not a list")])]),
            "1\\. not a list",
        ),
    ]
}

#[test]
fn adf_to_markdown_for_each_element() {
    for (name, document, markdown) in elements() {
        assert_eq!(to_markdown(&document), markdown, "{name}");
    }
}

#[test]
fn nodes_with_no_markdown_form_keep_their_text() {
    let document = doc(vec![
        para(vec![
            t("Hi "),
            json!({"type": "mention", "attrs": {"id": "1", "text": "@Sam"}}),
            t(" "),
            json!({"type": "emoji", "attrs": {"shortName": ":ok:", "text": "OK"}}),
            t(" "),
            json!({"type": "status", "attrs": {"text": "DONE"}}),
            t(" "),
            json!({"type": "inlineCard", "attrs": {"url": "https://example.com/x"}}),
            t(" "),
            json!({"type": "date", "attrs": {"timestamp": "1794412800000"}}),
        ]),
        json!({"type": "panel", "attrs": {"panelType": "info"}, "content": [para(vec![t("note")])]}),
        json!({"type": "table", "content": [
            {"type": "tableRow", "content": [
                {"type": "tableHeader", "content": [para(vec![t("a")])]},
                {"type": "tableHeader", "content": [para(vec![t("b")])]}]},
            {"type": "tableRow", "content": [
                {"type": "tableCell", "content": [para(vec![t("1|2")])]},
                {"type": "tableCell", "content": [para(vec![t("3")])]}]},
        ]}),
        json!({"type": "taskList", "content": [
            {"type": "taskItem", "attrs": {"state": "DONE"}, "content": [t("done")]},
            {"type": "taskItem", "attrs": {"state": "TODO"}, "content": [t("todo")]}]}),
        json!({"type": "mediaSingle", "content": [{"type": "media", "attrs": {"id": "x"}}]}),
        json!({"type": "someFutureNode", "content": [para(vec![t("inside")])]}),
    ]);
    assert_eq!(
        to_markdown(&document),
        "Hi @Sam OK [DONE] https://example.com/x 2026-11-11\n\n> note\n\n| a | b |\n| --- | --- |\n| 1\\|2 | 3 |\n\n- [x] done\n- [ ] todo\n\n[attachment]\n\ninside"
    );
}

#[test]
fn empty_and_missing() {
    assert_eq!(to_markdown(&Value::Null), "");
    assert_eq!(to_markdown(&doc(vec![])), "");
    assert_eq!(
        to_markdown(&doc(vec![json!({"type": "paragraph", "content": []})])),
        ""
    );
}

#[test]
fn markdown_to_adf_for_each_element() {
    for (name, document, markdown) in elements() {
        assert_eq!(to_adf(markdown), document, "{name}");
    }
}

#[test]
fn other_spellings() {
    let cases = vec![
        (
            "__bold__ and _em_",
            doc(vec![para(vec![
                text("bold", vec![json!("strong")]),
                t(" and "),
                text("em", vec![json!("em")]),
            ])]),
        ),
        (
            "***both***",
            doc(vec![para(vec![text(
                "both",
                vec![json!("strong"), json!("em")],
            )])]),
        ),
        (
            "* star\n+ plus",
            doc(vec![json!({"type": "bulletList", "content": [
                item(vec![para(vec![t("star")])]), item(vec![para(vec![t("plus")])])]})]),
        ),
        (
            "1) one",
            doc(vec![
                json!({"type": "orderedList", "content": [item(vec![para(vec![t("one")])])]}),
            ]),
        ),
        (
            "# Title #",
            doc(vec![
                json!({"type": "heading", "attrs": {"level": 1}, "content": [t("Title")]}),
            ]),
        ),
        (
            "- a\n\n- b",
            doc(vec![json!({"type": "bulletList", "content": [
                item(vec![para(vec![t("a")])]), item(vec![para(vec![t("b")])])]})]),
        ),
        (
            "~~~\ncode\n~~~",
            doc(vec![json!({"type": "codeBlock", "content": [t("code")]})]),
        ),
        ("***", doc(vec![json!({"type": "rule"})])),
        ("a [b] c", doc(vec![para(vec![t("a [b] c")])])),
        (
            "intra_word_underscores",
            doc(vec![para(vec![t("intra_word_underscores")])]),
        ),
    ];
    for (markdown, document) in cases {
        assert_eq!(to_adf(markdown), document, "{markdown}");
    }
}

#[test]
fn an_item_with_two_paragraphs_and_code() {
    let markdown = "- first\n\n  more\n\n  ```\n  code\n  ```\n- second";
    let expected = doc(vec![json!({"type": "bulletList", "content": [
        item(vec![para(vec![t("first")]), para(vec![t("more")]),
                  json!({"type": "codeBlock", "content": [t("code")]})]),
        item(vec![para(vec![t("second")])]),
    ]})]);
    assert_eq!(to_adf(markdown), expected);
    assert_eq!(to_markdown(&expected), markdown);
}

const DESCRIPTION: &str = "## Problem

The report export makes **no file** when the list is empty, see [the log](https://example.com/log).
It happens for each user.

## Direction

1. Check the list in `export_report`.
2. Show a message:
   - in the page
   - in the API answer
3. Add a test.

```python
def export_report(rows):
    if not rows:
        raise EmptyReport()
```

> The export of an empty list is an error, not an empty file.

---

Risks: *none known*, ~~old idea~~.";

#[test]
fn markdown_to_adf_and_back() {
    assert_eq!(to_markdown(&to_adf(DESCRIPTION)), DESCRIPTION);
}

#[test]
fn adf_to_markdown_and_back() {
    let document = to_adf(DESCRIPTION);
    assert_eq!(to_adf(&to_markdown(&document)), document);
    for (name, element, _markdown) in elements() {
        assert_eq!(to_adf(&to_markdown(&element)), element, "{name}");
    }
}
