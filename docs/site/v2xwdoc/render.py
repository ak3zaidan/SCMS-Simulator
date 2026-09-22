"""The page shell: one template, one stylesheet, no JavaScript.

Every page the generator writes goes through `page()`, so the navigation, the
provenance footer and the "this page is generated" banner cannot differ between pages.

The site is deliberately static in the strongest sense: no client-side script, no web
fonts, no build-time network access. It opens from `file://` as well as from a web
server, which matters because the first reader of this site is usually someone who has
just cloned the repository and wants to know what the simulator does and does not
model.
"""

import html
import os

__all__ = ["Nav", "page", "badge", "table", "escape", "GENERATED_NOTE"]

GENERATED_NOTE = (
    "This page is generated from the repository. Editing it by hand will not survive "
    "the next build."
)


def escape(text):
    """HTML-escape a plain string."""
    return html.escape(str(text), quote=True)


class Nav:
    """The site navigation: sections of `(title, href)` entries."""

    def __init__(self, sections):
        self.sections = sections

    def render(self, current_href, depth):
        prefix = "../" * depth
        parts = ['<nav class="sidebar" aria-label="Site sections">']
        parts.append('<a class="brand" href="' + prefix + 'index.html">')
        parts.append("<span>V2X World Simulator</span>")
        parts.append("<small>documentation</small></a>")
        for title, entries in self.sections:
            parts.append("<h2>" + escape(title) + "</h2><ul>")
            for label, href, note in entries:
                cls = ' class="current"' if href == current_href else ""
                parts.append(
                    "<li><a"
                    + cls
                    + ' href="'
                    + prefix
                    + href
                    + '">'
                    + escape(label)
                    + "</a>"
                    + ('<small>' + escape(note) + "</small>" if note else "")
                    + "</li>"
                )
            parts.append("</ul>")
        parts.append("</nav>")
        return "".join(parts)


def _toc(headings, depth_limit=3):
    entries = [h for h in headings if 2 <= h[0] <= depth_limit]
    if len(entries) < 3:
        return ""
    parts = ['<aside class="toc" aria-label="On this page"><h2>On this page</h2><ul>']
    for level, text, anchor in entries:
        parts.append(
            '<li class="lvl-'
            + str(level)
            + '"><a href="#'
            + anchor
            + '">'
            + escape(text.replace("`", ""))
            + "</a></li>"
        )
    parts.append("</ul></aside>")
    return "".join(parts)


def page(
    out_dir,
    href,
    title,
    body,
    nav,
    headings=None,
    subtitle="",
    generated_from=None,
    stamp="",
):
    """Write one page and return its path.

    `generated_from` is the provenance line for a generated page: what in the
    repository the page was built out of. A hand-written page passes `None`.
    """
    depth = href.count("/")
    prefix = "../" * depth
    head = [
        "<!DOCTYPE html>",
        '<html lang="en">',
        "<head>",
        '<meta charset="utf-8"/>',
        '<meta name="viewport" content="width=device-width, initial-scale=1"/>',
        "<title>" + escape(title) + " — V2X World Simulator</title>",
        '<link rel="stylesheet" href="' + prefix + 'assets/site.css"/>',
        "</head>",
        "<body>",
        '<a class="skip" href="#main">Skip to content</a>',
    ]
    parts = list(head)
    parts.append(nav.render(href, depth))
    parts.append('<main id="main">')
    parts.append('<header class="page-head">')
    parts.append("<h1>" + escape(title) + "</h1>")
    if subtitle:
        parts.append('<p class="subtitle">' + escape(subtitle) + "</p>")
    if generated_from:
        parts.append(
            '<p class="provenance"><strong>Generated.</strong> '
            + GENERATED_NOTE
            + " Source: "
            + generated_from
            + "</p>"
        )
    parts.append("</header>")
    if headings:
        parts.append(_toc(headings))
    parts.append('<article class="prose">')
    parts.append(body)
    parts.append("</article>")
    parts.append('<footer class="page-foot">')
    parts.append(
        "<p>Built by <code>docs/site/build.py</code> from the repository working "
        "tree. The build takes no timestamp, so the same tree produces the same "
        "bytes.</p>"
    )
    if stamp:
        parts.append("<p>Tree: <code>" + escape(stamp) + "</code></p>")
    parts.append("</footer>")
    parts.append("</main></body></html>")

    path = os.path.join(out_dir, href)
    parent = os.path.dirname(path)
    if parent:
        os.makedirs(parent, exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write("\n".join(parts))
    return path


def badge(kind, text):
    """A small coloured label. `kind` is a CSS modifier, not free text."""
    return '<span class="badge badge-' + escape(kind) + '">' + escape(text) + "</span>"


def table(headers, rows, classes="", aligns=None):
    """A table from already-rendered cells; `headers` and cells are HTML."""
    aligns = aligns or ["left"] * len(headers)
    parts = ['<div class="table-wrap"><table class="' + classes + '"><thead><tr>']
    for n, header in enumerate(headers):
        parts.append('<th class="a-' + aligns[n] + '">' + header + "</th>")
    parts.append("</tr></thead><tbody>")
    for row in rows:
        parts.append("<tr>")
        for n, cell in enumerate(row):
            align = aligns[n] if n < len(aligns) else "left"
            parts.append('<td class="a-' + align + '">' + cell + "</td>")
        parts.append("</tr>")
    parts.append("</tbody></table></div>")
    return "".join(parts)
