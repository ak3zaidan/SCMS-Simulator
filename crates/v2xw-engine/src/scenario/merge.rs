//! Base-plus-overlay merge (03-interfaces.md §13, `meta.base`).
//!
//! A scenario may name a base; the base is loaded, the overlay is merged onto it, and the
//! result is what runs. The merge happens on the **untyped document**, before
//! deserialisation, for one reason worth stating: merging typed structs cannot tell "the
//! author wrote `equipped_fraction: 1.0`" from "the author wrote nothing and the default
//! is 1.0", so a base that sets `0.3` would be silently overwritten by every overlay that
//! does not mention it. On the document, absent is absent.
//!
//! # The rule
//!
//! * **Mapping ∪ mapping**: merged key by key, recursively.
//! * **Anything else**: the overlay replaces the base outright.
//! * **A sequence is a value, not a set.** An overlay's `exporters: [a]` replaces the
//!   base's `exporters: [a, b, c]` rather than appending to it. Appending would make
//!   "run only this exporter" inexpressible, and an overlay that wants the base's list
//!   plus one more writes the list.
//! * **`null` deletes.** An overlay key whose value is `null` removes the base's key, so
//!   an overlay can turn a base's optional block off. Without this the only way to unset
//!   something a base set would be to stop using the base.
//!
//! `meta.base` itself is dropped from the merged document: the reference has been
//! followed, and leaving it would make the result look like it still needs resolving.

use serde_json::{Map, Value};

/// The key that names a base, at the top level of `meta`.
pub const BASE_KEY: &str = "base";

/// Merges `overlay` onto `base`, returning the result.
///
/// See the module documentation for the rule. Neither argument is modified.
pub fn merge(base: &Value, overlay: &Value) -> Value {
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            let mut out: Map<String, Value> = b.clone();
            for (k, v) in o {
                if v.is_null() {
                    out.remove(k);
                } else if let Some(existing) = out.get(k) {
                    let merged = merge(existing, v);
                    out.insert(k.clone(), merged);
                } else {
                    out.insert(k.clone(), v.clone());
                }
            }
            Value::Object(out)
        }
        _ => overlay.clone(),
    }
}

/// Reads and removes `meta.base` from a document, returning what it named.
///
/// Removing it is the point: after the base has been merged in, a leftover `meta.base`
/// would send a second load round the same loop.
pub fn take_base(doc: &mut Value) -> Option<String> {
    let meta = doc.get_mut("meta")?.as_object_mut()?;
    match meta.remove(BASE_KEY) {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mappings_merge_key_by_key_and_recursively() {
        let base = json!({"time": {"duration_s": 600, "mobility_step_ms": 100}, "seed": 1});
        let overlay = json!({"time": {"duration_s": 60}});
        assert_eq!(
            merge(&base, &overlay),
            json!({"time": {"duration_s": 60, "mobility_step_ms": 100}, "seed": 1})
        );
    }

    #[test]
    fn a_sequence_is_replaced_not_appended() {
        let base = json!({"exporters": [{"id": "a"}, {"id": "b"}]});
        let overlay = json!({"exporters": [{"id": "a"}]});
        assert_eq!(merge(&base, &overlay), json!({"exporters": [{"id": "a"}]}));
    }

    #[test]
    fn a_null_in_the_overlay_deletes_the_base_key() {
        let base = json!({"detection": {"ma": {"id": "x"}}, "seed": 2});
        let overlay = json!({"detection": null});
        assert_eq!(merge(&base, &overlay), json!({"seed": 2}));
    }

    #[test]
    fn the_base_reference_is_consumed_so_a_merge_does_not_loop() {
        let mut doc = json!({"meta": {"name": "n", "base": "presets/urban.yaml"}});
        assert_eq!(take_base(&mut doc).as_deref(), Some("presets/urban.yaml"));
        assert_eq!(take_base(&mut doc), None);
        assert_eq!(doc, json!({"meta": {"name": "n"}}));
    }
}
