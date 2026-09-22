"""The defect register page, parsed from `docs/design/findings/`.

A research tool that hides its own bug history is harder to trust, not easier: the
reader's question is never "does this code have defects" but "what kind of defects does
this project find, and how". So the site publishes the registers rather than a summary
of them, and the summary it does build is derived from the same text.

The registers are prose written by different reviewers at different times, so the parser
is tolerant by construction. It recognises a finding by a heading that carries either a
severity word or a `file:line` location, and it **never drops a heading it cannot
classify** -- an unrecognised severity lands in its own group, labelled as
unrecognised. Filtering findings against a hard-coded list of severity words is exactly
how a report loses rows without anyone noticing.
"""

import os
import re

from . import md, render
from .render import escape

__all__ = ["load_registers", "page", "Finding", "Register"]

# Ranked worst-first. Unrecognised labels are kept and sorted after these, never
# dropped; see `_severity_of`.
SEVERITY_RANK = {
    "critical": 0,
    "high": 1,
    "major": 1,
    "medium": 2,
    "minor": 3,
    "low": 3,
    "info": 4,
    "note": 4,
    "retracted": 5,
}

SEVERITY_CSS = {
    "critical": "critical",
    "high": "high",
    "major": "high",
    "medium": "medium",
    "minor": "low",
    "low": "low",
    "info": "info",
    "note": "info",
    "retracted": "retracted",
}

_HEADING_RE = re.compile(r"^(#{1,6})\s+(.*?)\s*$")
_LABEL_RE = re.compile(r"[\[(]([A-Za-z][A-Za-z -]{1,20})[\])]")
_LOCATION_RE = re.compile(r"([\w./-]+\.(?:rs|ts|tsx|py|md|toml|json|yaml)):(\d+)")
_ID_RE = re.compile(r"^([A-Z]{1,3}\d+(?:\.\d+)?)\b")
_RETRACTED_RE = re.compile(r"\bRETRACTED\b")
_FIELD_RE = re.compile(r"^\*\*(?P<name>[A-Za-z ]+)[.:]\*\*\s*(?P<body>.*)$")


def _severity_of(heading):
    """`(label, rank, recognised)` for a finding heading."""
    if _RETRACTED_RE.search(heading):
        return ("retracted", SEVERITY_RANK["retracted"], True)
    for match in _LABEL_RE.finditer(heading):
        raw = match.group(1).strip().lower()
        if raw in SEVERITY_RANK:
            return (raw, SEVERITY_RANK[raw], True)
    for match in _LABEL_RE.finditer(heading):
        raw = match.group(1).strip().lower()
        # A bracketed word that is not a severity we know. Keep it visible.
        if raw and not raw.startswith("http"):
            return (raw, 90, False)
    return ("unlabelled", 95, False)


class Finding:
    def __init__(self, register, heading, level, body_lines, repo_root):
        self.register = register
        self.heading = heading
        self.level = level
        self.body = "\n".join(body_lines).strip()
        self.severity, self.rank, self.recognised = _severity_of(heading)
        ident = _ID_RE.match(heading)
        self.ident = ident.group(1) if ident else ""
        self.locations = []
        for match in _LOCATION_RE.finditer(heading + "\n" + self.body[:400]):
            path = match.group(1)
            marker = "SCMS-Simulator/"
            if marker in path:
                path = path.split(marker, 1)[1]
            self.locations.append((path, match.group(2)))
        self.crate = ""
        for path, _line in self.locations:
            if path.startswith("crates/"):
                self.crate = path.split("/")[1]
                break
            if path.startswith("ui/"):
                self.crate = "ui"
                break
        self.fields = self._fields()

    def _fields(self):
        out = {}
        current = None
        for line in self.body.split("\n"):
            match = _FIELD_RE.match(line.strip())
            if match:
                current = match.group("name").strip().lower()
                out[current] = [match.group("body")]
            elif current and line.strip():
                out[current].append(line.strip())
            elif current:
                current = None
        return dict((k, " ".join(v).strip()) for k, v in out.items())

    @property
    def title(self):
        """The heading with the severity label and the absolute paths taken out."""
        text = self.heading
        if self.ident:
            text = text[len(self.ident) :]
        text = _LABEL_RE.sub("", text)
        text = _LOCATION_RE.sub("", text)
        text = text.replace("/Users/", "").replace("SCMS-Simulator/", "")
        text = text.replace("`", "")
        return " ".join(text.split()).strip(" -\u2014:\u00b7") or (
            self.ident or "finding"
        )

    @property
    def summary(self):
        problem = self.fields.get("problem") or self.body
        problem = re.sub(r"\s+", " ", problem).strip()
        if len(problem) <= 200:
            return problem
        cut = problem[:200].rsplit(" ", 1)[0]
        return cut + "…"

    @property
    def anchor(self):
        base = self.ident or self.title
        return md.slugify(self.register.slug + "-" + base)


class Register:
    def __init__(self, path, repo_root):
        self.path = path
        self.rel = os.path.relpath(path, repo_root)
        self.slug = os.path.splitext(os.path.basename(path))[0]
        with open(path, "rb") as handle:
            raw = handle.read()
        # One of the registers carries a stray NUL byte, which makes `grep` treat it as
        # a binary file and skip it silently. Read bytes and clean, so this page cannot
        # inherit that failure.
        self.had_nul = b"\x00" in raw
        text = raw.replace(b"\x00", b"").decode("utf-8", "replace")
        self.lines = text.split("\n")
        self.title = self.lines[0].lstrip("# ").strip() if self.lines else self.slug
        self.intro, self.findings, self.other_sections = self._split(repo_root)

    def _split(self, repo_root):
        intro = []
        findings = []
        others = []
        index = 1
        current = None
        buffer = []
        heading = None
        level = 0
        while index < len(self.lines):
            line = self.lines[index]
            match = _HEADING_RE.match(line)
            if match:
                if heading is not None:
                    self._emit(heading, level, buffer, findings, others, repo_root)
                elif buffer:
                    intro.extend(buffer)
                heading = match.group(2)
                level = len(match.group(1))
                buffer = []
            else:
                buffer.append(line)
            index += 1
        if heading is not None:
            self._emit(heading, level, buffer, findings, others, repo_root)
        elif buffer:
            intro.extend(buffer)
        return "\n".join(intro).strip(), findings, others

    def _emit(self, heading, level, buffer, findings, others, repo_root):
        looks_like_finding = bool(
            _LOCATION_RE.search(heading)
            or _ID_RE.match(heading)
            or _LABEL_RE.search(heading)
        )
        if looks_like_finding:
            findings.append(Finding(self, heading, level, buffer, repo_root))
        else:
            others.append((heading, level, "\n".join(buffer).strip()))

    def counts(self):
        out = {}
        for finding in self.findings:
            out[finding.severity] = out.get(finding.severity, 0) + 1
        return out


def load_registers(directory, repo_root):
    """Every `.md` under the findings directory, oldest name first."""
    if not os.path.isdir(directory):
        return []
    names = sorted(n for n in os.listdir(directory) if n.endswith(".md"))
    return [Register(os.path.join(directory, n), repo_root) for n in names]


# --- the page -----------------------------------------------------------------------


def _severity_badge(finding):
    css = SEVERITY_CSS.get(finding.severity, "unknown")
    label = finding.severity
    if not finding.recognised:
        label = finding.severity + " (unrecognised)"
    return render.badge(css, label)


def _location_html(finding, repo_url):
    out = []
    for path, line in finding.locations[:2]:
        text = path + ":" + line
        if repo_url:
            href = repo_url + path + "#L" + line
            out.append('<a href="' + href + '"><code>' + escape(text) + "</code></a>")
        else:
            out.append("<code>" + escape(text) + "</code>")
    return "<br/>".join(out) or '<span class="dim">—</span>'


def page(registers, repo_url, link_rewriter=None):
    """The whole register: a roll-up, then every finding as its reviewer wrote it."""
    all_findings = []
    for register in registers:
        all_findings.extend(register.findings)
    if not all_findings:
        return (
            '<div class="callout callout-warn"><p>No registers were found under '
            "<code>docs/design/findings/</code>. That is almost certainly a build "
            "problem rather than a clean bill of health.</p></div>"
        )

    severities = sorted(
        set(f.severity for f in all_findings),
        key=lambda s: (SEVERITY_RANK.get(s, 90), s),
    )
    roll_rows = []
    for register in registers:
        counts = register.counts()
        cells = [
            '<a href="#r-' + escape(register.slug) + '">' + escape(register.title) + "</a>",
            str(len(register.findings)),
        ]
        for severity in severities:
            count = counts.get(severity, 0)
            cells.append(str(count) if count else '<span class="dim">·</span>')
        roll_rows.append(cells)
    totals = ["<strong>All registers</strong>", "<strong>" + str(len(all_findings)) + "</strong>"]
    for severity in severities:
        total = sum(1 for f in all_findings if f.severity == severity)
        totals.append("<strong>" + str(total) + "</strong>")
    roll_rows.append(totals)

    parts = []
    parts.append(
        '<h2 id="roll-up">What has been found so far'
        '<a class="anchor" href="#roll-up">#</a></h2>'
    )
    parts.append(
        "<p>Each row is one independent review. The reviewers were given the crate and "
        "the standards, not the builders' tests, and several of them wrote their own "
        "tooling — a second ASN.1 encoder, a second MCAP parser, an independent RFC "
        "6979 implementation — precisely so that a shared assumption could not hide a "
        "defect from both sides.</p>"
    )
    parts.append(
        render.table(
            ["Register", "Findings"] + [escape(s) for s in severities],
            roll_rows,
            "findings-rollup",
            ["left", "right"] + ["right"] * len(severities),
        )
    )

    unrecognised = [f for f in all_findings if not f.recognised]
    if unrecognised:
        parts.append(
            '<div class="callout"><p><strong>'
            + str(len(unrecognised))
            + " findings carry a label this generator does not rank.</strong> They are "
            "listed with their label as written rather than dropped, because a report "
            "that silently filters on a fixed set of severity words is a report that "
            "loses rows.</p></div>"
        )

    nul = [r for r in registers if r.had_nul]
    if nul:
        parts.append(
            '<div class="callout callout-warn"><p><strong>One register file contains a '
            "stray NUL byte</strong> (<code>"
            + "</code>, <code>".join(escape(r.rel) for r in nul)
            + "</code>), which makes <code>grep</code> treat it as binary and skip it "
            "without saying so. This page reads the file as bytes and strips the NUL, "
            "so nothing is missing here, but any grep-driven search over the registers "
            "will quietly miss that file until the byte is removed.</p></div>"
        )

    parts.append(
        '<h2 id="by-severity">Every finding, worst first'
        '<a class="anchor" href="#by-severity">#</a></h2>'
    )
    rows = []
    for finding in sorted(
        all_findings, key=lambda f: (f.rank, f.register.slug, f.ident)
    ):
        rows.append(
            [
                _severity_badge(finding),
                escape(finding.ident) or '<span class="dim">—</span>',
                "<code>" + escape(finding.crate or "—") + "</code>",
                '<a href="#' + finding.anchor + '">' + escape(finding.title) + "</a>",
                _location_html(finding, repo_url),
            ]
        )
    parts.append(
        render.table(
            ["Severity", "Id", "Component", "Finding", "Where"], rows, "findings"
        )
    )

    for register in registers:
        parts.append(
            '<h2 id="r-'
            + escape(register.slug)
            + '">'
            + escape(register.title)
            + '<a class="anchor" href="#r-'
            + escape(register.slug)
            + '">#</a></h2>'
        )
        parts.append(
            '<p class="dim">Source: <code>'
            + escape(register.rel)
            + "</code> — reproduced as its reviewer wrote it.</p>"
        )
        if register.intro:
            body, _ = md.render(register.intro, link_rewriter)
            parts.append(body)
        for heading, level, body_text in register.other_sections:
            parts.append(
                "<h3>" + md.inline(heading, link_rewriter) + "</h3>"
            )
            if body_text:
                body, _ = md.render(body_text, link_rewriter)
                parts.append(body)
        for finding in register.findings:
            parts.append('<section class="finding" id="' + finding.anchor + '">')
            parts.append(
                "<h3>"
                + _severity_badge(finding)
                + " "
                + (escape(finding.ident) + " " if finding.ident else "")
                + escape(finding.title)
                + "</h3>"
            )
            if finding.locations:
                parts.append(
                    '<p class="dim">' + _location_html(finding, repo_url) + "</p>"
                )
            body, _ = md.render(finding.body, link_rewriter)
            parts.append(body)
            parts.append("</section>")
    return "".join(parts)
