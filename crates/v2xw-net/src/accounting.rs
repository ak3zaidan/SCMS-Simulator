//! Byte accounting: invariant **I-N1** as a type.
//!
//! 03-interfaces.md §5 states it in one line: "every byte counted in `bytes_on_wire` is
//! attributed to exactly one accounting bucket (air / cellular UL / cellular DL / backhaul
//! / backend)". 04-models.md §7 repeats it for the network layer. The invariant is what
//! makes a run's totals addable: a frame that reached a vehicle over the air and a report
//! that reached the backend over an RSU's backhaul are different costs, and an aggregate
//! that counted one transmission in two buckets would overstate the load on both.
//!
//! # How the type makes double-counting impossible
//!
//! The rule is enforced by ownership rather than by review:
//!
//! 1. Bytes enter the accounting as a [`BytesOnWire`] token. The token is **not** `Copy`
//!    and **not** `Clone`, so there is only ever one of it.
//! 2. [`ByteLedger::credit`] takes the token **by value**. Crediting consumes it, so the
//!    same bytes cannot be credited to a second bucket — the second call would not
//!    compile.
//! 3. The token is `#[must_use]`, and its [`Drop`] fires a debug assertion if it is
//!    dropped without being credited or explicitly [`BytesOnWire::discard`]ed, **and in
//!    every build — release included — counts it in [`unattributed_drops`]**. Losing
//!    bytes is as wrong as counting them twice, and this is the half of the invariant a
//!    move-only type cannot express on its own; an assertion that compiles away is
//!    enforced only where it does not matter, since a simulation run is a release build.
//!    A run reports [`unattributed_drops`] and [`unattributed_bytes`] at the end, so a
//!    lost byte shows up as a number rather than as silence.
//! 4. The ledger keeps an independent witness, [`ByteLedger::credited_total`], incremented
//!    once per credit. [`ByteLedger::is_consistent`] compares it against the sum of the
//!    buckets, so a future bucket added without a matching accumulator is caught by a test
//!    rather than by a reader.
//!
//! The assertion is a `debug_assert!` and is suppressed while the thread is already
//! panicking, because a panic inside `drop` during unwinding aborts the process and would
//! replace the real failure with this one. The counter is incremented in that case too:
//! the bytes really were lost, whatever else was going wrong at the time.
//!
//! What the type **cannot** prevent is a caller *minting* a second token for the same
//! transmission, which no local type could. The discipline that closes that gap is a
//! convention the type makes visible rather than enforces: the layer that computes a
//! `bytes_on_wire` figure mints exactly one token for it and hands the token on, and
//! [`BytesOnWire::origin`] records which layer that was, so a double credit shows up in the
//! ledger as two credits from the same origin for one transmission rather than as an
//! anonymous discrepancy.
//!
//! # Why the counters are integers
//!
//! Bytes are counted in `u64`, never in `f64`. Integer addition is exact and associative,
//! so a per-node ledger merged across a phase-parallel map ([`ByteLedger::merge`]) gives
//! the same totals whatever order the tasks finished in — the one reduction in this crate
//! that needs no sorting by id (02-architecture.md §6.4). `u64` also cannot overflow in
//! practice: a 10,000-node run at 10 Hz and 400 B per message needs 2.3 × 10^14 years to
//! reach `u64::MAX`.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// How many [`BytesOnWire`] tokens this process has dropped without attributing them.
static UNATTRIBUTED_DROPS: AtomicU64 = AtomicU64::new(0);

/// How many bytes those tokens carried.
static UNATTRIBUTED_BYTES: AtomicU64 = AtomicU64::new(0);

/// How many [`BytesOnWire`] tokens have been dropped without being credited or discarded,
/// process-wide, in **every** build.
///
/// The release-safe half of invariant I-N1's "no byte is lost". A run reports this at the
/// end; anything but zero is an accounting bug, and the debug assertion in
/// [`BytesOnWire`]'s [`Drop`] names the first one when the same code runs under
/// `cargo test`.
///
/// Process-wide rather than per-ledger because a token that is dropped never reaches a
/// ledger — that is the whole failure — so there is no ledger to charge it to.
pub fn unattributed_drops() -> u64 {
    UNATTRIBUTED_DROPS.load(Ordering::Relaxed)
}

/// How many bytes the tokens counted by [`unattributed_drops`] carried.
pub fn unattributed_bytes() -> u64 {
    UNATTRIBUTED_BYTES.load(Ordering::Relaxed)
}

/// One accounting bucket. Every byte belongs to exactly one of these (invariant I-N1).
///
/// Closed against casual extension: a new bucket means a new accumulator in
/// [`ByteLedger`], a new column in every exported aggregate and a decision about which
/// existing bucket it was carved out of. `#[non_exhaustive]` keeps adding one a
/// non-breaking change for downstream matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Bucket {
    /// The 5.9 GHz sidelink: ITS-G5 / DSRC frames and C-V2X PC5 transmissions.
    ///
    /// Counted once per *transmission*, not once per receiver: a broadcast CAM heard by
    /// forty vehicles is one frame on the air.
    Air,
    /// Cellular uplink, UE to network (Uu).
    CellularUl,
    /// Cellular downlink, network to UE (Uu).
    CellularDl,
    /// An RSU's backhaul link to the operator's network.
    Backhaul,
    /// Between backend entities (RA/EA/AA, MA, CRL distribution).
    Backend,
}

impl Bucket {
    /// Every bucket, in declaration order. The order is the one exports and reports use.
    pub const ALL: [Bucket; 5] = [
        Bucket::Air,
        Bucket::CellularUl,
        Bucket::CellularDl,
        Bucket::Backhaul,
        Bucket::Backend,
    ];

    /// The bucket's stable name, as records and exports spell it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Bucket::Air => "air",
            Bucket::CellularUl => "cellular-ul",
            Bucket::CellularDl => "cellular-dl",
            Bucket::Backhaul => "backhaul",
            Bucket::Backend => "backend",
        }
    }
}

impl core::fmt::Display for Bucket {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A quantity of bytes that has been produced but not yet attributed to a bucket.
///
/// Move-only by design; see the module documentation. The usual shape is one statement:
///
/// ```
/// use v2xw_net::accounting::{Bucket, ByteLedger, BytesOnWire};
///
/// let mut ledger = ByteLedger::new();
/// ledger.credit(BytesOnWire::new(404, "node.tx"), Bucket::Air);
/// assert_eq!(ledger.get(Bucket::Air), 404);
/// assert_eq!(ledger.total(), 404);
/// ```
///
/// Crediting the same bytes twice does not compile, because the first credit consumed the
/// token:
///
/// ```compile_fail
/// use v2xw_net::accounting::{Bucket, ByteLedger, BytesOnWire};
/// let mut ledger = ByteLedger::new();
/// let bytes = BytesOnWire::new(404, "node.tx");
/// ledger.credit(bytes, Bucket::Air);
/// ledger.credit(bytes, Bucket::Backhaul);   // error[E0382]: use of moved value
/// ```
#[derive(Debug)]
#[must_use = "every byte must be attributed to exactly one bucket (invariant I-N1): \
              credit it to a ByteLedger, or call discard() if nothing was sent"]
pub struct BytesOnWire {
    bytes: u32,
    origin: &'static str,
    settled: bool,
}

impl BytesOnWire {
    /// A token for `bytes` bytes produced by `origin`.
    ///
    /// `origin` is a compile-time label for the layer that produced the count — the
    /// recording channel is the conventional choice (`"node.tx"`, `"proto.msg"`) — and it
    /// appears in the debug assertion when a token is dropped unattributed, which is what
    /// makes that failure findable.
    pub const fn new(bytes: u32, origin: &'static str) -> Self {
        Self {
            bytes,
            origin,
            settled: false,
        }
    }

    /// How many bytes this token carries.
    pub const fn bytes(&self) -> u32 {
        self.bytes
    }

    /// The label of the layer that produced the count.
    pub const fn origin(&self) -> &'static str {
        self.origin
    }

    /// Settles the token **without** crediting any bucket, returning the byte count.
    ///
    /// The legitimate escape: a transmission that was gated away by DCC, a PDU refused by
    /// the MTU check, a cellular send that found no coverage. Those bytes never existed on
    /// any medium, so attributing them to a bucket would overstate the load — but dropping
    /// the token silently would make a genuine accounting bug indistinguishable from this
    /// case, which is why the intent has to be spelled out.
    pub fn discard(mut self) -> u32 {
        self.settled = true;
        self.bytes
    }

    /// Marks the token settled and yields its bytes. Private: the only settling paths are
    /// [`ByteLedger::credit`] and [`BytesOnWire::discard`].
    fn settle(&mut self) -> u32 {
        debug_assert!(!self.settled, "BytesOnWire settled twice");
        self.settled = true;
        self.bytes
    }
}

impl Drop for BytesOnWire {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        // Counted first, and in every build: a simulation run is a release build, where
        // the assertion below compiles away.
        UNATTRIBUTED_DROPS.fetch_add(1, Ordering::Relaxed);
        UNATTRIBUTED_BYTES.fetch_add(u64::from(self.bytes), Ordering::Relaxed);
        // Suppressed during unwinding: a panic in `drop` while panicking aborts, which
        // would hide the failure that is actually being reported.
        debug_assert!(
            std::thread::panicking(),
            "{} B from {} were dropped without being attributed to a bucket \
             (invariant I-N1); credit them to a ByteLedger or call discard()",
            self.bytes,
            self.origin
        );
    }
}

/// Where every byte of a run went: one accumulator per [`Bucket`], plus the witness that
/// makes invariant I-N1 checkable.
///
/// A run holds one of these per node (or per node and per phase, merged with
/// [`ByteLedger::merge`]); metric providers read it through [`ByteLedger::get`] and
/// [`ByteLedger::buckets`].
/// **Not `Copy`.** A ledger is folded into a total exactly once, and [`ByteLedger::merge`]
/// takes its source by value to say so: a `Copy` ledger merged twice doubled every bucket
/// *and* the witness, so [`ByteLedger::is_consistent`] still returned true and only
/// [`ByteLedger::credits`] hinted at it — and a caller that does not know the expected
/// credit count cannot read that hint. `Clone` is kept, so a caller that really does want
/// a second copy writes `merge(other.clone())` and says so at the call site.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteLedger {
    /// Bytes on the 5.9 GHz sidelink.
    air: u64,
    /// Bytes on the cellular uplink.
    cellular_ul: u64,
    /// Bytes on the cellular downlink.
    cellular_dl: u64,
    /// Bytes on RSU backhaul links.
    backhaul: u64,
    /// Bytes between backend entities.
    backend: u64,
    /// How many tokens were credited.
    credits: u64,
    /// The sum of every credited token, accumulated independently of the buckets.
    credited_total: u64,
}

impl ByteLedger {
    /// An empty ledger.
    pub const fn new() -> Self {
        Self {
            air: 0,
            cellular_ul: 0,
            cellular_dl: 0,
            backhaul: 0,
            backend: 0,
            credits: 0,
            credited_total: 0,
        }
    }

    /// Attributes `bytes` to `bucket`, consuming the token, and returns the bucket's new
    /// total.
    pub fn credit(&mut self, mut bytes: BytesOnWire, bucket: Bucket) -> u64 {
        let n = u64::from(bytes.settle());
        let slot = self.slot_mut(bucket);
        *slot += n;
        let total = *slot;
        self.credits += 1;
        self.credited_total += n;
        total
    }

    /// One bucket's total, bytes.
    pub const fn get(&self, bucket: Bucket) -> u64 {
        match bucket {
            Bucket::Air => self.air,
            Bucket::CellularUl => self.cellular_ul,
            Bucket::CellularDl => self.cellular_dl,
            Bucket::Backhaul => self.backhaul,
            Bucket::Backend => self.backend,
        }
    }

    /// Every bucket and its total, in [`Bucket::ALL`] order.
    pub fn buckets(&self) -> [(Bucket, u64); 5] {
        Bucket::ALL.map(|b| (b, self.get(b)))
    }

    /// The sum of the buckets — every byte the ledger knows about.
    pub const fn total(&self) -> u64 {
        self.air + self.cellular_ul + self.cellular_dl + self.backhaul + self.backend
    }

    /// How many tokens have been credited.
    pub const fn credits(&self) -> u64 {
        self.credits
    }

    /// The sum of every credited token, accumulated independently of the buckets.
    ///
    /// The witness for invariant I-N1: it must equal [`ByteLedger::total`], and it is
    /// computed without ever looking at a bucket, so the two agree only if each credit
    /// landed in exactly one accumulator.
    pub const fn credited_total(&self) -> u64 {
        self.credited_total
    }

    /// True if the buckets sum to the independently accumulated total (invariant I-N1).
    pub const fn is_consistent(&self) -> bool {
        self.total() == self.credited_total
    }

    /// Folds `other` into `self`, bucket by bucket, **consuming it**.
    ///
    /// By value, and [`ByteLedger`] is not `Copy`, for the same reason [`BytesOnWire`] is
    /// not: a source that can be merged twice will be. Folding the same per-node ledger
    /// into a total twice doubles every bucket and the witness together, so
    /// [`ByteLedger::is_consistent`] goes on returning true and nothing reports the error
    /// — which makes it the easier of the two double-counting mistakes to make in a
    /// phase-parallel reduction over a `BTreeMap<NodeId, ByteLedger>`, and the harder to
    /// notice. Consuming the source turns it into a borrow-checker error.
    ///
    /// Integer addition is associative, so merging per-node ledgers in any order gives the
    /// same result — which is why this reduction, unlike the float reductions of
    /// 02-architecture.md §6.4, needs no sorting by id.
    ///
    /// ```
    /// use v2xw_net::accounting::{Bucket, ByteLedger, BytesOnWire};
    ///
    /// let mut node = ByteLedger::new();
    /// node.credit(BytesOnWire::new(1_000, "phy"), Bucket::Air);
    /// let mut total = ByteLedger::new();
    /// total.merge(node);
    /// assert_eq!(total.get(Bucket::Air), 1_000);
    /// ```
    ///
    /// Merging the same ledger a second time does not compile:
    ///
    /// ```compile_fail
    /// use v2xw_net::accounting::{Bucket, ByteLedger, BytesOnWire};
    ///
    /// let mut node = ByteLedger::new();
    /// node.credit(BytesOnWire::new(1_000, "phy"), Bucket::Air);
    /// let mut total = ByteLedger::new();
    /// total.merge(node);
    /// total.merge(node); // `node` was moved by the first merge
    /// ```
    pub fn merge(&mut self, other: ByteLedger) {
        self.air += other.air;
        self.cellular_ul += other.cellular_ul;
        self.cellular_dl += other.cellular_dl;
        self.backhaul += other.backhaul;
        self.backend += other.backend;
        self.credits += other.credits;
        self.credited_total += other.credited_total;
    }

    /// The accumulator for one bucket.
    fn slot_mut(&mut self, bucket: Bucket) -> &mut u64 {
        match bucket {
            Bucket::Air => &mut self.air,
            Bucket::CellularUl => &mut self.cellular_ul,
            Bucket::CellularDl => &mut self.cellular_dl,
            Bucket::Backhaul => &mut self.backhaul,
            Bucket::Backend => &mut self.backend,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The headline test of invariant I-N1: whatever is credited, the buckets sum to
    /// exactly the bytes that went in, and the witness agrees.
    #[test]
    fn the_buckets_sum_exactly_to_what_was_credited() {
        let mut ledger = ByteLedger::new();
        let credited: [(u32, Bucket); 7] = [
            (404, Bucket::Air),
            (52, Bucket::Air),
            (1_398, Bucket::CellularUl),
            (180, Bucket::CellularDl),
            (2_304, Bucket::Backhaul),
            (162, Bucket::Backend),
            (858, Bucket::Backend),
        ];
        let mut expected = 0u64;
        for (n, bucket) in credited {
            ledger.credit(BytesOnWire::new(n, "test"), bucket);
            expected += u64::from(n);
        }

        assert_eq!(ledger.total(), expected);
        assert_eq!(ledger.credited_total(), expected);
        assert!(ledger.is_consistent());
        assert_eq!(ledger.credits(), 7);
        assert_eq!(ledger.get(Bucket::Air), 404 + 52);
        assert_eq!(ledger.get(Bucket::Backend), 162 + 858);
        // …and the sum over the reported pairs is the same number a metric provider gets.
        let by_bucket: u64 = ledger.buckets().iter().map(|(_, n)| *n).sum();
        assert_eq!(by_bucket, expected);
    }

    #[test]
    fn discarded_bytes_reach_no_bucket() {
        let mut ledger = ByteLedger::new();
        ledger.credit(BytesOnWire::new(100, "test"), Bucket::Air);
        // A frame DCC gated away: the bytes existed as a decision, never as a transmission.
        assert_eq!(BytesOnWire::new(404, "node.tx").discard(), 404);
        assert_eq!(ledger.total(), 100);
        assert!(ledger.is_consistent());
    }

    #[test]
    fn merging_ledgers_is_order_independent() {
        let build = |pairs: &[(u32, Bucket)]| {
            let mut l = ByteLedger::new();
            for (n, b) in pairs {
                l.credit(BytesOnWire::new(*n, "test"), *b);
            }
            l
        };
        let a = build(&[(100, Bucket::Air), (7, Bucket::Backend)]);
        let b = build(&[(200, Bucket::CellularUl)]);
        let c = build(&[(300, Bucket::Air), (11, Bucket::Backhaul)]);

        let mut forward = ByteLedger::new();
        for l in [&a, &b, &c] {
            forward.merge(l.clone());
        }
        let mut backward = ByteLedger::new();
        for l in [&c, &b, &a] {
            backward.merge(l.clone());
        }
        assert_eq!(forward, backward);
        assert_eq!(forward.total(), 618);
        assert!(forward.is_consistent());
        // And the double-merge the type now refuses: every bucket AND the witness would
        // double together, so `is_consistent` would go on saying true. The compile-fail
        // doctest on `merge` is the statement that it cannot happen; this is the statement
        // of why it mattered.
        let mut doubled = ByteLedger::new();
        doubled.merge(a.clone());
        doubled.merge(a.clone());
        assert_eq!(doubled.total(), 2 * a.total());
        assert!(
            doubled.is_consistent(),
            "the witness doubles with the buckets, which is why this was invisible"
        );
        assert_eq!(doubled.credits(), 2 * a.credits());
    }

    #[test]
    fn a_token_carries_its_origin_for_the_diagnostic() {
        let t = BytesOnWire::new(404, "node.tx");
        assert_eq!(t.bytes(), 404);
        assert_eq!(t.origin(), "node.tx");
        let _ = t.discard();
    }

    /// Dropping a token without settling it is a bug. It is **counted in every build**,
    /// and in a debug build it also panics with a message naming the bytes and where they
    /// came from.
    ///
    /// The counter is the half that matters at run time: a simulation run is a release
    /// build, where the assertion compiles away, and before this existed 404 B minted and
    /// dropped under `--release` vanished with no assertion, no counter and no record.
    #[test]
    fn dropping_bytes_unattributed_is_counted_in_every_build() {
        let drops = unattributed_drops();
        let bytes = unattributed_bytes();
        let result = std::panic::catch_unwind(|| {
            let _lost = BytesOnWire::new(404, "node.tx");
        });
        assert!(
            unattributed_drops() > drops,
            "the drop was not counted: {} then {}",
            drops,
            unattributed_drops()
        );
        assert!(
            unattributed_bytes() >= bytes + 404,
            "the bytes were not counted"
        );
        if cfg!(debug_assertions) {
            let err = result.expect_err("the debug assertion did not fire");
            let message = err
                .downcast_ref::<String>()
                .map(String::as_str)
                .unwrap_or_default();
            assert!(message.contains("404 B from node.tx"), "{message}");
            assert!(
                message.contains("without being attributed to a bucket"),
                "{message}"
            );
        } else {
            assert!(result.is_ok(), "a release build must not panic here");
        }
        // A settled token is not counted, in any build.
        let drops = unattributed_drops();
        let _ = BytesOnWire::new(11, "node.tx").discard();
        let mut ledger = ByteLedger::new();
        ledger.credit(BytesOnWire::new(22, "node.tx"), Bucket::Air);
        assert_eq!(unattributed_drops(), drops);
    }

    #[test]
    fn bucket_names_are_the_exported_spellings() {
        assert_eq!(Bucket::CellularUl.as_str(), "cellular-ul");
        assert_eq!(Bucket::CellularUl.to_string(), "cellular-ul");
        assert_eq!(
            serde_json::to_string(&Bucket::CellularDl).unwrap(),
            "\"cellular-dl\""
        );
        assert_eq!(Bucket::ALL.len(), 5, "five buckets, per invariant I-N1");
    }
}
