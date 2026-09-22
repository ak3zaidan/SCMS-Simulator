//! The legacy canonical-JSON encoding, byte for byte.
//!
//! The v1 profile has to be *byte*-compatible with what the frozen Python engine wrote,
//! because published results pin `manifest.json → data_digest_sha256`, and that digest is
//! taken over the JSONL files' bytes. The legacy encoder is one call
//! (`scms_sim_ref/scms_core/crypto_abstract.py::canonical_bytes`):
//!
//! ```python
//! json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8")
//! ```
//!
//! Three of those four settings are cheap to reproduce and one is not.
//!
//! * `sort_keys=True` — every object's keys in code-point order. A [`serde_json::Map`]
//!   preserves insertion order, so this module sorts rather than trusting the builder, and
//!   Rust's `str` ordering is UTF-8 byte order, which agrees with code-point order.
//! * `separators=(",", ":")` — no whitespace, which is `serde_json`'s compact form.
//! * `ensure_ascii=True` — every non-ASCII scalar escaped as `\uXXXX`, and every
//!   astral-plane scalar as a surrogate pair. `serde_json` emits raw UTF-8 instead, so
//!   [`write_string`] does the escaping.
//! * **Float spelling** — the expensive one, and the reason this module exists rather than
//!   a `canonical_json` call. Python formats a float with `repr`, `serde_json` with ryū,
//!   and the two disagree at both ends of the range: `1e-6` is `1e-06` in Python and
//!   `1e-6` in `serde_json`, and `1e16` is `1e+16` against `1e16`. A dataset that carries
//!   a probability at the 1e-6 grid — which D9 declares as the default grid for an unnamed
//!   unit — would therefore have a different digest for the same numbers.
//!
//! # The float rule, stated
//!
//! CPython's `float_repr_style` is *short*: the shortest decimal string that round-trips,
//! laid out by `format_float_short` as **fixed** notation when the decimal point's
//! position `decpt` satisfies `-4 < decpt <= 16`, and as exponential notation with a
//! signed, at-least-two-digit exponent otherwise. Boundaries, all checked in the tests
//! below against the real `json.dumps`: `0.0001` is fixed and `1e-05` is not; `1e15` is
//! fixed (`1000000000000000.0`) and `1e+16` is not.
//!
//! Rust's `Display` for `f64` is also shortest-round-trip and is *always* positional, so
//! this module takes the digits and the exponent from it and re-lays them out Python's
//! way. That keeps one shortest-digits implementation rather than two.

use std::fmt::Write as _;

/// Writes one JSON value in the legacy canonical encoding.
///
/// Keys are sorted, there is no whitespace, every non-ASCII scalar is escaped and every
/// float is spelled the way CPython's `repr` spells it.
pub fn canonical(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

/// One JSONL line: [`canonical`] plus the `\n` the legacy writer appended.
///
/// The legacy files were opened with `newline="\n"`, so the separator is a bare line feed
/// on every platform.
pub fn canonical_line(value: &serde_json::Value) -> String {
    let mut s = canonical(value);
    s.push('\n');
    s
}

fn write_value(out: &mut String, value: &serde_json::Value) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(true) => out.push_str("true"),
        serde_json::Value::Bool(false) => out.push_str("false"),
        serde_json::Value::Number(n) => write_number(out, n),
        serde_json::Value::String(s) => write_string(out, s),
        serde_json::Value::Array(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, v);
            }
            out.push(']');
        }
        serde_json::Value::Object(m) => {
            // `sort_keys=True`. The map's own order is insertion order, so a builder that
            // happened to insert in a different order would otherwise change the bytes.
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, k);
                out.push(':');
                write_value(out, &m[*k]);
            }
            out.push('}');
        }
    }
}

fn write_number(out: &mut String, n: &serde_json::Number) {
    if let Some(i) = n.as_i64() {
        let _ = write!(out, "{i}");
    } else if let Some(u) = n.as_u64() {
        let _ = write!(out, "{u}");
    } else if let Some(f) = n.as_f64() {
        out.push_str(&python_repr(f));
    } else {
        // An arbitrary-precision literal this build cannot hold as either integer or
        // float. Emitting its own text is the only faithful option.
        out.push_str(&n.to_string());
    }
}

/// `json.dumps`'s string encoding with `ensure_ascii=True`.
///
/// The escape set is Python's: the two mandatory escapes, the five short forms, `\uXXXX`
/// for every other control character, and `\uXXXX` for every scalar above U+007F, with a
/// surrogate pair above U+FFFF.
pub fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                let cp = c as u32;
                if cp <= 0xffff {
                    let _ = write!(out, "\\u{cp:04x}");
                } else {
                    let v = cp - 0x1_0000;
                    let hi = 0xd800 + (v >> 10);
                    let lo = 0xdc00 + (v & 0x3ff);
                    let _ = write!(out, "\\u{hi:04x}\\u{lo:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// CPython's `repr` of a finite `f64`, which is also `json.dumps`'s spelling of it.
///
/// A non-finite value has no JSON spelling and the legacy writer would have emitted
/// Python's `NaN` / `Infinity`, which is not JSON at all; every float in an export is
/// quantised and finite by then (D9), so this returns `null` for one rather than writing a
/// token no parser accepts. [`crate::dataset`]'s writers reject a non-finite value before
/// it reaches here.
pub fn python_repr(x: f64) -> String {
    if x.is_nan() || x.is_infinite() {
        return "null".to_string();
    }
    let negative = x.is_sign_negative();
    let (digits, decpt) = shortest_digits(x.abs());
    let mut out = String::with_capacity(digits.len() + 8);
    if negative {
        out.push('-');
    }
    if digits == "0" {
        // Python renders zero as `0.0`, with the sign preserved: `repr(-0.0) == '-0.0'`.
        out.push_str("0.0");
        return out;
    }
    // `format_float_short`: fixed notation iff -4 < decpt <= 16.
    if decpt > -4 && decpt <= 16 {
        if decpt <= 0 {
            out.push_str("0.");
            for _ in 0..-decpt {
                out.push('0');
            }
            out.push_str(&digits);
        } else {
            let d = decpt as usize;
            if d >= digits.len() {
                out.push_str(&digits);
                for _ in 0..(d - digits.len()) {
                    out.push('0');
                }
                out.push_str(".0");
            } else {
                out.push_str(&digits[..d]);
                out.push('.');
                out.push_str(&digits[d..]);
            }
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        let _ = write!(out, "e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    out
}

/// The shortest round-tripping decimal digits of a non-negative, finite `f64`, and the
/// position of the decimal point relative to the first digit.
///
/// `x == 0.digits * 10^decpt`. Rust's `Display` is shortest-round-trip and always
/// positional, so the digits come from it and only the layout is redone.
fn shortest_digits(x: f64) -> (String, i32) {
    debug_assert!(x.is_finite() && x >= 0.0);
    let s = format!("{x}");
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s.as_str(), ""),
    };
    let mut combined = String::with_capacity(int_part.len() + frac_part.len());
    combined.push_str(int_part);
    combined.push_str(frac_part);
    let lead = combined.find(|c| c != '0');
    let Some(lead) = lead else {
        return ("0".to_string(), 0);
    };
    let decpt = int_part.len() as i32 - lead as i32;
    let trimmed = combined[lead..].trim_end_matches('0');
    let digits = if trimmed.is_empty() { "0" } else { trimmed };
    (digits.to_string(), decpt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every one of these expectations is what `json.dumps` printed, checked against
    /// CPython 3.12 rather than reasoned about: the boundaries are the whole point.
    #[test]
    fn floats_are_spelled_the_way_python_spells_them() {
        for (x, want) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (9.5, "9.5"),
            (0.001, "0.001"),
            (123.457, "123.457"),
            (0.1 + 0.2, "0.30000000000000004"),
            // the fixed/exponential boundary at the small end: 1e-4 is fixed, 1e-5 is not
            (1e-4, "0.0001"),
            (1e-5, "1e-05"),
            (1e-6, "1e-06"),
            (1e-7, "1e-07"),
            // …and at the large end: 1e15 is fixed, 1e16 is not
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1e17, "1e+17"),
            (1e22, "1e+22"),
            (1.5e300, "1.5e+300"),
            (5e-324, "5e-324"),
            (-42.25, "-42.25"),
        ] {
            assert_eq!(python_repr(x), want, "python_repr({x})");
        }
    }

    #[test]
    fn a_grid_value_at_every_declared_quantum_round_trips_to_its_python_spelling() {
        // D9's grids, at the smallest non-zero value each one can carry. The 1e-6 grid is
        // the one `serde_json` would have spelled `1e-6` and Python spells `1e-06`, which
        // is the defect this module exists for.
        assert_eq!(python_repr(1e-3), "0.001");
        assert_eq!(python_repr(1e-2), "0.01");
        assert_eq!(python_repr(1e-4), "0.0001");
        assert_eq!(python_repr(1e-6), "1e-06");
        assert_eq!(python_repr(1e-7), "1e-07");
        assert_ne!(
            python_repr(1e-6),
            serde_json::to_string(&1e-6f64).expect("json"),
            "the two encoders disagree here, which is why this module is not canonical_json"
        );
    }

    #[test]
    fn object_keys_are_sorted_and_there_is_no_whitespace() {
        let v = json!({"z": 1, "a": {"n": 2, "m": 3}, "b": [1, 2]});
        assert_eq!(canonical(&v), r#"{"a":{"m":3,"n":2},"b":[1,2],"z":1}"#);
    }

    #[test]
    fn non_ascii_is_escaped_the_way_ensure_ascii_escapes_it() {
        let v = json!({"k": "a\u{e9}b\u{1f600}\n\"x\""});
        // CPython, with ensure_ascii=True, renders an astral-plane scalar as a surrogate
        // PAIR — the detail a naive four-hex-digit escape gets wrong:
        //   json.dumps({"k": "a\xe9b\U0001f600\n\"x\""})
        //     == '{"k":"a\\u00e9b\\ud83d\\ude00\\n\\"x\\""}'
        let want = "{\"k\":\"a\\u00e9b\\ud83d\\ude00\\n\\\"x\\\"\"}";
        assert_eq!(canonical(&v), want);
    }

    #[test]
    fn integers_stay_integers_and_whole_floats_keep_their_point() {
        // The distinction matters: the legacy `num_entries` is an int and the legacy
        // `ingest_time` is a float, and `9` and `9.0` are different bytes.
        let v = json!({"n": 9, "t": 9.0});
        assert_eq!(canonical(&v), r#"{"n":9,"t":9.0}"#);
    }

    #[test]
    fn a_line_ends_in_exactly_one_line_feed() {
        let line = canonical_line(&json!({"a": 1}));
        assert_eq!(line, "{\"a\":1}\n");
    }

    #[test]
    fn a_non_finite_value_is_null_rather_than_a_token_no_parser_accepts() {
        assert_eq!(python_repr(f64::NAN), "null");
        assert_eq!(python_repr(f64::INFINITY), "null");
    }
}
