//! Hardware profiles: the schema of 06-node-models.md §1, and the loader for the data
//! files that carry it.
//!
//! A profile is *data*, not code: a YAML file under `profiles/hardware/` whose every
//! numeric field carries either a citation or an explicit admission that the vendor does
//! not publish it, together with a plan for measuring it. Rule **H1** of §1 is that there
//! is no third case, and [`HardwareProfile::validate`] is where it is enforced; rule
//! **H2** is that a profile with no secure element says `hsm.kind: none` rather than
//! leaving the block empty, so that "all crypto runs on the CPU" is a stated decision
//! rather than an accident of an absent key.
//!
//! # Why a field is a sum type
//!
//! The obvious schema — `cores: u32` — cannot express what the hardware research sheet
//! actually found, which is that Cohda publishes the MK5's DMIPS rating and not its core
//! clock. Given a plain `u32` an implementer has exactly two options: invent a number, or
//! drop the field. Both are worse than the truth. [`Field`] is the truth: either a value
//! with the source it came from, or no value with the measurement that would produce one.
//!
//! The consequence is visible in the model card. A published field becomes a
//! [`v2xw_core::card::Parameter`] with a real [`v2xw_core::card::Source`]; an unpublished
//! one becomes a parameter whose source kind is
//! [`v2xw_core::card::SourceKind::TodoCalibrate`] carrying the calibration plan, which is
//! exactly what registry rule R1 reports on. Counting the second kind is how this crate
//! answers "how much of this profile is real".
//!
//! # Deviations from the document's YAML, and why
//!
//! §7 writes the alternative software-crypto cost tables as nested maps inside
//! `software_crypto` (`class_cortex_a53_1_2ghz: {…}`), and the Luna appliance's tier
//! table inside `hsm`. Both are carried here under their own top-level keys
//! (`software_crypto_classes`, `hsm_tiers`) so that `software_crypto` and `hsm.ops` have
//! one value type each and a reader never has to guess whether a key is an operation or a
//! class. No number and no citation is changed by the move. Unknown keys are ignored, as
//! §7's conventions paragraph requires, so the `# ext` fields (GNSS, OS, environmental)
//! survive in the file without the loader having to model them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::model::Model;
use v2xw_core::time::Duration;

use crate::error::{NodeError, Result};

// ---------------------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------------------

/// Why a field carries no value, or what qualifies the value it carries.
///
/// The spellings are the research sheets' own, carried through rather than normalised:
/// §7's conventions paragraph says `NOT PUBLISHED` and `UNVERIFIED` tags are kept exactly
/// as the sheets record them, and a status this crate did not anticipate deserialises into
/// [`FieldStatus::Other`] rather than failing the load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldStatus {
    /// The vendor does not publish it.
    NotPublished,
    /// Nobody has measured it for this simulator yet.
    TodoCalibrate,
    /// A figure from a different device, board or chipset generation.
    Proxy,
    /// A figure from the previous generation of the same chipset.
    ProxyPreviousGeneration,
    /// A 3GPP evaluation-methodology assumption, not device data.
    EvaluationAssumption,
    /// A published figure the source itself treats as unconfirmed.
    Unverified,
    /// A published lower bound (`>2500/s`), not a measured rate.
    LowerBound,
    /// A vendor claim that reads as system-level rather than silicon-level.
    VendorClaimSystemLevel,
    /// Published, but the sheet flags it as implausible.
    AnomalousAsPublished,
    /// The field does not exist on this device, so there is nothing to publish and
    /// nothing to measure: a backend server has no antenna gain, and a profile with
    /// `hsm.kind: none` has no secure-element queue depth (rule H2).
    ///
    /// Distinguished from [`FieldStatus::NotPublished`] because conflating the two would
    /// make the `todo-calibrate` page list work that can never be done, which is how a
    /// gap report stops being read.
    NotApplicable,
    /// Any other spelling the sheets use.
    #[serde(untagged)]
    Other(String),
}

impl FieldStatus {
    /// True when the status means "there is no number here yet".
    ///
    /// Only these two reach the registry's `todo-calibrate` page: a `proxy` or a
    /// `lower-bound` field *has* a value and a citation, and reporting it as uncalibrated
    /// would drown the fields that genuinely have nothing.
    pub fn is_missing(&self) -> bool {
        matches!(self, FieldStatus::NotPublished | FieldStatus::TodoCalibrate)
    }

    /// True when the field does not exist on this device.
    pub fn is_not_applicable(&self) -> bool {
        matches!(self, FieldStatus::NotApplicable)
    }

    /// The spelling used in cards and reports.
    pub fn as_str(&self) -> &str {
        match self {
            FieldStatus::NotPublished => "not-published",
            FieldStatus::TodoCalibrate => "todo-calibrate",
            FieldStatus::Proxy => "proxy",
            FieldStatus::ProxyPreviousGeneration => "proxy-previous-generation",
            FieldStatus::EvaluationAssumption => "evaluation-assumption",
            FieldStatus::Unverified => "unverified",
            FieldStatus::LowerBound => "lower-bound",
            FieldStatus::VendorClaimSystemLevel => "vendor-claim-system-level",
            FieldStatus::AnomalousAsPublished => "anomalous-as-published",
            FieldStatus::NotApplicable => "not-applicable",
            FieldStatus::Other(s) => s.as_str(),
        }
    }
}

/// One numeric or textual field of a hardware profile, with its provenance.
///
/// Deserialises from either a bare scalar (`cores: 2`) or the qualified map form
/// (`{value: null, status: not-published, calibration: "…"}`). A bare scalar carries no
/// citation of its own and inherits the enclosing block's `source`, which is how §7 writes
/// the common case.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Field<T> {
    /// The value, when one is published.
    pub value: Option<T>,
    /// What qualifies the value, or why there is none.
    pub status: Option<FieldStatus>,
    /// Where the value came from.
    pub source: Option<String>,
    /// How to obtain the value. Rule H1 requires this whenever `value` is `None`.
    pub calibration: Option<String>,
}

impl<T> Field<T> {
    /// A published value with its citation.
    pub fn published(value: T, source: impl Into<String>) -> Self {
        Field {
            value: Some(value),
            status: None,
            source: Some(source.into()),
            calibration: None,
        }
    }

    /// An unpublished field with the measurement that would fill it.
    pub fn missing(status: FieldStatus, calibration: impl Into<String>) -> Self {
        Field {
            value: None,
            status: Some(status),
            source: None,
            calibration: Some(calibration.into()),
        }
    }

    /// True when no value is published.
    pub fn is_missing(&self) -> bool {
        self.value.is_none()
    }

    /// The value, if any.
    pub fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }
}

impl<T: Copy> Field<T> {
    /// The value or a caller-supplied stand-in.
    ///
    /// The stand-in is the caller's choice and therefore the caller's assumption: nothing
    /// in this module ever supplies one, because a default here would be exactly the
    /// invented number rule H1 exists to prevent.
    pub fn or(&self, fallback: T) -> T {
        self.value.unwrap_or(fallback)
    }
}

impl<T> Default for Field<T> {
    fn default() -> Self {
        Field {
            value: None,
            status: Some(FieldStatus::NotPublished),
            source: None,
            calibration: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FieldRepr<T> {
    Qualified(QualifiedField<T>),
    Bare(T),
}

#[derive(Deserialize)]
struct QualifiedField<T> {
    #[serde(default = "none")]
    value: Option<T>,
    #[serde(default)]
    status: Option<FieldStatus>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    calibration: Option<String>,
}

fn none<T>() -> Option<T> {
    None
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Field<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> core::result::Result<Self, D::Error> {
        Ok(match FieldRepr::<T>::deserialize(d)? {
            FieldRepr::Bare(value) => Field {
                value: Some(value),
                status: None,
                source: None,
                calibration: None,
            },
            FieldRepr::Qualified(q) => Field {
                value: q.value,
                status: q.status,
                source: q.source,
                calibration: q.calibration,
            },
        })
    }
}

// ---------------------------------------------------------------------------------------
// The profile
// ---------------------------------------------------------------------------------------

/// What kind of node a profile describes (06-node-models.md §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeKind {
    /// An on-board unit.
    Obu,
    /// A roadside unit.
    Rsu,
    /// A vulnerable-road-user device.
    VruDevice,
    /// A backend server.
    BackendServer,
    /// A cellular base station.
    BaseStation,
    /// A network-attached HSM appliance.
    HsmAppliance,
}

/// What kind of security hardware a profile has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HsmKind {
    /// A discrete secure element on its own bus.
    SecureElement,
    /// A security block inside the main SoC.
    SocHsm,
    /// No security hardware: rule H2 sends every operation to the CPU.
    None,
    /// A dedicated accelerator or appliance.
    Accelerator,
}

/// Where an operation runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunsOn {
    /// Inside the secure element or SoC HSM.
    Hsm,
    /// In a separate hardware engine (a baseband verification engine, say).
    Accelerator,
    /// On the application processor.
    Cpu,
}

/// A citation attached to a whole profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSource {
    /// What kind of document this is (`datasheet`, `product-brief`, `paper`, …).
    pub kind: String,
    /// The reference itself, with the research-sheet anchor §7 requires.
    #[serde(rename = "ref")]
    pub reference: String,
    /// When it was read.
    #[serde(default)]
    pub accessed: Option<String>,
}

/// The CPU block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuSpec {
    /// Core count.
    #[serde(default)]
    pub cores: Field<u32>,
    /// Core clock, hertz.
    #[serde(default)]
    pub clock_hz: Field<f64>,
    /// Instruction set / core family.
    #[serde(default)]
    pub arch: Field<String>,
    /// Dhrystone MIPS rating.
    #[serde(default)]
    pub dmips: Field<f64>,
}

/// One cryptographic operation on the security hardware.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HsmOp {
    /// Operations per second.
    #[serde(default)]
    pub throughput_per_s: Field<f64>,
    /// Per-operation latency, microseconds.
    #[serde(default)]
    pub latency_us: Field<f64>,
    /// Which engine runs it.
    #[serde(default)]
    pub runs_on: Field<RunsOn>,
}

impl HsmOp {
    /// The modelled service time for one call.
    ///
    /// The published latency when there is one; otherwise the reciprocal of the published
    /// throughput, which §7.1 names explicitly as the fallback ("until then service time =
    /// 1/throughput (500 us, derived) with a single HSM server") and marks `derived`. When
    /// neither is published the answer is `None`, not a guess: a node whose HSM has no cost
    /// anchor must be configured with one, or told to run the operation in software.
    pub fn service_time(&self) -> Option<Duration> {
        if let Some(us) = self.latency_us.value {
            return Some(Duration::from_secs_f64(us / 1e6));
        }
        let rate = self.throughput_per_s.value?;
        (rate > 0.0).then(|| Duration::from_secs_f64(1.0 / rate))
    }
}

/// The security-hardware block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HsmSpec {
    /// What kind of security hardware.
    pub kind: HsmKind,
    /// The part, when named.
    #[serde(default)]
    pub part: Field<String>,
    /// Per-operation costs, keyed by the operation id the profiles use
    /// (`ecdsa-p256-verify`, `ecdsa-p256-sign`, …).
    #[serde(default)]
    pub ops: BTreeMap<String, HsmOp>,
    /// How many requests the host may have outstanding.
    #[serde(default)]
    pub queue_depth: Field<u32>,
    /// How many engines serve the queue.
    #[serde(default)]
    pub servers: Field<u32>,
}

impl Default for HsmSpec {
    fn default() -> Self {
        HsmSpec {
            kind: HsmKind::None,
            part: Field::default(),
            ops: BTreeMap::new(),
            queue_depth: Field::default(),
            servers: Field::default(),
        }
    }
}

/// A software cost-table entry: the measured microseconds for one call, or nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CostUs {
    /// Microseconds per call.
    pub us: Option<f64>,
    /// What qualifies the figure, or why there is none.
    pub status: Option<FieldStatus>,
    /// Where it came from.
    pub source: Option<String>,
    /// How to measure it.
    pub calibration: Option<String>,
    /// The published rate, when the source gives one; recorded for provenance only.
    pub ops_per_s: Option<f64>,
}

#[derive(Deserialize)]
struct CostUsRepr {
    #[serde(default = "none")]
    us: Option<f64>,
    #[serde(default)]
    status: Option<FieldStatus>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    calibration: Option<String>,
    #[serde(default)]
    ops_per_s: Option<f64>,
}

impl<'de> Deserialize<'de> for CostUs {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> core::result::Result<Self, D::Error> {
        let r = CostUsRepr::deserialize(d)?;
        Ok(CostUs {
            us: r.us,
            status: r.status,
            source: r.source,
            calibration: r.calibration,
            ops_per_s: r.ops_per_s,
        })
    }
}

impl CostUs {
    /// The modelled service time, if the figure is published.
    pub fn service_time(&self) -> Option<Duration> {
        self.us.map(|us| Duration::from_secs_f64(us / 1e6))
    }
}

/// Transmit-power limits, dBm.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TxPower {
    /// Minimum.
    #[serde(default)]
    pub min: Field<f64>,
    /// Maximum.
    #[serde(default)]
    pub max: Field<f64>,
    /// The power a scenario starts a node at.
    #[serde(default)]
    pub default: Field<f64>,
}

/// The antenna block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AntennaSpec {
    /// Gain, dBi.
    #[serde(default)]
    pub gain_dbi: Field<f64>,
    /// Height above ground, metres.
    #[serde(default)]
    pub height_m: Field<f64>,
    /// Radiation pattern.
    #[serde(default)]
    pub pattern: Field<String>,
}

/// The radio block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RadioSpec {
    /// Radio access technologies.
    #[serde(default)]
    pub rat: Vec<String>,
    /// The chipset, when named.
    #[serde(default)]
    pub chipset: Field<String>,
    /// Transmit power limits.
    #[serde(default)]
    pub tx_power_dbm: TxPower,
    /// System receive sensitivity, dBm.
    #[serde(default)]
    pub sensitivity_dbm: Field<f64>,
    /// Receiver noise figure, dB.
    #[serde(default)]
    pub noise_figure_db: Field<f64>,
    /// The antenna.
    #[serde(default)]
    pub antenna: AntennaSpec,
}

/// How many bytes each stored item costs, and the fixed memory a node uses before it
/// stores anything.
///
/// `storage_model: inherit` in a profile means these defaults, which come from
/// 04-models.md §9 and 05-protocols.md §3 rather than from any vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageModel {
    /// Bytes per stored certificate.
    pub cert_bytes_per_entry: u32,
    /// Bytes per CRL entry.
    pub crl_bytes_per_entry: u32,
    /// Bytes per neighbour-table entry.
    pub neighbor_bytes_per_entry: u32,
    /// Fixed RAM the node occupies before any store grows.
    pub baseline_ram_bytes: u64,
}

impl Default for StorageModel {
    fn default() -> Self {
        StorageModel {
            // 05-protocols.md §2.5: "≈ 40 B/entry [BRECHT §VI-F]" for a linkage-seed CRL
            // entry (32 B of seeds plus framing); a `HashedId10` entry with its `Time32` is
            // 14 B by IEEE 1609.2 clause 7, so 40 is the larger of the two and the one the
            // store-size accounting of §2.2 uses.
            crl_bytes_per_entry: 40,
            // 06-node-models.md §2.2 gives 120 B per certificate entry.
            cert_bytes_per_entry: 120,
            // 06-node-models.md §2.2: "entries × ~200 B (`TODO: calibrate`)".
            neighbor_bytes_per_entry: 200,
            baseline_ram_bytes: 0,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StorageModelRepr {
    /// `storage_model: inherit` — the string is the keyword itself, which the loader
    /// checks by shape rather than by value: any scalar here means "take the defaults".
    Inherit(#[allow(dead_code)] String),
    Explicit(StorageModel),
}

fn de_storage<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> core::result::Result<StorageModel, D::Error> {
    Ok(match StorageModelRepr::deserialize(d)? {
        StorageModelRepr::Inherit(_) => StorageModel::default(),
        StorageModelRepr::Explicit(s) => s,
    })
}

/// A hardware profile: 06-node-models.md §1, with §7's data.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HardwareProfile {
    /// Registry id, e.g. `obu/cohda-mk5`.
    pub id: String,
    /// What kind of node it describes.
    pub kind: NodeKind,
    /// The profile's own version (rule H3: profiles are versioned and the manifest pins
    /// them).
    pub version: String,
    /// One line on what this profile is and what is doubtful about it.
    #[serde(default)]
    pub purpose: String,
    /// True for the profile 11-open-questions A3 proposes as the default OBU.
    #[serde(default)]
    pub reference_profile: bool,
    /// True for a profile describing a device nobody sells.
    #[serde(default)]
    pub hypothetical: bool,
    /// The documents behind it.
    #[serde(default)]
    pub sources: Vec<ProfileSource>,
    /// The application processor.
    #[serde(default)]
    pub cpu: CpuSpec,
    /// RAM, bytes.
    #[serde(default)]
    pub ram_bytes: Field<u64>,
    /// Non-volatile storage, bytes.
    #[serde(default)]
    pub flash_bytes: Field<u64>,
    /// Security hardware.
    #[serde(default)]
    pub hsm: HsmSpec,
    /// Software fallback costs, keyed by operation id.
    #[serde(default)]
    pub software_crypto: BTreeMap<String, CostUs>,
    /// Alternative software cost tables, keyed by class then operation. §7's
    /// `class_*` maps, lifted out of `software_crypto` (see the module documentation).
    #[serde(default)]
    pub software_crypto_classes: BTreeMap<String, BTreeMap<String, CostUs>>,
    /// The radio.
    #[serde(default)]
    pub radio: RadioSpec,
    /// Power draw, watts.
    #[serde(default)]
    pub power_w: Field<f64>,
    /// Store-size accounting.
    #[serde(default, deserialize_with = "de_storage")]
    pub storage_model: StorageModel,
    /// The model card, built once at load.
    #[serde(skip, default = "empty_card")]
    card: ModelCard,
}

fn empty_card() -> ModelCard {
    ModelCard::new(
        "hardware-profile/unloaded",
        Family::HardwareProfile,
        "0",
        "-",
    )
}

impl HardwareProfile {
    /// Parses a profile from YAML and validates it against rules H1 and H2.
    ///
    /// # Errors
    /// [`NodeError::ProfileParse`] when the YAML does not match the schema,
    /// [`NodeError::ProfileInvalid`] when a rule is broken, and [`NodeError::Card`] when
    /// the card the profile generates does not validate.
    pub fn from_yaml(text: &str) -> Result<Self> {
        let mut p: HardwareProfile =
            serde_yml::from_str(text).map_err(|source| NodeError::ProfileParse {
                id: "<unparsed>".to_string(),
                source,
            })?;
        p.validate()?;
        p.card = p.build_card();
        p.card.validate().map_err(|source| NodeError::Card {
            id: p.id.clone(),
            source,
        })?;
        Ok(p)
    }

    /// Checks rules H1 and H2 of 06-node-models.md §1.
    ///
    /// # Errors
    /// [`NodeError::ProfileInvalid`] naming the first field that breaks one.
    pub fn validate(&self) -> Result<()> {
        if self.hsm.kind == HsmKind::None && !self.hsm.ops.is_empty() {
            return Err(NodeError::ProfileInvalid {
                id: self.id.clone(),
                rule: "H2",
                what: format!(
                    "hsm.kind is `none` but {} operation(s) are declared on it; a profile \
                     without security hardware charges every operation to the CPU",
                    self.hsm.ops.len()
                ),
            });
        }
        for (name, f) in self.field_report() {
            if f.value_present {
                continue;
            }
            // A not-applicable field still owes the reader a sentence saying why it does
            // not exist, so the H1 check below covers it too; what it does not owe is a
            // place on the todo-calibrate page.
            if f.calibration.is_none_or(|c| c.trim().is_empty()) {
                return Err(NodeError::ProfileInvalid {
                    id: self.id.clone(),
                    rule: "H1",
                    what: format!("`{name}` has no value and no calibration plan"),
                });
            }
        }
        Ok(())
    }

    /// Every schema field this crate models, in a stable order, with whether it carries a
    /// value and what its provenance is.
    ///
    /// The order is the declaration order of the schema, which is fixed source text, so
    /// two runs produce the same report and the same card. Nothing here iterates a
    /// `HashMap`: `software_crypto` and `hsm.ops` are `BTreeMap`s.
    pub fn field_report(&self) -> Vec<(String, FieldFacts<'_>)> {
        let mut out: Vec<(String, FieldFacts<'_>)> = Vec::new();
        macro_rules! push {
            ($name:expr, $facts:expr $(,)?) => {
                out.push(($name, $facts))
            };
        }

        push!("cpu.cores".into(), FieldFacts::of(&self.cpu.cores));
        push!("cpu.clock_hz".into(), FieldFacts::of(&self.cpu.clock_hz));
        push!("cpu.arch".into(), FieldFacts::of(&self.cpu.arch));
        push!("cpu.dmips".into(), FieldFacts::of(&self.cpu.dmips));
        push!("ram_bytes".into(), FieldFacts::of(&self.ram_bytes));
        push!("flash_bytes".into(), FieldFacts::of(&self.flash_bytes));
        push!("hsm.part".into(), FieldFacts::of(&self.hsm.part));
        for (op, spec) in &self.hsm.ops {
            push!(
                format!("hsm.ops.{op}.throughput_per_s"),
                FieldFacts::of(&spec.throughput_per_s),
            );
            push!(
                format!("hsm.ops.{op}.latency_us"),
                FieldFacts::of(&spec.latency_us),
            );
            push!(
                format!("hsm.ops.{op}.runs_on"),
                FieldFacts::of(&spec.runs_on),
            );
        }
        push!(
            "hsm.queue_depth".into(),
            FieldFacts::of(&self.hsm.queue_depth),
        );
        push!("hsm.servers".into(), FieldFacts::of(&self.hsm.servers));
        for (op, cost) in &self.software_crypto {
            push!(
                format!("software_crypto.{op}.us"),
                FieldFacts::of_cost(cost)
            );
        }
        for (class, table) in &self.software_crypto_classes {
            for (op, cost) in table {
                push!(
                    format!("software_crypto_classes.{class}.{op}.us"),
                    FieldFacts::of_cost(cost),
                );
            }
        }
        push!("radio.chipset".into(), FieldFacts::of(&self.radio.chipset));
        push!(
            "radio.tx_power_dbm.min".into(),
            FieldFacts::of(&self.radio.tx_power_dbm.min),
        );
        push!(
            "radio.tx_power_dbm.max".into(),
            FieldFacts::of(&self.radio.tx_power_dbm.max),
        );
        push!(
            "radio.tx_power_dbm.default".into(),
            FieldFacts::of(&self.radio.tx_power_dbm.default),
        );
        push!(
            "radio.sensitivity_dbm".into(),
            FieldFacts::of(&self.radio.sensitivity_dbm),
        );
        push!(
            "radio.noise_figure_db".into(),
            FieldFacts::of(&self.radio.noise_figure_db),
        );
        push!(
            "radio.antenna.gain_dbi".into(),
            FieldFacts::of(&self.radio.antenna.gain_dbi),
        );
        push!(
            "radio.antenna.height_m".into(),
            FieldFacts::of(&self.radio.antenna.height_m),
        );
        push!(
            "radio.antenna.pattern".into(),
            FieldFacts::of(&self.radio.antenna.pattern),
        );
        push!("power_w".into(), FieldFacts::of(&self.power_w));
        out
    }

    /// How many of this profile's fields carry no value and therefore need calibrating.
    pub fn todo_calibrate_count(&self) -> usize {
        self.field_report()
            .iter()
            .filter(|(_, f)| f.needs_calibration())
            .count()
    }

    /// The names of the fields that need calibrating, in schema order.
    pub fn todo_calibrate_fields(&self) -> Vec<String> {
        self.field_report()
            .into_iter()
            .filter(|(_, f)| f.needs_calibration())
            .map(|(n, _)| n)
            .collect()
    }

    /// The service time this profile gives one cryptographic operation, and where it runs.
    ///
    /// Resolution order, which is rule H2 plus §2.1's "service times come from the
    /// primitive cost tables scaled by the profile":
    ///
    /// 1. the HSM's own figure for the operation, when the profile has security hardware
    ///    and publishes one;
    /// 2. otherwise the software cost table, charged to the CPU.
    ///
    /// `None` means the profile publishes neither, which a scenario must resolve before a
    /// node using it can run — deliberately, because the alternative is an invented number
    /// that would silently set the load of every run.
    pub fn op_cost(&self, op: &str) -> Option<(Duration, RunsOn)> {
        if self.hsm.kind != HsmKind::None
            && let Some(spec) = self.hsm.ops.get(op)
            && let Some(t) = spec.service_time()
        {
            return Some((t, spec.runs_on.value.unwrap_or(RunsOn::Hsm)));
        }
        let t = self.software_crypto.get(op)?.service_time()?;
        Some((t, RunsOn::Cpu))
    }

    /// The profile's model card (03-interfaces.md §12, family `hardware-profile`).
    pub fn card(&self) -> &ModelCard {
        &self.card
    }

    fn build_card(&self) -> ModelCard {
        let mut card = ModelCard::new(
            &self.id,
            Family::HardwareProfile,
            &self.version,
            if self.purpose.is_empty() {
                format!("Hardware profile for {}.", self.id)
            } else {
                self.purpose.clone()
            },
        );
        // A profile is data, not a fidelity choice: the same numbers describe the device
        // whichever tier the scenario runs the node at (06-node-models.md §2.1 changes how
        // many servers consume them, not what they are).
        card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
        card.sources = self
            .sources
            .iter()
            .map(|s| {
                let mut src = Source::new(source_kind(&s.kind), s.reference.clone());
                src.accessed = s.accessed.clone();
                src
            })
            .collect();
        for (name, facts) in self.field_report() {
            // A field that does not exist on this device is not a parameter of the model:
            // a backend server has no antenna gain and a profile with `hsm.kind: none` has
            // no secure-element queue depth. Carrying them as parameters would put thirty
            // entries on the registry's todo-calibrate page describing work that can never
            // be done, which is how a gap report stops being read. The YAML still explains
            // each one, and `HardwareProfile::validate` still demands that explanation.
            if facts.status.is_some_and(FieldStatus::is_not_applicable) {
                continue;
            }
            let unit = unit_of(&name).to_string();
            let mut p = if facts.value_present {
                Parameter::new(
                    name,
                    unit,
                    facts.default.clone(),
                    Source::new(
                        SourceKind::Datasheet,
                        facts
                            .source
                            .map(str::to_string)
                            .unwrap_or_else(|| self.first_source_ref()),
                    ),
                )
            } else {
                let mut p = Parameter::new(
                    name,
                    unit,
                    serde_json::Value::Null,
                    Source::todo_calibrate(
                        facts
                            .status
                            .map(|s| s.as_str().to_string())
                            .unwrap_or_else(|| "not-published".to_string()),
                    ),
                );
                p.calibration = facts.calibration.map(str::to_string);
                p
            };
            if let Some(status) = facts.status
                && facts.value_present
            {
                p.source.note = Some(status.as_str().to_string());
            }
            card.parameters.push(p);
        }
        card.assumptions.push(
            "Service times come from the profile's own figures only; where a vendor \
             publishes none, the profile says so and the scenario must supply one \
             (06-node-models.md §1 rule H1)."
                .to_string(),
        );
        let report = self.field_report();
        let not_applicable = report
            .iter()
            .filter(|(_, f)| f.status.is_some_and(FieldStatus::is_not_applicable))
            .count();
        card.limitations.push(format!(
            "{} of {} fields that exist on this device carry no published value and are \
             listed on the registry's todo-calibrate page (rule R1); a further {} fields \
             of the schema do not exist on this device at all.",
            self.todo_calibrate_count(),
            report.len() - not_applicable,
            not_applicable
        ));
        card.validation = Validation::new(ValidationStatus::LiteratureChecked);
        card
    }

    fn first_source_ref(&self) -> String {
        self.sources
            .first()
            .map(|s| s.reference.clone())
            .unwrap_or_else(|| format!("{} (profile-level sources)", self.id))
    }
}

impl Model for HardwareProfile {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

/// The provenance of one field, flattened so that [`HardwareProfile::field_report`] can
/// return every field of a heterogeneous schema in one list.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldFacts<'a> {
    /// Whether the field carries a value.
    pub value_present: bool,
    /// The value as JSON, for the model card's `default`.
    pub default: serde_json::Value,
    /// What qualifies the value, or why there is none.
    pub status: Option<&'a FieldStatus>,
    /// Where it came from.
    pub source: Option<&'a str>,
    /// How to measure it.
    pub calibration: Option<&'a str>,
}

impl FieldFacts<'_> {
    /// True when the field carries no value and the value is one that could exist.
    pub fn needs_calibration(&self) -> bool {
        !self.value_present && !self.status.is_some_and(FieldStatus::is_not_applicable)
    }
}

impl<'a> FieldFacts<'a> {
    fn of<T: Serialize>(f: &'a Field<T>) -> Self {
        FieldFacts {
            value_present: f.value.is_some(),
            default: f
                .value
                .as_ref()
                .and_then(|v| serde_json::to_value(v).ok())
                .unwrap_or(serde_json::Value::Null),
            status: f.status.as_ref(),
            source: f.source.as_deref(),
            calibration: f.calibration.as_deref(),
        }
    }

    fn of_cost(c: &'a CostUs) -> Self {
        FieldFacts {
            value_present: c.us.is_some(),
            default: c
                .us
                .and_then(|v| serde_json::to_value(v).ok())
                .unwrap_or(serde_json::Value::Null),
            status: c.status.as_ref(),
            source: c.source.as_deref(),
            calibration: c.calibration.as_deref(),
        }
    }
}

fn source_kind(kind: &str) -> SourceKind {
    match kind {
        "paper" => SourceKind::Paper,
        "spec" | "standard" => SourceKind::Standard,
        "benchmark" | "dataset" => SourceKind::Dataset,
        "code" => SourceKind::Code,
        "gap" => SourceKind::TodoCalibrate,
        // Product briefs, information sheets, FCC filings, security policies and press
        // releases are all vendor documents; the sheet's own `kind` string is kept in the
        // reference text, so nothing is lost by mapping them to one enum variant.
        _ => SourceKind::Datasheet,
    }
}

fn unit_of(name: &str) -> &'static str {
    if name.ends_with("_hz") {
        "Hz"
    } else if name.ends_with("_bytes") {
        "B"
    } else if name.ends_with(".us") || name.ends_with("_us") {
        "us"
    } else if name.ends_with("per_s") {
        "1/s"
    } else if name.ends_with("_dbm")
        || name.ends_with("dbm.min")
        || name.ends_with("dbm.max")
        || name.ends_with("dbm.default")
    {
        "dBm"
    } else if name.ends_with("_db") {
        "dB"
    } else if name.ends_with("_dbi") {
        "dBi"
    } else if name.ends_with("_m") {
        "m"
    } else if name.ends_with("_w") {
        "W"
    } else if name.ends_with("cores") || name.ends_with("queue_depth") || name.ends_with("servers")
    {
        "count"
    } else if name.ends_with("dmips") {
        "DMIPS"
    } else {
        "-"
    }
}
