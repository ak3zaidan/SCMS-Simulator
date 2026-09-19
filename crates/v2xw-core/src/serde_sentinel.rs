//! Symmetric serde codecs for the crate's in-band float sentinels.
//!
//! Three contract types use a non-finite `f64` as an in-band sentinel:
//! [`crate::weather::WeatherState::visibility_m`] (`INFINITY` = "unrestricted"),
//! [`crate::weather::DrivingEffects::max_decel_mps2`] and its `visibility_m`
//! (`INFINITY` = "not capped by the weather"), and the error-ellipse axes of
//! [`crate::belief::PositionEstimate::no_fix`] (`INFINITY` = "this position means
//! nothing"). Each of those is the *neutral* value of its type — the weather of every
//! scenario that says nothing about weather, the effects a weather model must return for
//! it, and the belief every node holds before its first fix, in a tunnel or under jamming.
//!
//! # The defect this module closes
//!
//! JSON has no infinity. `serde_json` writes a non-finite `f64` as `null`, and the derived
//! `Deserialize` for `f64` refuses `null`. So before this module existed, every one of those
//! constants serialised "successfully" and then failed to come back:
//!
//! ```text
//! {"kind":"clear","intensity":0.0,"visibility_m":null,"surface":"dry"}
//!   -> invalid type: null, expected f64 at line 1 column 51
//! ```
//!
//! A run could record a clear-weather keyframe and then not replay its own recording. The
//! doc comments admitted the hazard in prose ("a recorder that emits JSON substitutes a
//! finite value or omits the field") but the types offered no mechanism to do either.
//!
//! # The codec
//!
//! [`f64_inf`] makes the encoding symmetric instead of merely lossy: a non-finite value is
//! written as the format's *absent* value (`None`, which is `null` in JSON and YAML) and an
//! absent value is read back as [`f64_inf::SENTINEL`]. A finite value is written and read as
//! itself, so nothing about an ordinary reading changes — including its digest, since the
//! bytes for a finite field are the ones they always were, and the bytes for a sentinel field
//! are the `null` `serde_json` already wrote.
//!
//! Use it as `#[serde(with = "crate::serde_sentinel::f64_inf")]` on the field.

/// A `f64` field whose documented "absent / unbounded" value is [`f64::INFINITY`].
///
/// Writes a non-finite value as the format's absent value (`null` in JSON and YAML) and
/// reads an absent value back as [`SENTINEL`](f64_inf::SENTINEL). Finite values are
/// untouched in both directions, so the round trip is the identity on every value the
/// field's documented domain contains.
///
/// # The one asymmetry, stated
///
/// `NaN` and [`f64::NEG_INFINITY`] are *also* written as absent and therefore read back as
/// `+INFINITY`. Neither is in the documented domain of any field this codec is applied to —
/// `is_well_formed` on all three owning types rejects a negative or `NaN` visibility, cap or
/// ellipse axis — so the case is a bug upstream, and turning it into the field's neutral
/// element is a more useful failure than a decode error that names the wrong culprit.
///
/// ```
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize, PartialEq, Debug)]
/// struct Reading {
///     #[serde(with = "v2xw_core::serde_sentinel::f64_inf")]
///     visibility_m: f64,
/// }
///
/// let unrestricted = Reading { visibility_m: f64::INFINITY };
/// let json = serde_json::to_string(&unrestricted).unwrap();
/// assert_eq!(json, r#"{"visibility_m":null}"#);
/// assert_eq!(serde_json::from_str::<Reading>(&json).unwrap(), unrestricted);
///
/// let fog = Reading { visibility_m: 40.0 };
/// let json = serde_json::to_string(&fog).unwrap();
/// assert_eq!(json, r#"{"visibility_m":40.0}"#);
/// assert_eq!(serde_json::from_str::<Reading>(&json).unwrap(), fog);
/// ```
pub mod f64_inf {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// The value an absent (`null`) field is read back as: [`f64::INFINITY`].
    pub const SENTINEL: f64 = f64::INFINITY;

    /// Serialises a finite value as itself and a non-finite one as the absent value.
    ///
    /// # Errors
    /// Whatever the underlying serialiser returns.
    pub fn serialize<S: Serializer>(x: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        let carried = if x.is_finite() { Some(*x) } else { None };
        carried.serialize(serializer)
    }

    /// Deserialises a number as itself and an absent value as [`SENTINEL`].
    ///
    /// # Errors
    /// Whatever the underlying deserialiser returns for something that is neither a number
    /// nor the absent value.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or(SENTINEL))
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Holder {
        #[serde(with = "super::f64_inf")]
        x: f64,
    }

    /// The property the three contract types need: serialise, deserialise, get the same
    /// bits — for the sentinel as much as for an ordinary reading.
    #[test]
    fn the_sentinel_round_trips_through_json_and_yaml() {
        for value in [f64::INFINITY, 0.0, -0.0, 40.0, 1e-9, -12.5, f64::MAX] {
            let h = Holder { x: value };
            let json = serde_json::to_string(&h).unwrap();
            let back: Holder = serde_json::from_str(&json).unwrap();
            assert_eq!(back.x.to_bits(), value.to_bits(), "{json}");

            let yaml = serde_yml::to_string(&h).unwrap();
            let back: Holder = serde_yml::from_str(&yaml).unwrap();
            assert_eq!(back.x.to_bits(), value.to_bits(), "{yaml}");
        }
    }

    /// The wire spelling is the one `serde_json` already wrote for a non-finite float, so
    /// no recorded artefact changes shape and no digest moves.
    #[test]
    fn a_sentinel_is_written_as_null_and_a_number_as_itself() {
        assert_eq!(
            serde_json::to_string(&Holder { x: f64::INFINITY }).unwrap(),
            r#"{"x":null}"#
        );
        assert_eq!(
            serde_json::to_string(&Holder { x: 4.5 }).unwrap(),
            r#"{"x":4.5}"#
        );
    }

    /// The documented asymmetry: the two non-finite values that are outside every owning
    /// field's domain come back as the sentinel rather than as a decode error.
    #[test]
    fn the_out_of_domain_non_finites_collapse_to_the_sentinel() {
        for bad in [f64::NAN, f64::NEG_INFINITY] {
            let json = serde_json::to_string(&Holder { x: bad }).unwrap();
            assert_eq!(json, r#"{"x":null}"#);
            assert_eq!(
                serde_json::from_str::<Holder>(&json).unwrap().x,
                f64::INFINITY
            );
        }
    }

    /// A field that is present but of the wrong type is still an error: the codec adds a
    /// reading for `null`, it does not make the field lenient.
    #[test]
    fn a_wrong_type_is_still_refused() {
        assert!(serde_json::from_str::<Holder>(r#"{"x":"40"}"#).is_err());
        assert!(serde_json::from_str::<Holder>(r#"{"x":true}"#).is_err());
        assert!(
            serde_json::from_str::<Holder>("{}").is_err(),
            "still required"
        );
    }
}
