//! What a transmitted message said reaches the recording: every `node.tx` names the
//! pseudonym that signed it and carries the message's decoded content, so the followed
//! vehicle's view can show both, and a pseudonym change is visible on the air.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use v2xw_engine::{Engine, MemoryRecorder, Scenario};
use v2xw_metrics::channels::{NodeTxView, decode};

fn scenarios() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scenarios")
}

fn transmissions(s: Scenario) -> Vec<NodeTxView> {
    let mut engine = Engine::build(s, "").expect("builds");
    let mut recorder = MemoryRecorder::new();
    engine.run(&mut recorder).expect("runs");
    recorder
        .records()
        .iter()
        .filter(|(_, r)| r.channel == "node.tx")
        .map(|(_, r)| decode(r).expect("node.tx decodes"))
        .collect()
}

#[test]
fn every_bsm_on_the_air_carries_its_pseudonym_and_its_decoded_content() {
    let mut s = Scenario::load(scenarios().join("phase1-grid.yaml")).expect("loads");
    s.time.duration_s = 4.0;
    s.actors.vehicles.demand.rate_veh_per_h = Some(3000.0);
    // Rotate every second so a four-second run must show a change.
    s.security.pseudonym_change.strategy = "time".to_string();
    s.security.pseudonym_change.period_s = Some(1.0);
    let txs = transmissions(s);
    let bsms: Vec<&NodeTxView> = txs
        .iter()
        .filter(|t| t.msg_type.as_deref() == Some("bsm"))
        .collect();
    assert!(bsms.len() > 50, "only {} BSMs on the air", bsms.len());

    let mut counts: BTreeMap<(u32, String), Vec<u8>> = BTreeMap::new();
    let mut pseudonyms: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for t in &bsms {
        let p = t.pseudonym.as_deref().expect("a BSM names its pseudonym");
        assert_eq!(p.len(), 16, "a HashedId8 is sixteen hex digits: {p}");
        let c = t.content.as_ref().expect("a BSM carries its content");
        // The temporary id is the pseudonym's first four octets, so it rotates with it.
        assert_eq!(c.temp_id.as_deref(), Some(&p[..8]), "temp id vs pseudonym");
        let lat = c.lat_deg.expect("a latitude was encoded");
        let lon = c.lon_deg.expect("a longitude was encoded");
        assert!((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon));
        let v = c.speed_mps.expect("a speed was encoded");
        let claimed = c.claimed_speed_mps.expect("the claim is recorded");
        // J2735 speed has a 0.02 m/s LSB.
        assert!((v - claimed).abs() <= 0.02 + 1e-9, "encoded {v} vs claimed {claimed}");
        assert!(c.sec_mark_ms.is_some_and(|m| m < 60_000));
        counts
            .entry((t.node.index(), p.to_string()))
            .or_default()
            .push(c.msg_count.expect("msgCnt"));
        let seen = pseudonyms.entry(t.node.index()).or_default();
        if seen.last().map(String::as_str) != Some(p) {
            seen.push(p.to_string());
        }
    }
    // msgCnt advances by one per message (mod 128) under one pseudonym.
    for ((node, _), c) in &counts {
        for w in c.windows(2) {
            assert_eq!(w[1], (w[0] + 1) % 128, "node {node}: msgCnt {} then {}", w[0], w[1]);
        }
    }
    assert!(
        pseudonyms.values().any(|p| p.len() > 1),
        "a one-second rotation period left every vehicle on its first pseudonym: {pseudonyms:?}"
    );
}
