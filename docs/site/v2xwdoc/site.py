"""Assembling the site: which pages exist, what each is built from, and the link rules.

A page is either hand-written Markdown under `content/`, a body generated from the
repository, or -- most of them -- a hand-written lead followed by a generated body. The
split matters: the prose says what a reader should take from the page, and the
generated part says what is actually in the code. Nobody has to remember to update the
second half.
"""

import os
import shutil

from . import cards as cards_mod
from . import findings as findings_mod
from . import md, render, scenario as scenario_mod

__all__ = ["build", "BuildResult"]

REPO_URL = "https://github.com/ak3zaidan/SCMS-Simulator/blob/main/"

SCHEMA_SOURCE = "crates/v2xw-engine/src/scenario/schema.rs"
FINDINGS_DIR = "docs/design/findings"

# Markdown files in the repository that the site itself renders, so a link to one of
# them points at the page rather than at GitHub.
PAGE_FOR_SOURCE = {
    "docs/design/02-architecture.md": "architecture.html",
    "docs/design/12-build-decisions.md": None,
}


class BuildResult:
    def __init__(self):
        self.pages = []
        self.warnings = []
        self.notes = []


def _link_rewriter(current_href):
    """Rewrite Markdown link targets for the generated site.

    Repository-relative links become links into the source tree on GitHub, because the
    site is a view onto the repository and a reader following a link wants the file.
    Anchors, absolute URLs and links to other site pages are left alone.
    """

    def rewrite(target):
        if target.startswith("#") or "://" in target or target.startswith("mailto:"):
            return target
        if target.endswith(".html"):
            return target
        cleaned = target.split("#")[0]
        anchor = target[len(cleaned) :]
        mapped = PAGE_FOR_SOURCE.get(cleaned)
        if mapped:
            return mapped + anchor
        return REPO_URL + cleaned.lstrip("./") + anchor

    return rewrite


def _nav():
    return render.Nav(
        [
            (
                "Start here",
                [
                    ("Overview", "index.html", ""),
                    ("Architecture", "architecture.html", ""),
                    ("Methodology", "methodology.html", "what each tier models"),
                ],
            ),
            (
                "Reference",
                [
                    ("Model reference", "models.html", "generated from cards"),
                    ("Scenario schema", "scenario.html", "generated from the loader"),
                    ("Glossary", "glossary.html", ""),
                ],
            ),
            (
                "Extending",
                [("Write a plug-in", "extending.html", "one interface, end to end")],
            ),
            (
                "Honesty",
                [
                    ("Calibration debt", "calibration.html", "uncited defaults"),
                    ("Validation status", "validation.html", "what was checked"),
                    ("Defect register", "defects.html", "what went wrong"),
                ],
            ),
            ("About", [("Building this site", "building.html", "")]),
        ]
    )


def _read_content(site_dir, name, repo_root):
    path = os.path.join(site_dir, "content", name)
    if not os.path.isfile(path):
        return ""
    with open(path, "r", encoding="utf-8") as handle:
        text = handle.read()
    return md.expand_includes(text, repo_root)


def build(repo_root, site_dir, out_dir, cards_path, stamp=""):
    """Build the whole site into `out_dir`. Returns a `BuildResult`."""
    result = BuildResult()
    os.makedirs(out_dir, exist_ok=True)

    catalogue = cards_mod.load(cards_path)
    if not catalogue.present:
        result.warnings.append(
            "no model-card dump at "
            + os.path.relpath(cards_path, repo_root)
            + ": the model reference, the calibration page and the validation page "
            "will say they are empty"
        )
    else:
        result.notes.append(
            str(len(catalogue.models)) + " models read from the card dump"
        )
        for stage in catalogue.stages:
            if not stage.get("ok", True):
                result.warnings.append(
                    "card dump stage failed: "
                    + str(stage.get("name"))
                    + ": "
                    + str(stage.get("error"))
                )

    schema_path = os.path.join(repo_root, SCHEMA_SOURCE)
    schema = None
    if os.path.isfile(schema_path):
        schema = scenario_mod.extract(schema_path)
        result.notes.append(
            str(len(schema.structs))
            + " scenario structs and "
            + str(len(schema.enums))
            + " enumerations read from "
            + SCHEMA_SOURCE
        )
    else:
        result.warnings.append("no scenario schema source at " + SCHEMA_SOURCE)

    registers = findings_mod.load_registers(
        os.path.join(repo_root, FINDINGS_DIR), repo_root
    )
    if registers:
        total = sum(len(r.findings) for r in registers)
        result.notes.append(
            str(total) + " findings read from " + str(len(registers)) + " registers"
        )
    else:
        result.warnings.append("no defect registers under " + FINDINGS_DIR)

    source_link = lambda rel: '<a href="' + REPO_URL + rel + '"><code>' + rel + "</code></a>"

    pages = [
        {
            "href": "index.html",
            "title": "V2X World Simulator",
            "subtitle": "A deterministic V2X simulator that states what it does not model.",
            "content": "index.md",
        },
        {
            "href": "architecture.html",
            "title": "Architecture",
            "subtitle": "What the engine is made of and what each part may touch.",
            "content": "architecture.md",
        },
        {
            "href": "methodology.html",
            "title": "Methodology",
            "subtitle": "The fidelity ladder, and what each tier deliberately ignores.",
            "content": "methodology.md",
        },
        {
            "href": "models.html",
            "title": "Model reference",
            "subtitle": "Every registered model, its equations, parameters and sources.",
            "content": "models.md",
            "body": lambda: cards_mod.model_reference(catalogue),
            "generated_from": "the engine's model registry, via "
            + source_link("docs/site/tools/cardgen"),
        },
        {
            "href": "scenario.html",
            "title": "Scenario schema",
            "subtitle": "Every field a scenario file may set, with its unit and default.",
            "content": "scenario.md",
            "body": (
                (lambda: scenario_mod.page(schema, SCHEMA_SOURCE))
                if schema
                else (
                    lambda: '<div class="callout callout-warn"><p>The schema source '
                    "was not found in this tree.</p></div>"
                )
            ),
            "generated_from": source_link(SCHEMA_SOURCE),
        },
        {
            "href": "extending.html",
            "title": "Writing a plug-in",
            "subtitle": "One interface, end to end: card, model, registration, test.",
            "content": "extending.md",
        },
        {
            "href": "calibration.html",
            "title": "Calibration debt",
            "subtitle": "Every default that is still an implementer's guess.",
            "content": "calibration.md",
            "body": lambda: cards_mod.calibration_page(catalogue),
            "generated_from": "the model registry's <code>todo-calibrate</code> report",
        },
        {
            "href": "validation.html",
            "title": "Validation status",
            "subtitle": "What has been checked against the world, and what has not.",
            "content": "validation.md",
            "body": lambda: cards_mod.validation_page(catalogue),
            "generated_from": "the validation field of every registered model card",
        },
        {
            "href": "defects.html",
            "title": "Defect register",
            "subtitle": "What independent review found, and what it teaches.",
            "content": "defects.md",
            "body": lambda: findings_mod.page(
                registers, REPO_URL, _link_rewriter("defects.html")
            ),
            "generated_from": source_link(FINDINGS_DIR + "/"),
        },
        {
            "href": "glossary.html",
            "title": "Glossary",
            "subtitle": "The terms this project uses, in the sense it uses them.",
            "content": "glossary.md",
        },
        {
            "href": "building.html",
            "title": "Building this site",
            "subtitle": "How the site is generated and how it is published.",
            "content": "building.md",
        },
    ]

    nav = _nav()
    for spec in pages:
        href = spec["href"]
        rewriter = _link_rewriter(href)
        lead_html = ""
        headings = []
        if spec.get("content"):
            text = _read_content(site_dir, spec["content"], repo_root)
            if text:
                lead_html, headings = md.render(text, rewriter)
            else:
                result.warnings.append("missing content file: " + spec["content"])
        body_html = spec["body"]() if spec.get("body") else ""
        path = render.page(
            out_dir,
            href,
            spec["title"],
            lead_html + body_html,
            nav,
            headings=headings,
            subtitle=spec.get("subtitle", ""),
            generated_from=spec.get("generated_from"),
            stamp=stamp,
        )
        result.pages.append(path)

    assets_src = os.path.join(site_dir, "assets")
    assets_dst = os.path.join(out_dir, "assets")
    if os.path.isdir(assets_src):
        if os.path.isdir(assets_dst):
            shutil.rmtree(assets_dst)
        shutil.copytree(assets_src, assets_dst)
    else:
        result.warnings.append("no assets directory at " + assets_src)
    return result
