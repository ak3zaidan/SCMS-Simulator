"""The model reference, the calibration page and the validation page.

All three are generated from one input: the card dump that `docs/site/tools/cardgen`
writes by building the engine's own registry and serialising every registration. The
registry is the thing the engine actually runs, so a model that is renamed, retired or
given a new default changes this site on the next build without anyone remembering to
edit prose (ADR 0007 decision 3).

If the dump is missing the pages still build, and say plainly that they are empty
because nobody ran the exporter. A documentation site that silently omits the model
reference is worse than one that says it is missing.
"""

import json
import os

from . import render
from .render import escape

__all__ = ["Catalogue", "load", "MISSING_HELP"]

MISSING_HELP = (
    "No card dump was found. Build it with <code>just --justfile docs/site/justfile "
    "cards</code> (which runs <code>docs/site/tools/cardgen</code>) and then rebuild "
    "the site. Until then this page lists nothing, which is a statement about this "
    "build and not about the engine."
)

# The card schema's own vocabulary, in the spelling `serde` writes (kebab-case).
SOURCE_KINDS = {
    "standard": ("standard", "Standard or clause"),
    "paper": ("paper", "Paper or report"),
    "datasheet": ("datasheet", "Vendor datasheet"),
    "dataset": ("dataset", "Published dataset"),
    "code": ("code", "Another implementation"),
    "todo-calibrate": ("todo", "Not yet cited"),
}

VALIDATION_STATUS = {
    "unvalidated": ("unvalidated", "Unvalidated", "Nothing has been checked."),
    "unit-tested": (
        "unit-tested",
        "Unit tested",
        "The implementation is tested against itself: invariants, edge cases, "
        "determinism. Nothing outside this repository has confirmed the numbers.",
    ),
    "literature-checked": (
        "literature-checked",
        "Literature checked",
        "Outputs were compared against published figures, tables or reference curves.",
    ),
    "field-checked": (
        "field-checked",
        "Field checked",
        "Outputs were compared against measurements from a real deployment.",
    ),
}

# Ordered worst-first, which is the order the validation page reads best in.
VALIDATION_ORDER = ["unvalidated", "unit-tested", "literature-checked", "field-checked"]

TIERS = ["abstract", "medium", "high"]


def _fmt_value(value):
    """A card default, which is arbitrary JSON, as compact readable text."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return "null"
    if isinstance(value, (int, float)):
        return repr(value)
    if isinstance(value, str):
        return value
    return json.dumps(value, sort_keys=True, separators=(", ", ": "))


def _source_html(source):
    if not isinstance(source, dict):
        return "<em>no source</em>"
    kind = source.get("kind", "")
    css, label = SOURCE_KINDS.get(kind, ("other", kind or "unknown"))
    out = render.badge(css, label) + " " + escape(source.get("ref", ""))
    if source.get("accessed"):
        out += ' <small class="dim">accessed ' + escape(source["accessed"]) + "</small>"
    if source.get("note"):
        out += '<br/><small class="dim">' + escape(source["note"]) + "</small>"
    return out


class Model:
    """One registration from the dump: a card plus how the registry holds it."""

    def __init__(self, entry):
        self.entry = entry
        self.card = entry.get("card", {})

    @property
    def id(self):
        return self.card.get("id", "")

    @property
    def version(self):
        return self.card.get("version", "")

    @property
    def family(self):
        return self.card.get("family", "unknown")

    @property
    def purpose(self):
        return self.card.get("purpose", "")

    @property
    def tiers(self):
        return self.card.get("tier", [])

    @property
    def parameters(self):
        return self.card.get("parameters", [])

    @property
    def validation(self):
        return self.card.get("validation") or {}

    @property
    def status(self):
        return self.validation.get("status", "unvalidated")

    @property
    def content_hash(self):
        return self.entry.get("content_hash", "")

    @property
    def anchor(self):
        return "m-" + self.id.replace("/", "-")

    def todo_parameters(self):
        """Parameters whose source is `todo-calibrate` (card rule R1)."""
        out = []
        for param in self.parameters:
            source = param.get("source") or {}
            if source.get("kind") == "todo-calibrate":
                out.append(param)
        return out

    def uncited_parameters(self):
        """Parameters with no source at all — which a valid card cannot have."""
        return [p for p in self.parameters if not (p.get("source") or {}).get("kind")]


class Catalogue:
    """Every registration in the dump, plus what the dump says about its own coverage."""

    def __init__(self, data, path):
        self.path = path
        self.data = data or {}
        self.models = [Model(e) for e in self.data.get("models", [])]
        self.models.sort(key=lambda m: m.id)

    @property
    def present(self):
        return bool(self.models)

    @property
    def stages(self):
        return self.data.get("stages", [])

    @property
    def uncovered(self):
        return self.data.get("coverage", {}).get("uncovered", [])

    @property
    def engine_version(self):
        return self.data.get("engine_version", "")

    def families(self):
        out = {}
        for model in self.models:
            out.setdefault(model.family, []).append(model)
        return sorted(out.items())

    def counts_by_status(self):
        out = dict((s, 0) for s in VALIDATION_ORDER)
        for model in self.models:
            out[model.status] = out.get(model.status, 0) + 1
        return out


def load(path):
    """Read a card dump, or return an empty catalogue when there is none."""
    if not os.path.isfile(path):
        return Catalogue(None, path)
    with open(path, "r", encoding="utf-8") as handle:
        data = json.load(handle)
    return Catalogue(data, path)


# --- pages ---------------------------------------------------------------------------


def _missing_banner():
    return '<div class="callout callout-warn"><p>' + MISSING_HELP + "</p></div>"


def _coverage_note(catalogue):
    if not catalogue.present:
        return ""
    parts = []
    failed = [s for s in catalogue.stages if not s.get("ok", True)]
    if failed:
        rows = []
        for stage in failed:
            rows.append(
                [
                    "<code>" + escape(stage.get("name", "")) + "</code>",
                    escape(stage.get("error", "")),
                ]
            )
        parts.append(
            '<div class="callout callout-warn"><p><strong>Some registration stages '
            "failed while the dump was made, so models they own are missing from this "
            "page.</strong></p>"
            + render.table(["Stage", "Error"], rows)
            + "</div>"
        )
    if catalogue.uncovered:
        items = "".join(
            "<li><code>" + escape(name) + "</code></li>" for name in catalogue.uncovered
        )
        parts.append(
            '<div class="callout"><p><strong>Known coverage gap.</strong> The exporter '
            "builds the registry through the crate-level registration entry points "
            "that exist today. These crates publish their cards through per-model "
            "constructors instead, so their models are not in this listing yet:</p><ul>"
            + items
            + "</ul><p>The gap is in the exporter, not in the cards: every model in "
            "those crates still carries a card, and the engine still validates and "
            "pins it at run time.</p></div>"
        )
    return "".join(parts)


def model_reference(catalogue):
    """The model reference: every registered model, its equations and its parameters."""
    if not catalogue.present:
        return _missing_banner()
    parts = [_coverage_note(catalogue)]
    parts.append(
        "<p>"
        + str(len(catalogue.models))
        + " registered models, grouped by the plug-in family they implement. Every "
        "number below is the default the card declares; a scenario overrides it by "
        "name, and the run manifest pins the card's content hash so a replay cannot "
        "silently use a different one.</p>"
    )

    index_rows = []
    for family, models in catalogue.families():
        links = ", ".join(
            '<a href="#' + m.anchor + '"><code>' + escape(m.id) + "</code></a>"
            for m in models
        )
        index_rows.append([escape(family), str(len(models)), links])
    parts.append("<h2 id=\"families\">Families<a class=\"anchor\" href=\"#families\">#</a></h2>")
    parts.append(render.table(["Family", "Models", "Ids"], index_rows))

    for family, models in catalogue.families():
        parts.append(
            '<h2 id="f-'
            + escape(family)
            + '">'
            + escape(family)
            + '<a class="anchor" href="#f-'
            + escape(family)
            + '">#</a></h2>'
        )
        for model in models:
            parts.append(_model_section(model))
    return "".join(parts)


def _model_section(model):
    parts = ['<section class="model" id="' + model.anchor + '">']
    parts.append(
        '<h3><code>'
        + escape(model.id)
        + "</code> <small>v"
        + escape(model.version)
        + '</small><a class="anchor" href="#'
        + model.anchor
        + '">#</a></h3>'
    )
    status_css, status_label, _ = VALIDATION_STATUS.get(
        model.status, ("unvalidated", model.status, "")
    )
    tier_text = ", ".join(model.tiers) if model.tiers else "none declared"
    chips = [render.badge("tier", "tier: " + tier_text)]
    chips.append(render.badge(status_css, status_label))
    determinism = model.card.get("determinism") or {}
    if determinism.get("uses_rng"):
        domains = ", ".join(determinism.get("rng_domains", [])) or "unnamed streams"
        chips.append(render.badge("rng", "draws: " + domains))
    else:
        chips.append(render.badge("norng", "no random draws"))
    todo = model.todo_parameters()
    if todo:
        chips.append(render.badge("todo", str(len(todo)) + " to calibrate"))
    parts.append('<p class="chips">' + " ".join(chips) + "</p>")
    parts.append("<p>" + escape(model.purpose) + "</p>")

    equations = model.card.get("equations") or []
    if equations:
        parts.append("<h4>Equations</h4>")
        for equation in equations:
            parts.append(
                '<figure class="equation"><pre><code>'
                + escape(equation.get("latex_or_text", ""))
                + "</code></pre><figcaption><strong>"
                + escape(equation.get("name", ""))
                + "</strong>"
                + (
                    " — " + escape(equation["notes"])
                    if equation.get("notes")
                    else ""
                )
                + "</figcaption></figure>"
            )

    if model.parameters:
        parts.append("<h4>Parameters</h4>")
        rows = []
        for param in model.parameters:
            name = "<code>" + escape(param.get("name", "")) + "</code>"
            unit = escape(param.get("unit", ""))
            default = "<code>" + escape(_fmt_value(param.get("default"))) + "</code>"
            rng = param.get("range")
            if rng:
                default += (
                    '<br/><small class="dim">range '
                    + escape(_fmt_value(rng))
                    + "</small>"
                )
            source = _source_html(param.get("source"))
            if param.get("calibration"):
                source += (
                    '<br/><small class="dim"><strong>Plan:</strong> '
                    + escape(param["calibration"])
                    + "</small>"
                )
            rows.append([name, unit, default, source])
        parts.append(
            render.table(["Parameter", "Unit", "Default", "Source"], rows, "params")
        )

    for key, heading, lead in (
        ("assumptions", "Assumptions", "What the model takes to be true."),
        (
            "limitations",
            "Limitations",
            "Where the model is known to be wrong or untested.",
        ),
        (
            "ignores",
            "What this tier ignores",
            "Effects the next tier up models and this one does not.",
        ),
    ):
        values = model.card.get(key) or []
        if values:
            parts.append("<h4>" + heading + "</h4>")
            parts.append('<p class="dim">' + lead + "</p><ul>")
            parts.extend("<li>" + escape(v) + "</li>" for v in values)
            parts.append("</ul>")

    sources = model.card.get("sources") or []
    if sources:
        parts.append("<h4>Sources</h4><ul>")
        parts.extend("<li>" + _source_html(s) + "</li>" for s in sources)
        parts.append("</ul>")

    tests = (model.validation.get("tests") or [])
    refs = (model.validation.get("references") or [])
    if tests or refs:
        parts.append("<h4>Validation</h4><ul>")
        for ref in refs:
            parts.append("<li>" + _source_html(ref) + "</li>")
        for test in tests:
            parts.append("<li><code>" + escape(test) + "</code></li>")
        parts.append("</ul>")

    cost = model.card.get("cost")
    if cost:
        text = []
        if cost.get("per_call_us") is not None:
            text.append(escape(str(cost["per_call_us"])) + " us per call")
        if cost.get("notes"):
            text.append(escape(cost["notes"]))
        parts.append("<h4>Cost</h4><p>" + " — ".join(text) + "</p>")

    parts.append(
        '<p class="hash"><small>card content hash <code>'
        + escape(model.content_hash)
        + "</code> · licence <code>"
        + escape(_fmt_value(model.entry.get("licence")))
        + "</code> · hosting <code>"
        + escape(_fmt_value(model.entry.get("hosting")))
        + "</code></small></p>"
    )
    parts.append("</section>")
    return "".join(parts)


def calibration_page(catalogue):
    """Every default that is still an implementer's guess, with its calibration plan."""
    if not catalogue.present:
        return _missing_banner()
    rows = []
    for model in catalogue.models:
        for param in model.todo_parameters():
            source = param.get("source") or {}
            rows.append(
                [
                    '<a href="models.html#'
                    + model.anchor
                    + '"><code>'
                    + escape(model.id)
                    + "</code></a>",
                    "<code>" + escape(param.get("name", "")) + "</code>",
                    escape(param.get("unit", "")),
                    "<code>" + escape(_fmt_value(param.get("default"))) + "</code>",
                    escape(source.get("ref", "")),
                    escape(param.get("calibration") or ""),
                ]
            )
    unciteds = []
    for model in catalogue.models:
        for param in model.uncited_parameters():
            unciteds.append(
                [
                    "<code>" + escape(model.id) + "</code>",
                    "<code>" + escape(param.get("name", "")) + "</code>",
                ]
            )

    parts = [_coverage_note(catalogue)]
    if not rows:
        parts.append(
            '<div class="callout callout-ok"><p>No registered model carries a '
            "<code>todo-calibrate</code> default in this build. That is a claim about "
            "the cards, not a claim that every number is right: a default can be cited "
            "to a source and still be the wrong choice for a given study. The "
            "<a href=\"validation.html\">validation page</a> says which models have "
            "been checked against anything outside this repository.</p></div>"
        )
    else:
        parts.append(
            "<p><strong>"
            + str(len(rows))
            + "</strong> parameter defaults across <strong>"
            + str(len(set(r[0] for r in rows)))
            + "</strong> models are values an implementer chose, not values a source "
            "supports. The card schema refuses such a default unless it carries a "
            "calibration plan, so every row below states how the number is meant to be "
            "replaced. Treat a result that is sensitive to one of these as "
            "provisional.</p>"
        )
        parts.append(
            render.table(
                ["Model", "Parameter", "Unit", "Current default", "What it stands for", "Calibration plan"],
                rows,
                "calibration",
            )
        )
    if unciteds:
        parts.append(
            '<div class="callout callout-warn"><h2>Parameters with no source at '
            "all</h2><p>A card carrying one of these cannot be registered, so their "
            "presence here means the dump was made by something other than the "
            "registry. Report it.</p>"
            + render.table(["Model", "Parameter"], unciteds)
            + "</div>"
        )
    return "".join(parts)


def _statuses_present(catalogue):
    """The schema's statuses, worst first, plus any status the dump carries that this
    generator does not know about.

    Filtering on a hard-coded list of labels is how a report quietly loses rows, so
    anything unrecognised is shown rather than skipped.
    """
    seen = [m.status for m in catalogue.models]
    extra = sorted(set(s for s in seen if s not in VALIDATION_ORDER))
    return [s for s in VALIDATION_ORDER if s in seen] + extra


def validation_page(catalogue):
    """What has been checked against something outside this repository, and what has not."""
    if not catalogue.present:
        return _missing_banner()
    counts = catalogue.counts_by_status()
    total = len(catalogue.models)
    summary_rows = []
    for status in _statuses_present(catalogue):
        css, label, meaning = VALIDATION_STATUS.get(
            status,
            (
                "unknown",
                status + " (unrecognised)",
                "This status is not one the card schema defines. Either the schema "
                "gained a value and this generator did not, or the dump is not a card "
                "dump. Do not read it as a check having been done.",
            ),
        )
        count = counts.get(status, 0)
        share = "%.0f%%" % (100.0 * count / total) if total else "-"
        summary_rows.append(
            [render.badge(css, label), str(count), share, escape(meaning)]
        )
    parts = [_coverage_note(catalogue)]
    parts.append(
        "<p>Validation status is a field on every model card, and the card is what the "
        "engine registers, so this table cannot drift from the code. It can, however, "
        "be optimistic: the status says what kind of check was done, not how hard the "
        "check was. Read it with the "
        '<a href="defects.html">defect register</a>, which records what independent '
        "verification found in models that were already marked as tested.</p>"
    )
    parts.append(
        render.table(
            ["Status", "Models", "Share", "What the status means"],
            summary_rows,
            "validation-summary",
        )
    )
    for status in _statuses_present(catalogue):
        models = [m for m in catalogue.models if m.status == status]
        if not models:
            continue
        css, label, _ = VALIDATION_STATUS.get(
            status, ("unknown", status + " (unrecognised)", "")
        )
        parts.append(
            '<h2 id="s-' + css + '">' + escape(label) + " — "
            + str(len(models))
            + ' models<a class="anchor" href="#s-' + css + '">#</a></h2>'
        )
        rows = []
        for model in models:
            evidence = []
            for ref in model.validation.get("references") or []:
                evidence.append(escape(ref.get("ref", "")))
            for test in model.validation.get("tests") or []:
                evidence.append("<code>" + escape(test) + "</code>")
            rows.append(
                [
                    '<a href="models.html#'
                    + model.anchor
                    + '"><code>'
                    + escape(model.id)
                    + "</code></a>",
                    escape(model.family),
                    ", ".join(model.tiers),
                    "<br/>".join(evidence) or '<em class="dim">nothing recorded</em>',
                ]
            )
        parts.append(
            render.table(["Model", "Family", "Tiers", "Evidence"], rows, "validation")
        )
    return "".join(parts)
