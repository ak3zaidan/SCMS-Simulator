//! The published scenario surface: what a generated settings form is built from.
//!
//! 13-product-direction.md §2 settles the shape of the problem. The scenario has around
//! twenty top-level sections with deep nesting and the model registry holds every model's
//! parameters with their units, defaults, ranges and cited sources; hand-written forms
//! would cover a subset on the day they were written and drift the day after. So the form
//! is *generated*, and this module is what it is generated from.
//!
//! The rule that makes it worth trusting: **nothing here names a scenario field.** The
//! field tree comes from a build-time reflection of `schema.rs` itself (see the crate's
//! `build.rs`), the ranges and closed value sets come from the tables the loader's own
//! validator reads ([`crate::scenario::validate::BOUNDS`],
//! [`crate::scenario::validate::CHOICES`]), the implementation status comes from
//! [`crate::scenario::validate::KEY_STATUS`], and the model parameters come from the
//! registry's cards. A field appears in the page because it exists in the type, not
//! because somebody remembered it.
//!
//! # What is published
//!
//! | Key | What it is |
//! |---|---|
//! | `schema` | JSON Schema 2020-12 over the whole scenario, with `unit`, `default`, `x-status` and `x-path` on every node |
//! | `fields` | the same tree flattened to one row per editable leaf, which is what a searchable form wants |
//! | `groups` | the sections, in the order a user thinks about them |
//! | `slots` | every place the scenario picks a model, with the ids that are selectable there |
//! | `models` | every registered model: its card, its parameters with units, defaults, ranges and sources, and its `todo: calibrate` list |
//! | `statuses` | the implementation-status vocabulary, so the page can render it without hard-coding the words |
//!
//! # The three guarantees the tests hold
//!
//! * **No field the loader accepts is missing.** `every_shipped_scenario_is_described`
//!   walks each scenario in `scenarios/` and requires the schema to describe every key in
//!   it.
//! * **No field the loader rejects is offered.** `the_published_defaults_load` builds a
//!   document out of the published defaults and feeds it to the loader, whose
//!   `deny_unknown_fields` rejects an invented key.
//! * **No field is offered unclassified.** `every_leaf_has_a_status` requires every leaf
//!   to match a [`KEY_STATUS`](crate::scenario::validate::KEY_STATUS) row, so a new field
//!   cannot reach the page without someone saying whether the engine acts on it.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use v2xw_core::card::Family;
use v2xw_core::registry::Registry;

use crate::scenario::validate::{self, Status};

/// One field of a reflected struct or struct-like enum variant.
///
/// Filled by the build script from the declaration in `schema.rs`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RField {
    /// The field's Rust name.
    pub(crate) rust_name: &'static str,
    /// Its name on the wire, after any `#[serde(rename)]` or `rename_all`.
    pub(crate) wire_name: &'static str,
    /// Its Rust type, as written.
    pub(crate) ty: &'static str,
    /// Its doc comment's first paragraph.
    pub(crate) doc: &'static str,
    /// Whether it has a `#[serde(default)]`, which is what makes it optional to write.
    pub(crate) has_default: bool,
    /// Whether it is `#[serde(flatten)]`ed into its parent.
    pub(crate) flatten: bool,
    /// The `#[serde(with = "…")]` module, if any.
    pub(crate) with: &'static str,
}

/// One variant of a reflected enum.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RVariant {
    /// Its name on the wire.
    pub(crate) wire_name: &'static str,
    /// Its doc comment.
    pub(crate) doc: &'static str,
    /// Its fields, for a struct-like variant.
    pub(crate) fields: &'static [RField],
}

/// One reflected `pub struct` or `pub enum`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RType {
    /// The Rust type name.
    pub(crate) name: &'static str,
    /// Its doc comment.
    pub(crate) doc: &'static str,
    /// Whether it is an enum.
    pub(crate) is_enum: bool,
    /// Its internal tag, from `#[serde(tag = "…")]`; empty when there is none.
    pub(crate) tag: &'static str,
    /// Whether `#[serde(deny_unknown_fields)]` is on it.
    pub(crate) deny_unknown: bool,
    /// Its fields, for a struct.
    pub(crate) fields: &'static [RField],
    /// Its variants, for an enum.
    pub(crate) variants: &'static [RVariant],
}

include!(concat!(env!("OUT_DIR"), "/scenario_reflect.rs"));

/// How deep the walk goes before it stops describing and says so.
///
/// The scenario's own nesting is five deep at its worst. The limit exists so that a type
/// that somehow refers to itself cannot make this function run forever; a node that hits
/// it is published as an opaque JSON editor rather than dropped.
const MAX_DEPTH: usize = 12;

/// The sections, in the order 13-product-direction.md §2 asks for: grouped the way a user
/// thinks rather than by crate.
///
/// Keyed by the scenario's top-level field name. `every_section_has_a_group` fails the
/// build if a section is added to the schema and not placed here, so the grouping cannot
/// silently fall back to "Other".
const GROUPS: &[(&str, &str, &str)] = &[
    (
        "meta",
        "Run",
        "What this scenario is, who wrote it, and what it is based on.",
    ),
    (
        "schema",
        "Run",
        "What this scenario is, who wrote it, and what it is based on.",
    ),
    (
        "seed",
        "Run",
        "What this scenario is, who wrote it, and what it is based on.",
    ),
    (
        "time",
        "Run",
        "What this scenario is, who wrote it, and what it is based on.",
    ),
    (
        "world",
        "World",
        "The map: where it comes from, its buildings and its terrain.",
    ),
    (
        "actors",
        "Traffic",
        "What moves and what transmits: the fleet, its size, roadside units and the backend.",
    ),
    (
        "weather",
        "Environment",
        "The weather the run starts in and what it does to the radio.",
    ),
    (
        "radio",
        "Radio",
        "The radio stack: which technology, at which fidelity, with which models.",
    ),
    (
        "net",
        "Network",
        "The network layer between the application and the radio.",
    ),
    (
        "messages",
        "Messages",
        "Which V2X messages are generated and how they are encoded.",
    ),
    (
        "security",
        "Security",
        "The security envelope, the cryptography and the privacy policies.",
    ),
    (
        "nodes",
        "Nodes",
        "Which hardware the on-board units run on.",
    ),
    (
        "threats",
        "Threats",
        "Attackers, jammers and compromised infrastructure.",
    ),
    (
        "detection",
        "Detection",
        "Misbehaviour detectors and the authority that acts on them.",
    ),
    (
        "metrics",
        "Measurement",
        "What is measured, exported and swept.",
    ),
    (
        "exporters",
        "Measurement",
        "What is measured, exported and swept.",
    ),
    (
        "events",
        "Timeline",
        "Things that happen at a stated instant during the run.",
    ),
    (
        "experiment",
        "Measurement",
        "What is measured, exported and swept.",
    ),
];

/// Unit suffixes, longest first: the schema puts a field's unit in its name on purpose
/// (see `schema.rs`'s header), so the unit is read back off the name rather than
/// maintained in a second table that could disagree with it.
const UNIT_SUFFIXES: &[(&str, &str)] = &[
    ("_veh_per_h", "veh/h"),
    ("_mbps", "Mbit/s"),
    ("_kbps", "kbit/s"),
    ("_dbm", "dBm"),
    ("_dbi", "dBi"),
    ("_deg", "°"),
    ("_ms", "ms"),
    ("_us", "µs"),
    ("_ns", "ns"),
    ("_hz", "Hz"),
    ("_km", "km"),
    ("_db", "dB"),
    ("_m", "m"),
    ("_s", "s"),
];

/// The numeric leaves whose unit their name does not carry.
///
/// Short on purpose. `every_number_publishes_a_unit` fails the build when a numeric leaf
/// is neither suffixed, listed here, nor listed in [`DIMENSIONLESS`] — so a new number
/// cannot reach the page without a unit beside it, which is what
/// 13-product-direction.md §2 requires of every field.
const UNIT_OVERRIDES: &[(&str, &str)] = &[
    ("world.buildings.metres_per_level", "m/storey"),
    ("actors.vru.pedestrians", "people"),
    ("actors.vru.cyclists", "people"),
    ("actors.rsus[].site", "site id"),
    ("radio.tiers.focus.region.node", "node id"),
    ("threats.attackers[].count", "vehicles"),
    ("threats.attackers[].actor_ids[]", "actor id"),
    ("threats.compromised_rsus[]", "site id"),
    ("events[].t", "s"),
    ("events[].until", "s"),
    ("experiment.replications", "runs"),
];

/// The numeric leaves that genuinely have no unit, declared rather than left blank.
const DIMENSIONLESS: &[&str] = &[
    "seed",
    "experiment.seeds[]",
    "weather.intensity",
    "actors.vehicles.equipped_fraction",
    "actors.vehicles.classes.*.fraction",
    "actors.vru.device_fraction",
    "threats.attackers[].fraction",
];

/// One place the scenario picks a model, and which registry family fills it.
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    /// The dotted scenario path the choice is written at.
    pub path: &'static str,
    /// What to call it in the form.
    pub label: &'static str,
    /// The registry family whose models may go here.
    pub family: Family,
    /// How the choice is spelled: `true` for a bare id string, `false` for a
    /// `{id, params}` object.
    pub bare_id: bool,
}

/// Every swappable slot in the scenario.
///
/// The families are what makes this table not a duplicate: the *members* of each slot's
/// choice list come from the registry at run time, so a model added to a crate becomes
/// selectable without an edit here. Only the mapping from a scenario path to a family
/// lives here, and that mapping is what the schema itself cannot express.
///
/// A slot whose status is not `wired` is still published, because a page that hides it
/// cannot explain why the thing the owner asked for is missing. Its status says what
/// happens if it is set.
pub static SLOTS: &[Slot] = &[
    Slot {
        path: "world.source.generator",
        label: "World generator",
        family: Family::World,
        bare_id: true,
    },
    Slot {
        path: "actors.vehicles.demand.kind",
        label: "Demand model",
        family: Family::Mobility,
        bare_id: true,
    },
    Slot {
        path: "nodes.default_obu",
        label: "On-board unit",
        family: Family::HardwareProfile,
        bare_id: true,
    },
    Slot {
        path: "nodes.per_class.*",
        label: "On-board unit, per vehicle class",
        family: Family::HardwareProfile,
        bare_id: true,
    },
    Slot {
        path: "actors.rsus[].profile",
        label: "Roadside unit hardware",
        family: Family::HardwareProfile,
        bare_id: true,
    },
    Slot {
        path: "actors.backend.entities.*.profile",
        label: "Backend entity hardware",
        family: Family::HardwareProfile,
        bare_id: true,
    },
    Slot {
        path: "actors.backend.entities.*.service_model",
        label: "Backend service model",
        family: Family::ServiceModel,
        bare_id: true,
    },
    Slot {
        path: "actors.backend.entities.*.net",
        label: "Backend network model",
        family: Family::BackendNet,
        bare_id: true,
    },
    Slot {
        path: "actors.backend.protocol",
        label: "Credential-management protocol",
        family: Family::Protocol,
        bare_id: true,
    },
    Slot {
        path: "actors.rsus[].backhaul",
        label: "Roadside backhaul",
        family: Family::Backhaul,
        bare_id: true,
    },
    Slot {
        path: "net.fragmenter",
        label: "Fragmenter",
        family: Family::Fragmenter,
        bare_id: false,
    },
    Slot {
        path: "net.backhaul",
        label: "Backhaul link",
        family: Family::Backhaul,
        bare_id: false,
    },
    Slot {
        path: "net.uu",
        label: "Cellular uplink",
        family: Family::Cellular,
        bare_id: false,
    },
    Slot {
        path: "net.backend_net",
        label: "Backend network",
        family: Family::BackendNet,
        bare_id: false,
    },
    Slot {
        path: "messages.generator",
        label: "Message generator",
        family: Family::Generator,
        bare_id: false,
    },
    Slot {
        path: "security.protocol",
        label: "Credential protocol model",
        family: Family::Protocol,
        bare_id: false,
    },
    Slot {
        path: "threats.attackers[].id",
        label: "Attacker model",
        family: Family::Attacker,
        bare_id: true,
    },
    Slot {
        path: "threats.jammers[]",
        label: "Jammer",
        family: Family::Phy,
        bare_id: false,
    },
    Slot {
        path: "detection.local[]",
        label: "Local detector",
        family: Family::Detector,
        bare_id: false,
    },
    Slot {
        path: "detection.ma",
        label: "Misbehaviour-authority pipeline",
        family: Family::MaPipeline,
        bare_id: false,
    },
    Slot {
        path: "detection.responder",
        label: "Response model",
        family: Family::Responder,
        bare_id: false,
    },
];

/// The reflected type called `name`.
fn type_of(name: &str) -> Option<&'static RType> {
    TYPES.iter().find(|t| t.name == name)
}

/// The last path segment of a Rust type, so `v2xw_core::weather::WeatherKind` resolves as
/// `WeatherKind`.
fn base_name(ty: &str) -> &str {
    ty.rsplit("::").next().unwrap_or(ty).trim()
}

/// The inside of `Wrapper<…>`, if `ty` is one.
///
/// The head is compared by its last path segment, so `Option<f64>` and
/// `std::option::Option<f64>` are both recognised.
fn inner<'a>(ty: &'a str, wrapper: &str) -> Option<&'a str> {
    let ty = ty.trim();
    if !ty.ends_with('>') {
        return None;
    }
    let open = ty.find('<')?;
    if base_name(&ty[..open]) != wrapper {
        return None;
    }
    Some(ty[open + 1..ty.len() - 1].trim())
}

/// Splits `A, B` at the top-level comma of a two-parameter generic.
fn split_two(args: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (i, ch) in args.char_indices() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return Some((args[..i].trim(), args[i + 1..].trim())),
            _ => {}
        }
    }
    None
}

/// `[T; N]` into `(T, N)`.
fn array_of(ty: &str) -> Option<(&str, usize)> {
    let body = ty.trim().strip_prefix('[')?.strip_suffix(']')?;
    let (elem, len) = body.rsplit_once(';')?;
    Some((elem.trim(), len.trim().parse().ok()?))
}

/// The JSON Schema type for a Rust primitive, or `None` if it is not one.
fn primitive(ty: &str) -> Option<&'static str> {
    match base_name(ty) {
        "String" | "str" => Some("string"),
        "bool" => Some("boolean"),
        "f64" | "f32" => Some("number"),
        "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize" => {
            Some("integer")
        }
        _ => None,
    }
}

/// The unit for a leaf at `path`, derived from its name, or declared where the name cannot
/// carry it.
fn unit_for(path: &str) -> &'static str {
    if let Some((_, u)) = UNIT_OVERRIDES.iter().find(|(p, _)| *p == path) {
        return u;
    }
    let leaf = path.rsplit('.').next().unwrap_or(path);
    let leaf = leaf.trim_end_matches("[]");
    for (suffix, unit) in UNIT_SUFFIXES {
        if leaf.ends_with(suffix) {
            return unit;
        }
    }
    ""
}

/// A field's label: its name in words, with a unit suffix taken off because the unit is
/// shown beside the control rather than inside its name.
fn label_for(name: &str, unit: &str) -> String {
    let mut base = name;
    if !unit.is_empty() {
        for (suffix, u) in UNIT_SUFFIXES {
            if *u == unit && name.ends_with(suffix) && name.len() > suffix.len() {
                base = &name[..name.len() - suffix.len()];
                break;
            }
        }
    }
    let spaced = base.replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    }
}

/// The group a path belongs to: `(group, blurb)`.
fn group_for(path: &str) -> (&'static str, &'static str) {
    let section = path.split(['.', '[']).next().unwrap_or(path);
    GROUPS
        .iter()
        .find(|(k, _, _)| *k == section)
        .map(|(_, g, blurb)| (*g, *blurb))
        .unwrap_or(("Other", ""))
}

/// The JSON Pointer (RFC 6901) for a dotted path, which is what `scenario.set {patch}`
/// takes. `*` and `[]` stay as they are: they are placeholders, and a page substitutes
/// the concrete key or index before it patches.
fn pointer_for(path: &str) -> String {
    let mut out = String::new();
    for segment in path.split('.') {
        let (name, rest) = match segment.find('[') {
            Some(i) => (&segment[..i], &segment[i..]),
            None => (segment, ""),
        };
        if !name.is_empty() {
            out.push('/');
            out.push_str(&name.replace('~', "~0").replace('/', "~1"));
        }
        if !rest.is_empty() {
            out.push_str("/-");
        }
    }
    out
}

/// One node of the published JSON Schema.
struct Walk<'a> {
    /// The defaults document: `Scenario::minimal()` serialised, which is the loader's own
    /// answer to "what happens if the author writes nothing".
    defaults: &'a Value,
    /// Every leaf the walk produced, for the flat index and for the tests.
    leaves: Vec<Value>,
}

impl Walk<'_> {
    /// Builds the schema node for a value of type `ty` at `path`.
    fn node(&mut self, ty: &str, path: &str, doc: &str, depth: usize) -> Value {
        let mut out = Map::new();
        if !doc.is_empty() {
            out.insert("description".into(), json!(doc));
        }
        if depth >= MAX_DEPTH {
            out.insert("x-opaque".into(), json!(true));
            out.insert("x-rust-type".into(), json!(ty));
            // `with_meta` is what gives a node its identity — path, pointer, group,
            // unit, default, range and implementation status. Returning before it, as
            // this branch and the free-form branch below both did, published the field
            // with no path at all: invisible to a form keyed by path, and invisible to
            // every table that names a path, so a `KEY_STATUS` row for one of these
            // silently did nothing.
            self.with_meta(path, &mut out);
            return Value::Object(out);
        }

        // `serde_json::Value` is the schema's escape hatch: a model's own parameter
        // struct, edited as JSON, because the shape belongs to the model and not here.
        if matches!(base_name(ty), "Value") {
            out.insert("x-any".into(), json!(true));
            out.insert("x-rust-type".into(), json!(ty));
            // Before `push_leaf`, so the flat index carries the path too. These are the
            // model-parameter blocks — `actors.vehicles.demand.params`,
            // `threats.attackers[].params` — which is to say exactly the fields a
            // generated settings form most needs to be able to name.
            self.with_meta(path, &mut out);
            self.push_leaf(path, "json", &out);
            return Value::Object(out);
        }
        if let Some(t) = inner(ty, "Option") {
            let mut node = self.node(t, path, doc, depth);
            if let Value::Object(map) = &mut node {
                map.insert("x-nullable".into(), json!(true));
            }
            return node;
        }
        if let Some(t) = inner(ty, "Vec") {
            out.insert("type".into(), json!("array"));
            out.insert(
                "items".into(),
                self.node(t, &format!("{path}[]"), "", depth + 1),
            );
            self.with_meta(path, &mut out);
            return Value::Object(out);
        }
        if let Some((elem, len)) = array_of(ty) {
            out.insert("type".into(), json!("array"));
            out.insert("minItems".into(), json!(len));
            out.insert("maxItems".into(), json!(len));
            out.insert(
                "items".into(),
                self.node(elem, &format!("{path}[]"), "", depth + 1),
            );
            self.with_meta(path, &mut out);
            return Value::Object(out);
        }
        // Only the ordered map: the std hash map is forbidden in engine-facing code (the
        // conformance kit's hash-order firewall), so no scenario field can have that type.
        if let Some(args) = inner(ty, "BTreeMap")
            && let Some((_, value_ty)) = split_two(args)
        {
            out.insert("type".into(), json!("object"));
            out.insert("x-keyed".into(), json!(true));
            out.insert(
                "additionalProperties".into(),
                self.node(value_ty, &format!("{path}.*"), "", depth + 1),
            );
            self.with_meta(path, &mut out);
            return Value::Object(out);
        }
        if let Some(kind) = primitive(ty) {
            // The one field whose wire form is not its Rust form: the master seed is
            // written as `0xdeadbeef`, as `"0xdeadbeef"` or as a decimal, and serialises
            // as hexadecimal. The schema says so rather than claiming an integer the
            // document does not contain.
            if path == "seed" {
                out.insert("type".into(), json!(["string", "integer"]));
                out.insert(
                    "x-accepts".into(),
                    json!("a decimal integer, or a 0x-prefixed hexadecimal string"),
                );
            } else {
                out.insert("type".into(), json!(kind));
            }
            self.with_meta(path, &mut out);
            let widget = if out.contains_key("enum") {
                "enum"
            } else {
                kind
            };
            self.push_leaf(path, widget, &out);
            return Value::Object(out);
        }
        let Some(t) = type_of(base_name(ty)) else {
            // A type from a crate the reflection does not read. It is published as an
            // opaque JSON editor with its Rust name, which is the honest answer: the page
            // can still edit it and can say it has no field list for it.
            out.insert("x-opaque".into(), json!(true));
            out.insert("x-rust-type".into(), json!(ty));
            self.with_meta(path, &mut out);
            self.push_leaf(path, "json", &out);
            return Value::Object(out);
        };
        if doc.is_empty() && !t.doc.is_empty() {
            out.insert("description".into(), json!(t.doc));
        }
        if t.is_enum {
            return self.enum_node(t, path, out, depth);
        }
        out.insert("type".into(), json!("object"));
        if t.deny_unknown {
            out.insert("additionalProperties".into(), json!(false));
        }
        let mut props = Map::new();
        let mut required = Vec::new();
        for f in t.fields {
            if f.flatten {
                // A flattened map absorbs whatever keys the parent does not claim; the
                // timeline's per-kind parameters are the case. The keys are published as
                // free-form rather than invented, and `x-flattened-from` says which field
                // they belong to so a form can group them.
                out.insert("additionalProperties".into(), json!(true));
                out.insert("x-flattened-from".into(), json!(f.wire_name));
                continue;
            }
            let child_path = format!("{path}.{}", f.wire_name);
            let child_path = child_path.trim_start_matches('.').to_string();
            let mut child = self.node(f.ty, &child_path, f.doc, depth + 1);
            if let Value::Object(map) = &mut child {
                if !f.with.is_empty() {
                    map.insert("x-serde-with".into(), json!(f.with));
                }
                if f.rust_name != f.wire_name {
                    // The one case in this schema is `type` on a timeline item, which is
                    // a Rust keyword. A page that shows the Rust name would name a key
                    // the document does not have, so the wire name is the property key
                    // and the Rust name is a note beside it.
                    map.insert("x-rust-field".into(), json!(f.rust_name));
                }
            }
            let optional = inner(f.ty, "Option").is_some();
            if !f.has_default && !optional {
                required.push(json!(f.wire_name));
            }
            props.insert(f.wire_name.to_string(), child);
        }
        out.insert("properties".into(), Value::Object(props));
        if !required.is_empty() {
            out.insert("required".into(), Value::Array(required));
        }
        self.with_meta(path, &mut out);
        Value::Object(out)
    }

    /// The node for an enum: a string with a closed list of values when every variant is a
    /// unit, and a tagged `oneOf` otherwise.
    fn enum_node(
        &mut self,
        t: &RType,
        path: &str,
        mut out: Map<String, Value>,
        depth: usize,
    ) -> Value {
        let unit_only = t.variants.iter().all(|v| v.fields.is_empty());
        if unit_only {
            out.insert("type".into(), json!("string"));
            out.insert(
                "enum".into(),
                Value::Array(t.variants.iter().map(|v| json!(v.wire_name)).collect()),
            );
            let mut described = Map::new();
            for v in t.variants {
                if !v.doc.is_empty() {
                    described.insert(v.wire_name.to_string(), json!(v.doc));
                }
            }
            if !described.is_empty() {
                out.insert("x-enum-descriptions".into(), Value::Object(described));
            }
            self.with_meta(path, &mut out);
            self.push_leaf(path, "enum", &out);
            return Value::Object(out);
        }
        let tag = if t.tag.is_empty() { "kind" } else { t.tag };
        let mut branches = Vec::new();
        for v in t.variants {
            let mut props = Map::new();
            props.insert(tag.to_string(), json!({"const": v.wire_name}));
            let mut required = vec![json!(tag)];
            for f in v.fields {
                let child_path = format!("{path}.{}", f.wire_name);
                let child = self.node(f.ty, &child_path, f.doc, depth + 1);
                if !f.has_default && inner(f.ty, "Option").is_none() {
                    required.push(json!(f.wire_name));
                }
                props.insert(f.wire_name.to_string(), child);
            }
            let mut branch = Map::new();
            branch.insert("type".into(), json!("object"));
            branch.insert("title".into(), json!(v.wire_name));
            if !v.doc.is_empty() {
                branch.insert("description".into(), json!(v.doc));
            }
            branch.insert("properties".into(), Value::Object(props));
            branch.insert("required".into(), Value::Array(required));
            branches.push(Value::Object(branch));
        }
        out.insert("type".into(), json!("object"));
        out.insert("x-tag".into(), json!(tag));
        out.insert(
            "x-variants".into(),
            Value::Array(t.variants.iter().map(|v| json!(v.wire_name)).collect()),
        );
        out.insert("oneOf".into(), Value::Array(branches));
        self.with_meta(path, &mut out);
        Value::Object(out)
    }

    /// Adds everything that is true of a node because of *where* it is: its path, its
    /// unit, its default, its range, its choices, and whether the engine acts on it.
    fn with_meta(&self, path: &str, out: &mut Map<String, Value>) {
        if path.is_empty() {
            return;
        }
        out.insert("x-path".into(), json!(path));
        out.insert("x-pointer".into(), json!(pointer_for(path)));
        let (group, blurb) = group_for(path);
        out.insert("x-group".into(), json!(group));
        if !blurb.is_empty() {
            out.insert("x-group-blurb".into(), json!(blurb));
        }
        let unit = unit_for(path);
        if !unit.is_empty() {
            out.insert("unit".into(), json!(unit));
        }
        let leaf = path.rsplit('.').next().unwrap_or(path);
        out.insert("title".into(), json!(label_for(leaf, unit)));

        if let Some(default) = self.default_at(path) {
            out.insert("default".into(), default);
        }
        if let Some(b) = validate::bound_of(path) {
            if b.exclusive_lo {
                out.insert("exclusiveMinimum".into(), json!(b.lo));
            } else {
                out.insert("minimum".into(), json!(b.lo));
            }
            if b.hi.is_finite() {
                out.insert("maximum".into(), json!(b.hi));
            }
            out.insert("x-range".into(), json!(b.describe()));
        }
        if let Some(c) = validate::choices_of(path) {
            out.insert("enum".into(), json!(c.values));
            if !c.narrowed_by.is_empty() {
                out.insert(
                    "x-enum-narrowed".into(),
                    json!({"by": c.narrowed_by, "when": c.narrowed_when,
                           "to": c.narrowed_to}),
                );
            }
        }
        if let Some(slot) = SLOTS.iter().find(|s| s.path == path) {
            out.insert("x-slot".into(), json!(slot.family.to_string()));
        }
        match validate::status_of(path) {
            Some(k) => {
                out.insert("x-status".into(), json!(k.status));
                out.insert("x-status-note".into(), json!(k.note));
                out.insert(
                    "x-implemented".into(),
                    json!(matches!(k.status, Status::Wired | Status::Descriptive)),
                );
                out.insert("x-status-from".into(), json!(k.path));
            }
            None => {
                // Published as unknown rather than as implemented. A page that is told
                // "unknown" can grey the control; a page told "implemented" would offer
                // a control that may do nothing, which is the failure §2 forbids.
                out.insert("x-status".into(), json!("unknown"));
                out.insert("x-implemented".into(), json!(false));
                out.insert(
                    "x-status-note".into(),
                    json!("This build does not say whether the engine acts on this field."),
                );
            }
        }
    }

    /// The loader's default at `path`, read out of the serialised minimal scenario.
    ///
    /// A path through a list or a map has no default — there is no element to read — and
    /// nor has a field the minimal scenario omits, which is the honest answer for an
    /// optional field with no default.
    fn default_at(&self, path: &str) -> Option<Value> {
        if path.contains('[') || path.contains(".*") {
            return None;
        }
        let mut cur = self.defaults;
        for segment in path.split('.') {
            cur = cur.get(segment)?;
        }
        Some(cur.clone())
    }

    /// Records a leaf in the flat index.
    fn push_leaf(&mut self, path: &str, widget: &str, node: &Map<String, Value>) {
        if path.is_empty() {
            return;
        }
        let mut row = node.clone();
        row.insert("kind".into(), json!(widget));
        self.leaves.push(Value::Object(row));
    }
}

/// The published JSON Schema and the flat field index, built together because the index is
/// the leaves the schema walk found.
fn walk() -> (Value, Vec<Value>) {
    let defaults = serde_json::to_value(crate::Scenario::minimal()).unwrap_or(Value::Null);
    let mut walk = Walk {
        defaults: &defaults,
        leaves: Vec::new(),
    };
    let mut root = walk.node("Scenario", "", "", 0);
    if let Value::Object(map) = &mut root {
        map.insert(
            "$schema".into(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        map.insert("$id".into(), json!(crate::scenario::CURRENT_SCHEMA));
        map.insert("title".into(), json!("V2X World scenario"));
    }
    (root, walk.leaves)
}

/// The scenario surface: the whole bundle a generated settings form is built from.
///
/// One call, because the page wants it once per connection and the parts reference each
/// other: a field's `x-slot` names a family, and `models` is where that family's choices
/// are.
pub fn surface() -> Value {
    let (schema, fields) = walk();
    json!({
        "version": crate::scenario::CURRENT_SCHEMA,
        "engine": {
            "rustc": env!("V2XW_RUSTC_VERSION"),
            "target": env!("V2XW_TARGET"),
            "commit": env!("V2XW_GIT_COMMIT"),
            "source": env!("V2XW_GIT_DIRTY"),
        },
        "generated_from": [
            "crates/v2xw-engine/src/scenario/schema.rs (reflected at build time)",
            "crates/v2xw-engine/src/scenario/validate.rs (BOUNDS, CHOICES, KEY_STATUS)",
            "v2xw_core::registry::Registry (model cards)",
        ],
        "validator": {
            "method": "scenario.validate",
            "note": "The same rules the loader runs. A form validating a field inline and \
                     the engine refusing it cannot disagree, because there is one \
                     implementation.",
        },
        "groups": groups(),
        "statuses": statuses(),
        "schema": schema,
        "fields": fields,
        "slots": slots(),
        "models": models(),
    })
}

/// Just the JSON Schema, for a caller that wants nothing else.
pub fn schema() -> Value {
    walk().0
}

/// One row per editable leaf: the searchable form's own index.
pub fn fields() -> Vec<Value> {
    walk().1
}

/// The sections, in the order a user thinks about them.
pub fn groups() -> Value {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for (_, group, blurb) in GROUPS {
        if !seen.iter().any(|(g, _)| g == group) {
            seen.push((group, blurb));
        }
    }
    Value::Array(
        seen.into_iter()
            .map(|(name, blurb)| {
                let sections: Vec<&str> = GROUPS
                    .iter()
                    .filter(|(_, g, _)| *g == name)
                    .map(|(s, _, _)| *s)
                    .collect();
                json!({"name": name, "description": blurb, "sections": sections})
            })
            .collect(),
    )
}

/// The implementation-status vocabulary, so the page renders the words this build means
/// rather than words it guessed.
pub fn statuses() -> Value {
    json!([
        {"id": "wired", "label": "Active",
         "note": "The engine reads this and it changes the run."},
        {"id": "partial", "label": "Partly active",
         "note": "The engine reads part of this, or acts on it only under the conditions \
                  the field's own note states."},
        {"id": "not-implemented", "label": "Not yet implemented",
         "note": "Validated and recorded in the scenario digest, and read by nothing. \
                  Editing it does not change the run."},
        {"id": "refused", "label": "Only its implemented values load",
         "note": "The loader refuses any value this build cannot act on, so the field is \
                  real but its choices are narrower than the schema's."},
        {"id": "descriptive", "label": "Description",
         "note": "Describes the scenario: shown in the page and kept in the run's record. \
                  By design it changes nothing the run computes."},
        {"id": "unknown", "label": "Unclassified",
         "note": "This build does not say whether the engine acts on it."},
    ])
}

/// Every swappable slot with the ids that are actually selectable there.
pub fn slots() -> Value {
    let registry = catalogue();
    Value::Array(
        SLOTS
            .iter()
            .map(|slot| {
                let selectable: Vec<Value> = registry
                    .iter_by_id()
                    .filter(|(_, m)| m.card.family == slot.family)
                    .map(|(_, m)| {
                        json!({"id": m.card.id, "version": m.card.version,
                               "purpose": m.card.purpose,
                               "tiers": m.card.tier,
                               "implemented": registry.resolve(&m.card.id)
                                   .is_some_and(|r| registry.has_implementation(r))})
                    })
                    .collect();
                let status = validate::status_of(slot.path);
                json!({
                    "path": slot.path,
                    "pointer": pointer_for(slot.path),
                    "label": slot.label,
                    "family": slot.family.to_string(),
                    "spelling": if slot.bare_id { "id" } else { "{id, params}" },
                    "selectable": selectable,
                    "status": status.map(|k| k.status),
                    "status_note": status.map(|k| k.note),
                })
            })
            .collect(),
    )
}

/// The registry this build can publish, for the catalogue only.
///
/// Deliberately **not** the registry a run builds. `crate::wiring::register_all` decides
/// what a run pins in its manifest, and its content is part of what a replay checks; this
/// one exists to answer "what could the scenario name", so it registers every crate that
/// offers a registration entry point and records what it could not register rather than
/// failing.
fn catalogue() -> Registry {
    let mut registry = Registry::new();
    let _ = crate::wiring::register_all(&mut registry);
    for card in v2xw_world::model_cards() {
        if !registry.contains(&card.id) {
            let _ = registry.register(card);
        }
    }
    for card in v2xw_world::dem::model_cards() {
        if !registry.contains(&card.id) {
            let _ = registry.register(card);
        }
    }
    for card in v2xw_world::sumo::model_cards() {
        if !registry.contains(&card.id) {
            let _ = registry.register(card);
        }
    }
    for card in v2xw_radio::jamming::cards() {
        if !registry.contains(&card.id) {
            let _ = registry.register(card);
        }
    }
    // The backend-connectivity and authority models the security path selects
    // (`net.uu`, `net.backhaul`, `detection.ma`), so their slots offer them.
    for card in crate::backend::catalogue_cards() {
        if !registry.contains(&card.id) {
            let _ = registry.register(card);
        }
    }
    let _ = v2xw_proto::register_all(&mut registry);
    // The security crate's registration takes a civil clock because a certificate has a
    // validity window. The schema's own default `time.t0` is used, so the catalogue is a
    // property of the build and not of a wall clock.
    if let Ok(wall) = v2xw_core::time::WallClock::parse_rfc3339("2027-03-04T07:00:00Z") {
        let _ = v2xw_sec::register_all(&mut registry, wall);
    }
    registry
}

/// A card parameter's range as `{minimum, maximum}` when it is a two-element numeric
/// interval, else `null`.
///
/// `v2xw_core::card` has this logic for its own error messages and keeps it private, so
/// the shape is read off the array here rather than that function being made public for
/// one caller. The raw array is published beside it either way, so a page can render an
/// enumerated range this does not reduce.
fn numeric_bounds(range: Option<&[Value]>) -> Value {
    match range {
        Some([lo, hi]) => match (lo.as_f64(), hi.as_f64()) {
            (Some(lo), Some(hi)) => json!({"minimum": lo, "maximum": hi}),
            _ => Value::Null,
        },
        _ => Value::Null,
    }
}

/// Every registered model, with every parameter's unit, default, range and source.
///
/// The card is serialised whole rather than field by field, so a card field added in
/// `v2xw-core` reaches the page without an edit here. The derived keys beside it are the
/// ones a card does not carry: the content hash the manifest pins, whether an
/// implementation is linked in, and the `todo: calibrate` list.
pub fn models() -> Value {
    let registry = catalogue();
    let mut by_family: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for (r, m) in registry.iter_by_id() {
        let todo: Vec<Value> = m
            .card
            .todo_calibrate()
            .map(|p| json!({"name": p.name, "plan": p.calibration}))
            .collect();
        let params: Vec<Value> = m
            .card
            .parameters
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "unit": p.unit,
                    "default": p.default,
                    "range": p.range,
                    "range_bounds": numeric_bounds(p.range.as_deref()),
                    "source": p.source,
                    "calibration": p.calibration,
                    "todo_calibrate": p.source.kind
                        == v2xw_core::card::SourceKind::TodoCalibrate,
                })
            })
            .collect();
        let row = json!({
            "id": m.card.id,
            "version": m.card.version,
            "family": m.card.family.to_string(),
            "tiers": m.card.tier,
            "purpose": m.card.purpose,
            "parameters": params,
            "todo_calibrate": todo,
            "equations": m.card.equations,
            "assumptions": m.card.assumptions,
            "limitations": m.card.limitations,
            "ignores": m.card.ignores,
            "sources": m.card.sources,
            "validation": m.card.validation,
            "determinism": m.card.determinism,
            "cost": m.card.cost,
            "api_version": m.card.api_version,
            "licence": m.licence.to_string(),
            "in_process_allowed": m.licence.allows_in_process(),
            "hosting": m.hosting,
            "content_hash": m.content_hash_hex(),
            "implemented": registry.has_implementation(r),
        });
        by_family
            .entry(m.card.family.to_string())
            .or_default()
            .push(row);
    }
    json!({
        "count": registry.len(),
        "by_family": by_family,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workspace root, from this crate's manifest directory.
    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root is two levels above this crate")
    }

    /// The build-time reflection found the schema at all.
    ///
    /// This is the smoke test for the parse in `build.rs`: a formatting change that broke
    /// it would otherwise show up as a schema with no fields, which every other test here
    /// would pass vacuously.
    #[test]
    fn the_reflection_found_the_scenario() {
        let scenario = type_of("Scenario").expect("`Scenario` is reflected");
        assert!(
            scenario.fields.len() >= 18,
            "the scenario has eighteen top-level sections; the reflection found {}",
            scenario.fields.len()
        );
        assert!(scenario.deny_unknown, "`Scenario` denies unknown fields");
        for name in [
            "Time",
            "WorldSpec",
            "Actors",
            "Radio",
            "Net",
            "Messages",
            "Security",
            "Nodes",
            "Threats",
            "Detection",
            "TimelineItem",
            "Experiment",
            "WeatherKind",
            "Tier",
            "WorldSourceSpec",
            "GeoBbox",
            "HighwayPreset",
            "SurfaceCondition",
        ] {
            assert!(type_of(name).is_some(), "{name} is reflected");
        }
    }

    /// Every top-level section is placed in a group a user would look for it in.
    #[test]
    fn every_section_has_a_group() {
        let scenario = type_of("Scenario").expect("`Scenario` is reflected");
        for f in scenario.fields {
            let (group, _) = group_for(f.wire_name);
            assert_ne!(
                group, "Other",
                "scenario section `{}` has no entry in publish::GROUPS, so a generated \
                 form would file it under Other",
                f.wire_name
            );
        }
    }

    /// No leaf reaches the page unclassified.
    ///
    /// This is the rule 13-product-direction.md §2 states as "a field the engine does not
    /// actually act on must not be shown": the page can only honour it if every field
    /// arrives with a status, so a new field fails this test until someone says what
    /// happens when it is edited.
    #[test]
    fn every_leaf_has_a_status() {
        let unclassified: Vec<String> = fields()
            .iter()
            .filter(|f| f.get("x-status").and_then(Value::as_str) == Some("unknown"))
            .filter_map(|f| f.get("x-path").and_then(Value::as_str).map(str::to_string))
            .collect();
        assert!(
            unclassified.is_empty(),
            "these leaves have no row in validate::KEY_STATUS: {unclassified:#?}"
        );
    }

    /// Every number published carries a unit, or is declared dimensionless.
    #[test]
    fn every_number_publishes_a_unit() {
        let mut missing = Vec::new();
        for f in fields() {
            let kind = f.get("kind").and_then(Value::as_str).unwrap_or("");
            if kind != "number" && kind != "integer" {
                continue;
            }
            let path = f
                .get("x-path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let unit = f.get("unit").and_then(Value::as_str).unwrap_or("");
            if unit.is_empty() && !DIMENSIONLESS.contains(&path.as_str()) {
                missing.push(path);
            }
        }
        assert!(
            missing.is_empty(),
            "these numeric fields publish no unit; suffix the name, add a \
             publish::UNIT_OVERRIDES row, or declare them in publish::DIMENSIONLESS: \
             {missing:#?}"
        );
    }

    /// Every path the validator's tables and the slot table name is a path the schema
    /// publishes.
    ///
    /// The tables are the one hand-written part of this module, and a typo in one is
    /// silent: a bound that names no field simply never fires, and a status row that
    /// names no field leaves the real field unclassified. This test is what makes them
    /// safe to hand-write.
    #[test]
    fn the_tables_name_real_fields() {
        let published: std::collections::BTreeSet<String> = fields()
            .iter()
            .filter_map(|f| f.get("x-path").and_then(Value::as_str).map(str::to_string))
            .collect();
        // Containers are published too, and a status row may name one.
        let containers: std::collections::BTreeSet<String> = published
            .iter()
            .flat_map(|p| {
                let mut out = Vec::new();
                let mut cur = String::new();
                for seg in p.split('.') {
                    if !cur.is_empty() {
                        cur.push('.');
                    }
                    cur.push_str(seg);
                    out.push(cur.clone());
                    out.push(cur.trim_end_matches("[]").to_string());
                }
                out
            })
            .collect();
        let known = |p: &str| published.contains(p) || containers.contains(p);

        let mut unknown = Vec::new();
        for b in validate::BOUNDS {
            if !known(b.path) {
                unknown.push(format!("BOUNDS {}", b.path));
            }
        }
        for c in validate::CHOICES {
            if !known(c.path) {
                unknown.push(format!("CHOICES {}", c.path));
            }
        }
        for k in validate::KEY_STATUS {
            if !known(k.path) {
                unknown.push(format!("KEY_STATUS {}", k.path));
            }
        }
        for s in SLOTS {
            if !known(s.path) {
                unknown.push(format!("SLOTS {}", s.path));
            }
        }
        for (p, _) in UNIT_OVERRIDES {
            if !known(p) {
                unknown.push(format!("UNIT_OVERRIDES {p}"));
            }
        }
        for p in DIMENSIONLESS {
            if !known(p) {
                unknown.push(format!("DIMENSIONLESS {p}"));
            }
        }
        assert!(
            unknown.is_empty(),
            "these table rows name a path the schema does not publish, so they do \
             nothing: {unknown:#?}"
        );
    }

    /// The document built from the published defaults is one the loader accepts.
    ///
    /// `Scenario` denies unknown fields, so a property name the reflection invented — a
    /// mis-parsed `#[serde(rename)]`, a field read off the wrong line — fails here rather
    /// than becoming a form control bound to a key the engine rejects.
    #[test]
    fn the_published_defaults_load() {
        let (_, leaves) = walk();
        let mut doc = serde_json::to_value(crate::Scenario::minimal()).expect("minimal");
        let mut set = 0usize;
        for leaf in &leaves {
            let Some(path) = leaf.get("x-path").and_then(Value::as_str) else {
                continue;
            };
            if path.contains('[') || path.contains(".*") {
                continue;
            }
            let Some(default) = leaf.get("default") else {
                continue;
            };
            let mut cur = &mut doc;
            let segments: Vec<&str> = path.split('.').collect();
            for segment in &segments[..segments.len() - 1] {
                cur = cur
                    .as_object_mut()
                    .and_then(|m| m.get_mut(*segment))
                    .expect("a default's parent is in the minimal document");
            }
            if let Some(map) = cur.as_object_mut() {
                map.insert(segments[segments.len() - 1].to_string(), default.clone());
                set += 1;
            }
        }
        assert!(
            set > 30,
            "only {set} defaults were replayed into the document"
        );
        let scenario = crate::Scenario::from_document(doc)
            .expect("a document of the published defaults must deserialise");
        scenario
            .validate()
            .expect("a document of the published defaults must validate");
    }

    /// Whether `schema` describes the key structure of `doc`, collecting what it misses.
    fn describes(schema: &Value, doc: &Value, path: &str, missing: &mut Vec<String>) {
        let Some(node) = schema.as_object() else {
            return;
        };
        if node.get("x-any").and_then(Value::as_bool) == Some(true)
            || node.get("x-opaque").and_then(Value::as_bool) == Some(true)
            || node.get("additionalProperties") == Some(&json!(true))
        {
            return;
        }
        match doc {
            Value::Object(fields) => {
                if let Some(branches) = node.get("oneOf").and_then(Value::as_array) {
                    // A tagged union: the branch the document's tag selects is the one
                    // that has to describe it.
                    let tag = node.get("x-tag").and_then(Value::as_str).unwrap_or("kind");
                    let chosen = fields.get(tag).and_then(Value::as_str).unwrap_or("");
                    if let Some(branch) = branches
                        .iter()
                        .find(|b| b.get("title").and_then(Value::as_str) == Some(chosen))
                    {
                        describes(branch, doc, path, missing);
                    }
                    return;
                }
                if let Some(extra) = node.get("additionalProperties") {
                    for (key, value) in fields {
                        describes(extra, value, &format!("{path}.{key}"), missing);
                    }
                    return;
                }
                let Some(props) = node.get("properties").and_then(Value::as_object) else {
                    return;
                };
                for (key, value) in fields {
                    match props.get(key) {
                        Some(child) => describes(child, value, &format!("{path}.{key}"), missing),
                        None => missing.push(format!("{path}.{key}")),
                    }
                }
            }
            Value::Array(items) => {
                if let Some(child) = node.get("items") {
                    for (i, item) in items.iter().enumerate() {
                        describes(child, item, &format!("{path}[{i}]"), missing);
                    }
                }
            }
            _ => {}
        }
    }

    /// Every key of every scenario this repository ships is described by the schema.
    ///
    /// The reverse direction of `the_published_defaults_load`, and the one that catches a
    /// *missing* field: the shipped scenarios between them set most of the schema, so a
    /// field the reflection failed to read shows up here as a key nothing describes.
    #[test]
    fn every_shipped_scenario_is_described() {
        let schema = schema();
        let dir = root().join("scenarios");
        let mut checked = 0usize;
        let mut missing = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("the scenarios directory ships with the repository")
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
            .collect();
        entries.sort();
        for path in entries {
            let scenario = match crate::Scenario::load(&path) {
                Ok(s) => s,
                // A scenario this build refuses is a different finding, reported by the
                // loader's own tests; it cannot tell this test anything about coverage.
                Err(_) => continue,
            };
            let doc = serde_json::to_value(&scenario).expect("a scenario serialises");
            describes(
                &schema,
                &doc,
                path.display().to_string().as_str(),
                &mut missing,
            );
            checked += 1;
        }
        assert!(checked >= 4, "only {checked} shipped scenarios loaded");
        assert!(
            missing.is_empty(),
            "the published schema describes no such key, so a generated form would not \
             offer it: {missing:#?}"
        );
    }

    /// Every slot offers at least one selectable model, or says plainly that it offers
    /// none.
    ///
    /// A slot with an empty list is not a bug — several families ship no model in this
    /// build — but a slot whose emptiness is invisible would be a free-text box for an id
    /// that cannot exist. The test pins which ones are empty so that a family gaining a
    /// model is a deliberate change.
    #[test]
    fn slots_report_what_they_can_offer() {
        let slots = slots();
        let rows = slots.as_array().expect("slots is an array");
        assert_eq!(rows.len(), SLOTS.len());
        let empty: Vec<&str> = rows
            .iter()
            .filter(|r| {
                r.get("selectable")
                    .and_then(Value::as_array)
                    .is_none_or(std::vec::Vec::is_empty)
            })
            .filter_map(|r| r.get("path").and_then(Value::as_str))
            .collect();
        for slot in ["nodes.default_obu", "actors.rsus[].profile"] {
            assert!(
                !empty.contains(&slot),
                "{slot} must offer the shipped hardware profiles"
            );
        }
    }

    /// The model catalogue publishes a unit, a default and a source for every parameter.
    #[test]
    fn every_model_parameter_publishes_its_provenance() {
        let models = models();
        let by_family = models["by_family"].as_object().expect("by_family");
        assert!(
            models["count"].as_u64().unwrap_or(0) >= 10,
            "the catalogue found {} models",
            models["count"]
        );
        let mut params = 0usize;
        for rows in by_family.values() {
            for row in rows.as_array().expect("a family is an array") {
                for p in row["parameters"].as_array().expect("parameters") {
                    assert!(p.get("unit").is_some(), "{p} has no unit");
                    assert!(p.get("default").is_some(), "{p} has no default");
                    assert!(
                        p["source"].get("kind").is_some(),
                        "{p} has no source kind, so the page cannot say where the \
                         default came from"
                    );
                    params += 1;
                }
            }
        }
        assert!(params > 20, "only {params} model parameters were published");
    }

    /// The new range checks can actually go red.
    ///
    /// Written as an injection rather than an inspection, because this repository has
    /// already found four checks that could not fail. Each case sets one field out of
    /// range and requires the loader to name that field.
    #[test]
    fn the_new_bounds_refuse_what_they_claim_to() {
        let cases: &[(&str, Value)] = &[
            ("weather.intensity", json!(40.0)),
            ("weather.visibility_m", json!(0.0)),
            ("security.pseudonym_change.distance_m", json!(0.5)),
            ("nodes.default_obu", json!("node/obu/reference")),
        ];
        for (path, value) in cases {
            let mut doc =
                serde_json::to_value(crate::Scenario::minimal()).expect("minimal serialises");
            let segments: Vec<&str> = path.split('.').collect();
            let mut cur = &mut doc;
            for segment in &segments[..segments.len() - 1] {
                cur = cur
                    .as_object_mut()
                    .and_then(|m| m.get_mut(*segment))
                    .expect("the parent exists in the minimal document");
            }
            cur.as_object_mut()
                .expect("an object")
                .insert(segments[segments.len() - 1].to_string(), value.clone());
            let scenario = crate::Scenario::from_document(doc).expect("still deserialises");
            let errors = validate::validate(&scenario);
            assert!(
                errors.iter().any(|e| e.to_string().contains(path)),
                "setting {path} to {value} produced no error naming it: {errors:?}"
            );
        }
    }

    /// The minimal scenario — the starting point a page offers — is valid, and its
    /// default hardware profile is one that ships.
    #[test]
    fn the_minimal_scenario_names_a_profile_that_ships() {
        let scenario = crate::Scenario::minimal();
        scenario.validate().expect("the minimal scenario validates");
        assert!(
            v2xw_node::profiles::get(&scenario.nodes.default_obu).is_some(),
            "the default on-board unit `{}` does not ship, so every run that does not \
             name one silently gets the reference profile instead",
            scenario.nodes.default_obu
        );
    }

    /// The surface bundle has every section the page is told to expect.
    #[test]
    fn the_surface_carries_every_section() {
        let s = surface();
        for key in [
            "version",
            "engine",
            "generated_from",
            "validator",
            "groups",
            "statuses",
            "schema",
            "fields",
            "slots",
            "models",
        ] {
            assert!(s.get(key).is_some(), "the surface has no `{key}`");
        }
        assert_eq!(s["version"], json!(crate::scenario::CURRENT_SCHEMA));
    }
}
