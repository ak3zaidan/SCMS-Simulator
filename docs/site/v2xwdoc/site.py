"""Assembling the site: which pages exist, what each is built from, and the link rules.

A page is either hand-written Markdown under `content/`, a body generated from the
repository, or -- most of them -- a hand-written lead followed by a generated body. The
split matters: the prose says what a reader should take from the page, and the
generated part says what is actually in the code. Nobody has to remember to update the
second half.
"""

import os
import shutil

from . import campaign as campaign_mod
from . import cards as cards_mod
from . import findings as findings_mod
from . import gate as gate_mod
from . import md, render, scenario as scenario_mod

__all__ = ["build", "BuildResult"]

REPO_URL = "https://github.com/ak3zaidan/SCMS-Simulator/blob/main/"

SCHEMA_SOURCE = "crates/v2xw-engine/src/scenario/schema.rs"
FINDINGS_DIR = "docs/design/findings"
# The validation suite's output, which the campaign page joins against the registry. Its
# schema is documented in `v2xwdoc/campaign.py`; a missing file is reported, never skipped.
VALIDATION_RUNS = os.path.join("docs", "site", "generated", "validation-runs.json")

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
                [
                    ("Plug-in tutorial", "tutorial.html", "a detector, end to end"),
                    ("Write a plug-in", "extending.html", "the propagation family"),
                ],
            ),
            (
                "Honesty",
                [
                    ("Calibration debt", "calibration.html", "uncited defaults"),
                    ("Completeness gate", "gate.html", "the Phase 6 release gate"),
                    ("Validation status", "validation.html", "what was checked"),
                    ("Validation campaign", "campaign.html", "claims against measurements"),
                    ("Defect register", "defects.html", "what went wrong"),
                    ("Release readiness", "release.html", "what is not ready"),
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


def build(repo_root, site_dir, out_dir, cards_path, stamp="", runs_path=None):
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

    runs_path = runs_path or os.path.join(repo_root, VALIDATION_RUNS)
    runs = campaign_mod.load(runs_path)
    if runs.present:
        result.notes.append(
            str(len(runs.cases))
            + " validation case(s) read from "
            + os.path.relpath(runs_path, repo_root)
        )
        if not runs.schema_matches:
            result.warnings.append(
                "the validation-run document declares schema "
                + repr(runs.schema)
                + " and this generator reads "
                + repr(campaign_mod.CAMPAIGN_SCHEMA)
            )
    else:
        result.warnings.append(
            "no validation-run output at "
            + os.path.relpath(runs_path, repo_root)
            + ": the validation campaign page will report what the model cards claim "
            "and nothing about whether it was measured"
        )

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
            "href": "tutorial.html",
            "title": "Plug-in tutorial: a detector, end to end",
            "subtitle": "From an empty file to a model that passes the conformance kit.",
            "content": "tutorial.md",
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
            "href": "gate.html",
            "title": "Model-card completeness gate",
            "subtitle": "No uncited high-tier default without somebody measuring it.",
            "content": "gate.md",
            "body": lambda: gate_mod.page(
                catalogue, cards_mod.coverage_note(catalogue)
            ),
            "generated_from": "the gate's own verdict, computed by "
            + source_link("crates/v2xw-metrics/src/gate.rs")
            + " and carried in the card dump",
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
            "href": "campaign.html",
            "title": "Validation campaign",
            "subtitle": "Every model: what was checked, against what, and with what result.",
            "content": "campaign.md",
            "body": lambda: campaign_mod.page(
                catalogue, runs, registers, REPO_URL
            ),
            "generated_from": "the model registry, the validation-run output ("
            + source_link(VALIDATION_RUNS.replace(os.sep, "/"))
            + ") and the defect registers",
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
            "href": "release.html",
            "title": "Release readiness",
            "subtitle": "What must be true for a 1.0 tag, and what is not true yet.",
            "content": "release.md",
            "generated_from": source_link("docs/RELEASE-CHECKLIST.md")
            + ", included verbatim at build time",
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

    if catalogue.present:
        if not gate_mod.present(catalogue):
            result.warnings.append(
                "the card dump carries no gate verdict: rebuild it with "
                "`just --justfile docs/site/justfile cards` so the completeness gate page "
                "can say whether the gate passes"
            )
        elif gate_mod.failing(catalogue):
            # A warning, and therefore a `--strict` failure, for the same reason the
            # campaign's contradictions are: the roadmap states this as a release gate,
            # and a gate that only appears on a page nobody fails on is a preference.
            gate = catalogue.data["gate"]
            result.warnings.append(
                "model-card completeness gate: " + str(gate.get("summary") or "failed")
            )
        else:
            result.notes.append(
                "model-card completeness gate: " + str(catalogue.data["gate"].get("summary") or "passed")
            )

    if catalogue.present and runs.present:
        disagreements = campaign_mod.contradictions(catalogue, runs)
        refuted = [d for d in disagreements if d[1] == "claims-with-a-failing-case"]
        unsupported = [d for d in disagreements if d[1] == "claims-without-a-case"]
        # A refuted or unsupported claim is a warning and therefore a `--strict` failure:
        # 04-models.md §13's rule is that a failing case blocks its models from being
        # labelled checked, and a rule nothing enforces is a preference.
        for model, _kind, _cases in refuted:
            result.warnings.append(
                "validation campaign: " + model.id + " claims " + model.status
                + " and a validation case naming it FAILED"
            )
        for model, _kind, _cases in unsupported:
            result.warnings.append(
                "validation campaign: " + model.id + " claims " + model.status
                + " and no validation case names it"
            )
        stale = [d for d in disagreements if d[1] == "measured-but-not-claimed"]
        if stale:
            result.notes.append(
                str(len(stale))
                + " model(s) have a passing case and a card that has not caught up"
            )

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
