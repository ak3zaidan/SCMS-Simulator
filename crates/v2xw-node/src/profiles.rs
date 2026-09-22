//! The hardware profiles that ship with the crate (06-node-models.md §7).
//!
//! They are embedded with [`include_str!`] rather than read from disk at run time for two
//! reasons. A profile is part of the engine build the run manifest pins, so it has to be
//! inside the artefact the manifest is describing; and reading a data directory at run time
//! would make a run depend on the working directory, which is exactly the kind of ambient
//! input the determinism contract excludes. The same YAML files live under
//! `crates/v2xw-node/profiles/hardware/` and are the authoring copy.

use std::sync::OnceLock;

use crate::error::Result;
use crate::profile::HardwareProfile;

/// `(file stem, YAML)` for every shipped profile, in id order.
pub const PROFILE_SOURCES: &[(&str, &str)] = &[
    (
        "backend/cloud-vm-x86",
        include_str!("../profiles/hardware/backend-cloud-vm-x86.yaml"),
    ),
    (
        "backend/hsm-appliance-luna7",
        include_str!("../profiles/hardware/backend-hsm-appliance-luna7.yaml"),
    ),
    (
        "bs/lte-macro-generic",
        include_str!("../profiles/hardware/bs-lte-macro-generic.yaml"),
    ),
    (
        "obu/cohda-mk5",
        include_str!("../profiles/hardware/obu-cohda-mk5.yaml"),
    ),
    (
        "obu/cohda-mk6c-qualcomm-9150",
        include_str!("../profiles/hardware/obu-cohda-mk6c-qualcomm-9150.yaml"),
    ),
    (
        "obu/generic-automotive-soc-no-hsm",
        include_str!("../profiles/hardware/obu-generic-automotive-soc-no-hsm.yaml"),
    ),
    (
        "obu/pq-capable-hypothetical",
        include_str!("../profiles/hardware/obu-pq-capable-hypothetical.yaml"),
    ),
    (
        "obu/unex-obu-301-craton2",
        include_str!("../profiles/hardware/obu-unex-obu-301-craton2.yaml"),
    ),
    (
        "rsu/cohda-mk5-rsu",
        include_str!("../profiles/hardware/rsu-cohda-mk5-rsu.yaml"),
    ),
    (
        "rsu/commsignia-its-rs4",
        include_str!("../profiles/hardware/rsu-commsignia-its-rs4.yaml"),
    ),
];

/// The id 11-open-questions A3 proposes as the default OBU.
pub const REFERENCE_OBU: &str = "obu/unex-obu-301-craton2";

static LOADED: OnceLock<Vec<HardwareProfile>> = OnceLock::new();

/// Every shipped profile, parsed and validated once, in id order.
///
/// # Panics
/// If a shipped profile does not parse or breaks rule H1 or H2. That is a build defect in
/// this crate's own data, not a user error, and [`all_checked`] is the fallible spelling
/// for anyone who would rather see the message than the panic.
pub fn all() -> &'static [HardwareProfile] {
    LOADED.get_or_init(|| all_checked().expect("shipped hardware profiles must be valid"))
}

/// Every shipped profile, parsed and validated, in id order.
///
/// # Errors
/// The first profile that does not parse or does not satisfy rules H1 and H2.
pub fn all_checked() -> Result<Vec<HardwareProfile>> {
    PROFILE_SOURCES
        .iter()
        .map(|(_, yaml)| HardwareProfile::from_yaml(yaml))
        .collect()
}

/// One shipped profile by its registry id.
pub fn get(id: &str) -> Option<&'static HardwareProfile> {
    all().iter().find(|p| p.id == id)
}

/// How many fields across every shipped profile carry no published value.
///
/// This is the number the registry's `todo-calibrate` page reports and the honest answer
/// to "how much of the hardware model is real".
pub fn todo_calibrate_total() -> usize {
    all()
        .iter()
        .map(HardwareProfile::todo_calibrate_count)
        .sum()
}

/// Every uncalibrated field across every shipped profile, as `profile-id::field.path`,
/// in profile-id then schema order.
pub fn todo_calibrate_index() -> Vec<String> {
    all()
        .iter()
        .flat_map(|p| {
            p.todo_calibrate_fields()
                .into_iter()
                .map(move |f| format!("{}::{f}", p.id))
        })
        .collect()
}

/// Registers every shipped profile with a model registry.
///
/// # Errors
/// [`v2xw_core::registry::RegistryError`] if a card fails validation or an id is already
/// taken.
pub fn register_all(
    registry: &mut v2xw_core::registry::Registry,
) -> core::result::Result<Vec<v2xw_core::registry::ModelRef>, v2xw_core::registry::RegistryError> {
    let mut refs = Vec::new();
    for p in all() {
        refs.push(registry.register_model(std::sync::Arc::new(p.clone()))?);
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{HsmKind, NodeKind};

    /// Every shipped profile parses, satisfies rules H1 and H2, and produces a card that
    /// validates — which is also the assertion that every unpublished field carries a
    /// calibration plan, because `ModelCard::validate` enforces registry rule R1.
    #[test]
    fn every_shipped_profile_loads_and_validates() {
        let profiles = all_checked().expect("all profiles load");
        assert_eq!(profiles.len(), PROFILE_SOURCES.len());
        for (i, (stem, _)) in PROFILE_SOURCES.iter().enumerate() {
            assert_eq!(
                &profiles[i].id, stem,
                "file stem must match the declared id"
            );
            profiles[i].card().validate().expect("card validates");
        }
    }

    /// The ids are in sorted order, so `all()` is a deterministic listing and
    /// `todo_calibrate_index` does not depend on a directory walk.
    #[test]
    fn profiles_are_listed_in_id_order() {
        let ids: Vec<&str> = PROFILE_SOURCES.iter().map(|(s, _)| *s).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
    }

    /// The reference profile is the one 11-open-questions A3 names, and it is the only one
    /// flagged as such.
    #[test]
    fn exactly_one_reference_obu() {
        let refs: Vec<&str> = all()
            .iter()
            .filter(|p| p.reference_profile)
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(refs, vec![REFERENCE_OBU]);
        assert_eq!(get(REFERENCE_OBU).unwrap().kind, NodeKind::Obu);
    }

    /// Rule H2: the two software-only OBUs and the two profiles with no security hardware
    /// declare `hsm.kind: none` and carry no operations on it.
    #[test]
    fn rule_h2_holds_for_the_profiles_without_security_hardware() {
        for id in [
            "obu/generic-automotive-soc-no-hsm",
            "obu/pq-capable-hypothetical",
            "backend/cloud-vm-x86",
            "bs/lte-macro-generic",
        ] {
            let p = get(id).unwrap();
            assert_eq!(p.hsm.kind, HsmKind::None, "{id}");
            assert!(p.hsm.ops.is_empty(), "{id}");
        }
    }

    /// The published numbers survive the round trip from YAML into the typed schema, and
    /// the unpublished ones stay unpublished. Spot checks on the two profiles whose
    /// figures 06-node-models §7.9 tabulates most completely.
    #[test]
    fn published_figures_round_trip() {
        let craton = get(REFERENCE_OBU).unwrap();
        assert_eq!(craton.cpu.cores.get(), Some(&2));
        assert_eq!(craton.cpu.clock_hz.get(), Some(&6.0e8));
        assert_eq!(craton.ram_bytes.get(), Some(&134_217_728));
        let verify = &craton.hsm.ops["ecdsa-p256-verify"];
        assert_eq!(verify.throughput_per_s.get(), Some(&2500.0));
        assert!(verify.latency_us.is_missing());
        let sign = &craton.hsm.ops["ecdsa-p256-sign"];
        assert_eq!(sign.latency_us.get(), Some(&9000.0));

        let pi = get("obu/generic-automotive-soc-no-hsm").unwrap();
        assert_eq!(pi.software_crypto["ecdsa-p256-verify"].us, Some(645.0));
        assert_eq!(pi.software_crypto["ml-dsa-44-verify"].us, Some(311.0));
        assert!(pi.software_crypto["ml-dsa-65-verify"].us.is_none());
    }

    /// The whole point of the schema: nothing invents a number. Every field the report
    /// lists is either published or accompanied by the measurement that would publish it.
    #[test]
    fn every_uncalibrated_field_carries_a_plan() {
        for p in all() {
            for (name, facts) in p.field_report() {
                if facts.value_present {
                    continue;
                }
                let plan = facts.calibration.unwrap_or("");
                assert!(
                    plan.len() > 20,
                    "{}::{name} has no usable calibration plan",
                    p.id
                );
            }
        }
        assert_eq!(
            todo_calibrate_index().len(),
            todo_calibrate_total(),
            "the index and the count must agree"
        );
    }
}
