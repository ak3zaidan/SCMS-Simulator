"""The validation campaign report: for every model, what has been checked, against
what, and with what result.

10-roadmap.md Phase 6 asks for a "validation campaign report". This module is it, and
the requirement that shapes every line below is that it must be **generated, so it
cannot go stale**. It therefore joins three inputs and reports where they disagree:

1. **The registry**, through the model-card dump (`cards.py`): every model's declared
   `validation.status`, the references it names and the tests it names. This is what the
   code *claims*.
2. **The run outputs**: a validation-run document written by the validation suite
   (04-models.md §13), one record per `ValidationCase` with its target, its tolerance,
   what was observed and whether it passed. This is what was *measured*.
3. **The defect registers** under `docs/design/findings/` (`findings.py`): what
   independent review found. This is what was *wrong anyway*.

The third input is the one that makes the page worth reading. Every defect on the
register was found in code whose own test suite was green, so a model marked
`unit-tested` with three defects against it is a different object from a model marked
`unit-tested` with none, and no single field can say so.

# The contradiction table is the point

A card that claims `literature-checked` while no validation case names its model, or
while every case naming it failed, is **claiming more than the runs support**. That is
the failure mode a hand-written report has: it was true when it was written. This page
lists every such contradiction at the top, and a build with `--strict` fails on one.

# The run-output schema

`docs/site/generated/validation-runs.json`, schema `v2xw/validation-campaign/1`:

    {
      "schema": "v2xw/validation-campaign/1",
      "engine_version": "0.1.0",
      "cases": [
        {
          "id": "pdr-vs-distance-urban-medium",
          "models": ["radio/propagation/log-distance-shadowing"],
          "scenario": "scenarios/pdr-vs-distance.yaml",
          "statistic": "pdr in the 100-125 m bin",
          "target": 0.62,
          "tolerance": 0.05,
          "tolerance_kind": "absolute",
          "observed": 0.58,
          "outcome": "pass",
          "source": "Bai and Krishnan 2006, Fig. 4 (R3 §H.1)",
          "seeds": 10,
          "plan_digest": "3f2a...",
          "note": "digitisation uncertainty +-0.3 dB is folded into the tolerance"
        }
      ]
    }

Every field but `id` and `models` is optional and the page says which are missing rather
than filling them in. `outcome` is one of `pass`, `fail`, `disabled` or `not-run`;
anything else is shown as-is under an "unrecognised" label, because filtering findings
against a hard-coded vocabulary is how a report loses rows in silence.
"""

import json
import os

from . import render
from .cards import VALIDATION_STATUS, VALIDATION_ORDER
from .render import escape

__all__ = [
    "Campaign",
    "Case",
    "load",
    "page",
    "contradictions",
    "MISSING_RUNS_HELP",
    "CAMPAIGN_SCHEMA",
]

CAMPAIGN_SCHEMA = "v2xw/validation-campaign/1"

MISSING_RUNS_HELP = (
    "No validation-run document was found. The campaign page then reports the "
    "<em>claims</em> on the model cards and nothing about whether they were measured, "
    "which is exactly the kind of half-report this page exists to replace. Produce one "
    "by running the validation suite and writing "
    "<code>docs/site/generated/validation-runs.json</code> in the schema documented in "
    "<code>docs/site/v2xwdoc/campaign.py</code>, then rebuild."
)

# Outcome vocabulary, with the CSS modifier and what each one means.
OUTCOMES = {
    "pass": ("ok", "Pass", "The statistic was inside the target's tolerance."),
    "fail": (
        "fail",
        "Fail",
        "The statistic was outside the tolerance. The model must not be labelled "
        "literature-checked while this stands.",
    ),
    "disabled": (
        "disabled",
        "Disabled",
        "The case exists and its target is UNVERIFIED, so it is carried as a disabled "
        "row rather than deleted — the report then shows what is missing instead of "
        "hiding it (04-models.md §13).",
    ),
    "not-run": (
        "notrun",
        "Not run",
        "The case is defined and this campaign did not execute it.",
    ),
}

# Outcomes worst-first, which is the order the page reads best in.
OUTCOME_ORDER = ["fail", "disabled", "not-run", "pass"]

# The statuses that assert a check against something outside this repository.
EXTERNAL_STATUSES = ("literature-checked", "field-checked")


class Case:
    """One validation case as the run output recorded it."""

    def __init__(self, record):
        self.record = record if isinstance(record, dict) else {}

    @property
    def id(self):
        return str(self.record.get("id") or "")

    @property
    def models(self):
        models = self.record.get("models")
        if isinstance(models, str):
            return [models]
        if isinstance(models, list):
            return [str(m) for m in models]
        return []

    @property
    def outcome(self):
        return str(self.record.get("outcome") or "not-run")

    @property
    def recognised(self):
        return self.outcome in OUTCOMES

    @property
    def source(self):
        return str(self.record.get("source") or "")

    @property
    def statistic(self):
        return str(self.record.get("statistic") or "")

    @property
    def scenario(self):
        return str(self.record.get("scenario") or "")

    @property
    def note(self):
        return str(self.record.get("note") or "")

    @property
    def plan_digest(self):
        return str(self.record.get("plan_digest") or "")

    @property
    def seeds(self):
        value = self.record.get("seeds")
        return value if isinstance(value, int) else None

    def numbers(self):
        """`(target, tolerance, observed)` as text, with a dash for a missing one.

        Rendered rather than compared: whether the case passed is the suite's verdict and
        not this generator's, and recomputing it here would let the page disagree with the
        run that produced it.
        """

        def fmt(key):
            value = self.record.get(key)
            if isinstance(value, bool) or value is None:
                return "&mdash;"
            if isinstance(value, (int, float)):
                return escape(repr(value))
            return escape(str(value))

        kind = self.record.get("tolerance_kind")
        tolerance = fmt("tolerance")
        if kind and tolerance != "&mdash;":
            tolerance += ' <small class="dim">' + escape(str(kind)) + "</small>"
        return fmt("target"), tolerance, fmt("observed")

    def missing_fields(self):
        """Which documented fields this record left out."""
        wanted = ("statistic", "target", "tolerance", "observed", "source", "scenario")
        return [f for f in wanted if not self.record.get(f) and self.record.get(f) != 0]


class Campaign:
    """The validation-run document, or the honest absence of one."""

    def __init__(self, data, path):
        self.path = path
        self.data = data or {}
        self.cases = [Case(r) for r in self.data.get("cases", [])]
        self.cases.sort(key=lambda c: c.id)

    @property
    def present(self):
        return bool(self.cases)

    @property
    def schema(self):
        return str(self.data.get("schema") or "")

    @property
    def schema_matches(self):
        """True when the document declares the schema this generator reads.

        A document with another schema is still shown — dropping it would be the silent
        loss this module's docstring warns about — but the page says the version it read
        was not the version it expected.
        """
        return self.schema == CAMPAIGN_SCHEMA

    @property
    def engine_version(self):
        return str(self.data.get("engine_version") or "")

    def by_model(self):
        """`model id -> [Case]`, in case-id order."""
        out = {}
        for case in self.cases:
            for model in case.models:
                out.setdefault(model, []).append(case)
        return out

    def counts_by_outcome(self):
        out = {}
        for case in self.cases:
            out[case.outcome] = out.get(case.outcome, 0) + 1
        return out

    def outcomes_present(self):
        """The outcomes this document carries, worst first, unrecognised ones last.

        The same rule `findings.py` keeps for severity labels: an outcome this generator
        does not know is shown rather than dropped.
        """
        seen = [c.outcome for c in self.cases]
        extra = sorted(set(o for o in seen if o not in OUTCOME_ORDER))
        return [o for o in OUTCOME_ORDER if o in seen] + extra


def load(path):
    """Read a validation-run document, or return an empty campaign."""
    if not os.path.isfile(path):
        return Campaign(None, path)
    with open(path, "r", encoding="utf-8") as handle:
        data = json.load(handle)
    return Campaign(data, path)


# --- the join ------------------------------------------------------------------------


def findings_for(model_id, registers):
    """Every finding whose text names `model_id`.

    A substring match on the heading and the body. Blunt, and deliberately so: the
    alternative is a structured field on every finding that a reviewer has to remember to
    fill in, and a register written by a human reviewer will not have it. A false positive
    here costs a reader one click; a false negative hides a defect, which is the failure
    that matters.
    """
    out = []
    for register in registers:
        for finding in register.findings:
            haystack = finding.heading + "\n" + finding.body
            if model_id and model_id in haystack:
                out.append(finding)
    return out


def contradictions(catalogue, campaign):
    """Every model whose card claims more than the runs support.

    Three kinds, and they are different problems:

    * `claims-without-a-case` — the card says `literature-checked` or `field-checked` and
      no validation case names the model at all. Nothing measured it.
    * `claims-with-a-failing-case` — a case names it and failed. The claim is refuted, not
      merely unsupported.
    * `measured-but-not-claimed` — a case passed and the card still says `unvalidated` or
      `unit-tested`. Harmless to a reader and still worth listing: it means the card was
      not updated when the campaign ran, so the *other* direction of drift is happening
      too and the next claim may be equally stale.
    """
    by_model = campaign.by_model()
    out = []
    for model in catalogue.models:
        cases = by_model.get(model.id, [])
        outcomes = [c.outcome for c in cases]
        claims_external = model.status in EXTERNAL_STATUSES
        if claims_external and not cases:
            out.append((model, "claims-without-a-case", cases))
        elif claims_external and "fail" in outcomes:
            out.append((model, "claims-with-a-failing-case", cases))
        elif not claims_external and "pass" in outcomes:
            out.append((model, "measured-but-not-claimed", cases))
    return out


CONTRADICTION_TEXT = {
    "claims-without-a-case": (
        "warn",
        "Claims an external check, and no case measured it",
        "The card asserts a check against something outside this repository and no "
        "validation case names the model. Either the case is missing from the campaign or "
        "the status is wrong; both are fixable and neither is fixable by reading the card.",
    ),
    "claims-with-a-failing-case": (
        "fail",
        "Claims an external check, and a case failed",
        "The claim is refuted rather than unsupported. Until the case passes or the "
        "target is corrected, this model must not be labelled as checked.",
    ),
    "measured-but-not-claimed": (
        "ok",
        "Measured, and the card has not caught up",
        "A case passed and the card still records a weaker status. The number is better "
        "than the claim, which is the safe direction — but it shows the card is not being "
        "updated when the campaign runs.",
    ),
}


# --- the page ------------------------------------------------------------------------


def _missing_runs_banner():
    return '<div class="callout callout-warn"><p>' + MISSING_RUNS_HELP + "</p></div>"


def page(catalogue, campaign, registers, repo_url=""):
    """The whole campaign report."""
    parts = []

    if not catalogue.present:
        from .cards import MISSING_HELP

        parts.append('<div class="callout callout-warn"><p>' + MISSING_HELP + "</p></div>")
        return "".join(parts)

    if not campaign.present:
        parts.append(_missing_runs_banner())
    elif not campaign.schema_matches:
        parts.append(
            '<div class="callout callout-warn"><p>The validation-run document declares '
            "schema <code>"
            + escape(campaign.schema or "(none)")
            + "</code> and this generator reads <code>"
            + escape(CAMPAIGN_SCHEMA)
            + "</code>. Its rows are shown below anyway — dropping them would lose the "
            "campaign silently — but a field this generator does not know about is not "
            "displayed, so read the document itself before quoting this page.</p></div>"
        )

    parts.append(_summary(catalogue, campaign))
    parts.append(_contradictions_section(catalogue, campaign))
    parts.append(_cases_section(campaign))
    parts.append(_per_model_section(catalogue, campaign, registers, repo_url))
    return "".join(parts)


def _summary(catalogue, campaign):
    total = len(catalogue.models)
    counts = catalogue.counts_by_status()
    external = sum(counts.get(s, 0) for s in EXTERNAL_STATUSES)
    outcome_counts = campaign.counts_by_outcome()
    passed = outcome_counts.get("pass", 0)
    failed = outcome_counts.get("fail", 0)

    rows = [
        [
            "Registered models",
            str(total),
            "Every model the engine's registry holds, from the card dump.",
        ],
        [
            "Models whose card claims an external check",
            str(external),
            "<code>literature-checked</code> or <code>field-checked</code>: a comparison "
            "against a published figure or a measurement.",
        ],
        [
            "Models a validation case names",
            str(len(campaign.by_model())),
            "How many of them the campaign actually measured.",
        ],
        [
            "Validation cases",
            str(len(campaign.cases)),
            "One row per <code>ValidationCase</code> of 04-models.md §13.",
        ],
        [
            "Cases passed",
            str(passed),
            "Inside the stated tolerance.",
        ],
        [
            "Cases failed",
            str(failed),
            "Outside it. A failing case blocks its models' tier from being labelled "
            "checked.",
        ],
    ]
    out = [
        "<p>This page is generated from three inputs — the model registry, the "
        "validation-run output and the defect registers — and it reports where they "
        "<em>disagree</em>. A hand-written validation report is true on the day it is "
        "written; this one is true on the day it is built, and the "
        "<a href=\"#contradictions\">contradiction table</a> is what makes the "
        "difference visible.</p>"
    ]
    if campaign.engine_version:
        out.append(
            '<p class="dim">Campaign run against engine <code>'
            + escape(campaign.engine_version)
            + "</code>.</p>"
        )
    out.append(render.table(["Quantity", "Count", "What it is"], rows, "campaign-summary"))
    return "".join(out)


def _contradictions_section(catalogue, campaign):
    found = contradictions(catalogue, campaign)
    out = [
        '<h2 id="contradictions">Where the claims and the measurements disagree'
        '<a class="anchor" href="#contradictions">#</a></h2>'
    ]
    if not campaign.present:
        out.append(
            '<div class="callout"><p>No validation-run document, so there is nothing to '
            "compare the cards against and this table is empty for a reason that is about "
            "this build rather than about the engine. It is <strong>not</strong> a clean "
            "bill of health.</p></div>"
        )
        return "".join(out)
    if not found:
        out.append(
            '<div class="callout callout-ok"><p>Every card\'s validation status is '
            "consistent with the campaign's outcomes: no model claims an external check "
            "that no case measured, no claim is refuted by a failing case, and no passing "
            "case is missing from the card it supports.</p></div>"
        )
        return "".join(out)

    out.append(
        "<p><strong>"
        + str(len(found))
        + "</strong> model(s) whose card and whose measurements say different things. "
        "Each row names which of the three disagreements it is.</p>"
    )
    rows = []
    # Worst first: a refuted claim, then an unsupported one, then a stale card.
    order = {
        "claims-with-a-failing-case": 0,
        "claims-without-a-case": 1,
        "measured-but-not-claimed": 2,
    }
    for model, kind, cases in sorted(found, key=lambda f: (order.get(f[1], 9), f[0].id)):
        css, label, meaning = CONTRADICTION_TEXT[kind]
        status_css, status_label, _ = VALIDATION_STATUS.get(
            model.status, ("unvalidated", model.status, "")
        )
        case_text = (
            ", ".join("<code>" + escape(c.id) + "</code>" for c in cases)
            or '<em class="dim">none</em>'
        )
        rows.append(
            [
                '<a href="models.html#'
                + model.anchor
                + '"><code>'
                + escape(model.id)
                + "</code></a>",
                render.badge(status_css, status_label),
                render.badge(css, label),
                case_text,
                escape(meaning),
            ]
        )
    out.append(
        render.table(
            ["Model", "Card says", "Disagreement", "Cases", "What it means"],
            rows,
            "contradictions",
        )
    )
    return "".join(out)


def _cases_section(campaign):
    out = [
        '<h2 id="cases">Every validation case<a class="anchor" href="#cases">#</a></h2>'
    ]
    if not campaign.present:
        out.append('<p class="dim">No cases: there is no run document.</p>')
        return "".join(out)

    legend = []
    for outcome in campaign.outcomes_present():
        css, label, meaning = OUTCOMES.get(
            outcome,
            (
                "unknown",
                outcome + " (unrecognised)",
                "This outcome is not one this generator knows. It is shown rather than "
                "dropped; do not read it as a pass.",
            ),
        )
        legend.append(
            [
                render.badge(css, label),
                str(campaign.counts_by_outcome().get(outcome, 0)),
                escape(meaning),
            ]
        )
    out.append(render.table(["Outcome", "Cases", "What it means"], legend, "outcomes"))

    rows = []
    rank = {o: i for i, o in enumerate(OUTCOME_ORDER)}
    for case in sorted(campaign.cases, key=lambda c: (rank.get(c.outcome, 9), c.id)):
        css, label, _ = OUTCOMES.get(
            case.outcome, ("unknown", case.outcome + " (unrecognised)", "")
        )
        target, tolerance, observed = case.numbers()
        models = (
            "<br/>".join("<code>" + escape(m) + "</code>" for m in case.models)
            or '<em class="dim">no model named</em>'
        )
        detail = escape(case.statistic) or '<em class="dim">not stated</em>'
        if case.scenario:
            detail += '<br/><small class="dim">' + escape(case.scenario) + "</small>"
        if case.seeds is not None:
            detail += '<br/><small class="dim">' + str(case.seeds) + " seed(s)</small>"
        if case.plan_digest:
            detail += (
                '<br/><small class="dim">plan <code>'
                + escape(case.plan_digest[:12])
                + "</code></small>"
            )
        source = escape(case.source) or '<em class="dim">no source cited</em>'
        if case.note:
            source += '<br/><small class="dim">' + escape(case.note) + "</small>"
        missing = case.missing_fields()
        if missing:
            source += (
                '<br/><small class="dim">record omits: '
                + escape(", ".join(missing))
                + "</small>"
            )
        rows.append(
            [
                "<code>" + escape(case.id) + "</code>",
                render.badge(css, label),
                models,
                detail,
                observed,
                target,
                tolerance,
                source,
            ]
        )
    out.append(
        render.table(
            [
                "Case",
                "Outcome",
                "Models",
                "What was measured",
                "Observed",
                "Target",
                "Tolerance",
                "Against what",
            ],
            rows,
            "cases",
        )
    )
    out.append(
        '<p class="dim">The verdict in the outcome column is the <em>suite\'s</em>, not '
        "this page's: the observed value and the tolerance are printed for a reader to "
        "check, and nothing here recomputes the comparison. A page that re-derived the "
        "verdict could disagree with the run that produced it, which would be worse than "
        "either answer alone.</p>"
    )
    return "".join(out)


def _per_model_section(catalogue, campaign, registers, repo_url):
    by_model = campaign.by_model()
    out = [
        '<h2 id="models">Every model, and what has been checked about it'
        '<a class="anchor" href="#models">#</a></h2>',
        "<p>One row per registered model, worst status first. The <em>defects</em> column "
        "is the join that a status field cannot make: every defect on the "
        '<a href="defects.html">register</a> was found in code whose own tests were '
        "green, so a model with three against it is not the same object as a model with "
        "none, whatever both cards say.</p>",
    ]
    statuses = [s for s in VALIDATION_ORDER if any(m.status == s for m in catalogue.models)]
    extra = sorted(
        set(m.status for m in catalogue.models if m.status not in VALIDATION_ORDER)
    )
    for status in statuses + extra:
        models = [m for m in catalogue.models if m.status == status]
        if not models:
            continue
        css, label, meaning = VALIDATION_STATUS.get(
            status,
            (
                "unknown",
                status + " (unrecognised)",
                "Not a status the card schema defines. Do not read it as a check having "
                "been done.",
            ),
        )
        out.append(
            '<h3 id="c-'
            + css
            + '">'
            + escape(label)
            + " &mdash; "
            + str(len(models))
            + ' model(s)<a class="anchor" href="#c-'
            + css
            + '">#</a></h3>'
        )
        out.append('<p class="dim">' + escape(meaning) + "</p>")
        rows = []
        for model in models:
            cases = by_model.get(model.id, [])
            outcomes = {}
            for case in cases:
                outcomes[case.outcome] = outcomes.get(case.outcome, 0) + 1
            if cases:
                measured = " ".join(
                    render.badge(
                        OUTCOMES.get(o, ("unknown", o, ""))[0],
                        OUTCOMES.get(o, ("unknown", o, ""))[1] + " x" + str(n),
                    )
                    for o, n in sorted(outcomes.items())
                )
                measured += "<br/>" + "<br/>".join(
                    "<code>" + escape(c.id) + "</code>" for c in cases
                )
            else:
                measured = '<em class="dim">no case names this model</em>'

            against = []
            for ref in model.validation.get("references") or []:
                if isinstance(ref, dict) and ref.get("ref"):
                    against.append(escape(ref["ref"]))
            for case in cases:
                if case.source and escape(case.source) not in against:
                    against.append(escape(case.source))
            tests = [
                "<code>" + escape(t) + "</code>"
                for t in (model.validation.get("tests") or [])
            ]

            defects = findings_for(model.id, registers)
            if defects:
                defect_text = "<br/>".join(
                    '<a href="defects.html#'
                    + f.anchor
                    + '">'
                    + escape(f.severity)
                    + ": "
                    + escape(f.title[:70])
                    + "</a>"
                    for f in defects[:4]
                )
                if len(defects) > 4:
                    defect_text += (
                        '<br/><small class="dim">and '
                        + str(len(defects) - 4)
                        + " more</small>"
                    )
            else:
                defect_text = '<span class="dim">&mdash;</span>'

            rows.append(
                [
                    '<a href="models.html#'
                    + model.anchor
                    + '"><code>'
                    + escape(model.id)
                    + "</code></a>",
                    escape(model.family),
                    ", ".join(model.tiers) or '<em class="dim">none</em>',
                    measured,
                    "<br/>".join(against) or '<em class="dim">nothing named</em>',
                    "<br/>".join(tests) or '<span class="dim">&mdash;</span>',
                    defect_text,
                ]
            )
        out.append(
            render.table(
                [
                    "Model",
                    "Family",
                    "Tiers",
                    "Measured by",
                    "Against what",
                    "Tests",
                    "Defects found",
                ],
                rows,
                "campaign",
            )
        )
    return "".join(out)
