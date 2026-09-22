"""A small Markdown subset, rendered to HTML with the standard library only.

Why not a Markdown library: the documentation site is built from the repository by
people who have `python3` and nothing else, and a documentation toolchain that needs
its own dependency tree is one more thing that can rot between releases. The subset
below is everything the repository's own Markdown actually uses -- headings, fenced
code, tables, lists, block quotes, rules, links, inline code, bold and italic -- and
it is deliberately *not* CommonMark.

Two deliberate departures from CommonMark, both to avoid corrupting technical prose:

* **Underscores never mean emphasis.** `snake_case_identifiers` appear all over this
  repository's docs and CommonMark would italicise the middle of them. Only `*` does
  emphasis here.
* **Raw HTML is escaped, not passed through.** A documentation generator that forwards
  arbitrary HTML from a source file is a generator whose output cannot be reasoned
  about. The one exception is the block-level output this module produces itself.

The renderer also expands `{% include %}` directives (see `expand_includes`), which is
what lets a page quote a section of a design document instead of copying it. A copied
paragraph drifts; an included one cannot.
"""

import html
import os
import re

__all__ = ["render", "expand_includes", "slugify", "IncludeError"]


class IncludeError(Exception):
    """An `{% include %}` directive named a file or a heading that does not exist."""


# --- inline ------------------------------------------------------------------------

# One pass over the raw text, so a construct that contains another -- a link whose
# label is `code`, which this repository's prose is full of -- is handled as one thing.
# Escaping happens per fragment, after the structure is known, never before.
_INLINE_RE = re.compile(
    r"(?P<code>(?P<ticks>`+)(?P<codebody>.+?)(?P=ticks))"
    r"|(?P<link>\[(?P<label>(?:[^\[\]`]|`[^`]*`)*)\]"
    r"\((?P<href>[^()\s]+)(?:\s+\"(?P<title>[^\"]*)\")?\))"
    r"|(?P<auto><(?P<url>https?://[^>\s]+)>)",
    re.DOTALL,
)
_BOLD_RE = re.compile(r"\*\*(\S(?:.*?\S)?)\*\*", re.DOTALL)
# Underscores are never emphasis here: `snake_case` identifiers are everywhere in this
# repository's prose and CommonMark would italicise the middle of them.
_ITALIC_RE = re.compile(r"(?<!\*)\*(?!\s)([^*\n]+?)(?<!\s)\*(?!\*)")


def _escape(text):
    """HTML-escape, quotes included."""
    return html.escape(text, quote=True)


def _emphasis(text):
    text = _escape(text)
    text = _BOLD_RE.sub(lambda m: "<strong>" + m.group(1) + "</strong>", text)
    text = _ITALIC_RE.sub(lambda m: "<em>" + m.group(1) + "</em>", text)
    return text


def _anchor(href, label_html, title=None):
    external = "://" in href or href.startswith("mailto:")
    attrs = ' href="' + _escape(href) + '"'
    if title:
        attrs += ' title="' + _escape(title) + '"'
    if external:
        attrs += ' rel="noreferrer noopener" target="_blank"'
    return "<a" + attrs + ">" + label_html + "</a>"


def inline(text, link_rewriter=None, depth=0):
    """Render inline markup in `text`.

    `link_rewriter`, when given, is called with each link target and returns the target
    to emit; it is how a link to a `.md` file in the repository becomes a link to the
    generated page or to the source tree.
    """
    out = []
    pos = 0
    for match in _INLINE_RE.finditer(text):
        out.append(_emphasis(text[pos : match.start()]))
        pos = match.end()
        if match.group("code") is not None:
            out.append("<code>" + _escape(match.group("codebody").strip()) + "</code>")
        elif match.group("link") is not None:
            href = match.group("href")
            if link_rewriter is not None:
                href = link_rewriter(href)
            label = match.group("label")
            label_html = (
                inline(label, link_rewriter, depth + 1) if depth < 3 else _escape(label)
            )
            out.append(_anchor(href, label_html, match.group("title")))
        else:
            url = match.group("url")
            out.append(_anchor(url, _escape(url)))
    out.append(_emphasis(text[pos:]))
    return "".join(out)


# --- blocks ------------------------------------------------------------------------

_HEADING_RE = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
_FENCE_RE = re.compile(r"^(\s*)(```+|~~~+)\s*([A-Za-z0-9_+-]*)\s*$")
_RULE_RE = re.compile(r"^\s*(?:-{3,}|\*{3,}|_{3,})\s*$")
_LIST_RE = re.compile(r"^(?P<indent>[ \t]*)(?P<marker>[-*+]|\d+[.)])[ \t]+(?P<text>.*)$")
_TABLE_SEP_RE = re.compile(r"^\s*\|?(?:\s*:?-{2,}:?\s*\|)+\s*:?-{2,}:?\s*\|?\s*$")

_SLUG_STRIP = re.compile(r"[^a-z0-9]+")


def slugify(text):
    """A stable anchor id for a heading: lower case, non-alphanumerics collapsed."""
    text = re.sub(r"`|\*\*|\*", "", text).strip().lower()
    slug = _SLUG_STRIP.sub("-", text).strip("-")
    return slug or "section"


def _split_row(line):
    line = line.strip()
    if line.startswith("|"):
        line = line[1:]
    if line.endswith("|") and not line.endswith("\\|"):
        line = line[:-1]
    cells = []
    current = ""
    escaped = False
    for ch in line:
        if escaped:
            current += ch
            escaped = False
        elif ch == "\\":
            escaped = True
        elif ch == "|":
            cells.append(current.strip())
            current = ""
        else:
            current += ch
    cells.append(current.strip())
    return cells


def _alignments(sep_line):
    out = []
    for cell in _split_row(sep_line):
        left = cell.startswith(":")
        right = cell.endswith(":")
        if left and right:
            out.append("center")
        elif right:
            out.append("right")
        else:
            out.append("left")
    return out


class _Renderer:
    def __init__(self, link_rewriter=None, heading_offset=0):
        self.link_rewriter = link_rewriter
        self.heading_offset = heading_offset
        self.headings = []

    def _inline(self, text):
        return inline(text, self.link_rewriter)

    def render(self, text):
        lines = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
        out = []
        i = 0
        while i < len(lines):
            line = lines[i]
            if line.strip() == "":
                i += 1
                continue
            fence = _FENCE_RE.match(line)
            if fence:
                i = self._code(lines, i, out, fence)
                continue
            heading = _HEADING_RE.match(line)
            if heading:
                self._heading(heading, out)
                i += 1
                continue
            if _RULE_RE.match(line) and not _LIST_RE.match(line):
                out.append("<hr/>")
                i += 1
                continue
            if line.lstrip().startswith(">"):
                i = self._quote(lines, i, out)
                continue
            if _LIST_RE.match(line):
                i = self._list(lines, i, out)
                continue
            if "|" in line and i + 1 < len(lines) and _TABLE_SEP_RE.match(lines[i + 1]):
                i = self._table(lines, i, out)
                continue
            i = self._paragraph(lines, i, out)
        return "\n".join(out)

    def _heading(self, match, out):
        level = min(6, len(match.group(1)) + self.heading_offset)
        text = match.group(2)
        anchor = slugify(text)
        self.headings.append((level, text, anchor))
        out.append(
            '<h{lvl} id="{a}">{body}<a class="anchor" href="#{a}" '
            'aria-label="Link to this section">#</a></h{lvl}>'.format(
                lvl=level, a=anchor, body=self._inline(text)
            )
        )

    def _code(self, lines, i, out, fence):
        marker = fence.group(2)[0] * 3
        lang = fence.group(3)
        body = []
        i += 1
        while i < len(lines):
            closing = _FENCE_RE.match(lines[i])
            if closing and closing.group(2).startswith(marker) and not closing.group(3):
                i += 1
                break
            body.append(lines[i])
            i += 1
        code = _escape("\n".join(body))
        if lang == "mermaid":
            out.append(
                '<figure class="diagram"><pre><code class="language-mermaid">'
                + code
                + "</code></pre><figcaption>Diagram source (Mermaid). The site renders "
                "no JavaScript, so the source is shown as written in the design "
                "document.</figcaption></figure>"
            )
        else:
            cls = ' class="language-' + lang + '"' if lang else ""
            out.append("<pre><code" + cls + ">" + code + "</code></pre>")
        return i

    def _quote(self, lines, i, out):
        body = []
        while i < len(lines) and lines[i].lstrip().startswith(">"):
            stripped = lines[i].lstrip()[1:]
            body.append(stripped[1:] if stripped.startswith(" ") else stripped)
            i += 1
        inner = _Renderer(self.link_rewriter, self.heading_offset)
        out.append("<blockquote>" + inner.render("\n".join(body)) + "</blockquote>")
        self.headings.extend(inner.headings)
        return i

    def _table(self, lines, i, out):
        header = _split_row(lines[i])
        aligns = _alignments(lines[i + 1])
        i += 2
        rows = []
        while i < len(lines) and "|" in lines[i] and lines[i].strip():
            rows.append(_split_row(lines[i]))
            i += 1
        parts = ["<div class=\"table-wrap\"><table>", "<thead><tr>"]
        for n, cell in enumerate(header):
            align = aligns[n] if n < len(aligns) else "left"
            parts.append('<th class="a-' + align + '">' + self._inline(cell) + "</th>")
        parts.append("</tr></thead><tbody>")
        for row in rows:
            parts.append("<tr>")
            for n, cell in enumerate(row):
                align = aligns[n] if n < len(aligns) else "left"
                parts.append(
                    '<td class="a-' + align + '">' + self._inline(cell) + "</td>"
                )
            parts.append("</tr>")
        parts.append("</tbody></table></div>")
        out.append("".join(parts))
        return i

    def _collect_items(self, lines, start):
        items = []
        i = start
        while i < len(lines):
            line = lines[i]
            match = _LIST_RE.match(line)
            if match and not _RULE_RE.match(line):
                indent = len(match.group("indent").expandtabs(4))
                ordered = match.group("marker")[0] not in "-*+"
                items.append([indent, ordered, [match.group("text")]])
                i += 1
                continue
            if line.strip() == "":
                nxt = i + 1
                if nxt < len(lines) and _LIST_RE.match(lines[nxt]):
                    i += 1
                    continue
                break
            if items and (line.startswith("  ") or line.startswith("\t")):
                items[-1][2].append(line.strip())
                i += 1
                continue
            break
        return items, i

    def _list(self, lines, i, out):
        items, i = self._collect_items(lines, i)
        out.append(self._build_list(items))
        return i

    def _build_list(self, items):
        parts = []
        stack = []  # (indent, tag)
        for indent, ordered, text_lines in items:
            tag = "ol" if ordered else "ul"
            while stack and indent < stack[-1][0]:
                parts.append("</li></" + stack.pop()[1] + ">")
            if not stack or indent > stack[-1][0]:
                parts.append("<" + tag + ">")
                stack.append((indent, tag))
            else:
                parts.append("</li>")
            parts.append("<li>" + self._inline(" ".join(text_lines).strip()))
        while stack:
            parts.append("</li></" + stack.pop()[1] + ">")
        return "".join(parts)

    def _paragraph(self, lines, i, out):
        body = []
        while i < len(lines):
            line = lines[i]
            if line.strip() == "":
                break
            if _HEADING_RE.match(line) or _FENCE_RE.match(line):
                break
            if _LIST_RE.match(line) or _RULE_RE.match(line):
                break
            if line.lstrip().startswith(">"):
                break
            if "|" in line and i + 1 < len(lines) and _TABLE_SEP_RE.match(lines[i + 1]):
                break
            body.append(line.strip())
            i += 1
        if body:
            out.append("<p>" + self._inline(" ".join(body)) + "</p>")
        return i


def render(text, link_rewriter=None, heading_offset=0):
    """Render `text`; returns `(html, headings)` with headings as `(level, text, id)`."""
    renderer = _Renderer(link_rewriter, heading_offset)
    body = renderer.render(text)
    return body, renderer.headings


# --- includes ----------------------------------------------------------------------

_INCLUDE_RE = re.compile(
    r"^\{%\s*include\s+(?P<path>[^\s#%]+)"
    r"(?:#(?P<heading>[^%|]+?))?"
    r"(?:\|\s*demote\s*=\s*(?P<demote>\d+)\s*)?%\}\s*$"
)


def _section_of(text, heading_prefix):
    """The lines of the section whose heading starts with `heading_prefix`."""
    lines = text.split("\n")
    wanted = heading_prefix.strip().lower()
    start = None
    level = 0
    for n, line in enumerate(lines):
        match = _HEADING_RE.match(line)
        if not match:
            continue
        title = match.group(2).strip().lower()
        if start is None and title.startswith(wanted):
            start = n
            level = len(match.group(1))
        elif start is not None and len(match.group(1)) <= level:
            return lines[start:n]
    if start is None:
        return None
    return lines[start:]


def _demote(lines, levels):
    if levels <= 0:
        return lines
    out = []
    in_fence = False
    for line in lines:
        if _FENCE_RE.match(line):
            in_fence = not in_fence
        match = None if in_fence else _HEADING_RE.match(line)
        if match:
            hashes = "#" * min(6, len(match.group(1)) + levels)
            out.append(hashes + " " + match.group(2))
        else:
            out.append(line)
    return out


def expand_includes(text, repo_root, depth=0):
    """Replace `{% include path[#Heading] [| demote=n] %}` lines with the source text.

    The path is relative to the repository root, so a page cites a design document by
    the same path a reader would open. A missing file or heading raises `IncludeError`
    rather than silently emitting nothing: a documentation build that quietly drops a
    section is how a site starts lying about what it contains.
    """
    if depth > 4:
        raise IncludeError("include nesting deeper than 4 levels")
    out = []
    for line in text.split("\n"):
        match = _INCLUDE_RE.match(line.strip())
        if not match:
            out.append(line)
            continue
        rel = match.group("path")
        path = os.path.join(repo_root, rel)
        if not os.path.isfile(path):
            raise IncludeError("include: no such file: " + rel)
        with open(path, "r", encoding="utf-8") as handle:
            source = handle.read()
        heading = match.group("heading")
        if heading:
            section = _section_of(source, heading)
            if section is None:
                raise IncludeError(
                    "include: " + rel + " has no heading starting with " + repr(heading)
                )
        else:
            section = source.split("\n")
        section = _demote(section, int(match.group("demote") or 0))
        body = expand_includes("\n".join(section), repo_root, depth + 1)
        where = "`" + rel + "`"
        if heading:
            where += " \u00a7" + heading.strip()
        out.append("")
        out.append("> Included verbatim from " + where + " at build time. Edit that")
        out.append("> file, not this page.")
        out.append("")
        out.append(body)
        out.append("")
    return "\n".join(out)
