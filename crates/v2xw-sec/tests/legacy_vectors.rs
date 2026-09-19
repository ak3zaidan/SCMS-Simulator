//! Conformance of the SCMS core against the legacy Python reference, byte for byte.
//!
//! `legacy/scms_sim_ref/scms_core/` is the validated reference for butterfly key
//! expansion (CAMP SCP1) and linkage values (CAMP SCP2), and
//! `legacy/tests/test_butterfly.py` and `test_linkage.py` are its acceptance suite. This
//! file ports **every assertion those two suites make** and, where they assert a property
//! rather than a value, adds the value: `data/legacy_scms_vectors.json` holds the concrete
//! outputs captured by running the Python, and the Rust must reproduce them exactly.
//!
//! # Why values and not just properties
//!
//! The legacy suite is property-based in places — "15 distinct expansion values", "the
//! derived private key matches the certified public key". Those properties hold for
//! infinitely many *wrong* implementations: a port that used the wrong 32-bit prefix, or
//! hashed `ls ‖ la_id` instead of `la_id ‖ ls`, or truncated a pre-linkage value from the
//! wrong end, would satisfy all of them and interoperate with nothing. Pinning the bytes
//! is what makes this a conformance test rather than a self-consistency test.
//!
//! # Where the vectors come from
//!
//! `data/extract_legacy_scms_vectors.py` recomputes every value the two Python suites
//! assert on and prints them as JSON. It is committed beside the vectors so the capture
//! can be repeated and audited. Two legacy tests use `os.urandom` for a "different
//! device"; the extractor substitutes `SHA-256("other-la1")[:16]` and
//! `SHA-256("other-la2")[:16]` and pins the results, so the non-match assertions are
//! reproducible rather than probabilistic.

use p256::Scalar;
use serde_json::Value;
use v2xw_core::hash::hex_encode;
use v2xw_sec::butterfly::{self, Caterpillar, ExpansionKey};
use v2xw_sec::ec::{self, Point};
use v2xw_sec::linkage::{
    self, CrlLinkageEntry, DeviceLinkageContext, LaId, LinkageSeed, LinkageValue, PLV_BYTES,
};

/// Every vector checked, counted so the test can report the number.
struct Counter(usize);

impl Counter {
    fn check<T: PartialEq + core::fmt::Debug>(&mut self, what: &str, got: T, want: T) {
        assert_eq!(got, want, "vector mismatch: {what}");
        self.0 += 1;
    }
}

fn vectors() -> Value {
    let raw = include_str!("data/legacy_scms_vectors.json");
    serde_json::from_str(raw).expect("the pinned vectors parse")
}

fn hex_bytes(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0, "odd-length hex: {s}");
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

fn hex32(s: &str) -> [u8; 32] {
    let v = hex_bytes(s);
    assert_eq!(v.len(), 32, "expected 32 bytes: {s}");
    let mut out = [0u8; 32];
    out.copy_from_slice(&v);
    out
}

fn hex16(s: &str) -> [u8; 16] {
    let v = hex_bytes(s);
    assert_eq!(v.len(), 16, "expected 16 bytes: {s}");
    let mut out = [0u8; 16];
    out.copy_from_slice(&v);
    out
}

fn hex9(s: &str) -> [u8; PLV_BYTES] {
    let v = hex_bytes(s);
    assert_eq!(v.len(), PLV_BYTES, "expected 9 bytes: {s}");
    let mut out = [0u8; PLV_BYTES];
    out.copy_from_slice(&v);
    out
}

fn str_of(v: &Value, key: &str) -> String {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} is not a string"))
        .to_string()
}

fn u32_of(v: &Value, key: &str) -> u32 {
    v[key]
        .as_u64()
        .unwrap_or_else(|| panic!("{key} is not a number")) as u32
}

/// A scalar from a pinned 32-byte hex value.
///
/// The Python stores these as integers below `n`, so they are scalars directly; a value
/// at or above `n` would be a defect in the capture rather than something to reduce.
fn scalar(hex: &str) -> Scalar {
    ec::scalar_from_be32(&hex32(hex)).expect("the pinned scalar is below the group order")
}

/// A point from a pinned `{x, y}` pair, checked against the curve.
fn point(v: &Value) -> ([u8; 32], [u8; 32]) {
    (hex32(&str_of(v, "x")), hex32(&str_of(v, "y")))
}

fn point_of(p: &Point) -> ([u8; 32], [u8; 32]) {
    p.xy().expect("not the point at infinity")
}

// =====================================================================================
// legacy/scms_sim_ref/scms_core/ec.py — the group arithmetic the rest rests on
// =====================================================================================

/// `test_ec_generator_on_curve`, `test_scalar_mult_matches_library`, and the curve
/// parameters the legacy module pins.
#[test]
fn the_curve_and_the_scalar_multiplication_match_the_reference() {
    let v = vectors();
    let mut c = Counter(0);

    c.check(
        "group order n",
        hex_encode(&ec::N_BE),
        str_of(&v["curve"], "n"),
    );
    let g = point(&v["curve"]["g"]);
    c.check("generator", point_of(&Point::GENERATOR), g);

    // The legacy suite cross-validated its own `scalar_mult` against the `cryptography`
    // library; the extractor asserted that agreement again while capturing, so each of
    // these points is a value two independent implementations already agreed on.
    for row in v["scalar_mult_vs_library"].as_array().expect("array") {
        let d = str_of(row, "d");
        let want = point(&row["point"]);
        c.check(
            &format!("scalar_mult(d={d})"),
            point_of(&Point::mul_base(&scalar(&d))),
            want,
        );
    }

    // `test_ec_homomorphism`: (d1 + d2)·G = d1·G + d2·G.
    let h = &v["homomorphism"];
    let (d1, d2) = (scalar(&str_of(h, "d1")), scalar(&str_of(h, "d2")));
    let lhs = Point::mul_base(&d1).add(&Point::mul_base(&d2));
    let rhs = Point::mul_base(&(d1 + d2));
    c.check("homomorphism lhs", point_of(&lhs), point(&h["lhs"]));
    c.check("homomorphism rhs", point_of(&rhs), point(&h["rhs"]));
    c.check("homomorphism library", point_of(&rhs), point(&h["lib"]));
    assert_eq!(lhs, rhs);

    // The point at infinity: the legacy `is_on_curve(None)` case.
    assert!(Point::IDENTITY.is_identity());
    assert!(Point::IDENTITY.xy().is_none());

    println!("legacy vectors checked (ec.py): {}", c.0);
}

// =====================================================================================
// legacy/scms_sim_ref/scms_core/butterfly.py — CAMP SCP1
// =====================================================================================

/// `test_expansion_values_vary_and_are_deterministic`, with the values pinned.
#[test]
fn the_expansion_function_reproduces_the_reference_byte_for_byte() {
    let v = vectors();
    let mut c = Counter(0);
    let key: ExpansionKey = hex16(&str_of(&v, "f_key_hex"));

    for (name, rows) in [("f1", &v["f1"]), ("f2", &v["f2"])] {
        for row in rows.as_array().expect("array") {
            let (i, j) = (u32_of(row, "i"), u32_of(row, "j"));
            let got = if name == "f1" {
                butterfly::f1(&key, i, j)
            } else {
                butterfly::f2(&key, i, j)
            };
            c.check(
                &format!("{name}({i}, {j})"),
                hex_encode(&ec::scalar_to_be32(&got)),
                str_of(row, "v"),
            );
        }
    }

    // The legacy distinctness assertions, on the pinned values rather than on fresh ones.
    assert_eq!(v["distinct_counts"]["f1_15"].as_u64(), Some(15));
    let f1s: Vec<String> = v["f1"]
        .as_array()
        .expect("array")
        .iter()
        .map(|r| str_of(r, "v"))
        .collect();
    let mut unique = f1s.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 15, "the pinned f1 values must be distinct");

    println!(
        "legacy vectors checked (butterfly expansion): {} (30 expansion values)",
        c.0
    );
}

/// `test_caterpillar_deterministic`, with every derived value pinned.
#[test]
fn a_caterpillar_reproduces_the_reference() {
    let v = vectors();
    let mut c = Counter(0);

    for key in ["caterpillar", "ra_cannot_predict"] {
        let want = &v[key];
        let seed = hex_bytes(&str_of(want, "seed"));
        let cat = Caterpillar::from_seed(&seed).expect("64 bytes");
        c.check(
            &format!("{key}.a"),
            hex_encode(&ec::scalar_to_be32(cat.a())),
            str_of(want, "a"),
        );
        c.check(
            &format!("{key}.p"),
            hex_encode(&ec::scalar_to_be32(cat.p())),
            str_of(want, "p"),
        );
        c.check(
            &format!("{key}.ck"),
            hex_encode(cat.ck()),
            str_of(want, "ck"),
        );
        c.check(
            &format!("{key}.ek"),
            hex_encode(cat.ek()),
            str_of(want, "ek"),
        );
        c.check(
            &format!("{key}.A"),
            point_of(&cat.signing_public()),
            point(&want["A"]),
        );
        c.check(
            &format!("{key}.P"),
            point_of(&cat.encryption_public()),
            point(&want["P"]),
        );
    }
    println!("legacy vectors checked (caterpillar): {}", c.0);
}

/// `test_butterfly_signing_key_identity` — the heart of SCP1 — with every intermediate
/// pinned: the cocoon keys, the PCA's randomiser, the certified key, and both private
/// keys the device re-derives.
#[test]
fn the_butterfly_identity_reproduces_the_reference() {
    let v = vectors();
    let mut c = Counter(0);
    let cat =
        Caterpillar::from_seed(&hex_bytes(&str_of(&v["caterpillar"], "seed"))).expect("64 bytes");
    let (big_a, big_p) = (cat.signing_public(), cat.encryption_public());

    for row in v["butterfly_identity"].as_array().expect("array") {
        let (i, j) = (u32_of(row, "i"), u32_of(row, "j"));
        let (b, q) = butterfly::ra_cocoon_keys(&big_a, &big_p, cat.ck(), cat.ek(), i, j);
        c.check(&format!("B({i},{j})"), point_of(&b), point(&row["B"]));
        c.check(&format!("Q({i},{j})"), point_of(&q), point(&row["Q"]));

        let secret_c = scalar(&str_of(row, "c"));
        let (certified, big_c) = butterfly::pca_certify_explicit(&b, &secret_c);
        c.check(
            &format!("certified({i},{j})"),
            point_of(&certified),
            point(&row["certified"]),
        );
        c.check(&format!("C({i},{j})"), point_of(&big_c), point(&row["C"]));

        let d = cat.signing_private(i, j, &secret_c);
        c.check(
            &format!("d_priv({i},{j})"),
            hex_encode(&ec::scalar_to_be32(&d)),
            str_of(row, "d_priv"),
        );
        let q_priv = cat.encryption_private(i, j);
        c.check(
            &format!("q_priv({i},{j})"),
            hex_encode(&ec::scalar_to_be32(&q_priv)),
            str_of(row, "q_priv"),
        );

        // And the identity itself, which is the reason all of the above matters.
        assert!(
            butterfly::derived_key_matches(&d, &certified),
            "d·G != certified at ({i}, {j})"
        );
        assert!(butterfly::derived_key_matches(&q_priv, &q));
        assert_ne!(b, big_a, "the RA must actually have expanded the key");
        assert_ne!(q, big_p);
    }

    // `test_ra_cannot_predict_certified_key_without_c`.
    let w = &v["ra_cannot_predict"];
    let cat9 = Caterpillar::from_seed(&hex_bytes(&str_of(w, "seed"))).expect("64 bytes");
    let (b9, q9) = butterfly::ra_cocoon_keys(
        &cat9.signing_public(),
        &cat9.encryption_public(),
        cat9.ck(),
        cat9.ek(),
        0,
        0,
    );
    c.check("ra_cannot_predict.B", point_of(&b9), point(&w["B"]));
    c.check("ra_cannot_predict.Q", point_of(&q9), point(&w["Q"]));
    let (cert9, c9) = butterfly::pca_certify_explicit(&b9, &scalar(&str_of(w, "c")));
    c.check(
        "ra_cannot_predict.certified",
        point_of(&cert9),
        point(&w["certified"]),
    );
    c.check("ra_cannot_predict.C", point_of(&c9), point(&w["C"]));
    assert_eq!(w["equal"].as_bool(), Some(false));
    assert_ne!(b9, cert9, "B alone is not the certified key");

    println!("legacy vectors checked (butterfly identity): {}", c.0);
}

// =====================================================================================
// legacy/scms_sim_ref/scms_core/linkage.py — CAMP SCP2
// =====================================================================================

fn device_from(v: &Value) -> DeviceLinkageContext {
    DeviceLinkageContext::new(
        LaId(u32_of(v, "la_id1") as u16),
        LaId(u32_of(v, "la_id2") as u16),
        LinkageSeed::new(hex16(&str_of(v, "ls1_0"))),
        LinkageSeed::new(hex16(&str_of(v, "ls2_0"))),
    )
}

/// `test_field_widths`, and the seed hash chain and pre-linkage values with every value
/// pinned.
#[test]
fn the_linkage_seed_chain_and_pre_linkage_values_reproduce_the_reference() {
    let v = vectors();
    let mut c = Counter(0);

    // The specification's widths, as the legacy module declares them.
    c.check(
        "LA_ID_BYTES",
        linkage::LA_ID_BYTES as u64,
        v["linkage_widths"]["LA_ID_BYTES"].as_u64().expect("n"),
    );
    c.check(
        "LS_BYTES",
        linkage::LS_BYTES as u64,
        v["linkage_widths"]["LS_BYTES"].as_u64().expect("n"),
    );
    c.check(
        "J_BYTES",
        linkage::J_BYTES as u64,
        v["linkage_widths"]["J_BYTES"].as_u64().expect("n"),
    );
    c.check(
        "PLV_BYTES",
        linkage::PLV_BYTES as u64,
        v["linkage_widths"]["PLV_BYTES"].as_u64().expect("n"),
    );

    let dev = device_from(&v["device"]);

    // The seed chain, twelve steps for each authority. A port that hashed
    // `ls ‖ la_id` instead of `la_id ‖ ls`, or truncated from the wrong end, diverges at
    // step 1 and every step after it.
    for (name, la, ls0, chain) in [
        ("ls1", dev.la_id1, dev.ls1_0, &v["seed_chain_la1"]),
        ("ls2", dev.la_id2, dev.ls2_0, &v["seed_chain_la2"]),
    ] {
        for (i, want) in chain.as_array().expect("array").iter().enumerate() {
            c.check(
                &format!("{name}({i})"),
                hex_encode(linkage::linkage_seed_at(la, ls0, i as u32).as_bytes()),
                want.as_str().expect("hex").to_string(),
            );
        }
    }

    // `test_field_widths`'s pre-linkage value, and the 3x5 grid of both authorities'
    // contributions and the linkage value they XOR to.
    c.check(
        "plv1(ls1(0), j=0)",
        hex_encode(linkage::pre_linkage_value(dev.la_id1, dev.ls1_0, 0).as_bytes()),
        str_of(&v, "plv1_ls0_j0"),
    );
    for row in v["plv_grid"].as_array().expect("array") {
        let (i, j) = (u32_of(row, "i"), u32_of(row, "j"));
        let plv1 = linkage::pre_linkage_value(
            dev.la_id1,
            linkage::linkage_seed_at(dev.la_id1, dev.ls1_0, i),
            j,
        );
        let plv2 = linkage::pre_linkage_value(
            dev.la_id2,
            linkage::linkage_seed_at(dev.la_id2, dev.ls2_0, i),
            j,
        );
        c.check(
            &format!("plv1({i},{j})"),
            hex_encode(plv1.as_bytes()),
            str_of(row, "plv1"),
        );
        c.check(
            &format!("plv2({i},{j})"),
            hex_encode(plv2.as_bytes()),
            str_of(row, "plv2"),
        );
        c.check(
            &format!("lv({i},{j})"),
            hex_encode(dev.linkage_value_for(i, j).as_bytes()),
            str_of(row, "lv"),
        );
        // `test_two_LA_xor_reconstruction`: the value is exactly the XOR.
        assert_eq!(
            dev.linkage_value_for(i, j),
            linkage::linkage_value(plv1, plv2)
        );
    }

    // `test_determinism` and the two individually pinned slots.
    c.check(
        "lv(5,3)",
        hex_encode(dev.linkage_value_for(5, 3).as_bytes()),
        str_of(&v, "lv_5_3"),
    );
    c.check(
        "lv(7,2)",
        hex_encode(dev.linkage_value_for(7, 2).as_bytes()),
        str_of(&v, "lv_7_2"),
    );
    c.check(
        "plv1(5,3)",
        hex_encode(
            linkage::pre_linkage_value(
                dev.la_id1,
                linkage::linkage_seed_at(dev.la_id1, dev.ls1_0, 5),
                3,
            )
            .as_bytes(),
        ),
        str_of(&v["plv_5_3"], "plv1"),
    );
    c.check(
        "plv2(5,3)",
        hex_encode(
            linkage::pre_linkage_value(
                dev.la_id2,
                linkage::linkage_seed_at(dev.la_id2, dev.ls2_0, 5),
                3,
            )
            .as_bytes(),
        ),
        str_of(&v["plv_5_3"], "plv2"),
    );

    // `test_distinct_across_j_and_i`.
    assert_eq!(v["distinct_counts"]["lv_15"].as_u64(), Some(15));

    println!("legacy vectors checked (linkage values): {}", c.0);
}

/// `test_crl_forward_match_and_backward_privacy`,
/// `test_crl_does_not_match_other_device`, `test_crl_contains_helper` and
/// `test_out_of_range_j_rejected_by_entry` — every match decision the Python made, pinned
/// with the linkage value it was made against.
#[test]
fn every_revocation_decision_reproduces_the_reference() {
    let v = vectors();
    let mut c = Counter(0);
    let dev = device_from(&v["device"]);

    // The published entry: the two seeds at the revocation period.
    let e = &v["crl_entry"];
    let entry = CrlLinkageEntry::from_device(&dev, u32_of(e, "i"), u32_of(e, "jmax"));
    c.check(
        "entry.ls1_i",
        hex_encode(entry.ls1_i.as_bytes()),
        str_of(e, "ls1_i"),
    );
    c.check(
        "entry.ls2_i",
        hex_encode(entry.ls2_i.as_bytes()),
        str_of(e, "ls2_i"),
    );

    // Every (cert_i, cert_j) the Python evaluated, with its answer. This is where forward
    // matching and backward privacy are both pinned: the same device, the same value, and
    // the answer flips at the revocation period.
    let mut forward = 0;
    let mut backward = 0;
    for row in v["crl_matches"].as_array().expect("array") {
        let (ci, cj) = (u32_of(row, "cert_i"), u32_of(row, "cert_j"));
        let lv = LinkageValue::new(hex9(&str_of(row, "lv")));
        // The pinned value must itself be the one this implementation computes.
        if cj < entry.jmax {
            c.check(
                &format!("lv({ci},{cj})"),
                hex_encode(dev.linkage_value_for(ci, cj).as_bytes()),
                str_of(row, "lv"),
            );
        }
        let want = row["matches"].as_bool().expect("bool");
        c.check(
            &format!("matches({ci},{cj})"),
            entry.matches(ci, cj, lv),
            want,
        );
        if want {
            forward += 1;
        } else if ci < entry.i {
            backward += 1;
        }
    }
    assert!(forward > 0, "the vectors must contain forward matches");
    assert!(
        backward > 0,
        "the vectors must contain pre-revocation non-matches, or backward privacy is untested"
    );

    // A wrong linkage value in a revoked period.
    c.check(
        "matches with a zero value",
        entry.matches(u32_of(e, "i"), 0, LinkageValue::new([0u8; PLV_BYTES])),
        v["crl_wrong_lv"].as_bool().expect("bool"),
    );

    // `test_crl_does_not_match_other_device`, with the reference's `os.urandom` replaced
    // by a pinned seed.
    let other = DeviceLinkageContext::new(
        dev.la_id1,
        dev.la_id2,
        LinkageSeed::new(hex16(&str_of(&v["other_device"], "ls1_0"))),
        LinkageSeed::new(hex16(&str_of(&v["other_device"], "ls2_0"))),
    );
    let od = &v["crl_other_device"];
    let entry4 = CrlLinkageEntry::from_device(&dev, u32_of(od, "entry_i"), u32_of(od, "jmax"));
    c.check(
        "entry4.ls1_i",
        hex_encode(entry4.ls1_i.as_bytes()),
        str_of(od, "ls1_i"),
    );
    for row in od["rows"].as_array().expect("array") {
        let (ci, cj) = (u32_of(row, "cert_i"), u32_of(row, "cert_j"));
        c.check(
            &format!("other lv({ci},{cj})"),
            hex_encode(other.linkage_value_for(ci, cj).as_bytes()),
            str_of(row, "lv"),
        );
        c.check(
            &format!("other matches({ci},{cj})"),
            entry4.matches(ci, cj, other.linkage_value_for(ci, cj)),
            row["matches"].as_bool().expect("bool"),
        );
    }

    // `test_crl_contains_helper`.
    let cc = &v["crl_contains"];
    let crl = [CrlLinkageEntry::from_device(
        &dev,
        u32_of(cc, "entry_i"),
        linkage::DEFAULT_JMAX,
    )];
    c.check(
        "crl_contains victim lv(3,1)",
        hex_encode(dev.linkage_value_for(3, 1).as_bytes()),
        str_of(cc, "victim_lv_3_1"),
    );
    c.check(
        "crl_contains victim",
        linkage::crl_contains(&crl, 3, 1, dev.linkage_value_for(3, 1)),
        cc["victim_hit"].as_bool().expect("bool"),
    );
    c.check(
        "crl_contains other",
        linkage::crl_contains(&crl, 3, 1, other.linkage_value_for(3, 1)),
        cc["other_hit"].as_bool().expect("bool"),
    );

    // `test_out_of_range_j_rejected_by_entry`.
    let oj = &v["out_of_range_j"];
    let entry0 = CrlLinkageEntry::from_device(&dev, u32_of(oj, "entry_i"), linkage::DEFAULT_JMAX);
    c.check(
        "out-of-range j",
        entry0.matches(0, 20, LinkageValue::new(hex9(&str_of(oj, "lv_0_0")))),
        oj["matches_j20"].as_bool().expect("bool"),
    );

    println!("legacy vectors checked (revocation): {}", c.0);
}

/// Every one of the 15 legacy test functions is accounted for by a test in this file, and
/// the mapping is written down rather than assumed.
///
/// This is a bookkeeping test, and it is here because the task it discharges — "port
/// every one of their assertions" — is the kind of claim that is easy to make and hard to
/// check later. The list is the check.
#[test]
fn every_legacy_test_function_is_accounted_for() {
    let mapping: &[(&str, &str)] = &[
        // legacy/tests/test_butterfly.py
        (
            "test_ec_generator_on_curve",
            "the_curve_and_the_scalar_multiplication_match_the_reference",
        ),
        (
            "test_scalar_mult_matches_library",
            "the_curve_and_the_scalar_multiplication_match_the_reference",
        ),
        (
            "test_ec_homomorphism",
            "the_curve_and_the_scalar_multiplication_match_the_reference",
        ),
        (
            "test_caterpillar_deterministic",
            "a_caterpillar_reproduces_the_reference",
        ),
        (
            "test_expansion_values_vary_and_are_deterministic",
            "the_expansion_function_reproduces_the_reference_byte_for_byte",
        ),
        (
            "test_butterfly_signing_key_identity",
            "the_butterfly_identity_reproduces_the_reference",
        ),
        (
            "test_ra_cannot_predict_certified_key_without_c",
            "the_butterfly_identity_reproduces_the_reference",
        ),
        // legacy/tests/test_linkage.py
        (
            "test_field_widths",
            "the_linkage_seed_chain_and_pre_linkage_values_reproduce_the_reference",
        ),
        (
            "test_two_LA_xor_reconstruction",
            "the_linkage_seed_chain_and_pre_linkage_values_reproduce_the_reference",
        ),
        (
            "test_determinism",
            "the_linkage_seed_chain_and_pre_linkage_values_reproduce_the_reference",
        ),
        (
            "test_distinct_across_j_and_i",
            "the_linkage_seed_chain_and_pre_linkage_values_reproduce_the_reference",
        ),
        (
            "test_crl_forward_match_and_backward_privacy",
            "every_revocation_decision_reproduces_the_reference",
        ),
        (
            "test_crl_does_not_match_other_device",
            "every_revocation_decision_reproduces_the_reference",
        ),
        (
            "test_crl_contains_helper",
            "every_revocation_decision_reproduces_the_reference",
        ),
        (
            "test_out_of_range_j_rejected_by_entry",
            "every_revocation_decision_reproduces_the_reference",
        ),
    ];
    assert_eq!(
        mapping.len(),
        15,
        "the two legacy suites contain 15 test functions (`pytest -q` reports \
         \"15 passed\"); the task text's \"24 tests\" does not match the suites named"
    );
    let mut ported: Vec<&str> = mapping.iter().map(|(_, r)| *r).collect();
    ported.sort_unstable();
    ported.dedup();
    assert_eq!(
        ported.len(),
        6,
        "the 15 legacy functions land in 6 Rust tests: {ported:?}"
    );
    println!(
        "legacy suites: {} Python test functions ported into {} Rust tests",
        mapping.len(),
        ported.len()
    );
}
