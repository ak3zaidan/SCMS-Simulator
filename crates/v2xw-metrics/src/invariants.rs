//! Runnable invariant checks: which invariant failed, and with what numbers.
//!
//! The design documents state invariants with identifiers — 03-interfaces.md §4 and §5,
//! 05-protocols.md §7, 07-threats-and-detection.md, build decision D9 — and a stated
//! invariant that nothing checks is a comment. Each one here is a function over an
//! [`crate::ledger::EventLedger`] (or, for the two that compare runs, over two of them) that
//! returns an [`InvariantOutcome`]: how many things it examined, and for each violation the
//! subject, the statement that was broken and **the numbers**, because "I-N1 failed" is not
//! actionable and "frame 42 is attributed to both `air` (400 B) and `backhaul` (400 B)" is.
//!
//! | Id | Source | What is checked here |
//! |---|---|---|
//! | [`check_i_r3`] | 03-interfaces.md §4 | every lost frame carries **exactly one** cause, every received frame none; a node's reported busy time is at least its own transmitted airtime; airtime accounting is not partial |
//! | [`check_i_n1`] | 03-interfaces.md §5 | every byte is attributed to exactly one bucket, and every attribution names what it is attributing |
//! | [`check_i_n2`] | 03-interfaces.md §5 | a fragmenter's card documents its reassembly timeout and its loss amplification |
//! | [`check_i_m1`] | 03-interfaces.md §3 | mobility output within one step is ordered by `ActorId` |
//! | [`check_i_p4`] | 05-protocols.md §7 | revocation stage timestamps are emitted, in the order 05-protocols.md §8 fixes |
//! | [`check_i_t2`] | 03-interfaces.md §9 | no record carries a visibility tag its channel may not carry, and nothing GT-tainted sits on a NODE channel |
//! | [`check_i_t3`] | 07-threats-and-detection.md | every attack action that changed bytes on the air is on a GT channel, names its true actor, and corresponds to a transmission |
//! | [`check_i_s1`] | 03-interfaces.md §6 | two runs differing only in crypto mode produce identical verification outcomes and identical sizes |
//! | [`check_i_c1`] | 03-interfaces.md §17 | two runs emit byte-identical records |
//! | [`check_d9_quantisation`] | build decision D9 | every float in every emitted sample is finite and sits on the grid the writer quantised it onto |
//!
//! # What a check does when it has no data
//!
//! It reports [`InvariantOutcome::skipped`] with the reason, and `held()` answers `true`.
//! An invariant no record bears on is not violated — but a run that thought it was checking
//! I-N1 and had no byte attributions should be told, not reassured, so the reason travels
//! with the outcome and [`InvariantReport::skipped`] lists them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{Family, ModelCard};
use v2xw_core::ctx::OwnedRecord;

use crate::channels::{ByteBucket, RxOutcome, allowed_visibilities};
use crate::def::MetricSample;
use crate::error::{MetricError, Result};
use crate::ledger::EventLedger;
use crate::quant::Quantum;
use crate::security::stage_rank;

/// A number in a violation report: an exact integer, or a real already on a declared grid.
///
/// Two variants rather than one `f64` because most of these numbers are counts and bytes,
/// and printing a byte count as `4.0e2` in a diagnostic is a small indignity that adds up.
/// A real is quantised on construction, so a violation report is a D9-compliant artefact
/// like any other output.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Number {
    /// An exact count.
    Int(i64),
    /// A real, on the grid it was constructed with.
    Real(f64),
}

impl Number {
    /// An exact count.
    #[must_use]
    pub const fn int(i: i64) -> Self {
        Number::Int(i)
    }

    /// A real, rounded onto `q`.
    #[must_use]
    pub fn real(x: f64, q: Quantum) -> Self {
        Number::Real(q.quantise(x))
    }
}

impl core::fmt::Display for Number {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Number::Int(i) => write!(f, "{i}"),
            Number::Real(x) => write!(f, "{x}"),
        }
    }
}

/// One way in which one invariant was broken.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvariantViolation {
    /// The invariant's identifier, e.g. `"I-N1"`.
    ///
    /// A `String` rather than a `&'static str` so a report round-trips through JSON: a run
    /// records its invariant report beside its metrics, and a report that could be written
    /// and not read back would be a schema with no reader.
    pub invariant: String,
    /// What the violation was, in one sentence.
    pub detail: String,
    /// What it was about: a node, a frame id, a revocation id, a channel.
    pub subject: Option<String>,
    /// The numbers, named, in a fixed order.
    pub numbers: BTreeMap<String, Number>,
}

impl core::fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.invariant)?;
        if let Some(s) = &self.subject {
            write!(f, " [{s}]")?;
        }
        write!(f, ": {}", self.detail)?;
        if !self.numbers.is_empty() {
            let joined: Vec<String> = self
                .numbers
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            write!(f, " ({})", joined.join(", "))?;
        }
        Ok(())
    }
}

/// The result of running one invariant check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvariantOutcome {
    /// The invariant's identifier.
    pub invariant: String,
    /// The invariant as the design states it, so a failure report is self-contained.
    pub statement: String,
    /// How many subjects the check examined — the denominator a reader needs to judge
    /// "zero violations".
    pub checked: u64,
    /// The violations found.
    pub violations: Vec<InvariantViolation>,
    /// Why the check could not run, when it could not.
    pub skipped: Option<String>,
}

impl InvariantOutcome {
    /// A passing outcome.
    fn passed(invariant: &'static str, statement: &'static str, checked: u64) -> Self {
        Self {
            invariant: invariant.to_string(),
            statement: statement.to_string(),
            checked,
            violations: Vec::new(),
            skipped: None,
        }
    }

    /// An outcome that could not be evaluated.
    fn skipped(invariant: &'static str, statement: &'static str, why: impl Into<String>) -> Self {
        Self {
            invariant: invariant.to_string(),
            statement: statement.to_string(),
            checked: 0,
            violations: Vec::new(),
            skipped: Some(why.into()),
        }
    }

    /// True if no violation was found. A skipped check holds: an invariant no record bears
    /// on is not broken.
    #[must_use]
    pub fn held(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Every invariant check's outcome from one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvariantReport {
    /// The outcomes, in the order the checks were run.
    pub outcomes: Vec<InvariantOutcome>,
}

impl InvariantReport {
    /// A report over the given outcomes.
    #[must_use]
    pub const fn new(outcomes: Vec<InvariantOutcome>) -> Self {
        Self { outcomes }
    }

    /// True if every check held.
    #[must_use]
    pub fn held(&self) -> bool {
        self.outcomes.iter().all(InvariantOutcome::held)
    }

    /// Every violation, across every check.
    pub fn violations(&self) -> impl Iterator<Item = &InvariantViolation> {
        self.outcomes.iter().flat_map(|o| o.violations.iter())
    }

    /// The identifiers of the checks that found violations.
    #[must_use]
    pub fn failed(&self) -> Vec<&str> {
        self.outcomes
            .iter()
            .filter(|o| !o.held())
            .map(|o| o.invariant.as_str())
            .collect()
    }

    /// The checks that could not run, with their reasons.
    #[must_use]
    pub fn skipped(&self) -> Vec<(&str, &str)> {
        self.outcomes
            .iter()
            .filter_map(|o| o.skipped.as_deref().map(|why| (o.invariant.as_str(), why)))
            .collect()
    }

    /// `Ok(())` if every check held, otherwise an error naming every violation with its
    /// numbers.
    ///
    /// This is the form a run asserts: `report.assert_all()?`.
    ///
    /// # Errors
    /// [`MetricError::InvariantViolated`] listing every violation, one per line.
    pub fn assert_all(&self) -> Result<()> {
        let lines: Vec<String> = self.violations().map(ToString::to_string).collect();
        if lines.is_empty() {
            return Ok(());
        }
        Err(MetricError::InvariantViolated {
            count: lines.len(),
            detail: lines.join("; "),
        })
    }
}

/// Builds a violation.
fn violation(
    invariant: &'static str,
    subject: Option<String>,
    detail: String,
    numbers: &[(&str, Number)],
) -> InvariantViolation {
    InvariantViolation {
        invariant: invariant.to_string(),
        detail,
        subject,
        numbers: numbers
            .iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect(),
    }
}

// ---------------------------------------------------------------------------------------
// I-R3
// ---------------------------------------------------------------------------------------

/// The statement of I-R3, from 03-interfaces.md §4.
pub const I_R3: &str = "every `Lost` carries exactly one cause, and the channel accounting \
                        sums to the transmitted airtime";

/// **I-R3** — loss causes and airtime accounting.
///
/// Three things are checked, all of them consequences of the one sentence:
///
/// 1. Every reception whose outcome is `Lost` carries **exactly one** cause; none carries
///    zero (a loss with no reason is a loss that cannot be attributed, which is what
///    `pdr_by_cause` divides by) and none carries two (which would make the shares sum to
///    more than one).
/// 2. A reception whose outcome is `Ok` carries **no** cause.
/// 3. Airtime accounting is complete and consistent: on any channel where some
///    transmissions report an airtime, all of them must, and a node's reported channel busy
///    time must be at least its own transmitted airtime — a transmitter hears itself.
///
/// Needs `phy.rx`, `node.tx` and `mac.cbr`.
#[must_use]
pub fn check_i_r3(ledger: &EventLedger) -> InvariantOutcome {
    if ledger.rx.is_empty() && ledger.tx.is_empty() {
        return InvariantOutcome::skipped("I-R3", I_R3, "no phy.rx or node.tx records");
    }
    let mut violations = Vec::new();

    for (i, r) in ledger.rx.iter().enumerate() {
        let causes = r.all_causes();
        match (r.outcome, causes.len()) {
            (RxOutcome::Lost, 1) | (RxOutcome::Ok, 0) => {}
            (RxOutcome::Lost, n) => violations.push(violation(
                "I-R3",
                Some(format!("phy.rx[{i}] rx={} t_end={}", r.rx, r.t_end)),
                format!("a lost frame must carry exactly one cause, this one carries {n}"),
                &[("causes", Number::int(n as i64))],
            )),
            (RxOutcome::Ok, n) => violations.push(violation(
                "I-R3",
                Some(format!("phy.rx[{i}] rx={} t_end={}", r.rx, r.t_end)),
                format!("a received frame must carry no loss cause, this one carries {n}"),
                &[("causes", Number::int(n as i64))],
            )),
        }
    }

    // Airtime completeness, per channel.
    let mut with: BTreeMap<Option<u16>, u64> = BTreeMap::new();
    let mut without: BTreeMap<Option<u16>, u64> = BTreeMap::new();
    let mut tx_airtime: BTreeMap<v2xw_core::ids::NodeId, u64> = BTreeMap::new();
    for t in &ledger.tx {
        match t.airtime_us {
            Some(us) => {
                *with.entry(t.channel).or_insert(0) += 1;
                *tx_airtime.entry(t.node).or_insert(0) += us;
            }
            None => *without.entry(t.channel).or_insert(0) += 1,
        }
    }
    for (channel, n_with) in &with {
        if let Some(n_without) = without.get(channel) {
            violations.push(violation(
                "I-R3",
                Some(match channel {
                    Some(c) => format!("channel {c}"),
                    None => "unspecified channel".to_string(),
                }),
                "airtime accounting is partial: some transmissions on this channel report an \
                 airtime and others do not, so the channel's accounting cannot sum to the \
                 transmitted airtime"
                    .to_string(),
                &[
                    ("with_airtime", Number::int(*n_with as i64)),
                    ("without_airtime", Number::int(*n_without as i64)),
                ],
            ));
        }
    }

    // A node's reported busy time must cover its own transmissions.
    let mut busy: BTreeMap<v2xw_core::ids::NodeId, u64> = BTreeMap::new();
    for c in &ledger.cbr {
        if let Some(us) = c.busy_us {
            *busy.entry(c.node).or_insert(0) += us;
        }
    }
    for (node, own) in &tx_airtime {
        if let Some(reported) = busy.get(node)
            && reported < own
        {
            violations.push(violation(
                "I-R3",
                Some(format!("node {node}")),
                "the node's reported channel busy time is less than its own transmitted \
                 airtime: a transmitter hears itself, so the busy accounting has lost time"
                    .to_string(),
                &[
                    ("transmitted_us", Number::int(*own as i64)),
                    ("busy_us", Number::int(*reported as i64)),
                    ("missing_us", Number::int((*own - *reported) as i64)),
                ],
            ));
        }
    }

    InvariantOutcome {
        checked: (ledger.rx.len() + ledger.tx.len()) as u64,
        violations,
        ..InvariantOutcome::passed("I-R3", I_R3, 0)
    }
}

// ---------------------------------------------------------------------------------------
// I-N1
// ---------------------------------------------------------------------------------------

/// The statement of I-N1, from 03-interfaces.md §5.
pub const I_N1: &str = "every byte counted in `bytes_on_wire` is attributed to exactly one \
                        accounting bucket (air / cellular UL / cellular DL / backhaul / \
                        backend)";

/// **I-N1** — every byte belongs to exactly one bucket.
///
/// Two failures are possible and both are checked:
///
/// 1. **Two buckets for one frame.** Every attribution is keyed by the frame, PDU or message
///    id it is for; an id that appears under two buckets is a byte counted twice, and the
///    violation names both buckets and both byte counts.
/// 2. **An attribution with no id.** A record that says "400 bytes on the backhaul" without
///    saying *which* 400 bytes makes the invariant unfalsifiable. It is reported as
///    unattributable rather than ignored, because an invariant that cannot fail is not being
///    checked.
///
/// The passing outcome carries the per-bucket totals as its numbers, so the check also
/// serves as the byte-accounting summary.
///
/// Needs `node.tx` (which is the `air` bucket), `net.bytes` and `proto.msg`.
#[must_use]
pub fn check_i_n1(ledger: &EventLedger) -> InvariantOutcome {
    let mut by_id: BTreeMap<u64, Vec<(ByteBucket, u64)>> = BTreeMap::new();
    let mut unattributable: Vec<(ByteBucket, u64)> = Vec::new();
    let mut totals: BTreeMap<ByteBucket, u64> = BTreeMap::new();
    let mut attributions = 0_u64;

    // One attribution: `(what it is for, which bucket, how many bytes)`. An attribution with
    // no id goes on the unattributable list, which is itself an I-N1 finding.
    let mut attributions_list: Vec<(Option<u64>, ByteBucket, u64)> = Vec::new();
    for t in &ledger.tx {
        attributions_list.push((t.msg, ByteBucket::Air, t.bytes_on_wire));
    }
    for b in &ledger.bytes {
        attributions_list.push((b.id, b.bucket, b.bytes_on_wire));
    }
    for m in &ledger.proto_msg {
        // A protocol message without a declared transport is not an attribution at all: it
        // is a byte count with no bucket, which is the same failure as one with no id and is
        // reported as such (the bucket recorded here is only for the totals).
        // `transport.and(msg)` is `None` whenever either is missing, which is exactly the
        // unattributable case: a byte count with no bucket is as uncheckable as one with no
        // id. The bucket recorded beside it only feeds the totals.
        attributions_list.push((
            m.transport.and(m.msg),
            m.transport.unwrap_or(ByteBucket::Backend),
            m.bytes_on_wire,
        ));
    }
    for (id, bucket, bytes) in &attributions_list {
        attributions += 1;
        *totals.entry(*bucket).or_insert(0) += *bytes;
        match id {
            Some(i) => by_id.entry(*i).or_default().push((*bucket, *bytes)),
            None => unattributable.push((*bucket, *bytes)),
        }
    }

    if attributions == 0 {
        return InvariantOutcome::skipped(
            "I-N1",
            I_N1,
            "no node.tx, net.bytes or proto.msg records",
        );
    }

    let mut violations = Vec::new();
    for (id, attrs) in &by_id {
        let mut buckets: Vec<ByteBucket> = attrs.iter().map(|(b, _)| *b).collect();
        buckets.sort_unstable();
        buckets.dedup();
        if buckets.len() > 1 {
            let names: Vec<&str> = buckets.iter().map(|b| b.as_str()).collect();
            let numbers: Vec<(&str, Number)> = attrs
                .iter()
                .map(|(b, n)| (b.as_str(), Number::int(*n as i64)))
                .collect();
            violations.push(violation(
                "I-N1",
                Some(format!("id {id}")),
                format!(
                    "these bytes are attributed to {} buckets ({}) — every byte belongs to \
                     exactly one",
                    buckets.len(),
                    names.join(", ")
                ),
                &numbers,
            ));
        }
    }
    if !unattributable.is_empty() {
        let bytes: u64 = unattributable.iter().map(|(_, n)| *n).sum();
        violations.push(violation(
            "I-N1",
            None,
            format!(
                "{} byte attribution(s) name no frame, PDU or message id (or no transport), \
                 so they cannot be checked for double counting",
                unattributable.len()
            ),
            &[
                ("records", Number::int(unattributable.len() as i64)),
                ("bytes", Number::int(bytes as i64)),
            ],
        ));
    }

    // The per-bucket totals are the accounting this invariant is about, and a reader wants
    // them beside the verdict — but an `InvariantOutcome` carries numbers only on a
    // violation, so they are reached through `bucket_totals` instead of being smuggled into
    // a zero-violation report. `totals` is summed here anyway, because the double-counting
    // check needs the same walk.
    debug_assert_eq!(
        totals.values().sum::<u64>(),
        attributions_list.iter().map(|(_, _, b)| *b).sum::<u64>(),
        "every attribution is counted in exactly one bucket total"
    );
    InvariantOutcome {
        checked: attributions,
        violations,
        ..InvariantOutcome::passed("I-N1", I_N1, 0)
    }
}

/// The per-bucket byte totals an I-N1 check adds up, for a caller that wants the accounting
/// and not only the verdict.
#[must_use]
pub fn bucket_totals(ledger: &EventLedger) -> BTreeMap<ByteBucket, u64> {
    let mut totals: BTreeMap<ByteBucket, u64> = BTreeMap::new();
    for t in &ledger.tx {
        *totals.entry(ByteBucket::Air).or_insert(0) += t.bytes_on_wire;
    }
    for b in &ledger.bytes {
        *totals.entry(b.bucket).or_insert(0) += b.bytes_on_wire;
    }
    for m in &ledger.proto_msg {
        if let Some(bucket) = m.transport {
            *totals.entry(bucket).or_insert(0) += m.bytes_on_wire;
        }
    }
    totals
}

// ---------------------------------------------------------------------------------------
// I-N2
// ---------------------------------------------------------------------------------------

/// The statement of I-N2, from 03-interfaces.md §5.
pub const I_N2: &str = "fragmentation strategies must document reassembly timeout and loss \
                        amplification in their card";

/// **I-N2** — a fragmenter's card documents its timeout and its loss amplification.
///
/// A card check rather than a data check, because that is what the invariant is about. The
/// timeout must be a **declared parameter** (so a scenario can override it and the registry
/// can range-check it), and the loss amplification must appear in an equation or a stated
/// limitation (so a reader learns the formula, which 04-models.md §7.4 says every card
/// states).
///
/// Applies only to a card whose family is `fragmenter`; any other card is skipped.
#[must_use]
pub fn check_i_n2(card: &ModelCard) -> InvariantOutcome {
    if card.family != Family::Fragmenter {
        return InvariantOutcome::skipped(
            "I-N2",
            I_N2,
            format!("{} is a {} card, not a fragmenter", card.id, card.family),
        );
    }
    let mut violations = Vec::new();
    let has_timeout = card
        .parameters
        .iter()
        .any(|p| p.name.to_lowercase().contains("timeout"));
    if !has_timeout {
        violations.push(violation(
            "I-N2",
            Some(card.id.clone()),
            "the card declares no reassembly-timeout parameter".to_string(),
            &[("parameters", Number::int(card.parameters.len() as i64))],
        ));
    }
    let mentions_amplification = card.equations.iter().any(|e| {
        e.name.to_lowercase().contains("amplification")
            || e.latex_or_text.to_lowercase().contains("amplification")
    }) || card
        .limitations
        .iter()
        .any(|l| l.to_lowercase().contains("amplification"));
    if !mentions_amplification {
        violations.push(violation(
            "I-N2",
            Some(card.id.clone()),
            "the card states no loss-amplification formula in an equation or a limitation"
                .to_string(),
            &[("equations", Number::int(card.equations.len() as i64))],
        ));
    }
    InvariantOutcome {
        checked: 1,
        violations,
        ..InvariantOutcome::passed("I-N2", I_N2, 0)
    }
}

// ---------------------------------------------------------------------------------------
// I-M1
// ---------------------------------------------------------------------------------------

/// The statement of I-M1, from 03-interfaces.md §3.
pub const I_M1: &str = "`step` output ordering is by `ActorId`";

/// **I-M1** — one mobility step's output is ordered by `ActorId`.
///
/// Within one `SimTime`, the `gt.kinematics` records must arrive in non-decreasing actor
/// order. This is the property that makes a phase-parallel mobility step reproducible
/// (02-architecture.md §6.4: "merged in `NodeId` order"), and it is visible in the recording,
/// so it is checkable there.
///
/// A violation names the frame's instant and the two ids that were out of order.
///
/// Needs `gt.kinematics`, in arrival order — which is why [`EventLedger`] does not sort.
#[must_use]
pub fn check_i_m1(ledger: &EventLedger) -> InvariantOutcome {
    if ledger.kinematics.is_empty() {
        return InvariantOutcome::skipped("I-M1", I_M1, "no gt.kinematics records");
    }
    let mut violations = Vec::new();
    let mut frames = 0_u64;
    let mut prev: Option<(v2xw_core::time::SimTime, v2xw_core::ids::ActorId)> = None;
    for k in &ledger.kinematics {
        match prev {
            Some((t, actor)) if t == k.t => {
                if k.actor < actor {
                    violations.push(violation(
                        "I-M1",
                        Some(format!("t={}", k.t)),
                        "one mobility step's output is out of actor order".to_string(),
                        &[
                            ("previous_actor", Number::int(i64::from(actor.index()))),
                            ("this_actor", Number::int(i64::from(k.actor.index()))),
                        ],
                    ));
                }
            }
            _ => frames += 1,
        }
        prev = Some((k.t, k.actor));
    }
    InvariantOutcome {
        checked: frames,
        violations,
        ..InvariantOutcome::passed("I-M1", I_M1, 0)
    }
}

// ---------------------------------------------------------------------------------------
// I-P4
// ---------------------------------------------------------------------------------------

/// The statement of I-P4, from 05-protocols.md §7.
pub const I_P4: &str = "revocation stage timestamps are emitted";

/// **I-P4** — revocation stage timestamps are emitted, and in order.
///
/// Three things:
///
/// 1. Every stage id is one 05-protocols.md §8 names. A stage a protocol invented cannot be
///    placed in the latency decomposition and is reported.
/// 2. The emitted stages' timestamps are non-decreasing along §8's order. A `published`
///    earlier than its `issued` is a causality violation, not a rounding artefact.
/// 3. A revocation that reached `published` or beyond has emitted `decision` and `issued`:
///    a list entry with no decision behind it is a revocation nobody can account for.
///
/// Needs `proto.revocation`.
#[must_use]
pub fn check_i_p4(ledger: &EventLedger) -> InvariantOutcome {
    if ledger.revocation.is_empty() {
        return InvariantOutcome::skipped("I-P4", I_P4, "no proto.revocation records");
    }
    let mut by_id: BTreeMap<String, BTreeMap<String, v2xw_core::time::SimTime>> = BTreeMap::new();
    let mut violations = Vec::new();
    for r in &ledger.revocation {
        if stage_rank(&r.stage).is_none() {
            violations.push(violation(
                "I-P4",
                Some(r.id.clone()),
                format!(
                    "stage `{}` is not one of the stage ids 05-protocols.md §8 fixes, so it \
                     cannot be placed in the latency decomposition",
                    r.stage
                ),
                &[("t", Number::int(r.t as i64))],
            ));
            continue;
        }
        by_id
            .entry(r.id.clone())
            .or_default()
            .entry(r.stage.clone())
            .or_insert(r.t);
    }

    for (id, stages) in &by_id {
        let mut ordered: Vec<(usize, &String, v2xw_core::time::SimTime)> = stages
            .iter()
            .filter_map(|(name, t)| stage_rank(name).map(|r| (r, name, *t)))
            .collect();
        ordered.sort_by_key(|(r, _, _)| *r);
        for pair in ordered.windows(2) {
            let (_, from, t_from) = &pair[0];
            let (_, to, t_to) = &pair[1];
            if t_to < t_from {
                violations.push(violation(
                    "I-P4",
                    Some(id.clone()),
                    format!("stage `{to}` is timestamped before the earlier stage `{from}`"),
                    &[
                        ("t_from", Number::int(*t_from as i64)),
                        ("t_to", Number::int(*t_to as i64)),
                    ],
                ));
            }
        }
        let furthest = ordered.last().map(|(r, _, _)| *r).unwrap_or(0);
        if furthest >= stage_rank("published").unwrap_or(usize::MAX) {
            for required in ["decision", "issued"] {
                if !stages.contains_key(required) {
                    violations.push(violation(
                        "I-P4",
                        Some(id.clone()),
                        format!(
                            "this revocation reached `published` or beyond but never emitted \
                             `{required}`"
                        ),
                        &[("stages_emitted", Number::int(stages.len() as i64))],
                    ));
                }
            }
        }
    }

    InvariantOutcome {
        checked: by_id.len() as u64,
        violations,
        ..InvariantOutcome::passed("I-P4", I_P4, 0)
    }
}

// ---------------------------------------------------------------------------------------
// I-T2
// ---------------------------------------------------------------------------------------

/// The statement of I-T2, from 03-interfaces.md §9 and §17's recorder-discipline item.
pub const I_T2: &str = "`Detector` and `MaPipeline` run with the node's belief only; \
                        exporters tag their outputs `NODE`, and no record whose visibility \
                        is GT-tainted reaches a NODE channel";

/// **I-T2** — nothing ground-truth-tainted sits on a node channel.
///
/// For every `(channel, visibility)` pair the run produced:
///
/// 1. the tag must be one 03-interfaces.md §14 allows for that channel
///    ([`allowed_visibilities`]); and
/// 2. if every tag §14 allows for the channel is node-visible, a record carrying a
///    GT-tainted tag is a leak — `Visibility::allowed_on_node_channel` as a check, which is
///    exactly what §17's "recorder discipline" item asks for.
///
/// A channel §14 does not list is reported as unlisted rather than judged: a plug-in may
/// invent a channel, and this check cannot know what it may carry.
///
/// Needs only the `(channel, visibility)` tally, which [`EventLedger`] keeps for every
/// record including the ones it filtered out.
#[must_use]
pub fn check_i_t2(ledger: &EventLedger) -> InvariantOutcome {
    if ledger.visibility_seen.is_empty() {
        return InvariantOutcome::skipped("I-T2", I_T2, "no records");
    }
    let mut violations = Vec::new();
    for ((channel, visibility), count) in &ledger.visibility_seen {
        match allowed_visibilities(channel) {
            Some(allowed) => {
                if !allowed.contains(visibility) {
                    let names: Vec<String> = allowed.iter().map(ToString::to_string).collect();
                    violations.push(violation(
                        "I-T2",
                        Some(channel.clone()),
                        format!(
                            "records carry visibility `{visibility}`, which this channel may \
                             not: 03-interfaces.md §14 allows {}",
                            names.join(" or ")
                        ),
                        &[("records", Number::int(*count as i64))],
                    ));
                }
                let node_only = allowed.iter().all(|v| v.allowed_on_node_channel());
                if node_only && visibility.is_gt_tainted() {
                    violations.push(violation(
                        "I-T2",
                        Some(channel.clone()),
                        format!(
                            "a ground-truth-tainted record (`{visibility}`) reached a NODE \
                             channel"
                        ),
                        &[("records", Number::int(*count as i64))],
                    ));
                }
            }
            None => violations.push(violation(
                "I-T2",
                Some(channel.clone()),
                "this channel is not in 03-interfaces.md §14's table, so what it may carry is \
                 undeclared and the leak rule cannot be applied to it"
                    .to_string(),
                &[("records", Number::int(*count as i64))],
            )),
        }
    }
    InvariantOutcome {
        checked: ledger.visibility_seen.len() as u64,
        violations,
        ..InvariantOutcome::passed("I-T2", I_T2, 0)
    }
}

// ---------------------------------------------------------------------------------------
// I-T3
// ---------------------------------------------------------------------------------------

/// The statement of I-T3, from 03-interfaces.md §9 and 07-threats-and-detection.md.
pub const I_T3: &str = "every attack action that changes bytes on the air is logged as a GT \
                        event (`gt.attack.action`) with the true actor id, on a GT channel \
                        only";

/// **I-T3** — an attack that changed bytes on the air is logged, with its true actor, and
/// corresponds to a transmission.
///
/// The first two clauses are structural — the channel's tag is checked by
/// [`check_i_t2`] and the actor id is a required field of the record — so what is left, and
/// what this check does, is the part that can actually be wrong:
///
/// 1. An action claiming to have changed bytes on the air must name the **message** it
///    changed, so that the claim can be joined to the transmission. An action that names no
///    message is not verifiable, and an unverifiable log entry is the failure mode I-T3
///    exists to prevent.
/// 2. The named message must appear in `node.tx`. An attack logged with no corresponding
///    transmission means either the log or the transmission is missing, and either way the
///    ground-truth record of what went on the air is incomplete.
///
/// Needs `gt.attack.action` and `node.tx`.
#[must_use]
pub fn check_i_t3(ledger: &EventLedger) -> InvariantOutcome {
    let on_air: Vec<&crate::channels::GtAttackActionView> = ledger
        .attacks
        .iter()
        .filter(|a| a.changed_bytes_on_air)
        .collect();
    if on_air.is_empty() {
        return InvariantOutcome::skipped(
            "I-T3",
            I_T3,
            "no gt.attack.action records that changed bytes on the air",
        );
    }
    if ledger.tx.is_empty() {
        return InvariantOutcome::skipped(
            "I-T3",
            I_T3,
            "no node.tx records to join the attack actions to",
        );
    }
    let transmitted: std::collections::BTreeSet<u64> =
        ledger.tx.iter().filter_map(|t| t.msg).collect();
    let mut violations = Vec::new();
    for a in &on_air {
        match a.msg {
            None => violations.push(violation(
                "I-T3",
                Some(format!("actor {} t={}", a.actor, a.t)),
                format!(
                    "the action `{}` claims to have changed bytes on the air but names no \
                     message, so the claim cannot be joined to a transmission",
                    a.action
                ),
                &[("t", Number::int(a.t as i64))],
            )),
            Some(msg) if !transmitted.contains(&msg) => violations.push(violation(
                "I-T3",
                Some(format!("actor {} msg={}", a.actor, msg)),
                format!(
                    "the action `{}` names a message that never appears in node.tx",
                    a.action
                ),
                &[("msg", Number::int(msg as i64))],
            )),
            Some(_) => {}
        }
    }
    InvariantOutcome {
        checked: on_air.len() as u64,
        violations,
        ..InvariantOutcome::passed("I-T3", I_T3, 0)
    }
}

// ---------------------------------------------------------------------------------------
// I-S1
// ---------------------------------------------------------------------------------------

/// The statement of I-S1, from 03-interfaces.md §6.
pub const I_S1: &str = "for any run, `Real` and `Modeled` crypto modes produce identical \
                        verification outcomes, identical sizes, and identical event logs \
                        except the `crypto_mode` manifest field";

/// **I-S1** — the two crypto modes agree.
///
/// Compares two ledgers from runs that differ only in `crypto_mode`:
///
/// * the verification outcome sequences must be identical, element for element; and
/// * the transmitted sizes must be identical, element for element.
///
/// A violation names the index and both values, because "the logs differ" sends the reader
/// back to the data and "verification 4,218 was `valid` in modeled mode and `invalid` in
/// real mode" does not.
///
/// Needs `node.verify` and `node.tx` in both ledgers.
#[must_use]
pub fn check_i_s1(modeled: &EventLedger, real: &EventLedger) -> InvariantOutcome {
    if modeled.verify.is_empty() && real.verify.is_empty() && modeled.tx.is_empty() {
        return InvariantOutcome::skipped(
            "I-S1",
            I_S1,
            "neither ledger has node.verify or node.tx records",
        );
    }
    let mut violations = Vec::new();
    if modeled.verify.len() != real.verify.len() {
        violations.push(violation(
            "I-S1",
            None,
            "the two modes produced different numbers of verifications".to_string(),
            &[
                ("modeled", Number::int(modeled.verify.len() as i64)),
                ("real", Number::int(real.verify.len() as i64)),
            ],
        ));
    }
    for (i, (a, b)) in modeled.verify.iter().zip(real.verify.iter()).enumerate() {
        if a.outcome != b.outcome {
            violations.push(violation(
                "I-S1",
                Some(format!("node.verify[{i}]")),
                format!(
                    "verification outcome differs between modes: modeled `{:?}`, real `{:?}`",
                    a.outcome, b.outcome
                ),
                &[("index", Number::int(i as i64))],
            ));
        }
    }
    if modeled.tx.len() != real.tx.len() {
        violations.push(violation(
            "I-S1",
            None,
            "the two modes produced different numbers of transmissions".to_string(),
            &[
                ("modeled", Number::int(modeled.tx.len() as i64)),
                ("real", Number::int(real.tx.len() as i64)),
            ],
        ));
    }
    for (i, (a, b)) in modeled.tx.iter().zip(real.tx.iter()).enumerate() {
        if a.bytes_on_wire != b.bytes_on_wire {
            violations.push(violation(
                "I-S1",
                Some(format!("node.tx[{i}]")),
                "transmitted size differs between modes, so the size model and the real \
                 encoder disagree"
                    .to_string(),
                &[
                    ("modeled_bytes", Number::int(a.bytes_on_wire as i64)),
                    ("real_bytes", Number::int(b.bytes_on_wire as i64)),
                ],
            ));
        }
    }
    InvariantOutcome {
        checked: (modeled.verify.len() + modeled.tx.len()) as u64,
        violations,
        ..InvariantOutcome::passed("I-S1", I_S1, 0)
    }
}

// ---------------------------------------------------------------------------------------
// I-C1
// ---------------------------------------------------------------------------------------

/// The statement of I-C1, from 03-interfaces.md §1.2 and §17.
pub const I_C1: &str = "a plug-in may not keep wall-clock time, thread ids, or \
                        process-global RNG state; two runs emit byte-identical records";

/// **I-C1** — two runs emit byte-identical records.
///
/// Compares two record streams directly: channel, visibility and JSON bytes, in order. The
/// violation names the index, the channel and the two payloads (truncated to 200 bytes each,
/// because a diff is for reading).
///
/// This is the conformance kit's determinism check in a form a test can call.
#[must_use]
pub fn check_i_c1(a: &[OwnedRecord], b: &[OwnedRecord]) -> InvariantOutcome {
    if a.is_empty() && b.is_empty() {
        return InvariantOutcome::skipped("I-C1", I_C1, "both runs emitted no records");
    }
    let mut violations = Vec::new();
    if a.len() != b.len() {
        violations.push(violation(
            "I-C1",
            None,
            "the two runs emitted different numbers of records".to_string(),
            &[
                ("run_a", Number::int(a.len() as i64)),
                ("run_b", Number::int(b.len() as i64)),
            ],
        ));
    }
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        if x.channel != y.channel {
            violations.push(violation(
                "I-C1",
                Some(format!("record {i}")),
                format!(
                    "the two runs emitted different channels here: `{}` and `{}`",
                    x.channel, y.channel
                ),
                &[("index", Number::int(i as i64))],
            ));
            continue;
        }
        if x.visibility != y.visibility {
            violations.push(violation(
                "I-C1",
                Some(format!("record {i} on {}", x.channel)),
                format!(
                    "the two runs tagged this record differently: `{}` and `{}`",
                    x.visibility, y.visibility
                ),
                &[("index", Number::int(i as i64))],
            ));
        }
        if x.json != y.json {
            violations.push(violation(
                "I-C1",
                Some(format!("record {i} on {}", x.channel)),
                format!(
                    "the two runs emitted different bytes here: `{}` versus `{}`",
                    truncate(&x.json),
                    truncate(&y.json)
                ),
                &[("index", Number::int(i as i64))],
            ));
        }
    }
    InvariantOutcome {
        checked: a.len().min(b.len()) as u64,
        violations,
        ..InvariantOutcome::passed("I-C1", I_C1, 0)
    }
}

/// A record's payload, as at most 200 characters of text, for a diff message.
fn truncate(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.chars().count() <= 200 {
        return s.into_owned();
    }
    let head: String = s.chars().take(200).collect();
    format!("{head}…")
}

// ---------------------------------------------------------------------------------------
// D9
// ---------------------------------------------------------------------------------------

/// The statement of build decision D9's scanning test.
pub const D9: &str = "every exported float is finite and sits on the declared grid the \
                      writer quantised it onto; a scanning test fails the build if any \
                      output value sits off its grid";

/// **D9** — every float in every sample is finite and on the grid it was quantised onto.
///
/// The scanning check build decision D9 asks for, over the samples this crate produced.
/// [`MetricSample::new`] quantises, so a violation here means a sample was built some other
/// way — which is what the check is for.
///
/// # Which grid each float is held to
///
/// Not all of a sample's floats are on the metric's own grid, and that is deliberate:
/// [`crate::SampleValue::quantised`] puts a proportion's interval bounds on
/// [`Quantum::PROBABILITY`] and a ratio of sums' two sums on [`Quantum::SUM`]. The scan
/// therefore reads [`MetricSample::graded_floats`], which pairs each float with the grid the
/// writer used, and holds each float to that grid alone.
///
/// The earlier form of this check — `!s.quantum.holds(f) && !Quantum::PROBABILITY.holds(f)`
/// — **could not fail** for a wrongly quantised value. 1e-6 is the finest grid in the crate,
/// so a value on any coarser declared grid is also on the probability grid and the second
/// clause swallowed the first; what was left asked only "is this on the 1e-6 grid", which
/// is not what D9 states. A check that passes unconditionally is worse than no check,
/// because it reads as evidence, so the regression tests below inject a value quantised to
/// the *wrong* declared grid as well as a raw one.
///
/// # Non-finite values
///
/// `v2xw_core::math::is_on_grid` answers `true` for every non-finite value, by design: the
/// quantiser passes `NaN` and the infinities through rather than inventing a grid point for
/// them. A grid check alone therefore cannot see them, so this check tests finiteness
/// first — a `NaN` reaching a sample is a defect wherever it came from, and D9's promise
/// that an output value is a grid point is false for it.
///
/// A violation names the metric, the value and the grid it should have been on. A
/// non-finite value is reported by name in the detail rather than in the numbers, because
/// the numbers round-trip through JSON and `NaN` does not.
#[must_use]
pub fn check_d9_quantisation(samples: &[MetricSample]) -> InvariantOutcome {
    if samples.is_empty() {
        return InvariantOutcome::skipped("D9", D9, "no metric samples");
    }
    let mut violations = Vec::new();
    let mut floats = 0_u64;
    for s in samples {
        for (f, q) in s.graded_floats() {
            floats += 1;
            if !f.is_finite() {
                violations.push(violation(
                    "D9",
                    Some(s.key()),
                    format!(
                        "the value {f} is not finite, so it is on no grid: a quantiser \
                         passes it through untouched"
                    ),
                    &[("quantum", Number::Real(q.get()))],
                ));
                continue;
            }
            if !q.holds(f) {
                violations.push(violation(
                    "D9",
                    Some(s.key()),
                    format!("the value {f} is off the declared grid {}", q.get()),
                    &[
                        ("value", Number::Real(f)),
                        ("quantum", Number::Real(q.get())),
                    ],
                ));
            }
        }
    }
    InvariantOutcome {
        checked: floats,
        violations,
        ..InvariantOutcome::passed("D9", D9, 0)
    }
}

// ---------------------------------------------------------------------------------------
// The whole set
// ---------------------------------------------------------------------------------------

/// Runs every check that reads one run's records, in identifier order.
///
/// The three checks that need something else — [`check_i_n2`] (a card), [`check_i_s1`] (a
/// second run), [`check_i_c1`] (a second record stream) — are not included and are called
/// separately; [`check_d9_quantisation`] takes the run's samples and is included through
/// `samples`.
#[must_use]
pub fn check_all(ledger: &EventLedger, samples: &[MetricSample]) -> InvariantReport {
    InvariantReport::new(vec![
        check_i_m1(ledger),
        check_i_n1(ledger),
        check_i_p4(ledger),
        check_i_r3(ledger),
        check_i_t2(ledger),
        check_i_t3(ledger),
        check_d9_quantisation(samples),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::def::{Agg, DEFAULT_LEVEL, Dims, MetricDef, SampleValue};
    use crate::stats::{Estimate, Proportion};
    use serde_json::json;
    use v2xw_core::card::{Equation, Parameter, Source, SourceKind};
    use v2xw_core::ctx::Visibility;

    fn rec(channel: &'static str, visibility: Visibility, json: serde_json::Value) -> OwnedRecord {
        OwnedRecord {
            channel,
            visibility,
            json: serde_json::to_vec(&json).unwrap(),
        }
    }

    fn ledger(records: &[OwnedRecord]) -> EventLedger {
        let mut l = EventLedger::new();
        l.ingest_all(records);
        l
    }

    // --- I-R3 -------------------------------------------------------------------------

    fn good_rx(ok: bool) -> OwnedRecord {
        rec(
            "phy.rx",
            Visibility::NodeAndGt,
            if ok {
                json!({"t_start":0,"t_end":1,"rx":1,"outcome":"ok"})
            } else {
                json!({"t_start":0,"t_end":1,"rx":1,"outcome":"lost","cause":"collision"})
            },
        )
    }

    #[test]
    fn i_r3_holds_on_a_well_formed_run() {
        let l = ledger(&[good_rx(true), good_rx(false)]);
        let o = check_i_r3(&l);
        assert!(o.held(), "{:?}", o.violations);
        assert_eq!(o.checked, 2);
    }

    #[test]
    fn i_r3_detects_a_loss_with_no_cause() {
        let l = ledger(&[rec(
            "phy.rx",
            Visibility::NodeAndGt,
            json!({"t_start":0,"t_end":1,"rx":7,"outcome":"lost"}),
        )]);
        let o = check_i_r3(&l);
        assert!(!o.held());
        assert_eq!(o.violations[0].invariant, "I-R3");
        assert_eq!(o.violations[0].numbers["causes"], Number::Int(0));
        assert!(o.violations[0].detail.contains("exactly one cause"));
    }

    #[test]
    fn i_r3_detects_a_loss_with_two_causes() {
        let l = ledger(&[rec(
            "phy.rx",
            Visibility::NodeAndGt,
            json!({"t_start":0,"t_end":1,"rx":7,"outcome":"lost","cause":"collision",
                   "causes":["hidden-terminal"]}),
        )]);
        let o = check_i_r3(&l);
        assert!(!o.held());
        assert_eq!(o.violations[0].numbers["causes"], Number::Int(2));
    }

    #[test]
    fn i_r3_detects_a_received_frame_carrying_a_cause() {
        let l = ledger(&[rec(
            "phy.rx",
            Visibility::NodeAndGt,
            json!({"t_start":0,"t_end":1,"rx":7,"outcome":"ok","cause":"collision"}),
        )]);
        assert!(!check_i_r3(&l).held());
    }

    #[test]
    fn i_r3_detects_partial_airtime_accounting() {
        let l = ledger(&[
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":0,"node":1,"bytes_on_wire":100,"channel":180,"airtime_us":600}),
            ),
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":1,"node":1,"bytes_on_wire":100,"channel":180}),
            ),
        ]);
        let o = check_i_r3(&l);
        assert!(!o.held());
        let v = &o.violations[0];
        assert!(v.detail.contains("partial"), "{}", v.detail);
        assert_eq!(v.numbers["with_airtime"], Number::Int(1));
        assert_eq!(v.numbers["without_airtime"], Number::Int(1));
    }

    #[test]
    fn i_r3_detects_busy_time_that_does_not_cover_a_nodes_own_transmissions() {
        let l = ledger(&[
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":0,"node":1,"bytes_on_wire":100,"channel":180,"airtime_us":10_000}),
            ),
            rec(
                "mac.cbr",
                Visibility::Node,
                json!({"t":100_000_000,"node":1,"channel":180,"cbr":0.05,
                       "busy_us":5_000,"window_us":100_000}),
            ),
        ]);
        let o = check_i_r3(&l);
        assert!(!o.held());
        let v = o
            .violations
            .iter()
            .find(|v| v.detail.contains("hears itself"))
            .expect("the busy-time violation");
        assert_eq!(v.numbers["missing_us"], Number::Int(5_000));
    }

    // --- I-N1 -------------------------------------------------------------------------

    #[test]
    fn i_n1_holds_when_every_byte_has_one_bucket() {
        let l = ledger(&[
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":0,"node":1,"msg":1,"bytes_on_wire":400}),
            ),
            rec(
                "net.bytes",
                Visibility::Node,
                json!({"t":1,"id":2,"bucket":"backhaul","bytes_on_wire":1000}),
            ),
        ]);
        let o = check_i_n1(&l);
        assert!(o.held(), "{:?}", o.violations);
        assert_eq!(o.checked, 2);
        let totals = bucket_totals(&l);
        assert_eq!(totals[&ByteBucket::Air], 400);
        assert_eq!(totals[&ByteBucket::Backhaul], 1000);
    }

    #[test]
    fn i_n1_detects_one_frame_attributed_to_two_buckets() {
        let l = ledger(&[
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":0,"node":1,"msg":42,"bytes_on_wire":400}),
            ),
            rec(
                "net.bytes",
                Visibility::Node,
                json!({"t":1,"id":42,"bucket":"backhaul","bytes_on_wire":400}),
            ),
        ]);
        let o = check_i_n1(&l);
        assert!(!o.held());
        let v = &o.violations[0];
        assert_eq!(v.invariant, "I-N1");
        assert_eq!(v.subject.as_deref(), Some("id 42"));
        assert_eq!(v.numbers["air"], Number::Int(400));
        assert_eq!(v.numbers["backhaul"], Number::Int(400));
    }

    #[test]
    fn i_n1_detects_an_attribution_with_no_id() {
        let l = ledger(&[rec(
            "net.bytes",
            Visibility::Node,
            json!({"t":1,"bucket":"backend","bytes_on_wire":700}),
        )]);
        let o = check_i_n1(&l);
        assert!(!o.held());
        assert_eq!(o.violations[0].numbers["bytes"], Number::Int(700));
    }

    #[test]
    fn i_n1_detects_a_protocol_message_with_no_transport() {
        let l = ledger(&[rec(
            "proto.msg",
            Visibility::Node,
            json!({"t":1,"msg":9,"bytes_on_wire":250}),
        )]);
        assert!(!check_i_n1(&l).held());
    }

    // --- I-N2 -------------------------------------------------------------------------

    fn fragmenter_card(with_timeout: bool, with_amplification: bool) -> ModelCard {
        let mut card = ModelCard::new(
            "net/fragmenter/test",
            Family::Fragmenter,
            "1.0.0",
            "A fragmenter.",
        );
        if with_timeout {
            card.parameters.push(Parameter::new(
                "reassembly_timeout_ms",
                "ms",
                json!(1000),
                Source::new(SourceKind::Standard, "ETSI EN 302 636-4-1"),
            ));
        }
        if with_amplification {
            card.equations.push(Equation::new(
                "loss_amplification",
                "P(SDU lost) = 1 − (1 − p)^n for n fragments",
            ));
        }
        card
    }

    #[test]
    fn i_n2_holds_for_a_complete_fragmenter_card() {
        let o = check_i_n2(&fragmenter_card(true, true));
        assert!(o.held(), "{:?}", o.violations);
        assert_eq!(o.checked, 1);
    }

    #[test]
    fn i_n2_detects_a_missing_timeout_and_a_missing_amplification() {
        let o = check_i_n2(&fragmenter_card(false, false));
        assert_eq!(o.violations.len(), 2);
        assert!(o.violations.iter().any(|v| v.detail.contains("timeout")));
        assert!(
            o.violations
                .iter()
                .any(|v| v.detail.contains("amplification"))
        );
    }

    #[test]
    fn i_n2_skips_a_card_of_another_family() {
        let card = ModelCard::new("radio/phy/test", Family::Phy, "1.0.0", "A PHY.");
        let o = check_i_n2(&card);
        assert!(o.held());
        assert!(o.skipped.is_some());
    }

    // --- I-M1 -------------------------------------------------------------------------

    fn kin(t: u64, actor: u32) -> OwnedRecord {
        rec(
            "gt.kinematics",
            Visibility::Gt,
            json!({"t":t,"actor":actor,"x_m":0.0,"y_m":0.0,"speed_mps":1.0}),
        )
    }

    #[test]
    fn i_m1_holds_when_each_step_is_in_actor_order() {
        let l = ledger(&[kin(0, 1), kin(0, 2), kin(0, 7), kin(100, 1), kin(100, 5)]);
        let o = check_i_m1(&l);
        assert!(o.held(), "{:?}", o.violations);
        assert_eq!(o.checked, 2, "two frames");
    }

    #[test]
    fn i_m1_detects_an_out_of_order_step() {
        let l = ledger(&[kin(0, 1), kin(0, 7), kin(0, 2)]);
        let o = check_i_m1(&l);
        assert!(!o.held());
        let v = &o.violations[0];
        assert_eq!(v.numbers["previous_actor"], Number::Int(7));
        assert_eq!(v.numbers["this_actor"], Number::Int(2));
        assert_eq!(v.subject.as_deref(), Some("t=0"));
    }

    // --- I-P4 -------------------------------------------------------------------------

    fn stage(stage: &str, t: u64, id: &str) -> OwnedRecord {
        rec(
            "proto.revocation",
            Visibility::Public,
            json!({"t":t,"stage":stage,"id":id}),
        )
    }

    #[test]
    fn i_p4_holds_for_a_well_formed_revocation() {
        let l = ledger(&[
            stage("detect", 0, "r1"),
            stage("decision", 1_000, "r1"),
            stage("issued", 2_000, "r1"),
            stage("published", 3_000, "r1"),
        ]);
        let o = check_i_p4(&l);
        assert!(o.held(), "{:?}", o.violations);
        assert_eq!(o.checked, 1);
    }

    #[test]
    fn i_p4_detects_a_stage_out_of_order() {
        let l = ledger(&[
            stage("decision", 5_000, "r1"),
            stage("issued", 6_000, "r1"),
            stage("published", 1_000, "r1"),
        ]);
        let o = check_i_p4(&l);
        assert!(!o.held());
        let v = o
            .violations
            .iter()
            .find(|v| v.detail.contains("before the earlier stage"))
            .expect("the ordering violation");
        assert_eq!(v.numbers["t_from"], Number::Int(6_000));
        assert_eq!(v.numbers["t_to"], Number::Int(1_000));
    }

    #[test]
    fn i_p4_detects_a_published_revocation_with_no_decision() {
        let l = ledger(&[
            stage("issued", 1_000, "r1"),
            stage("published", 2_000, "r1"),
        ]);
        let o = check_i_p4(&l);
        assert!(!o.held());
        assert!(o.violations[0].detail.contains("never emitted `decision`"));
    }

    /// The I-P4 clause that requires `decision` and `issued` behind a published revocation
    /// keys on three stage *names*, and reaches them through
    /// `stage_rank("published").unwrap_or(usize::MAX)`. If the name ever stopped being a
    /// declared stage, that fallback would make `furthest >= usize::MAX` false for every
    /// revocation and the clause would stop running — silently, with the check still
    /// reporting that it held. The names are pinned here so the fallback stays unreachable.
    #[test]
    fn the_stage_names_the_i_p4_clause_keys_on_are_declared_stages() {
        for stage in ["published", "decision", "issued"] {
            assert!(
                crate::security::stage_rank(stage).is_some(),
                "`{stage}` is no longer a declared stage, which disables an I-P4 clause"
            );
        }
        assert!(crate::security::stage_rank("published").unwrap() < usize::MAX);
    }

    #[test]
    fn i_p4_detects_an_invented_stage() {
        let l = ledger(&[stage("thought-about-it", 1_000, "r1")]);
        let o = check_i_p4(&l);
        assert!(!o.held());
        assert!(o.violations[0].detail.contains("not one of the stage ids"));
    }

    // --- I-T2 -------------------------------------------------------------------------

    #[test]
    fn i_t2_holds_when_every_record_carries_its_channels_tag() {
        let l = ledger(&[
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":0,"node":1,"bytes_on_wire":100}),
            ),
            good_rx(true),
            kin(0, 1),
            stage("detect", 0, "r1"),
        ]);
        let o = check_i_t2(&l);
        assert!(o.held(), "{:?}", o.violations);
    }

    #[test]
    fn i_t2_detects_ground_truth_on_a_node_channel() {
        let l = ledger(&[rec(
            "node.tx",
            Visibility::Gt,
            json!({"t":0,"node":1,"bytes_on_wire":100}),
        )]);
        let o = check_i_t2(&l);
        assert!(!o.held());
        assert!(
            o.violations
                .iter()
                .any(|v| v.detail.contains("reached a NODE channel")),
            "{:?}",
            o.violations
        );
    }

    #[test]
    fn i_t2_reports_an_unlisted_channel_rather_than_judging_it() {
        let l = ledger(&[rec("a.plugin.invented.this", Visibility::Node, json!({}))]);
        let o = check_i_t2(&l);
        assert!(!o.held());
        assert!(
            o.violations[0]
                .detail
                .contains("not in 03-interfaces.md §14")
        );
    }

    // --- I-T3 -------------------------------------------------------------------------

    #[test]
    fn i_t3_holds_when_every_on_air_action_names_a_real_transmission() {
        let l = ledger(&[
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":0,"node":1,"msg":5,"bytes_on_wire":300}),
            ),
            rec(
                "gt.attack.action",
                Visibility::Gt,
                json!({"t":0,"actor":1,"attacker":"ghost","action":"false-position","msg":5}),
            ),
        ]);
        let o = check_i_t3(&l);
        assert!(o.held(), "{:?}", o.violations);
        assert_eq!(o.checked, 1);
    }

    #[test]
    fn i_t3_detects_an_action_that_names_no_message() {
        let l = ledger(&[
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":0,"node":1,"msg":5,"bytes_on_wire":300}),
            ),
            rec(
                "gt.attack.action",
                Visibility::Gt,
                json!({"t":0,"actor":1,"attacker":"ghost","action":"false-position"}),
            ),
        ]);
        let o = check_i_t3(&l);
        assert!(!o.held());
        assert!(o.violations[0].detail.contains("names no message"));
    }

    #[test]
    fn i_t3_detects_an_action_whose_message_was_never_transmitted() {
        let l = ledger(&[
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":0,"node":1,"msg":5,"bytes_on_wire":300}),
            ),
            rec(
                "gt.attack.action",
                Visibility::Gt,
                json!({"t":0,"actor":1,"attacker":"ghost","action":"x","msg":99}),
            ),
        ]);
        let o = check_i_t3(&l);
        assert!(!o.held());
        assert_eq!(o.violations[0].numbers["msg"], Number::Int(99));
    }

    #[test]
    fn i_t3_skips_a_passive_attacker() {
        let l = ledger(&[rec(
            "gt.attack.action",
            Visibility::Gt,
            json!({"t":0,"actor":1,"attacker":"passive","action":"eavesdrop",
                   "changed_bytes_on_air":false}),
        )]);
        let o = check_i_t3(&l);
        assert!(o.held());
        assert!(o.skipped.is_some());
    }

    // --- I-S1 -------------------------------------------------------------------------

    fn verify(outcome: &str) -> OwnedRecord {
        rec(
            "node.verify",
            Visibility::Node,
            json!({"t_enqueue":0,"node":1,"outcome":outcome}),
        )
    }

    #[test]
    fn i_s1_holds_when_the_two_modes_agree() {
        let a = ledger(&[verify("valid"), verify("invalid")]);
        let b = ledger(&[verify("valid"), verify("invalid")]);
        let o = check_i_s1(&a, &b);
        assert!(o.held(), "{:?}", o.violations);
    }

    #[test]
    fn i_s1_detects_a_differing_outcome_and_names_the_index() {
        let a = ledger(&[verify("valid"), verify("valid")]);
        let b = ledger(&[verify("valid"), verify("invalid")]);
        let o = check_i_s1(&a, &b);
        assert!(!o.held());
        assert_eq!(o.violations[0].subject.as_deref(), Some("node.verify[1]"));
        assert_eq!(o.violations[0].numbers["index"], Number::Int(1));
    }

    #[test]
    fn i_s1_detects_a_differing_size() {
        let tx = |bytes: u64| {
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":0,"node":1,"bytes_on_wire":bytes}),
            )
        };
        let a = ledger(&[tx(400)]);
        let b = ledger(&[tx(401)]);
        let o = check_i_s1(&a, &b);
        assert!(!o.held());
        assert_eq!(o.violations[0].numbers["modeled_bytes"], Number::Int(400));
        assert_eq!(o.violations[0].numbers["real_bytes"], Number::Int(401));
    }

    // --- I-C1 -------------------------------------------------------------------------

    #[test]
    fn i_c1_holds_for_two_identical_runs() {
        let run = vec![kin(0, 1), good_rx(true)];
        let o = check_i_c1(&run, &run.clone());
        assert!(o.held(), "{:?}", o.violations);
        assert_eq!(o.checked, 2);
    }

    #[test]
    fn i_c1_detects_a_single_differing_byte() {
        let a = vec![kin(0, 1)];
        let b = vec![kin(0, 2)];
        let o = check_i_c1(&a, &b);
        assert!(!o.held());
        assert!(o.violations[0].detail.contains("different bytes"));
        assert_eq!(o.violations[0].numbers["index"], Number::Int(0));
    }

    #[test]
    fn i_c1_detects_a_differing_record_count_and_a_differing_tag() {
        let o = check_i_c1(&[kin(0, 1), kin(0, 2)], &[kin(0, 1)]);
        assert!(!o.held());
        assert_eq!(o.violations[0].numbers["run_a"], Number::Int(2));

        let tagged = rec(
            "gt.kinematics",
            Visibility::Node,
            json!({"t":0,"actor":1,"x_m":0.0,"y_m":0.0,"speed_mps":1.0}),
        );
        let o = check_i_c1(&[kin(0, 1)], &[tagged]);
        assert!(!o.held());
        assert!(
            o.violations[0]
                .detail
                .contains("tagged this record differently")
        );
    }

    // --- D9 ---------------------------------------------------------------------------

    fn a_def() -> MetricDef {
        MetricDef::new(
            "pdr",
            "ratio",
            Agg::Mean,
            Visibility::Node,
            Quantum::RATIO,
            "A test metric.",
        )
        .not_accounting_for("being real")
    }

    #[test]
    fn d9_holds_for_samples_built_the_normal_way() {
        let s = vec![MetricSample::new(
            &a_def(),
            0,
            Dims::new(),
            SampleValue::Scalar(Estimate::Value {
                point: 1.0 / 3.0,
                n: 9,
            }),
        )];
        let o = check_d9_quantisation(&s);
        assert!(o.held(), "{:?}", o.violations);
        assert_eq!(o.checked, 1);
    }

    #[test]
    fn d9_detects_an_unquantised_float_that_bypassed_the_constructor() {
        let mut s = MetricSample::new(
            &a_def(),
            0,
            Dims::new(),
            SampleValue::Scalar(Estimate::Value { point: 0.5, n: 9 }),
        );
        // The fields are public so a reader can destructure a sample; writing a raw float
        // back into one is exactly what the scanning check exists to catch.
        s.value = SampleValue::Scalar(Estimate::Value {
            point: 1.0 / 3.0,
            n: 9,
        });
        let o = check_d9_quantisation(&[s]);
        assert!(!o.held());
        assert_eq!(o.violations[0].invariant, "D9");
        assert!(
            o.violations[0].detail.contains("off the declared grid"),
            "{}",
            o.violations[0].detail
        );
    }

    /// The defect this check was written to catch and could not: a value that **was**
    /// quantised, onto the wrong grid.
    ///
    /// `pdr` declares `Quantum::RATIO` (1e-4). A value quantised to the probability grid
    /// (1e-6) is off it, and the old guard —
    /// `!s.quantum.holds(f) && !Quantum::PROBABILITY.holds(f)` — accepted it, because 1e-6
    /// is the finest grid in the crate, so its clause was true for every value on any
    /// coarser declared grid and swallowed the one that mattered. 0.333333 is the verifier's
    /// case.
    #[test]
    fn d9_detects_a_value_quantised_onto_the_wrong_declared_grid() {
        let def = a_def();
        assert_eq!(def.quantum, Quantum::RATIO, "the case needs the 1e-4 grid");
        let mut s = MetricSample::new(
            &def,
            0,
            Dims::new(),
            SampleValue::Scalar(Estimate::Value { point: 0.5, n: 9 }),
        );
        let wrong = Quantum::PROBABILITY.quantise(1.0 / 3.0);
        assert_eq!(wrong, 0.333_333);
        assert!(
            Quantum::PROBABILITY.holds(wrong) && !Quantum::RATIO.holds(wrong),
            "the injected value must be on the finer grid and off the declared one"
        );
        s.value = SampleValue::Scalar(Estimate::Value { point: wrong, n: 9 });
        let o = check_d9_quantisation(&[s]);
        assert!(!o.held(), "a wrongly quantised value must be a violation");
        assert_eq!(o.violations.len(), 1);
        assert_eq!(o.violations[0].numbers["quantum"], Number::Real(1e-4));
        assert!(
            o.violations[0].detail.contains("0.333333"),
            "{}",
            o.violations[0].detail
        );
    }

    /// …and the grids the writer genuinely uses are still accepted, so the fix is not a
    /// blanket "everything must be on the metric's own grid".
    ///
    /// A proportion's bounds are quantised onto 1e-6 and a ratio of sums' two sums onto
    /// 1e-3, whatever the metric's quantum. This metric declares 1e-2 (dB), which is coarser
    /// than both, so a check that held every float to the metric's grid would report the
    /// writer's own output.
    #[test]
    fn d9_accepts_the_finer_grids_the_writer_itself_uses() {
        let def = MetricDef::new(
            "rssi_ratio",
            "db",
            Agg::Mean,
            Visibility::Node,
            Quantum::DB,
            "A test metric on the coarse dB grid.",
        )
        .not_accounting_for("being real");
        let proportion = MetricSample::new(
            &def,
            0,
            Dims::new(),
            SampleValue::Ratio(Proportion::from_counts(30, 40).estimate(1, DEFAULT_LEVEL)),
        );
        let sums = MetricSample::new(
            &def,
            0,
            Dims::new(),
            SampleValue::Ratio(crate::stats::ratio_of_sums(1.0 / 3.0, 7.0, 40, 1)),
        );
        // The bounds and the sums really are off the metric's own 1e-2 grid…
        assert!(
            proportion
                .floats()
                .iter()
                .any(|f| !Quantum::DB.holds(*f) && Quantum::PROBABILITY.holds(*f))
        );
        assert!(
            sums.floats()
                .iter()
                .any(|f| !Quantum::DB.holds(*f) && Quantum::SUM.holds(*f))
        );
        // …and the check holds anyway, because each float is judged on the grid it was
        // quantised onto.
        let o = check_d9_quantisation(&[proportion, sums]);
        assert!(o.held(), "{:?}", o.violations);
        assert_eq!(o.checked, 6);
    }

    /// A non-finite value is on no grid, and the quantiser passes it through: `is_on_grid`
    /// answers `true` for every `NaN` and infinity, so only an explicit finiteness test can
    /// see one. This is the second half of the `ratio_of_sums` defect — the numerator guard
    /// closes the door, and this is the net under it.
    #[test]
    fn d9_detects_a_non_finite_value_that_no_grid_test_can_see() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                Quantum::RATIO.holds(bad) && Quantum::PROBABILITY.holds(bad),
                "a grid test alone cannot see {bad}"
            );
            let mut s = MetricSample::new(
                &a_def(),
                0,
                Dims::new(),
                SampleValue::Scalar(Estimate::Value { point: 0.5, n: 9 }),
            );
            s.value = SampleValue::Scalar(Estimate::Value { point: bad, n: 9 });
            let o = check_d9_quantisation(&[s]);
            assert!(!o.held(), "{bad} was accepted");
            assert!(
                o.violations[0].detail.contains("is not finite"),
                "{}",
                o.violations[0].detail
            );
            // The report still round-trips: a NaN in `numbers` would serialise as `null`
            // and fail to read back, which is why the value is named in the detail only.
            let report = InvariantReport::new(vec![o]);
            let json = serde_json::to_vec(&report).unwrap();
            assert_eq!(
                serde_json::from_slice::<InvariantReport>(&json).unwrap(),
                report
            );
        }
    }

    // --- the whole set ----------------------------------------------------------------

    #[test]
    fn check_all_runs_every_single_run_check_in_identifier_order() {
        let l = ledger(&[
            rec(
                "node.tx",
                Visibility::Node,
                json!({"t":0,"node":1,"msg":1,"bytes_on_wire":400,"airtime_us":600}),
            ),
            good_rx(true),
            kin(0, 1),
            stage("detect", 0, "r1"),
        ]);
        let report = check_all(&l, &[]);
        assert_eq!(
            report
                .outcomes
                .iter()
                .map(|o| o.invariant.as_str())
                .collect::<Vec<_>>(),
            vec!["I-M1", "I-N1", "I-P4", "I-R3", "I-T2", "I-T3", "D9"]
        );
        assert!(report.held(), "{:?}", report.failed());
        report.assert_all().unwrap();
        // The two checks with no data say why.
        let skipped: BTreeMap<&str, &str> = report.skipped().into_iter().collect();
        assert!(skipped.contains_key("I-T3"));
        assert!(skipped.contains_key("D9"));
    }

    #[test]
    fn assert_all_names_every_violation_with_its_numbers() {
        let l = ledger(&[
            rec(
                "phy.rx",
                Visibility::NodeAndGt,
                json!({"t_start":0,"t_end":1,"rx":7,"outcome":"lost"}),
            ),
            rec(
                "net.bytes",
                Visibility::Node,
                json!({"t":1,"bucket":"backend","bytes_on_wire":700}),
            ),
        ]);
        let report = check_all(&l, &[]);
        assert!(!report.held());
        let mut failed = report.failed();
        failed.sort_unstable();
        assert_eq!(failed, vec!["I-N1", "I-R3"]);
        let e = report.assert_all().unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("I-R3"), "{msg}");
        assert!(msg.contains("I-N1"), "{msg}");
        assert!(msg.contains("bytes=700"), "{msg}");
        assert!(matches!(e, MetricError::InvariantViolated { count: 2, .. }));
    }

    #[test]
    fn a_report_round_trips_through_json_so_a_run_can_record_it() {
        let l = ledger(&[rec(
            "phy.rx",
            Visibility::NodeAndGt,
            json!({"t_start":0,"t_end":1,"rx":7,"outcome":"lost"}),
        )]);
        let report = check_all(&l, &[]);
        let json = serde_json::to_vec(&report).unwrap();
        let back: InvariantReport = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn every_check_skips_cleanly_on_an_empty_ledger() {
        let l = EventLedger::new();
        let report = check_all(&l, &[]);
        assert!(report.held());
        assert_eq!(report.skipped().len(), report.outcomes.len());
        report.assert_all().unwrap();
    }
}
