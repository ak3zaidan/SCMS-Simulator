//! The whole attack catalogue in one namespace: the ported legacy renderings, the families
//! 07-threats-and-detection.md §2.2 adds, the compromised road-side unit, report poisoning
//! and the privacy observer.
//!
//! # Why a union rather than one enum
//!
//! A scenario file, a foundry genome and a legacy dataset column all carry an attack's
//! name as a string, and `tests/legacy_conformance.rs` asserts
//! [`crate::attack::AttackKind`] against the legacy Python source at test time so a silent
//! drift fails the build rather than the dataset. Adding new names to that enum would make
//! the assertion a moving target, so the new families live in their own enums and
//! [`CatalogEntry`] is the one namespace a scenario parses — [`CatalogEntry::parse`]
//! accepts every name in the catalogue and [`CatalogEntry::model_id`] says which model
//! renders it.
//!
//! # What is in it
//!
//! | Group | Count | Where |
//! |---|---|---|
//! | Legacy renderings (28 ported + selective dropping) | 29 | [`crate::attack_legacy`] |
//! | New families of §2.2 | 8 | [`crate::attack_ext`] |
//! | Compromised road-side unit | 6 | [`crate::attack_rsu`] |
//! | Report poisoning | 1 | [`crate::poison`] |
//! | Privacy observer | 1 | [`crate::privacy`] |

use crate::attack::{AttackFamily, AttackKind};
use crate::attack_ext::ExtendedAttackKind;
use crate::attack_rsu::RsuAttackKind;

/// One entry in the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogEntry {
    /// A ported legacy rendering.
    Legacy(AttackKind),
    /// One of the families 07-threats-and-detection.md §2.2 adds.
    New(ExtendedAttackKind),
    /// A compromised road-side unit's abuse.
    Rsu(RsuAttackKind),
    /// An insider filing false misbehaviour reports.
    ReportPoisoning,
    /// A passive adversary measuring how long it can track a vehicle.
    PrivacyObserver,
}

impl CatalogEntry {
    /// The name a scenario file and a foundry genome carry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CatalogEntry::Legacy(k) => k.as_str(),
            CatalogEntry::New(k) => k.as_str(),
            CatalogEntry::Rsu(k) => k.as_str(),
            CatalogEntry::ReportPoisoning => "ReportPoisoning",
            CatalogEntry::PrivacyObserver => "PrivacyObserver",
        }
    }

    /// Which behavioural family it belongs to.
    #[must_use]
    pub const fn family(self) -> AttackFamily {
        match self {
            CatalogEntry::Legacy(k) => k.family(),
            CatalogEntry::New(k) => k.family(),
            CatalogEntry::Rsu(k) => k.family(),
            CatalogEntry::ReportPoisoning => AttackFamily::Poisoning,
            CatalogEntry::PrivacyObserver => AttackFamily::Privacy,
        }
    }

    /// The id of the model that renders it.
    #[must_use]
    pub const fn model_id(self) -> &'static str {
        match self {
            CatalogEntry::Legacy(_) => crate::attack_legacy::MODEL_ID,
            CatalogEntry::New(_) => crate::attack_ext::MODEL_ID,
            CatalogEntry::Rsu(_) => crate::attack_rsu::MODEL_ID,
            CatalogEntry::ReportPoisoning => crate::poison::MODEL_ID,
            CatalogEntry::PrivacyObserver => crate::privacy::MODEL_ID,
        }
    }

    /// Whether the entry edits what goes on the air.
    ///
    /// False for report poisoning (it attacks the authority) and for the privacy observer
    /// (it transmits nothing at all), and false for the two road-side-unit abuses that act
    /// on the reporting path.
    #[must_use]
    pub const fn touches_the_air(self) -> bool {
        match self {
            CatalogEntry::Legacy(_) | CatalogEntry::New(_) => true,
            CatalogEntry::Rsu(k) => k.is_over_the_air(),
            CatalogEntry::ReportPoisoning | CatalogEntry::PrivacyObserver => false,
        }
    }

    /// Every entry in the catalogue: legacy first, in the legacy order.
    #[must_use]
    pub fn all() -> Vec<CatalogEntry> {
        let mut out: Vec<CatalogEntry> = AttackKind::ALL
            .into_iter()
            .map(CatalogEntry::Legacy)
            .collect();
        out.extend(ExtendedAttackKind::ALL.into_iter().map(CatalogEntry::New));
        out.extend(RsuAttackKind::ALL.into_iter().map(CatalogEntry::Rsu));
        out.push(CatalogEntry::ReportPoisoning);
        out.push(CatalogEntry::PrivacyObserver);
        out
    }

    /// How many attacks the catalogue holds.
    #[must_use]
    pub fn count() -> usize {
        AttackKind::ALL.len()
            + ExtendedAttackKind::ALL.len()
            + RsuAttackKind::ALL.len()
            + 2
    }

    /// Parses any name in the catalogue.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        if let Some(k) = AttackKind::parse(name) {
            return Some(CatalogEntry::Legacy(k));
        }
        if let Some(k) = ExtendedAttackKind::parse(name) {
            return Some(CatalogEntry::New(k));
        }
        if let Some(k) = RsuAttackKind::parse(name) {
            return Some(CatalogEntry::Rsu(k));
        }
        match name {
            "ReportPoisoning" => Some(CatalogEntry::ReportPoisoning),
            "PrivacyObserver" => Some(CatalogEntry::PrivacyObserver),
            _ => None,
        }
    }

    /// Every entry in one family, in catalogue order.
    #[must_use]
    pub fn in_family(family: AttackFamily) -> Vec<CatalogEntry> {
        CatalogEntry::all()
            .into_iter()
            .filter(|e| e.family() == family)
            .collect()
    }
}

impl core::fmt::Display for CatalogEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn the_catalogue_is_the_legacy_twenty_nine_plus_the_new_families() {
        let all = CatalogEntry::all();
        assert_eq!(all.len(), CatalogEntry::count());
        assert_eq!(all.len(), 29 + 8 + 6 + 2);
        // The legacy entries come first, in the legacy order, so a per-attack table from a
        // ported run lines up row for row with the legacy corpus.
        for (i, k) in AttackKind::ALL.into_iter().enumerate() {
            assert_eq!(all[i], CatalogEntry::Legacy(k));
        }
    }

    #[test]
    fn every_name_is_unique_and_parses_back() {
        let mut names = BTreeSet::new();
        for e in CatalogEntry::all() {
            assert!(names.insert(e.as_str()), "duplicate name {}", e.as_str());
            assert_eq!(CatalogEntry::parse(e.as_str()), Some(e));
            assert!(!e.model_id().is_empty());
        }
        assert_eq!(CatalogEntry::parse("NotAnAttack"), None);
    }

    #[test]
    fn the_four_new_families_have_members_and_the_legacy_nine_still_do() {
        for f in AttackFamily::ALL {
            assert!(
                !CatalogEntry::in_family(f).is_empty(),
                "family {f} has no attack, so no run can exercise it"
            );
        }
        assert_eq!(
            CatalogEntry::in_family(AttackFamily::Privacy),
            vec![CatalogEntry::PrivacyObserver]
        );
        assert_eq!(CatalogEntry::in_family(AttackFamily::Jamming).len(), 3);
        assert_eq!(
            CatalogEntry::in_family(AttackFamily::Infrastructure).len(),
            6
        );
    }

    #[test]
    fn the_attacks_that_never_touch_the_air_are_the_ones_on_the_authority() {
        let off_air: Vec<&str> = CatalogEntry::all()
            .into_iter()
            .filter(|e| !e.touches_the_air())
            .map(CatalogEntry::as_str)
            .collect();
        assert_eq!(
            off_air,
            [
                "SuppressForwardedReports",
                "PoisonForwardedReports",
                "ReportPoisoning",
                "PrivacyObserver"
            ]
        );
    }
}
