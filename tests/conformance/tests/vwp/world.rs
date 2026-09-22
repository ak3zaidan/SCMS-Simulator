//! §10.5 — the world payload. Items W1, W4 and W5.
//!
//! W2 (COOP/COEP/CORP headers) needs a live HTTP response, and W3 and W6 bind the client's
//! world loader.

use v2xw_world::procedural::{GridParams, grid};
use v2xw_world::{ImportOptions, World, serde_vwp};

/// The import options every case uses: a fixed date, so nothing reads a clock.
fn opts() -> ImportOptions {
    ImportOptions::default().imported_at("2026-09-18T00:00:00Z")
}

/// A small lattice: enough to hash, not enough to be slow.
fn small_world() -> World {
    grid(
        &GridParams::legacy().with_size(3, 3).with_block_m(120.0),
        &opts(),
    )
    .expect("the 3 x 3 legacy grid builds")
}

/// A lattice with **every** section populated — signals, crossings, buildings and RSU
/// sites.
///
/// W4 is about the two serialisations carrying the same sections, and the `legacy` preset
/// switches four of them off. A parity test over a world with no buildings would compare
/// zero against zero for the section most likely to be dropped.
fn rich_world() -> World {
    let mut params = GridParams::tr36885_urban();
    params.rsu_at_junctions = true;
    grid(&params, &opts()).expect("the tr36885-urban grid builds")
}

/// **W1** — "`GET /world/{hash}.vwb` returns a body whose SHA-256 equals `{hash}` and
/// `Hello.world_hash`."
///
/// The three have to be one number: the URL the client fetches, the digest stored inside
/// the file, and the digest recomputed from the bytes. The negative control changes one
/// byte of the body and shows the recomputation moves — without it, a `verify` that always
/// returned the stored hash would pass.
#[test]
fn w1_the_payload_url_hash_is_the_digest_of_the_body() {
    let payload = serde_vwp::write(&small_world()).expect("the payload writes");

    let recomputed = serde_vwp::verify(&payload.bytes).expect("the file verifies");
    assert_eq!(
        recomputed, payload.content_hash,
        "the digest recomputed from the bytes differs from the one the writer reported"
    );
    let stored = serde_vwp::stored_content_hash(&payload.bytes).expect("a stored hash");
    assert_eq!(
        stored, payload.content_hash,
        "the digest stored in the file differs from the one the writer reported"
    );

    let hex = payload.content_hash_hex();
    assert_eq!(hex.len(), 64, "a SHA-256 is 64 hex characters");
    assert_eq!(hex, v2xw_core::hash::hex_encode(&payload.content_hash));
    let url = payload.url_path();
    assert!(
        url.ends_with(&format!("{hex}.vwb")),
        "the URL `{url}` is not keyed by the payload digest"
    );

    // The injected fault: one flipped byte near the end of the body must change the
    // recomputed digest, which is what makes W3's client-side verification meaningful.
    let mut tampered = payload.bytes.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    let after = serde_vwp::verify(&tampered).expect("the tampered file still parses");
    assert_ne!(
        after, payload.content_hash,
        "flipping a byte of the body changed nothing, so the digest does not cover it"
    );
}

/// **W4** — `world_json_binary_parity`: "The JSON and binary forms of a world carry the
/// same lanes, buildings, junctions, signals, sites and bbox."
///
/// Named for the test §10.5 names. The counts are taken from the `World` model, so the two
/// serialisations are each compared against the thing they serialise rather than against
/// each other — two encoders that dropped the same section would otherwise agree.
#[test]
fn w4_world_json_binary_parity() {
    let world = rich_world();
    let payload = serde_vwp::write(&world).expect("the payload writes");
    let json = serde_vwp::to_json(&world).expect("the json form writes");

    assert_eq!(json["schema"], "vwp-world/1");
    assert_eq!(
        json["content_hash"],
        serde_json::Value::String(payload.content_hash_hex()),
        "the two forms must name the same world"
    );

    let counts = world.counts();
    let array_len = |key: &str| {
        json[key]
            .as_array()
            .unwrap_or_else(|| panic!("the json form has no `{key}` array"))
            .len()
    };
    assert_eq!(array_len("lanes"), counts.lanes);
    assert_eq!(array_len("junctions"), counts.junctions);
    assert_eq!(array_len("crossings"), counts.crossings);
    assert_eq!(array_len("buildings"), world.buildings.len());
    assert_eq!(array_len("sites"), world.sites.len());
    assert_eq!(
        array_len("signals"),
        world.signals.iter().map(|p| p.heads.len()).sum::<usize>()
    );

    // The check has something to check: every equality above holds trivially for a world
    // with nothing in it, which is the shape of a comparison that cannot fail.
    assert!(
        counts.lanes > 0
            && counts.junctions > 0
            && counts.crossings > 0
            && !world.buildings.is_empty()
            && !world.sites.is_empty()
            && !world.signals.is_empty(),
        "a section of the fixture world is empty, so the parity comparison compared \
         nothing for it: {counts:?}, {} buildings, {} sites, {} signal plans",
        world.buildings.len(),
        world.sites.len(),
        world.signals.len()
    );

    // §4.6's bounding box is an object, and it is the world's own.
    let bbox = json["bbox"]
        .as_object()
        .expect("the json form carries the bounding box");
    for key in ["min_x_m", "min_y_m", "max_x_m", "max_y_m", "min_z_m", "max_z_m"] {
        assert!(
            bbox.get(key).is_some_and(serde_json::Value::is_number),
            "bbox.{key} is missing or is not a number"
        );
    }
    assert_eq!(
        bbox["min_x_m"].as_f64().expect("a number"),
        world.bbox.min.x,
        "the json bbox is not the world's own"
    );
    assert_eq!(
        bbox["max_y_m"].as_f64().expect("a number"),
        world.bbox.max.y
    );
}

/// **W5** — "`world.generate` with the same params and seed produces the same `world_hash`
/// on every platform."
///
/// One platform's half: the generator is a pure function of its parameters, twice over, and
/// the *bytes* agree and not merely the hash. The three-platform half cannot be a test —
/// each CI job only checks its own assertions — and is the comparison
/// `.github/workflows/ci.yml` already runs over the importer's published digests, which
/// `src/bin/golden_digest.rs` extends to the engine.
#[test]
fn w5_world_generate_is_a_pure_function_of_its_parameters() {
    let params = GridParams::legacy().with_size(4, 3).with_block_m(150.0);
    let opts = opts();

    let a = serde_vwp::write(&grid(&params, &opts).expect("builds")).expect("writes");
    let b = serde_vwp::write(&grid(&params, &opts).expect("builds")).expect("writes");
    assert_eq!(
        a.content_hash, b.content_hash,
        "the same parameters produced two different worlds"
    );
    assert_eq!(
        a.bytes, b.bytes,
        "the hashes agree but the bytes do not, which means the hash does not cover \
         everything the file carries"
    );

    // The injected fault: change one parameter and the hash must move. A hash that is the
    // same for every world is not a world hash.
    let other = GridParams::legacy().with_size(4, 4).with_block_m(150.0);
    let c = serde_vwp::write(&grid(&other, &opts).expect("builds")).expect("writes");
    assert_ne!(
        a.content_hash, c.content_hash,
        "a 4 x 3 lattice and a 4 x 4 lattice hash the same"
    );
}
