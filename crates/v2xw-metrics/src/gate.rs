//! The model-card completeness gate — 10-roadmap.md Phase 6.
//!
//! The roadmap states the release gate in one line:
//!
//! > model-card completeness gate (no `todo-calibrate` on a `high`-tier default without a
//! > calibration issue)
//!
//! This module is that gate, as a function over the registry the engine actually builds
//! rather than as a sentence in a document. [`run`] walks every registration, finds every
//! parameter whose source kind is [`SourceKind::TodoCalibrate`] on a card that declares the
//! `high` tier, and looks for an entry in the **calibration-issue register** that covers
//! it. Anything uncovered is a failure, named with its model, its parameter, its current
//! default and what the default stands for, so the output is a work list and not a verdict.
//!
//! # Why the gate is not the card's own `calibration` plan
//!
//! [`v2xw_core::card::ModelCard::validate`] already enforces registry rule R1: a
//! `todo-calibrate` parameter must carry a calibration *plan*, or the model cannot be
//! registered at all. A plan is a sentence the implementer wrote. What the release gate
//! adds is that somebody has **taken the work on**: an issue with an id, an owner, a state
//! and the measurement that would close it. The difference is the difference between "this
//! number should be measured somehow" and "this number is measured by whom, how, and
//! tracked where", and it is the difference the roadmap is asking for, because a plan
//! nobody owns has never once been executed.
//!
//! # Why it lives in `v2xw-metrics`
//!
//! The gate reads model cards and produces a report; it touches no engine state, reads no
//! clock, draws no random numbers and performs no I/O. That is the same contract every
//! other check in this crate holds (see [`crate::invariants`]), and this crate is where the
//! runnable form of a stated rule belongs. The **caller** supplies the register's bytes,
//! so the gate cannot depend on a working directory: `docs/site/tools/cardgen` reads
//! `docs/calibration/issues.json` and hands the text to [`IssueRegister::from_json`].
//!
//! # The gate cannot be opened by writing a wildcard
//!
//! A coverage pattern names one model. `obu/cohda-mk5::hsm.*` is legal and covers a
//! family of fields on one device; `*::*` is **not** legal, and a malformed pattern is
//! itself a gate failure rather than a silently-ignored line ([`MalformedPattern`]). A
//! gate that can be satisfied by one line in a data file is not a gate, and the failure
//! mode this repository keeps finding is a check that cannot go red.
//!
//! # What the gate does not claim
//!
//! It reports on the registry it is handed. A model whose card exists but which nothing
//! registered is invisible to it, and `docs/site/tools/cardgen` states that coverage gap
//! in its own output rather than letting a partial registry read as a clean one. The gate
//! also counts, separately and visibly, the uncalibrated parameters on cards that do
//! **not** declare the `high` tier ([`GateReport::outside_gate`]): they are outside the
//! roadmap's rule, and an exemption nobody can see is indistinguishable from a pass.
//!
//! ```
//! use v2xw_metrics::gate::{self, IssueRegister};
//! use v2xw_core::registry::Registry;
//!
//! let registry = Registry::new();
//! let register = IssueRegister::empty();
//! let report = gate::run(&registry, &register);
//! assert!(report.passed()); // an empty registry has nothing uncalibrated in it
//! ```

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use v2xw_core::card::{Parameter, SourceKind, Tier};
use v2xw_core::registry::Registry;

use crate::error::{MetricError, Result};

/// The schema string the calibration-issue register declares.
///
/// A register that declares another schema is still read — dropping its rows would hide
/// the issues rather than the disagreement — and [`GateReport::register_schema_matches`]
/// says the version did not match, so a reader knows not to trust a field this build does
/// not understand.
pub const ISSUE_REGISTER_SCHEMA: &str = "v2xw/calibration-issues/1";

/// The schema string [`GateReport`] serialises itself under.
pub const GATE_SCHEMA: &str = "v2xw/calibration-gate/1";

/// The separator between the model id and the parameter name in a coverage pattern.
pub const PATTERN_SEPARATOR: &str = "::";

/// How far along a calibration issue is.
///
/// Only a state for which [`IssueState::covers`] is true satisfies the gate. A `closed`
/// issue against a parameter that is still `todo-calibrate` is a contradiction rather than
/// coverage: either the measurement was made and the card was not updated, or the issue
/// was closed without the measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IssueState {
    /// Accepted as work to be done; nobody has started.
    Open,
    /// Somebody is measuring it now.
    InProgress,
    /// Cannot proceed: the equipment, the device, the standard or the dataset is not
    /// available. Still coverage — a blocked issue is tracked work — and the register says
    /// what the block is.
    Blocked,
    /// Finished. Does **not** satisfy the gate, because a finished calibration should have
    /// replaced the `todo-calibrate` source with a citation.
    Closed,
}

impl IssueState {
    /// True when an issue in this state counts as coverage for the gate.
    #[must_use]
    pub fn covers(self) -> bool {
        !matches!(self, IssueState::Closed)
    }

    /// The spelling used in reports and in the register file.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            IssueState::Open => "open",
            IssueState::InProgress => "in-progress",
            IssueState::Blocked => "blocked",
            IssueState::Closed => "closed",
        }
    }
}

/// One tracked calibration issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationIssue {
    /// A stable id, e.g. `CAL-004`. Named in the register, in the report and wherever the
    /// work is tracked.
    pub id: String,
    /// One line saying what is uncalibrated.
    pub title: String,
    /// Who owns the measurement. Not optional: an unowned issue is a plan, and the card
    /// already carries the plan.
    pub owner: String,
    /// How far along it is.
    pub state: IssueState,
    /// The parameters it covers, as `model-id::parameter` patterns.
    ///
    /// The parameter half may be `*` for every parameter of that model, or a trailing-`*`
    /// prefix such as `hsm.*`. The model half must be a literal id: see the module
    /// documentation for why.
    pub covers: Vec<String>,
    /// The measurement that would close it — the device to bench, the dataset to fetch,
    /// the standard to buy. This is what distinguishes an issue from the card's plan.
    pub measurement: String,
    /// Where the work is tracked, if anywhere outside this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracker: Option<String>,
    /// Why it is blocked, when it is. Required reading for [`IssueState::Blocked`]; the
    /// gate does not enforce it, the register's own review does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
}

/// The calibration-issue register: the file the gate reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueRegister {
    /// The schema the file declares.
    #[serde(default)]
    pub schema: String,
    /// The issues, in file order.
    #[serde(default)]
    pub issues: Vec<CalibrationIssue>,
}

impl IssueRegister {
    /// A register with no issues, which is what a repository that has never opened one has.
    ///
    /// Distinguished from a *missing* register only by the caller: the gate's arithmetic is
    /// the same either way, and a report over an empty register lists every uncalibrated
    /// high-tier default as a failure, which is the honest answer.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            schema: ISSUE_REGISTER_SCHEMA.to_string(),
            issues: Vec::new(),
        }
    }

    /// Parses a register from JSON.
    ///
    /// Unknown keys are ignored, so a file may carry `_README` prose beside its data; a
    /// key this build does not know is not a reason to refuse the register.
    ///
    /// # Errors
    /// [`MetricError::Json`] if the text is not the register's shape.
    pub fn from_json(text: &str) -> Result<Self> {
        Ok(serde_json::from_str(text)?)
    }

    /// True when the file declares the schema this build reads.
    #[must_use]
    pub fn schema_matches(&self) -> bool {
        self.schema == ISSUE_REGISTER_SCHEMA
    }

    /// The issue with this id, if the register has one.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CalibrationIssue> {
        self.issues.iter().find(|i| i.id == id)
    }
}

/// A coverage pattern that does not parse, and why.
///
/// Malformed patterns fail the gate. A pattern is the only thing standing between a
/// tracked parameter and an untracked one, so a line that does not parse must not read as
/// either coverage or absence: it reads as a defect in the register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MalformedPattern {
    /// The issue the pattern is on.
    pub issue: String,
    /// The pattern, verbatim.
    pub pattern: String,
    /// What is wrong with it.
    pub problem: String,
}

/// Why one parameter fails the gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "reason", content = "detail")]
pub enum FailureReason {
    /// No issue in the register covers this parameter.
    NoIssue,
    /// Every issue that covers it is closed, which contradicts the card still marking the
    /// parameter uncalibrated. The detail names them.
    IssueClosed(Vec<String>),
    /// The parameter carries no calibration plan at all, which registry rule R1 should
    /// have refused at registration. Reported separately because it means the card reached
    /// this report by a path that did not validate it.
    NoPlan,
}

impl FailureReason {
    /// A short label for a table.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            FailureReason::NoIssue => "no issue",
            FailureReason::IssueClosed(_) => "issue closed",
            FailureReason::NoPlan => "no plan (rule R1)",
        }
    }
}

/// One parameter that fails the gate, with everything needed to act on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateFailure {
    /// The model's registry id.
    pub model: String,
    /// The model's version.
    pub version: String,
    /// The parameter's name, as the scenario spells it.
    pub parameter: String,
    /// Its unit.
    pub unit: String,
    /// Its current default, as JSON text. `null` means the profile or model publishes no
    /// value at all, which is the common case for an unpublished hardware figure.
    pub default: String,
    /// What the `todo-calibrate` source says the number stands for.
    pub stands_for: String,
    /// The calibration plan the card carries, empty if it carries none.
    pub plan: String,
    /// The tiers the card declares, so a reader can see why the parameter is in scope.
    pub tiers: Vec<String>,
    /// Why it failed.
    pub reason: FailureReason,
}

impl GateFailure {
    /// One line, for a terminal.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "{}::{} ({}) — {}: default {}, stands for {:?}",
            self.model,
            self.parameter,
            self.unit,
            self.reason.label(),
            self.default,
            self.stands_for
        )
    }
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReport {
    /// This report's schema.
    pub schema: String,
    /// The schema the register declared.
    pub register_schema: String,
    /// How many issues the register held.
    pub register_issues: usize,
    /// How many registrations were examined.
    pub models: usize,
    /// How many of them declare the `high` tier and are therefore in the gate's scope.
    pub high_tier_models: usize,
    /// How many parameters were examined, across every card.
    pub parameters: usize,
    /// How many parameters carry a `todo-calibrate` source, at any tier.
    pub todo_parameters: usize,
    /// How many of those sit on a card declaring the `high` tier — the gate's denominator.
    pub high_tier_todo_parameters: usize,
    /// How many of the gate's denominator a live issue covers.
    pub covered: usize,
    /// Every failure, sorted by model id then parameter name.
    pub failures: Vec<GateFailure>,
    /// Uncalibrated parameters on cards that do not declare the `high` tier.
    ///
    /// Outside the roadmap's rule and listed anyway, because an exemption that does not
    /// appear in the report is indistinguishable from a pass. These do **not** make
    /// [`GateReport::passed`] false.
    pub outside_gate: Vec<GateFailure>,
    /// Issues whose patterns matched no uncalibrated parameter in this registry, by id.
    ///
    /// Either the parameter was calibrated and the issue was not closed, or the pattern is
    /// aimed at a model this registry does not hold. Not a failure — a stale register is a
    /// smaller problem than an untracked number — but it is reported, because a register
    /// full of issues that cover nothing is how this gate would rot.
    pub unused_issues: Vec<String>,
    /// Patterns that did not parse. Each one fails the gate.
    pub malformed_patterns: Vec<MalformedPattern>,
}

impl GateReport {
    /// True when nothing failed: every uncalibrated `high`-tier default is covered by a
    /// live issue and every pattern in the register parses.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty() && self.malformed_patterns.is_empty()
    }

    /// True when the register declared the schema this build reads.
    #[must_use]
    pub fn register_schema_matches(&self) -> bool {
        self.register_schema == ISSUE_REGISTER_SCHEMA
    }

    /// Every failure as one line, malformed patterns first.
    ///
    /// The malformed patterns come first because a register that does not parse makes the
    /// rest of the report less trustworthy, not more.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .malformed_patterns
            .iter()
            .map(|m| {
                format!(
                    "register issue {}: pattern {:?} is malformed: {}",
                    m.issue, m.pattern, m.problem
                )
            })
            .collect();
        out.extend(self.failures.iter().map(GateFailure::line));
        out
    }

    /// A one-line summary, the shape a phase gate is reported in.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.passed() {
            format!(
                "PASS — {} of {} uncalibrated high-tier defaults covered by {} tracked issue(s)",
                self.covered, self.high_tier_todo_parameters, self.register_issues
            )
        } else {
            format!(
                "FAIL — {} of {} uncalibrated high-tier defaults have no tracked calibration \
                 issue ({} malformed pattern(s), {} further uncalibrated default(s) on \
                 cards that do not declare the high tier)",
                self.failures.len(),
                self.high_tier_todo_parameters,
                self.malformed_patterns.len(),
                self.outside_gate.len()
            )
        }
    }

    /// Turns a failing report into the crate's error, naming every failure.
    ///
    /// # Errors
    /// [`MetricError::GateFailed`] when [`GateReport::passed`] is false.
    pub fn assert_pass(&self) -> Result<()> {
        if self.passed() {
            return Ok(());
        }
        let lines = self.lines();
        Err(MetricError::GateFailed {
            count: lines.len(),
            detail: lines.join("\n"),
        })
    }

    /// The report as indented JSON, which is what the documentation build embeds.
    ///
    /// # Errors
    /// [`MetricError::Json`] if the report will not serialise.
    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

/// Checks one coverage pattern, returning the `(model, parameter)` halves or the problem.
///
/// The rules, and each one is there because its absence would let the gate be opened
/// without doing any work:
///
/// * exactly one `::`, so `a::b::c` is refused rather than silently split;
/// * a literal model id — no `*` — so no pattern can cover the whole registry;
/// * a non-empty parameter half, which is `*`, a trailing-`*` prefix, or a literal name.
///
/// # Errors
/// A sentence naming what is wrong, suitable for a report a human reads.
pub fn parse_pattern(pattern: &str) -> core::result::Result<(&str, &str), String> {
    let parts: Vec<&str> = pattern.split(PATTERN_SEPARATOR).collect();
    if parts.len() != 2 {
        return Err(format!(
            "a pattern is `model-id{PATTERN_SEPARATOR}parameter`, and this has {} \
             separator(s)",
            parts.len().saturating_sub(1)
        ));
    }
    let (model, param) = (parts[0].trim(), parts[1].trim());
    if model.is_empty() {
        return Err("the model half is empty".to_string());
    }
    if model.contains('*') {
        return Err(
            "the model half must be a literal registry id: a wildcard there would let one \
             line cover every model in the engine, which is not coverage"
                .to_string(),
        );
    }
    if param.is_empty() {
        return Err("the parameter half is empty; write `*` to mean every parameter".to_string());
    }
    let stars = param.matches('*').count();
    if stars > 1 || (stars == 1 && !param.ends_with('*')) {
        return Err(format!(
            "the parameter half may be `*`, a trailing-`*` prefix, or a literal name; \
             {param:?} is none of those"
        ));
    }
    Ok((model, param))
}

/// True when a well-formed pattern's parameter half matches `parameter`.
fn parameter_matches(pattern_param: &str, parameter: &str) -> bool {
    match pattern_param.strip_suffix('*') {
        Some(prefix) => parameter.starts_with(prefix),
        None => pattern_param == parameter,
    }
}

/// Runs the gate over a registry and a register.
///
/// Reads nothing but its two arguments: no file, no clock, no environment. The ordering of
/// everything it emits is derived from [`Registry::iter_by_id`] and from the parameter
/// order on each card, so two runs over the same tree produce the same report — the same
/// property every other artefact in this repository holds.
#[must_use]
pub fn run(registry: &Registry, register: &IssueRegister) -> GateReport {
    run_over_cards(registry.iter_by_id().map(|(_, e)| &e.card), register)
}

/// Runs the gate over a bare list of cards.
///
/// [`run`] is this function over [`Registry::iter_by_id`], and it is the spelling a
/// release check uses. This one exists because several crates publish their cards through
/// per-model constructors that nothing registers (`docs/site/tools/cardgen` names the
/// gap), and a gate that could only see registered models would report a smaller number
/// than the truth. It is also the only path on which [`FailureReason::NoPlan`] is
/// reachable: every [`Registry`] registration validates the card first, and registry rule
/// R1 refuses a `todo-calibrate` parameter with no plan.
///
/// The caller decides the order of `cards`, and everything the report emits follows it,
/// so pass them in id order if the report is going into a file.
#[must_use]
pub fn run_over_cards<'a>(
    cards: impl IntoIterator<Item = &'a v2xw_core::card::ModelCard>,
    register: &IssueRegister,
) -> GateReport {
    // Pattern table: issue index -> parsed patterns. Built once, and every malformed
    // pattern is recorded rather than skipped.
    let mut malformed_patterns: Vec<MalformedPattern> = Vec::new();
    let mut parsed: Vec<(usize, &str, &str)> = Vec::new();
    for (index, issue) in register.issues.iter().enumerate() {
        for pattern in &issue.covers {
            match parse_pattern(pattern) {
                Ok((model, param)) => parsed.push((index, model, param)),
                Err(problem) => malformed_patterns.push(MalformedPattern {
                    issue: issue.id.clone(),
                    pattern: pattern.clone(),
                    problem,
                }),
            }
        }
    }

    // Which issues matched something. A `BTreeSet` of indices rather than a `HashSet`:
    // this decides the order of `unused_issues`, which reaches an output.
    let mut used: BTreeSet<usize> = BTreeSet::new();

    let mut report = GateReport {
        schema: GATE_SCHEMA.to_string(),
        register_schema: register.schema.clone(),
        register_issues: register.issues.len(),
        models: 0,
        high_tier_models: 0,
        parameters: 0,
        todo_parameters: 0,
        high_tier_todo_parameters: 0,
        covered: 0,
        failures: Vec::new(),
        outside_gate: Vec::new(),
        unused_issues: Vec::new(),
        malformed_patterns,
    };

    for card in cards {
        report.models += 1;
        report.parameters += card.parameters.len();
        let high = card.implements_tier(Tier::High);
        if high {
            report.high_tier_models += 1;
        }
        for parameter in &card.parameters {
            if parameter.source.kind != SourceKind::TodoCalibrate {
                continue;
            }
            report.todo_parameters += 1;
            if high {
                report.high_tier_todo_parameters += 1;
            }

            // Every issue whose pattern reaches this (model, parameter).
            let mut live: Vec<&str> = Vec::new();
            let mut closed: Vec<&str> = Vec::new();
            // Destructured by value: the tuple is `Copy`, so `pattern_model` and
            // `pattern_param` are plain `&str` here rather than `&&str`, and the
            // comparisons below need no coercion to be obvious.
            for &(index, pattern_model, pattern_param) in &parsed {
                if pattern_model != card.id.as_str()
                    || !parameter_matches(pattern_param, parameter.name.as_str())
                {
                    continue;
                }
                used.insert(index);
                let issue = &register.issues[index];
                if issue.state.covers() {
                    live.push(issue.id.as_str());
                } else {
                    closed.push(issue.id.as_str());
                }
            }

            let plan = parameter.calibration.clone().unwrap_or_default();
            let reason = if plan.trim().is_empty() {
                // Rule R1 should have stopped this at registration. Reported first
                // because a parameter with no plan is a worse defect than an untracked
                // one, whatever the register says.
                Some(FailureReason::NoPlan)
            } else if !live.is_empty() {
                None
            } else if !closed.is_empty() {
                Some(FailureReason::IssueClosed(
                    closed.iter().map(|s| (*s).to_string()).collect(),
                ))
            } else {
                Some(FailureReason::NoIssue)
            };

            match reason {
                None => {
                    if high {
                        report.covered += 1;
                    }
                }
                Some(reason) => {
                    let failure = describe(card, parameter, plan, reason);
                    if high {
                        report.failures.push(failure);
                    } else {
                        report.outside_gate.push(failure);
                    }
                }
            }
        }
    }

    report.failures.sort_by(|a, b| {
        a.model
            .cmp(&b.model)
            .then_with(|| a.parameter.cmp(&b.parameter))
    });
    report.outside_gate.sort_by(|a, b| {
        a.model
            .cmp(&b.model)
            .then_with(|| a.parameter.cmp(&b.parameter))
    });
    report.unused_issues = register
        .issues
        .iter()
        .enumerate()
        .filter(|(index, _)| !used.contains(index))
        .map(|(_, issue)| issue.id.clone())
        .collect();
    report
}

/// Builds one failure row from the card and the parameter.
fn describe(
    card: &v2xw_core::card::ModelCard,
    parameter: &Parameter,
    plan: String,
    reason: FailureReason,
) -> GateFailure {
    GateFailure {
        model: card.id.clone(),
        version: card.version.clone(),
        parameter: parameter.name.clone(),
        unit: parameter.unit.clone(),
        default: parameter.default.to_string(),
        stands_for: parameter.source.reference.clone(),
        plan,
        tiers: card.tier.iter().map(Tier::to_string).collect(),
        reason,
    }
}

/// The failures grouped by model id, which is the shape the documentation page renders.
///
/// A plain function rather than a method so the grouping is available without cloning the
/// report; the map is a `BTreeMap`, so the order is the model id order and not an
/// iteration accident.
#[must_use]
pub fn by_model(failures: &[GateFailure]) -> BTreeMap<&str, Vec<&GateFailure>> {
    let mut out: BTreeMap<&str, Vec<&GateFailure>> = BTreeMap::new();
    for failure in failures {
        out.entry(failure.model.as_str()).or_default().push(failure);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use v2xw_core::card::{Family, ModelCard, Source};

    fn todo_param(name: &str, plan: Option<&str>) -> Parameter {
        Parameter {
            name: name.to_string(),
            unit: "us".to_string(),
            default: json!(null),
            range: None,
            source: Source::todo_calibrate("nobody has measured it"),
            calibration: plan.map(str::to_string),
        }
    }

    fn cited_param(name: &str) -> Parameter {
        Parameter::new(
            name,
            "dB",
            json!(3.0),
            Source::new(v2xw_core::card::SourceKind::Datasheet, "a real datasheet"),
        )
    }

    fn card_with(id: &str, tiers: Vec<Tier>, parameters: Vec<Parameter>) -> ModelCard {
        let mut card = ModelCard::new(id, Family::Metric, "1.0.0", "A card for the gate test.");
        card.tier = tiers;
        card.parameters = parameters;
        card
    }

    fn issue(id: &str, state: IssueState, covers: &[&str]) -> CalibrationIssue {
        CalibrationIssue {
            id: id.to_string(),
            title: "an issue".to_string(),
            owner: "somebody".to_string(),
            state,
            covers: covers.iter().map(|s| (*s).to_string()).collect(),
            measurement: "bench the device".to_string(),
            tracker: None,
            blocked_by: None,
        }
    }

    #[test]
    fn an_uncalibrated_high_tier_default_with_no_issue_fails() {
        let mut registry = Registry::new();
        registry
            .register(card_with(
                "metric/test/one",
                vec![Tier::High],
                vec![todo_param("threshold", Some("measure it on a test track"))],
            ))
            .expect("registers");
        let report = run(&registry, &IssueRegister::empty());
        assert!(!report.passed(), "{}", report.summary());
        assert_eq!(report.high_tier_todo_parameters, 1);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].reason, FailureReason::NoIssue);
        assert_eq!(report.covered, 0);
    }

    /// The same registry, one issue added: the gate goes green. Both directions are
    /// asserted in the same shape, because a gate whose green case is untested is a gate
    /// nobody knows the failure condition of.
    #[test]
    fn a_live_issue_covers_it_and_the_gate_goes_green() {
        let mut registry = Registry::new();
        registry
            .register(card_with(
                "metric/test/one",
                vec![Tier::High],
                vec![todo_param("threshold", Some("measure it on a test track"))],
            ))
            .expect("registers");
        let register = IssueRegister {
            schema: ISSUE_REGISTER_SCHEMA.to_string(),
            issues: vec![issue(
                "CAL-001",
                IssueState::Open,
                &["metric/test/one::threshold"],
            )],
        };
        let report = run(&registry, &register);
        assert!(report.passed(), "{}", report.summary());
        assert_eq!(report.covered, 1);
        assert!(report.unused_issues.is_empty());
        assert!(report.assert_pass().is_ok());
    }

    #[test]
    fn a_closed_issue_is_a_contradiction_rather_than_coverage() {
        let mut registry = Registry::new();
        registry
            .register(card_with(
                "metric/test/one",
                vec![Tier::High],
                vec![todo_param("threshold", Some("a plan"))],
            ))
            .expect("registers");
        let register = IssueRegister {
            schema: ISSUE_REGISTER_SCHEMA.to_string(),
            issues: vec![issue(
                "CAL-002",
                IssueState::Closed,
                &["metric/test/one::*"],
            )],
        };
        let report = run(&registry, &register);
        assert!(!report.passed());
        assert_eq!(
            report.failures[0].reason,
            FailureReason::IssueClosed(vec!["CAL-002".to_string()])
        );
    }

    #[test]
    fn a_blocked_issue_still_counts_as_tracked_work() {
        let mut registry = Registry::new();
        registry
            .register(card_with(
                "metric/test/one",
                vec![Tier::High],
                vec![todo_param("threshold", Some("a plan"))],
            ))
            .expect("registers");
        let mut blocked = issue("CAL-003", IssueState::Blocked, &["metric/test/one::*"]);
        blocked.blocked_by = Some("the vendor will not release the datasheet".to_string());
        let register = IssueRegister {
            schema: ISSUE_REGISTER_SCHEMA.to_string(),
            issues: vec![blocked],
        };
        assert!(run(&registry, &register).passed());
    }

    #[test]
    fn a_prefix_pattern_covers_a_family_of_fields_and_nothing_else() {
        let mut registry = Registry::new();
        registry
            .register(card_with(
                "obu/test-device",
                vec![Tier::Medium, Tier::High],
                vec![
                    todo_param("hsm.queue_depth", Some("a plan")),
                    todo_param("hsm.ops.sign.us", Some("a plan")),
                    todo_param("power_w", Some("a plan")),
                ],
            ))
            .expect("registers");
        let register = IssueRegister {
            schema: ISSUE_REGISTER_SCHEMA.to_string(),
            issues: vec![issue(
                "CAL-004",
                IssueState::Open,
                &["obu/test-device::hsm."],
            )],
        };
        // A literal `hsm.` matches nothing, because it is a literal and not a prefix.
        let literal = run(&registry, &register);
        assert_eq!(literal.failures.len(), 3);
        assert_eq!(literal.unused_issues, vec!["CAL-004".to_string()]);

        let register = IssueRegister {
            schema: ISSUE_REGISTER_SCHEMA.to_string(),
            issues: vec![issue(
                "CAL-004",
                IssueState::Open,
                &["obu/test-device::hsm.*"],
            )],
        };
        let prefix = run(&registry, &register);
        assert_eq!(prefix.covered, 2);
        assert_eq!(prefix.failures.len(), 1);
        assert_eq!(prefix.failures[0].parameter, "power_w");
    }

    /// The gate cannot be opened by one wildcard line, and the attempt is itself a
    /// failure rather than a line the reader never sees.
    #[test]
    fn a_registry_wide_wildcard_is_refused_and_fails_the_gate() {
        let mut registry = Registry::new();
        registry
            .register(card_with(
                "metric/test/one",
                vec![Tier::High],
                vec![todo_param("threshold", Some("a plan"))],
            ))
            .expect("registers");
        for pattern in ["*::*", "*::threshold", "metric/test/*::threshold"] {
            let register = IssueRegister {
                schema: ISSUE_REGISTER_SCHEMA.to_string(),
                issues: vec![issue("CAL-005", IssueState::Open, &[pattern])],
            };
            let report = run(&registry, &register);
            assert!(!report.passed(), "{pattern} must not open the gate");
            assert_eq!(report.malformed_patterns.len(), 1, "{pattern}");
            assert_eq!(report.failures.len(), 1, "{pattern}");
        }
    }

    #[test]
    fn a_pattern_with_the_wrong_shape_is_reported_with_its_problem() {
        assert!(parse_pattern("a::b::c").is_err());
        assert!(parse_pattern("noseparator").is_err());
        assert!(parse_pattern("::threshold").is_err());
        assert!(parse_pattern("model::").is_err());
        assert!(parse_pattern("model::a*b").is_err());
        assert_eq!(parse_pattern("model::*"), Ok(("model", "*")));
        assert_eq!(parse_pattern(" model :: a.b "), Ok(("model", "a.b")));
    }

    #[test]
    fn a_cited_default_is_not_in_the_gates_scope() {
        let mut registry = Registry::new();
        registry
            .register(card_with(
                "metric/test/cited",
                vec![Tier::High],
                vec![cited_param("path_loss_exponent")],
            ))
            .expect("registers");
        let report = run(&registry, &IssueRegister::empty());
        assert!(report.passed());
        assert_eq!(report.parameters, 1);
        assert_eq!(report.todo_parameters, 0);
    }

    #[test]
    fn a_card_that_does_not_declare_the_high_tier_is_listed_outside_the_gate() {
        let mut registry = Registry::new();
        registry
            .register(card_with(
                "metric/test/cheap",
                vec![Tier::Abstract],
                vec![todo_param("threshold", Some("a plan"))],
            ))
            .expect("registers");
        let report = run(&registry, &IssueRegister::empty());
        assert!(report.passed(), "the roadmap's rule is about high-tier defaults");
        assert_eq!(report.todo_parameters, 1);
        assert_eq!(report.high_tier_todo_parameters, 0);
        assert_eq!(report.outside_gate.len(), 1, "and it is still visible");
    }

    #[test]
    fn the_register_round_trips_through_json_and_ignores_prose_keys() {
        let text = r#"{
          "_README": ["prose a reader needs and a parser does not"],
          "schema": "v2xw/calibration-issues/1",
          "issues": [
            {
              "id": "CAL-001",
              "title": "t",
              "owner": "o",
              "state": "in-progress",
              "covers": ["a/b::c"],
              "measurement": "m"
            }
          ]
        }"#;
        let register = IssueRegister::from_json(text).expect("parses");
        assert!(register.schema_matches());
        assert_eq!(register.issues.len(), 1);
        assert_eq!(register.issues[0].state, IssueState::InProgress);
        assert!(register.get("CAL-001").is_some());
        assert!(register.get("CAL-999").is_none());
    }

    #[test]
    fn a_parameter_with_no_plan_fails_for_that_reason_and_not_for_a_missing_issue() {
        // Registry rule R1 stops this at registration, so it can only arrive through the
        // card list. The gate is the second line of defence and must name the real
        // problem rather than reporting a missing issue for a card that is itself broken.
        let card = card_with(
            "metric/test/noplan",
            vec![Tier::High],
            vec![todo_param("threshold", None)],
        );
        assert!(card.validate().is_err(), "R1 refuses it at registration");
        let mut registry = Registry::new();
        assert!(
            registry.register(card.clone()).is_err(),
            "and so does the registry"
        );

        let report = run_over_cards([&card], &IssueRegister::empty());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].reason, FailureReason::NoPlan);

        // Even with an issue covering it, the missing plan is still the failure: coverage
        // does not excuse a card that should not have registered.
        let register = IssueRegister {
            schema: ISSUE_REGISTER_SCHEMA.to_string(),
            issues: vec![issue(
                "CAL-006",
                IssueState::Open,
                &["metric/test/noplan::*"],
            )],
        };
        let covered = run_over_cards([&card], &register);
        assert_eq!(covered.failures.len(), 1);
        assert_eq!(covered.failures[0].reason, FailureReason::NoPlan);
    }

    #[test]
    fn the_summary_says_pass_or_fail_with_the_numbers() {
        let report = run(&Registry::new(), &IssueRegister::empty());
        assert!(report.summary().starts_with("PASS"));
        let mut registry = Registry::new();
        registry
            .register(card_with(
                "metric/test/one",
                vec![Tier::High],
                vec![todo_param("threshold", Some("a plan"))],
            ))
            .expect("registers");
        let failing = run(&registry, &IssueRegister::empty());
        assert!(failing.summary().starts_with("FAIL"));
        assert_eq!(failing.lines().len(), 1);
        assert!(failing.assert_pass().is_err());
    }
}
