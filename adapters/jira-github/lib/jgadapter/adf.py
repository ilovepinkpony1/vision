"""Atlassian Document Format (ADF) to Markdown, and Markdown to ADF.

`to_markdown` reads each node that Jira Cloud writes. A node with no Markdown form keeps its
text, such as a mention, a status, or a card. `to_adf` writes the elements that a description
needs: paragraphs, headings, bullet and numbered lists (nested), code blocks, quotes, rules,
inline code, bold, italic, strike, and links. A line break inside a paragraph is a hard break,
so that the lines of a text stay as they are.

The two are each other's inverse for those elements: to_markdown(to_adf(md)) == md for a text
in the form that to_markdown writes.
"""

import datetime
import re

# Inline characters that a text escapes with a backslash, so that it stays plain text.
ESCAPE = set("\\`*[]~")
ESCAPABLE = set("\\`*_[]~#>+-.!()|{}")


# ADF to Markdown ------------------------------------------------------------------------------


def to_markdown(doc):
    """The Markdown text of an ADF document. `None` and an empty document give ""."""
    if not isinstance(doc, dict):
        return "" if doc is None else str(doc)
    return "\n\n".join(b for b in _blocks(doc.get("content") or []) if b != "").strip("\n")


def _blocks(nodes):
    out = []
    for node in nodes:
        if isinstance(node, dict):
            out.append(_block(node))
    return out


def _block(node):
    kind = node.get("type")
    content = node.get("content") or []
    attrs = node.get("attrs") or {}
    if kind == "paragraph":
        return _escape_starts(_inline(content))
    if kind == "heading":
        level = attrs.get("level") if isinstance(attrs.get("level"), int) else 1
        level = min(max(level, 1), 6)
        return "#" * level + " " + _inline(content).replace("\n", " ")
    if kind == "bulletList":
        return "\n".join(_item("- ", item) for item in content if isinstance(item, dict))
    if kind == "orderedList":
        start = attrs.get("order") if isinstance(attrs.get("order"), int) else 1
        lines = []
        for n, item in enumerate(i for i in content if isinstance(i, dict)):
            lines.append(_item("%d. " % (start + n), item))
        return "\n".join(lines)
    if kind == "taskList":
        lines = []
        for item in content:
            if isinstance(item, dict):
                done = (item.get("attrs") or {}).get("state") == "DONE"
                lines.append(("- [x] " if done else "- [ ] ") + _inline(item.get("content") or []))
        return "\n".join(lines)
    if kind == "decisionList":
        return "\n".join("- " + _inline(i.get("content") or []) for i in content if isinstance(i, dict))
    if kind == "codeBlock":
        text = "".join(c.get("text", "") for c in content if isinstance(c, dict))
        fence = "```"
        while fence in text:
            fence += "`"
        language = attrs.get("language") if isinstance(attrs.get("language"), str) else ""
        return "%s%s\n%s\n%s" % (fence, language, text, fence)
    if kind in ("blockquote", "panel"):
        inner = "\n\n".join(_blocks(content))
        return "\n".join((">" + (" " + line if line else "")) for line in inner.split("\n"))
    if kind == "rule":
        return "---"
    if kind in ("expand", "nestedExpand"):
        title = attrs.get("title") if isinstance(attrs.get("title"), str) else ""
        inner = "\n\n".join(_blocks(content))
        return ("**%s**\n\n%s" % (_escape(title), inner)) if title else inner
    if kind == "table":
        return _table(content)
    if kind in ("mediaSingle", "mediaGroup", "media"):
        return "[attachment]"
    if kind in ("blockCard", "embedCard"):
        return attrs.get("url") or ""
    if kind == "text" or kind in INLINE:
        return _inline([node])
    # An unknown block: its children, or its text.
    if content:
        return "\n\n".join(_blocks(content))
    return _escape(node.get("text", "")) if isinstance(node.get("text"), str) else ""


def _item(marker, item):
    pad = " " * len(marker)
    parts = []
    for i, block in enumerate(b for b in (item.get("content") or []) if isinstance(b, dict)):
        text = _block(block)
        if i == 0:
            parts.append(text)
        elif block.get("type") in ("bulletList", "orderedList", "taskList"):
            parts.append("\n" + text)
        else:
            parts.append("\n\n" + text)
    body = "".join(parts)
    lines = body.split("\n")
    out = [marker + lines[0]]
    for line in lines[1:]:
        out.append(pad + line if line else "")
    return "\n".join(out)


def _table(rows):
    cells = []
    for row in rows:
        if isinstance(row, dict):
            cells.append([
                _inline_blocks(c.get("content") or []).replace("|", "\\|").replace("\n", " ")
                for c in row.get("content") or [] if isinstance(c, dict)
            ])
    if not cells:
        return ""
    width = max(len(r) for r in cells)
    cells = [r + [""] * (width - len(r)) for r in cells]
    lines = ["| " + " | ".join(cells[0]) + " |", "|" + " --- |" * width]
    lines += ["| " + " | ".join(r) + " |" for r in cells[1:]]
    return "\n".join(lines)


def _inline_blocks(blocks):
    return " ".join(_block(b) for b in blocks if isinstance(b, dict))


INLINE = {"text", "hardBreak", "mention", "emoji", "inlineCard", "status", "date", "placeholder", "mediaInline"}


def _inline(nodes):
    # Join neighbor text nodes with the same marks, so that `**a****b**` never happens.
    merged = []
    for node in nodes:
        if not isinstance(node, dict):
            continue
        if (
            node.get("type") == "text"
            and merged
            and merged[-1].get("type") == "text"
            and _marks(merged[-1]) == _marks(node)
        ):
            merged[-1] = dict(merged[-1], text=merged[-1].get("text", "") + node.get("text", ""))
        else:
            merged.append(node)
    # A run of nodes with the same link is one link, such as [`code` text](href).
    out = []
    i = 0
    while i < len(merged):
        href = _href(merged[i])
        if not href:
            out.append(_inline_node(merged[i]))
            i += 1
            continue
        j = i
        while j < len(merged) and _href(merged[j]) == href:
            j += 1
        label = "".join(_inline_node(_without_link(n)) for n in merged[i:j])
        out.append("[%s](%s)" % (label, _url(href)))
        i = j
    return "".join(out)


def _href(node):
    if node.get("type") != "text":
        return None
    for mark in node.get("marks") or []:
        if isinstance(mark, dict) and mark.get("type") == "link":
            return (mark.get("attrs") or {}).get("href") or None
    return None


def _without_link(node):
    marks = [m for m in node.get("marks") or [] if not (isinstance(m, dict) and m.get("type") == "link")]
    return dict(node, marks=marks)


def _marks(node):
    return [
        (m.get("type"), (m.get("attrs") or {}).get("href"))
        for m in node.get("marks") or []
        if isinstance(m, dict) and m.get("type") in ("strong", "em", "code", "strike", "link")
    ]


def _inline_node(node):
    kind = node.get("type")
    attrs = node.get("attrs") or {}
    if kind == "hardBreak":
        return "\n"
    if kind == "mention":
        text = attrs.get("text") or attrs.get("id") or ""
        return text if str(text).startswith("@") else "@" + str(text)
    if kind == "emoji":
        return str(attrs.get("text") or attrs.get("shortName") or "")
    if kind == "inlineCard":
        return str(attrs.get("url") or "")
    if kind == "status":
        return "[%s]" % _escape(str(attrs.get("text") or ""))
    if kind == "date":
        try:
            when = datetime.datetime.fromtimestamp(int(attrs.get("timestamp")) / 1000, datetime.timezone.utc)
            return when.strftime("%Y-%m-%d")
        except (TypeError, ValueError, OverflowError, OSError):
            return ""
    if kind == "mediaInline":
        return "[attachment]"
    if kind != "text":
        content = node.get("content")
        if isinstance(content, list):
            return _inline(content)
        return _escape(node.get("text", "")) if isinstance(node.get("text"), str) else ""
    text = node.get("text", "")
    if not isinstance(text, str) or text == "":
        return ""
    marks = dict((m, h) for m, h in _marks(node))
    if "code" in marks:
        out = _code_span(text)
    else:
        out = _escape(text)
        # Emphasis around spaces does not parse: keep the spaces outside of the markers.
        core = out.strip(" ")
        if core:
            lead = out[: len(out) - len(out.lstrip(" "))]
            trail = out[len(out.rstrip(" ")):]
            if "strike" in marks:
                core = "~~%s~~" % core
            if "em" in marks:
                # Inside `**`, an underscore keeps `***` from reading two ways.
                core = ("_%s_" if "strong" in marks else "*%s*") % core
            if "strong" in marks:
                core = "**%s**" % core
            out = lead + core + trail
    return out


def _code_span(text):
    run = 1
    while "`" * run in text:
        run += 1
    fence = "`" * run
    pad = " " if text.startswith("`") or text.endswith("`") else ""
    return fence + pad + text + pad + fence


def _url(href):
    return str(href).replace(" ", "%20").replace("(", "%28").replace(")", "%29")


def _escape(text):
    out = []
    for i, c in enumerate(text):
        if c in ESCAPE:
            out.append("\\" + c)
        elif c == "_":
            # An underscore inside a word never makes emphasis, so it stays as it is.
            before = text[i - 1] if i > 0 else " "
            after = text[i + 1] if i + 1 < len(text) else " "
            out.append("_" if before.isalnum() and after.isalnum() else "\\_")
        else:
            out.append(c)
    return "".join(out)


_STARTS = re.compile(r"^(#{1,6}(\s|$)|>|[-+](\s|$)|\d+[.)](\s|$)|(-{3,}|_{3,})\s*$|```|~~~)")


def _escape_starts(text):
    """A paragraph line that would read as a block marker gets a backslash."""
    lines = []
    for line in text.split("\n"):
        match = _STARTS.match(line)
        if match:
            digits = re.match(r"^\d+", line)
            if digits:
                line = digits.group(0) + "\\" + line[len(digits.group(0)):]
            else:
                line = "\\" + line
        lines.append(line)
    return "\n".join(lines)


# Markdown to ADF ------------------------------------------------------------------------------


_FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})\s*([^`\s]*)\s*$")
_HEADING = re.compile(r"^ {0,3}(#{1,6})(?:\s+(.*?))?\s*$")
_RULE = re.compile(r"^ {0,3}((\*\s*){3,}|(-\s*){3,}|(_\s*){3,})$")
_ITEM = re.compile(r"^( *)([-*+]|\d{1,9}[.)])( +|$)(.*)$")
_QUOTE = re.compile(r"^ {0,3}> ?(.*)$")


def to_adf(text):
    """The ADF document of a Markdown text."""
    lines = (text or "").replace("\r\n", "\n").replace("\r", "\n").replace("\t", "    ").split("\n")
    return {"type": "doc", "version": 1, "content": _parse_blocks(lines)}


def _parse_blocks(lines):
    blocks = []
    i = 0
    n = len(lines)
    while i < n:
        line = lines[i]
        if not line.strip():
            i += 1
            continue
        fence = _FENCE.match(line)
        if fence:
            marker = fence.group(1)
            body = []
            i += 1
            while i < n and not re.match(r"^ {0,3}" + re.escape(marker[0]) + "{%d,}\\s*$" % len(marker), lines[i]):
                body.append(lines[i])
                i += 1
            i += 1
            node = {"type": "codeBlock"}
            if fence.group(2):
                node["attrs"] = {"language": fence.group(2)}
            code = "\n".join(body)
            if code:
                node["content"] = [{"type": "text", "text": code}]
            blocks.append(node)
            continue
        heading = _HEADING.match(line)
        if heading:
            text = re.sub(r"\s+#+$", "", heading.group(2) or "")
            node = {"type": "heading", "attrs": {"level": len(heading.group(1))}}
            content = _parse_inline(text)
            if content:
                node["content"] = content
            blocks.append(node)
            i += 1
            continue
        if _RULE.match(line):
            blocks.append({"type": "rule"})
            i += 1
            continue
        if _QUOTE.match(line):
            inner = []
            while i < n and lines[i].strip() and _QUOTE.match(lines[i]):
                inner.append(_QUOTE.match(lines[i]).group(1))
                i += 1
            content = _parse_blocks(inner)
            blocks.append({"type": "blockquote", "content": content or [{"type": "paragraph"}]})
            continue
        item = _ITEM.match(line)
        if item and (item.group(4).strip() or item.group(3) == ""):
            node, i = _parse_list(lines, i)
            blocks.append(node)
            continue
        para = [line.strip()]
        i += 1
        while i < n and lines[i].strip() and not _starts_block(lines[i]):
            para.append(lines[i].strip())
            i += 1
        node = {"type": "paragraph"}
        content = _parse_inline("\n".join(para))
        if content:
            node["content"] = content
        blocks.append(node)
    return blocks


def _starts_block(line):
    item = _ITEM.match(line)
    return bool(
        _FENCE.match(line) or _HEADING.match(line) or _RULE.match(line) or _QUOTE.match(line)
        or (item and item.group(4).strip())
    )


def _parse_list(lines, i):
    first = _ITEM.match(lines[i])
    indent = len(first.group(1))
    ordered = first.group(2)[0].isdigit()
    node = {"type": "orderedList" if ordered else "bulletList", "content": []}
    if ordered:
        start = int(first.group(2)[:-1])
        if start != 1:
            node["attrs"] = {"order": start}
    n = len(lines)
    while i < n:
        item = _ITEM.match(lines[i])
        if not item or len(item.group(1)) != indent or item.group(2)[0].isdigit() != ordered:
            break
        width = len(item.group(1)) + len(item.group(2)) + max(len(item.group(3)), 1)
        body = [item.group(4)]
        i += 1
        while i < n:
            line = lines[i]
            if not line.strip():
                # A blank line belongs to the item when the next text line is inside it.
                j = i
                while j < n and not lines[j].strip():
                    j += 1
                if j < n and _indent(lines[j]) >= width:
                    body += [""] * (j - i)
                    i = j
                    continue
                sibling = _ITEM.match(lines[j]) if j < n else None
                if sibling and len(sibling.group(1)) == indent and sibling.group(2)[0].isdigit() == ordered:
                    # A loose list: the next item of the same list after a blank line.
                    i = j
                break
            if _indent(line) >= width:
                body.append(line[width:])
            elif _indent(line) > indent and _ITEM.match(line):
                body.append(line[_indent(line):])
            elif not _starts_block(line) and _indent(line) > indent:
                body.append(line.strip())
            else:
                break
            i += 1
        content = _parse_blocks(body) or [{"type": "paragraph"}]
        node["content"].append({"type": "listItem", "content": content})
    return node, i


def _indent(line):
    return len(line) - len(line.lstrip(" "))


def _parse_inline(text, marks=()):
    """The inline nodes of a text: text with marks, and a hard break for each line end."""
    nodes = []
    buf = []

    def flush():
        if buf:
            _add_text(nodes, "".join(buf), marks)
            del buf[:]

    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c == "\\" and i + 1 < n and text[i + 1] in ESCAPABLE:
            buf.append(text[i + 1])
            i += 2
            continue
        if c == "`":
            run = len(text[i:]) - len(text[i:].lstrip("`"))
            end = _find_code_end(text, i + run, run)
            if end is not None:
                flush()
                code = text[i + run:end]
                if len(code) >= 2 and code.startswith(" ") and code.endswith(" ") and code.strip():
                    code = code[1:-1]
                link = [m for m in marks if m[0] == "link"]
                _add_text(nodes, code, tuple(link) + (("code", None),))
                i = end + run
                continue
            buf.append(text[i:i + run])
            i += run
            continue
        if c == "[":
            link = _find_link(text, i)
            if link is not None:
                label, href, end = link
                flush()
                nodes += _parse_inline(label, marks + (("link", href),))
                i = end
                continue
        for delim, kinds in DELIMS:
            if text.startswith(delim, i) and _opens(text, i, delim):
                end = _find_close(text, i + len(delim), delim)
                if end is not None:
                    flush()
                    more = tuple((kind, None) for kind in kinds)
                    nodes += _parse_inline(text[i + len(delim):end], marks + more)
                    i = end + len(delim)
                    break
        else:
            buf.append(c)
            i += 1
            continue
    flush()
    return nodes


def _add_text(nodes, text, marks):
    """Adds text with marks; each line end becomes a hard break."""
    parts = text.split("\n")
    for k, part in enumerate(parts):
        if k > 0:
            nodes.append({"type": "hardBreak"})
        if part == "":
            continue
        node = {"type": "text", "text": part}
        adf_marks = []
        seen = set()
        for mark, href in marks:
            if mark in seen:
                continue
            seen.add(mark)
            if mark == "link":
                adf_marks.append({"type": "link", "attrs": {"href": href}})
            else:
                adf_marks.append({"type": mark})
        if adf_marks:
            node["marks"] = sorted(adf_marks, key=lambda m: ORDER.index(m["type"]))
        if nodes and nodes[-1].get("type") == "text" and nodes[-1].get("marks") == node.get("marks"):
            nodes[-1]["text"] += part
        else:
            nodes.append(node)


ORDER = ["link", "strong", "em", "strike", "code"]

DELIMS = (
    ("***", ("strong", "em")),
    ("___", ("strong", "em")),
    ("**", ("strong",)),
    ("__", ("strong",)),
    ("~~", ("strike",)),
    ("*", ("em",)),
    ("_", ("em",)),
)


def _opens(text, i, delim):
    after = text[i + len(delim)] if i + len(delim) < len(text) else " "
    if after.isspace():
        return False
    if delim[0] == "_":
        before = text[i - 1] if i > 0 else " "
        if before.isalnum():
            return False
    return True


def _find_close(text, start, delim):
    i = start
    n = len(text)
    while i < n:
        c = text[i]
        if c == "\\":
            i += 2
            continue
        if c == "`":
            run = len(text[i:]) - len(text[i:].lstrip("`"))
            end = _find_code_end(text, i + run, run)
            i = (end + run) if end is not None else i + run
            continue
        if text.startswith(delim, i) and i > start and not text[i - 1].isspace():
            after = text[i + len(delim)] if i + len(delim) < n else " "
            if delim[0] == "_" and after.isalnum():
                i += 1
                continue
            if len(delim) == 1 and after == delim:
                # `**` is not the end of `*`: skip the pair.
                i += 2
                continue
            return i
        i += 1
    return None


def _find_code_end(text, start, run):
    i = start
    while True:
        j = text.find("`" * run, i)
        if j < 0:
            return None
        end = j + run
        if (j == 0 or text[j - 1] != "`") and (end >= len(text) or text[end] != "`"):
            return j
        i = j + 1
        while i < len(text) and text[i] == "`":
            i += 1


def _find_link(text, i):
    depth = 0
    j = i
    n = len(text)
    while j < n:
        c = text[j]
        if c == "\\":
            j += 2
            continue
        if c == "[":
            depth += 1
        elif c == "]":
            depth -= 1
            if depth == 0:
                break
        j += 1
    else:
        return None
    if j + 1 >= n or text[j + 1] != "(":
        return None
    end = text.find(")", j + 2)
    if end < 0:
        return None
    href = text[j + 2:end].strip()
    if not href or " " in href or "\n" in href:
        return None
    return text[i + 1:j], href, end + 1
