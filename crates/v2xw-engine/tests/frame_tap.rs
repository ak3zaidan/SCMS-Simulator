//! The frame tap hands a viewer each frame's octets and changes nothing recorded.
//!
//! `RunRecorder::tap_frame` exists so the chase view's message inspector can decode the SPDU
//! a node actually signed. It is a tap, not a record: these tests hold it to that — the record
//! stream of a run whose recorder takes the taps is byte-identical to one whose recorder
//! ignores them — and to its own contract: one tap per `node.tx` record the node encoded,
//! with the same message id and exactly `spdu_bytes` octets.

use std::collections::BTreeMap;
use std::path::Path;

use v2xw_core::ctx::OwnedRecord;
use v2xw_core::ids::NodeId;
use v2xw_core::time::SimTime;
use v2xw_engine::{Engine, MemoryRecorder, RunRecorder, Scenario};
use v2xw_metrics::channels::{NodeTxView, decode};

/// A recorder that keeps the taps and forwards everything else.
struct Tapping {
    inner: MemoryRecorder,
    taps: BTreeMap<u64, (NodeId, Vec<u8>)>,
}

impl RunRecorder for Tapping {
    fn write(&mut self, at: SimTime, record: &OwnedRecord) {
        self.inner.write(at, record);
    }
    fn write_wire_frame(&mut self, frame: &v2xw_record::wire::Frame) {
        self.inner.write_wire_frame(frame);
    }
    fn tap_frame(&mut self, _at: SimTime, node: NodeId, msg: u64, spdu: &[u8]) {
        let previous = self.taps.insert(msg, (node, spdu.to_vec()));
        assert!(previous.is_none(), "message {msg} was tapped twice");
    }
}

fn scenario() -> Scenario {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut s = Scenario::load(root.join("scenarios/phase1-grid.yaml")).expect("loads");
    s.time.duration_s = 4.0;
    s.actors.vehicles.demand.rate_veh_per_h = Some(3000.0);
    s
}

#[test]
fn taps_change_no_record_and_carry_each_frames_octets() {
    let mut plain = MemoryRecorder::new();
    Engine::build(scenario(), "")
        .expect("builds")
        .run(&mut plain)
        .expect("runs");
    let mut tapping = Tapping {
        inner: MemoryRecorder::new(),
        taps: BTreeMap::new(),
    };
    Engine::build(scenario(), "")
        .expect("builds")
        .run(&mut tapping)
        .expect("runs");

    assert_eq!(
        plain.digest_hex(),
        tapping.inner.digest_hex(),
        "taking the taps changed the record stream"
    );

    let mut encoded = 0usize;
    for (_, r) in tapping
        .inner
        .records()
        .iter()
        .filter(|(_, r)| r.channel == "node.tx")
    {
        let tx: NodeTxView = decode(r).expect("node.tx decodes");
        let Some(spdu) = tx.spdu_bytes.filter(|_| tx.payload_bytes.is_some()) else {
            continue;
        };
        let msg = tx.msg.expect("a message id");
        let (node, octets) = tapping
            .taps
            .get(&msg)
            .unwrap_or_else(|| panic!("frame {msg} was put on the air and not tapped"));
        assert_eq!(*node, tx.node, "frame {msg} tapped for another node");
        assert_eq!(
            octets.len() as u64,
            spdu,
            "frame {msg}: tapped octets vs spdu_bytes"
        );
        encoded += 1;
    }
    assert!(
        encoded > 50,
        "only {encoded} encoded frames in a 4 s 3000 veh/h run"
    );
    assert_eq!(
        encoded,
        tapping.taps.len(),
        "a tap with no encoded node.tx record"
    );
}
