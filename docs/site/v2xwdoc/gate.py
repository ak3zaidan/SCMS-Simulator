"""The model-card completeness gate page.

10-roadmap.md Phase 6 states one release gate: *no `todo-calibrate` on a `high`-tier
default without a calibration issue*. The gate is implemented once, in Rust, over the
registry the engine actually builds (`crates/v2xw-metrics/src/gate.rs`), and
`docs/site/tools/cardgen` runs it and writes the verdict into the card dump under `gate`.

This module **renders that verdict and computes nothing**. That is the whole design: two
implementations of one rule drift, and the one that drifts is always the one in the
documentation, because nothing fails when it is wrong. So if the dump carries no `gate`
object, this page says the dump is too old rather than quietly re-deriving the answer from
the cards it can see.

The distinction the page insists on, because it is the one a reader gets wrong:

* **rule R1** — a `todo-calibrate` parameter carries a calibration *plan* — is enforced at
  registration, so it is always true of every model in the dump;
* **the gate** — that plan is *owned by somebody*, with a state and a measurement — is a
  release condition, and it is currently red.

A page that reported only the first would read as a clean bill of health.
"""

from . import render
from .render import escape

__all__ = ["present", "page", "failing", "MISSING_GATE_HELP"]

MISSING_GATE_HELP = (
    "The card dump carries no <code>gate</code> object, so this build cannot say whether "
    "the completeness gate passes. That means the dump was made by a version of "
    "<code>docs/site/tools/cardgen</code> from before the gate existed. Rebuild it with "
    "<code>just --justfile docs/site/justfile cards</code>. This page deliberately does "
    "<em>not</em> re-derive the verdict from the cards it can see: the gate is implemented "
    "once, in Rust, and a second implementation here would be free to disagree with the "
    "one that actually fails a release."
)

# The failure vocabulary the Rust gate serialises, with the CSS modifier and what each
# reason means. An unrecognised reason is shown as written rather than dropped.
REASONS = {
    "no-issue": (
        "fail",
        "No issue",
        "No entry in the calibration-issue register covers this parameter. Nobody has "
        "agreed to measure it.",
    ),
    "issue-closed": (
        "warn",
        "Issue closed",
        "Every issue covering it is closed, and the card still marks the parameter "
        "uncalibrated. Either the measurement was never made or the card was never "
        "updated; both are worth a red build.",
    ),
    "no-plan": (
        "critical",
        "No plan (rule R1)",
        "The parameter carries no calibration plan at all, which registry rule R1 refuses "
        "at registration. Its presence here means the card reached this report by a path "
        "that did not validate it — report this.",
    ),
}


def _reason_of(failure):
    """`(key, detail)` for one failure row, tolerant of an unrecognised reason."""
    reason = failure.get("reason")
    if isinstance(reason, str):
        return reason, None
    if isinstance(reason, dict):
        # Adjacent tagging: {"reason": "...", "detail": ...}
        return str(reason.get("reason") or ""), reason.get("detail")
    return "", None


def _reason_html(failure):
    key, detail = _reason_of(failure)
    css, label, _meaning = REASONS.get(key, ("unknown", (key or "unstated") + " (unrecognised)", ""))
    out = render.badge(css, label)
    if isinstance(detail, list) and detail:
        out += " " + ", ".join("<code>" + escape(d) + "</code>" for d in detail)
    elif isinstance(detail, str) and detail:
        out += " <code>" + escape(detail) + "</code>"
    return out


def present(catalogue):
    """True when the dump carries a gate verdict."""
    return isinstance(catalogue.data.get("gate"), dict)


def failing(catalogue):
    """True when the dump carries a verdict and that verdict is a failure.

    A missing verdict is **not** reported as failing: the honest answer to "did the gate
    pass" with no gate object is "this build does not know", and conflating the two would
    let a stale dump look like a red gate or a green one depending on which way the
    conflation went.
    """
    gate = catalogue.data.get("gate")
    if not isinstance(gate, dict):
        return False
    return not bool(gate.get("passed", False))


def _count(gate, key):
    value = gate.get(key)
    return value if isinstance(value, int) else 0


def _verdict_banner(gate):
    summary = str(gate.get("summary") or "")
    if gate.get("passed"):
        return (
            '<div class="callout callout-ok"><p><strong>The gate passes.</strong> '
            + escape(summary)
            + " Every uncalibrated <code>high</code>-tier default in the registry is "
            "covered by a tracked calibration issue. That is a claim about <em>tracking</em> "
            "and not about accuracy: a number with an owner is still an unmeasured number "
            "until the issue closes.</p></div>"
        )
    return (
        '<div class="callout callout-warn"><p><strong>The gate fails.</strong> '
        + escape(summary)
        + "</p><p>This is the state of the project, not a build problem. The register at "
        "<code>docs/calibration/issues.json</code> is empty because nobody has been "
        "assigned a calibration measurement. Writing issues into it with no owner behind "
        "them would turn this page green without a single measurement being made, which "
        "is the failure the gate exists to prevent.</p></div>"
    )


def _register_note(gate):
    path = str(gate.get("register_path") or "docs/calibration/issues.json")
    parts = []
    if not gate.get("register_present", True):
        parts.append(
            '<div class="callout callout-warn"><p><strong>The register file was not '
            "found</strong> at <code>"
            + escape(path)
            + "</code>. The gate ran against an empty register, so every uncalibrated "
            "default below is reported as untracked — which is the conservative direction, "
            "and is also what you would see if the file existed and were empty. Those two "
            "situations say different things about the repository, so check the path before "
            "reading the numbers.</p></div>"
        )
    error = gate.get("register_error")
    if isinstance(error, str) and error and gate.get("register_present", True):
        parts.append(
            '<div class="callout callout-warn"><p><strong>The register did not '
            "parse.</strong> <code>"
            + escape(error)
            + "</code>. The gate ran against an empty register rather than guessing at the "
            "file's intent.</p></div>"
        )
    declared = str(gate.get("register_schema") or "")
    if declared and declared != "v2xw/calibration-issues/1":
        parts.append(
            '<div class="callout"><p>The register declares schema <code>'
            + escape(declared)
            + "</code> and the gate reads <code>v2xw/calibration-issues/1</code>. Its "
            "issues were still read; a field this version does not know about was "
            "ignored.</p></div>"
        )
    return "".join(parts)


def _counts_table(gate):
    rows = [
        [
            "Registered models",
            str(_count(gate, "models")),
            "Every model in the card dump. The dump's own coverage gap applies: see below.",
        ],
        [
            "Models declaring the <code>high</code> tier",
            str(_count(gate, "high_tier_models")),
            "The gate's scope, because the roadmap's rule is about <code>high</code>-tier "
            "defaults.",
        ],
        [
            "Declared parameters",
            str(_count(gate, "parameters")),
            "Every parameter on every card. Invariant I-C3 requires each one a model reads "
            "at runtime to be here.",
        ],
        [
            "Uncalibrated defaults, any tier",
            str(_count(gate, "todo_parameters")),
            "Parameters whose source is <code>todo-calibrate</code>: a value an implementer "
            "chose, with a plan for replacing it.",
        ],
        [
            "Uncalibrated <code>high</code>-tier defaults",
            "<strong>" + str(_count(gate, "high_tier_todo_parameters")) + "</strong>",
            "The gate's denominator.",
        ],
        [
            "…covered by a live issue",
            str(_count(gate, "covered")),
            "An issue that is <code>open</code>, <code>in-progress</code> or "
            "<code>blocked</code>. A <code>closed</code> issue does not count.",
        ],
        [
            "…with no tracked issue",
            "<strong>" + str(len(gate.get("failures") or [])) + "</strong>",
            "Each one is a number in the engine that nobody has taken on.",
        ],
        [
            "Issues in the register",
            str(_count(gate, "register_issues")),
            "From <code>" + escape(str(gate.get("register_path") or "")) + "</code>.",
        ],
    ]
    return render.table(["Quantity", "Count", "What it is"], rows, "gate-summary")


def _failures_table(failures, caption_id, heading, lead):
    out = [
        '<h2 id="' + caption_id + '">' + heading + '<a class="anchor" href="#'
        + caption_id
        + '">#</a></h2>',
        "<p>" + lead + "</p>",
    ]
    if not failures:
        out.append('<p class="dim">None.</p>')
        return "".join(out)
    rows = []
    for failure in failures:
        model = str(failure.get("model") or "")
        anchor = "m-" + model.replace("/", "-")
        rows.append(
            [
                '<a href="models.html#' + escape(anchor) + '"><code>'
                + escape(model)
                + "</code></a>",
                "<code>" + escape(str(failure.get("parameter") or "")) + "</code>",
                escape(str(failure.get("unit") or "")),
                "<code>" + escape(str(failure.get("default"))) + "</code>",
                escape(str(failure.get("stands_for") or "")),
                _reason_html(failure),
                escape(str(failure.get("plan") or ""))
                or '<em class="dim">none</em>',
            ]
        )
    out.append(
        render.table(
            [
                "Model",
                "Parameter",
                "Unit",
                "Current default",
                "What it stands for",
                "Why it fails",
                "Plan on the card",
            ],
            rows,
            "gate-failures",
        )
    )
    return "".join(out)


def _reason_legend(gate):
    seen = []
    for failure in list(gate.get("failures") or []) + list(gate.get("outside_gate") or []):
        key, _detail = _reason_of(failure)
        if key not in seen:
            seen.append(key)
    if not seen:
        return ""
    rows = []
    for key in seen:
        css, label, meaning = REASONS.get(
            key,
            (
                "unknown",
                (key or "unstated") + " (unrecognised)",
                "This reason is not one this generator knows. It is shown rather than "
                "dropped; do not read it as a pass.",
            ),
        )
        rows.append([render.badge(css, label), escape(meaning)])
    return render.table(["Reason", "What it means"], rows, "gate-reasons")


def _malformed_section(gate):
    malformed = gate.get("malformed_patterns") or []
    out = [
        '<h2 id="malformed">Register patterns that did not parse'
        '<a class="anchor" href="#malformed">#</a></h2>'
    ]
    if not malformed:
        out.append(
            "<p>None. Every coverage pattern in the register is well formed. A pattern "
            "names one model by its literal id; a wildcard in the model half is refused, "
            "so no single line can cover the whole engine. A malformed pattern is itself "
            "a gate failure rather than a line that is silently ignored — "
            "<code>a_registry_wide_wildcard_is_refused_and_fails_the_gate</code> in "
            "<code>crates/v2xw-metrics/src/gate.rs</code> injects that line and asserts the "
            "gate stays red.</p>"
        )
        return "".join(out)
    rows = [
        [
            "<code>" + escape(str(m.get("issue") or "")) + "</code>",
            "<code>" + escape(str(m.get("pattern") or "")) + "</code>",
            escape(str(m.get("problem") or "")),
        ]
        for m in malformed
    ]
    out.append(
        '<div class="callout callout-warn"><p><strong>'
        + str(len(malformed))
        + " coverage pattern(s) did not parse, and each one fails the gate.</strong> A "
        "pattern is the only thing standing between a tracked number and an untracked one, "
        "so a line that does not parse must not become either.</p>"
        + render.table(["Issue", "Pattern", "Problem"], rows, "gate-malformed")
        + "</div>"
    )
    return "".join(out)


def _unused_section(gate):
    unused = gate.get("unused_issues") or []
    if not unused:
        return ""
    return (
        '<h2 id="unused">Issues that cover nothing'
        '<a class="anchor" href="#unused">#</a></h2>'
        "<p>These issues' patterns matched no uncalibrated parameter in this registry. "
        "Either the parameter was calibrated and the issue was not closed, or the pattern "
        "is aimed at a model this dump does not hold. Not a gate failure — a stale register "
        "is a smaller problem than an untracked number — and reported, because a register "
        "full of issues that cover nothing is how this gate would rot.</p>"
        + render.table(
            ["Issue"],
            [["<code>" + escape(str(i)) + "</code>"] for i in unused],
            "gate-unused",
        )
    )


def page(catalogue, coverage_note=""):
    """The whole gate page.

    `coverage_note` is `cards.py`'s own statement of which crates the dump reaches,
    passed in rather than recomputed so the two pages cannot disagree about it.
    """
    if not catalogue.present:
        from .cards import MISSING_HELP

        return '<div class="callout callout-warn"><p>' + MISSING_HELP + "</p></div>"
    if not present(catalogue):
        return '<div class="callout callout-warn"><p>' + MISSING_GATE_HELP + "</p></div>"

    gate = catalogue.data["gate"]
    parts = [_verdict_banner(gate), _register_note(gate)]
    if coverage_note:
        parts.append(coverage_note)
    parts.append(
        "<p>The gate is a function over the registry, in "
        "<code>crates/v2xw-metrics/src/gate.rs</code>, run by "
        "<code>docs/site/tools/cardgen --gate</code>. This page renders its verdict and "
        "computes nothing: a second implementation here would be free to disagree with the "
        "one that actually fails a release.</p>"
    )
    parts.append(_counts_table(gate))
    parts.append(_reason_legend(gate))
    parts.append(
        _failures_table(
            gate.get("failures") or [],
            "failures",
            "Every uncalibrated <code>high</code>-tier default with no tracked issue",
            "One row per number that a run at the <code>high</code> tier will use, that no "
            "source supports, and that nobody has agreed to measure. The "
            '<em>plan</em> column is the sentence registry rule R1 already requires; the '
            "gate exists because a plan with no owner has never once been executed.",
        )
    )
    parts.append(_malformed_section(gate))
    parts.append(
        _failures_table(
            gate.get("outside_gate") or [],
            "outside",
            "Outside the rule: uncalibrated defaults on cards that do not declare the "
            "<code>high</code> tier",
            "The roadmap's gate is about <code>high</code>-tier defaults, so these do not "
            "fail it. They are listed anyway, because an exemption that does not appear in "
            "the report is indistinguishable from a pass.",
        )
    )
    parts.append(_unused_section(gate))
    return "".join(parts)
