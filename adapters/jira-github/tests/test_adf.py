"""ADF to Markdown and Markdown to ADF, for each element, and the round trips."""

import unittest

import support  # noqa: F401 - puts lib/ on the path
from jgadapter import adf


def doc(*blocks):
    return {"type": "doc", "version": 1, "content": list(blocks)}


def para(*inline):
    return {"type": "paragraph", "content": list(inline)}


def text(value, *marks):
    node = {"type": "text", "text": value}
    if marks:
        node["marks"] = [m if isinstance(m, dict) else {"type": m} for m in marks]
    return node


def link(href):
    return {"type": "link", "attrs": {"href": href}}


def item(*blocks):
    return {"type": "listItem", "content": list(blocks)}


# Each element: the ADF document and its Markdown text. Each pair converts both ways.
ELEMENTS = {
    "paragraph": (doc(para(text("One line."))), "One line."),
    "two paragraphs": (doc(para(text("One.")), para(text("Two."))), "One.\n\nTwo."),
    "hard break": (doc(para(text("a"), {"type": "hardBreak"}, text("b"))), "a\nb"),
    "heading": (doc({"type": "heading", "attrs": {"level": 2}, "content": [text("Why")]}), "## Why"),
    "heading 6": (doc({"type": "heading", "attrs": {"level": 6}, "content": [text("Deep")]}), "###### Deep"),
    "bold": (doc(para(text("a "), text("bold", "strong"), text(" b"))), "a **bold** b"),
    "italic": (doc(para(text("an "), text("em", "em"), text(" word"))), "an *em* word"),
    "bold italic": (doc(para(text("both", "strong", "em"))), "**_both_**"),
    "strike": (doc(para(text("gone", "strike"))), "~~gone~~"),
    "inline code": (doc(para(text("run "), text("make test", "code"))), "run `make test`"),
    "code with a backtick": (doc(para(text("a`b", "code"))), "``a`b``"),
    "link": (doc(para(text("see "), text("the docs", link("https://example.com/docs")))),
             "see [the docs](https://example.com/docs)"),
    "code link": (doc(para(text("cfg", link("https://example.com/c"), "code"))), "[`cfg`](https://example.com/c)"),
    "bullet list": (doc({"type": "bulletList", "content": [item(para(text("one"))), item(para(text("two")))]}),
                    "- one\n- two"),
    "numbered list": (doc({"type": "orderedList", "content": [item(para(text("one"))), item(para(text("two")))]}),
                      "1. one\n2. two"),
    "numbered from 3": (doc({"type": "orderedList", "attrs": {"order": 3}, "content": [item(para(text("three")))]}),
                        "3. three"),
    "nested list": (
        doc({"type": "bulletList", "content": [
            item(para(text("top")), {"type": "orderedList", "content": [item(para(text("inner")))]}),
            item(para(text("next"))),
        ]}),
        "- top\n  1. inner\n- next",
    ),
    "code block": (
        doc({"type": "codeBlock", "attrs": {"language": "python"}, "content": [text("x = 1\n\nprint(x)")]}),
        "```python\nx = 1\n\nprint(x)\n```",
    ),
    "code block with a fence inside": (
        doc({"type": "codeBlock", "content": [text("```\nin\n```")]}),
        "````\n```\nin\n```\n````",
    ),
    "quote": (doc({"type": "blockquote", "content": [para(text("said"))]}), "> said"),
    "rule": (doc(para(text("a")), {"type": "rule"}, para(text("b"))), "a\n\n---\n\nb"),
    "escaped markers": (doc(para(text("2 * 3 = 6 and [x] and `y` and snake_case_name"))),
                        "2 \\* 3 = 6 and \\[x\\] and \\`y\\` and snake_case_name"),
    "line that looks like a list": (doc(para(text("- not a list"))), "\\- not a list"),
    "line that looks like a heading": (doc(para(text("# not a heading"))), "\\# not a heading"),
    "line that looks like a number": (doc(para(text("1. not a list"))), "1\\. not a list"),
}


class AdfToMarkdown(unittest.TestCase):
    def test_each_element(self):
        for name, (document, markdown) in ELEMENTS.items():
            with self.subTest(name):
                self.assertEqual(adf.to_markdown(document), markdown)

    def test_nodes_with_no_markdown_form_keep_their_text(self):
        document = doc(
            para(text("Hi "), {"type": "mention", "attrs": {"id": "1", "text": "@Sam"}}, text(" "),
                 {"type": "emoji", "attrs": {"shortName": ":ok:", "text": "OK"}}, text(" "),
                 {"type": "status", "attrs": {"text": "DONE"}}, text(" "),
                 {"type": "inlineCard", "attrs": {"url": "https://example.com/x"}}, text(" "),
                 {"type": "date", "attrs": {"timestamp": "1794412800000"}}),
            {"type": "panel", "attrs": {"panelType": "info"}, "content": [para(text("note"))]},
            {"type": "table", "content": [
                {"type": "tableRow", "content": [
                    {"type": "tableHeader", "content": [para(text("a"))]},
                    {"type": "tableHeader", "content": [para(text("b"))]}]},
                {"type": "tableRow", "content": [
                    {"type": "tableCell", "content": [para(text("1|2"))]},
                    {"type": "tableCell", "content": [para(text("3"))]}]},
            ]},
            {"type": "taskList", "content": [
                {"type": "taskItem", "attrs": {"state": "DONE"}, "content": [text("done")]},
                {"type": "taskItem", "attrs": {"state": "TODO"}, "content": [text("todo")]}]},
            {"type": "mediaSingle", "content": [{"type": "media", "attrs": {"id": "x"}}]},
            {"type": "someFutureNode", "content": [para(text("inside"))]},
        )
        self.assertEqual(
            adf.to_markdown(document),
            "Hi @Sam OK [DONE] https://example.com/x 2026-11-11\n\n> note\n\n"
            "| a | b |\n| --- | --- |\n| 1\\|2 | 3 |\n\n- [x] done\n- [ ] todo\n\n[attachment]\n\ninside",
        )

    def test_empty_and_missing(self):
        self.assertEqual(adf.to_markdown(None), "")
        self.assertEqual(adf.to_markdown(doc()), "")
        self.assertEqual(adf.to_markdown(doc(para())), "")


class MarkdownToAdf(unittest.TestCase):
    def test_each_element(self):
        for name, (document, markdown) in ELEMENTS.items():
            with self.subTest(name):
                self.assertEqual(adf.to_adf(markdown), document)

    def test_other_spellings(self):
        cases = {
            "__bold__ and _em_": doc(para(text("bold", "strong"), text(" and "), text("em", "em"))),
            "***both***": doc(para(text("both", "strong", "em"))),
            "* star\n+ plus": doc({"type": "bulletList", "content": [item(para(text("star"))), item(para(text("plus")))]}),
            "1) one": doc({"type": "orderedList", "content": [item(para(text("one")))]}),
            "# Title #": doc({"type": "heading", "attrs": {"level": 1}, "content": [text("Title")]}),
            "- a\n\n- b": doc({"type": "bulletList", "content": [item(para(text("a"))), item(para(text("b")))]}),
            "~~~\ncode\n~~~": doc({"type": "codeBlock", "content": [text("code")]}),
            "***": doc({"type": "rule"}),
            "a [b] c": doc(para(text("a [b] c"))),
            "intra_word_underscores": doc(para(text("intra_word_underscores"))),
        }
        for markdown, document in cases.items():
            with self.subTest(markdown):
                self.assertEqual(adf.to_adf(markdown), document)

    def test_an_item_with_two_paragraphs_and_code(self):
        markdown = "- first\n\n  more\n\n  ```\n  code\n  ```\n- second"
        expected = doc({"type": "bulletList", "content": [
            item(para(text("first")), para(text("more")), {"type": "codeBlock", "content": [text("code")]}),
            item(para(text("second"))),
        ]})
        self.assertEqual(adf.to_adf(markdown), expected)
        self.assertEqual(adf.to_markdown(expected), markdown)


DESCRIPTION = """## Problem

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

Risks: *none known*, ~~old idea~~."""


class RoundTrip(unittest.TestCase):
    def test_markdown_to_adf_and_back(self):
        self.assertEqual(adf.to_markdown(adf.to_adf(DESCRIPTION)), DESCRIPTION)

    def test_adf_to_markdown_and_back(self):
        document = adf.to_adf(DESCRIPTION)
        self.assertEqual(adf.to_adf(adf.to_markdown(document)), document)
        for name, (element, _markdown) in ELEMENTS.items():
            with self.subTest(name):
                self.assertEqual(adf.to_adf(adf.to_markdown(element)), element)


if __name__ == "__main__":
    unittest.main()
