//! A symmetric serde codec for `v2xw_core::ctx::Visibility`.
//!
//! # The defect this closes
//!
//! `Visibility` derives `Serialize` but derives `Deserialize` only under `cfg(test)`, so a
//! type in this crate that holds one — [`crate::MetricDef`], [`crate::MetricSample`] — can
//! be written and not read back. That matters here and not in the contract crate, because a
//! metric definition is a *document*: the catalog page is generated from it
//! (08-measurement-and-data.md §1), the run manifest carries the summary, and a tool that
//! compares two runs' metric tables has to parse what a run wrote. A schema that only
//! serialises is a schema whose own reader cannot exist.
//!
//! # The codec
//!
//! The wire form is the canonical kebab-case spelling 03-interfaces.md §14 lists — `gt`,
//! `node`, `node-and-gt`, `public`, `derived`, `mixed`, `meta` — which is exactly what
//! `Visibility`'s derived `Serialize` and its `Display` both produce, so nothing about the
//! recorded bytes changes and no digest moves. Reading is the inverse, and an unrecognised
//! spelling is an **error** rather than a default: `Visibility` is `#[non_exhaustive]`, so a
//! newer engine may write a tag this build does not know, and silently reading it as `node`
//! would put a ground-truth record on a node channel — the one mistake the visibility system
//! exists to prevent.
//!
//! Use it as `#[serde(with = "crate::vis")]` on the field.

use serde::de::{Error as _, Unexpected};
use serde::{Deserialize, Deserializer, Serializer};
use v2xw_core::ctx::Visibility;

/// Every spelling this codec knows, paired with its value.
const SPELLINGS: [(&str, Visibility); 7] = [
    ("gt", Visibility::Gt),
    ("node", Visibility::Node),
    ("node-and-gt", Visibility::NodeAndGt),
    ("public", Visibility::Public),
    ("derived", Visibility::Derived),
    ("mixed", Visibility::Mixed),
    ("meta", Visibility::Meta),
];

/// Writes the canonical kebab-case spelling.
///
/// # Errors
/// Whatever the underlying serialiser returns.
pub fn serialize<S: Serializer>(v: &Visibility, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&v.to_string())
}

/// Reads a canonical kebab-case spelling.
///
/// # Errors
/// A spelling this build does not know, which is a refusal rather than a default: see the
/// module documentation.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Visibility, D::Error> {
    let s = String::deserialize(deserializer)?;
    SPELLINGS
        .iter()
        .find(|(name, _)| *name == s.as_str())
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            D::Error::invalid_value(
                Unexpected::Str(&s),
                &"one of gt, node, node-and-gt, public, derived, mixed, meta",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Holder {
        #[serde(with = "super")]
        v: Visibility,
    }

    #[test]
    fn every_tag_round_trips_through_its_canonical_spelling() {
        for (name, v) in SPELLINGS {
            let json = serde_json::to_string(&Holder { v }).unwrap();
            assert_eq!(json, format!(r#"{{"v":"{name}"}}"#));
            assert_eq!(serde_json::from_str::<Holder>(&json).unwrap().v, v);
        }
    }

    /// The bytes are the ones the derived `Serialize` on `Visibility` already wrote, so
    /// nothing recorded changes shape and no digest moves.
    #[test]
    fn the_bytes_match_the_cores_own_encoding() {
        for (name, v) in SPELLINGS {
            assert_eq!(serde_json::to_string(&v).unwrap(), format!("\"{name}\""));
        }
    }

    #[test]
    fn an_unknown_tag_is_refused_rather_than_defaulted() {
        let e = serde_json::from_str::<Holder>(r#"{"v":"node-and-something-new"}"#).unwrap_err();
        assert!(e.to_string().contains("node-and-gt"), "{e}");
    }
}
