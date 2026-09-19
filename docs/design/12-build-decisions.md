# Build decisions (binding on all build agents)

Recorded 2026-09-18 during the Phase 0/1 build. These resolve choices the design left open or that the asset survey forced. Agents must follow them and must not re-litigate them.

## D1. Toolchain: bump to current stable

`rust-toolchain.toml` pins **the current stable (1.98.1, 2026-09-01)**, not 1.86.0. The 1.86 pin in the first scaffold pass was an artefact of what happened to be installed. Reasons: `rasn` 0.28+ requires 1.88+ and pinning 1.86 would force `rasn = "=0.27.0"` exactly for no benefit; the project is greenfield so there is no old compiler to support; determinism is unaffected because IEEE 754 basic operations are stable across compiler versions and all transcendentals go through the `libm` crate, never `std` (ADR 0003, ADR 0004).

Consequence: the CI matrix installs the same pinned stable on all three runners. Bumping the pin later is a deliberate act that re-baselines golden digests, and the manifest records the compiler version for exactly this reason.

## D2. Message encoders: real ETSI, hand-written BSM, size model for the rest

The asset survey established, with evidence, that `rasn-compiler` 0.16 generates Rust that **builds and works** for the ETSI stack, and generates Rust that **does not build** for SAE J2735 (170 errors, all from the `RegionalExtension {REG-EXT-ID-AND-TYPE : Set}` parameterised-type idiom over an information object class, which the compiler does not support; stripping the optional regional fields still leaves 105 errors, so it is not a one-line fix). Therefore:

| Message | Encoder | Status |
|---|---|---|
| CAM (EN 302 637-2 / TS 103 900), DENM, CPM, VAM | `rasn`-generated from the ETSI forge modules | real UPER, byte-exact |
| IEEE 1609.2 / TS 103 097 security envelope and certificates | `rasn`-generated (COER) | real, byte-exact |
| **SAE J2735 BSM** | **hand-written UPER codec for the simulated subset** (`BSMcoreData` Part I, plus the Part II containers the simulator actually fills) | real UPER for the fields we emit |
| J2735 SPaT, MAP, PSM, SRM, SSM | validated size model (`codec/size-model/j2735`) | exact size, placeholder bytes |

The hand-written BSM codec is written against the J2735 2024-09 ASN.1 now held locally, and is **cross-validated against a pycrate oracle**: encode in Rust, decode in Python with pycrate (or the `usdot-fhwa-stol/j2735decoder` wheels, which are pycrate-compiled), assert field-for-field equality, and round-trip. That conformance test is part of the crate's test suite and is the evidence that the bytes are genuinely J2735-conformant. Without that oracle the hand-written codec does not ship.

This satisfies the Phase 1 acceptance criterion ("real 1609.2-style signed BSMs") with genuinely real bytes on both the payload and the envelope, and it keeps `04-models.md` §8.3's build-time-import escape hatch for anyone who later has SAE's own toolchain.

## D3. J2735 ASN.1 files are never committed

The gathered J2735 modules carry an embedded SAE user-license agreement forbidding redistribution. The user has authorised local research use, and that is what we do: the files live in `third_party/asn1/j2735/` which is **git-ignored**, and the build reads them only as a reference for the hand-written codec and for the oracle test. Nothing derived from them that reproduces the specification text is committed either. The ETSI forge modules are BSD-3-Clause and **are** committed under `third_party/asn1/etsi/` with their LICENSE files and the provenance manifest.

Rationale for ignoring rather than committing despite the blanket permission: gitignoring costs nothing, loses no capability, and keeps a future public release of this repository clean without an archaeology exercise.

## D4. Encoding hygiene

Three ETSI/IEEE modules (CDD, `Ieee1609Dot2`, `Ieee1609Dot2BaseTypes`) are CP1252, not UTF-8, and `rasn-compiler` aborts on them. Keep the byte-exact originals and generate from the `normalized-utf8/` copies (comment characters only differ). Record both hashes in the provenance manifest.

## D5. Known upstream defects to carry as patches

Two one-token upstream problems were found and fixed during the survey; the build must carry them as recorded patches with a comment naming the upstream issue:
- ETSI `VAM-PDU-Descriptions.asn` lists `SequenceOfTrajectoryInterceptionIndication` twice in its IMPORTS.
- Generated 1609.2 code emits `EndEntityType([true,false].into_iter().collect())`, but `FixedBitString<8>` has no `FromIterator<bool>`.
- ETSI TS 102 941 (PKI) fails codegen on `WITH COMPONENTS` inner subtyping at `EtsiTs102941MessagesItss.asn:105:6`. TS 102 941 is a **Phase 4** need (ETSI PKI plug-in), not Phase 1, so it is deferred with this note rather than worked around now.
- ISO TS 19321 (IVI) is not on the ETSI forge, so IVIM alone will not resolve. IVIM is out of Phase 1 scope.

## D6. World coordinates

One convention everywhere: a local tangent plane in metres, East-North-Up, origin at the world's bounding-box south-west corner, recorded in `GeoOrigin { lat, lon, alt }` with the projection named in `WorldProvenance`. Headings are radians, ENU, 0 = east, counter-clockwise (the legacy engine's convention; the Java layer's compass bearing is not used). No `f32` for positions in engine state; `f32` appears only in the wire protocol and the renderer.

## D7. Phase 1 world

Manhattan, bounding box `-73.9900, 40.7440, -73.9680, 40.7620` (the legacy preset), OSM extract already downloaded to the scratchpad assets directory, 30 MB, 13,553 ways. The importer must be city-agnostic: nothing Manhattan-specific in code, only in the scenario file.

## D8. Engine crate

The design's crate list has no home for the main loop, the concrete event enum, or the scenario loader. Add **`v2xw-engine`**, sitting above `v2xw-node` and below `v2xw-server`/`v2xw-cli`. It owns: the scenario schema and loader, the concrete `Event` enum that instantiates `Scheduler<E>`, the world/actor/node state, the main loop with the phase-parallel structure of ADR 0004, and the run manifest assembly. `v2xw-server`, `v2xw-cli` and later `v2xw-py` all drive it.

## D9. Every exported float is quantised at the writer (evidence-backed)

No floating-point value reaches a recorded, exported or digested artefact in raw IEEE-754 form. One central writer-side encoder quantises each float to its field's declared grid, and a scanning test fails the build if any output value sits off its grid. Rationale, evidence and the measured caution about threshold flips are in ADR 0004, section "Writer-side quantisation of every exported float". Forensic detail: `docs/design/research/legacy-digest-forensics.md`.

Practical consequences for the build:
- Every schema field that carries a float declares its quantum (metres and seconds default to 1e-3, matching the legacy convention; dB to 1e-2; ratios to 1e-4; probabilities to 1e-6; **geodetic degrees to 1e-7**, which is 11 mm of latitude — the same order as the metre grid — and the resolution of the 1/10-microdegree latitude and longitude a CAM or a BSM carries). The quantum is part of the field's contract and appears in the model card or the dataset schema; the degrees one is declared as `v2xw_core::GeoOrigin::Q_DEG`.
- A **digest** hashes the integer multiple of the quantum (`v2xw_core::math::grid_index`), not the rounded float: a quantised `f64` is still a binary approximation of a decimal grid point, and the integer is the value two platforms are guaranteed to agree on.
- The MA dataset exporter keeps `st_bbox` for v1 schema compatibility but quantises it like every other float. This means the v1 profile will not reproduce the legacy frozen digests — those are unreproducible on any non-Windows host anyway, which is exactly why they are retired.
- Cross-engine validation against the legacy Python reference compares identifiers, enumerations, counts, orderings and revocation sets exactly, and floats within 1e-9, never by digest equality.

## D10. Transcendental results never drive cross-engine control flow directly

Quantise before any threshold comparison whose outcome is compared across engines (detector z-thresholds, Sybil cell bucketing, plausibility gates). Measured justification: perturbing every transcendental by one unit in the last place shifted one legacy configuration's report count from 3,082 to 3,135. Within a single engine build this is a non-issue because the `libm` crate makes all platforms agree bit for bit; it matters when comparing the Rust engine against the Python reference, and it would matter again if the math library were ever changed.

## D11. `v2xw-core` is the normative source for plug-in signatures; 03-interfaces.md tracks it

The crate `crates/v2xw-core` is the **normative source** for every Rust signature the design publishes. `03-interfaces.md` documents that surface and is maintained to match it. Where the two disagree on a signature, the crate is right and the document is stale, and the document is corrected in the same change that moved the crate.

Reason to record it rather than leave it implicit: an adversarial review on 2026-09-18 checked the `Ctx` trait as published in `03-interfaces.md` §1.1 with `rustc` and found that it **did not compile**, and would not have compiled for any implementer who trusted it. It declared `fn world(&self) -> &World` beside `fn rng(&mut self, …) -> &mut RngStream`, so a model holding a world reference could not also draw a random number (the first thing a mobility or a fading model does), and `fn emit<R: Record>(&mut self, r: R)` made the trait non-dyn-compatible, so no family trait extending it could be used as a trait object, which is how in-process plug-ins are called (ADR 0007 §8). The implementation had already solved both, with a `&self` accessor returning an `RngGuard`, an object-safe `emit_erased` plus a blanket-implemented `CtxExt::emit`, and associated `World` / `Actors` / `Payload` types. A design document that publishes uncompilable code is worse than no document, because implementers trust it.

The rule has a limit, and the limit is the point of writing it down:

| Kind of disagreement | Who wins | How it is resolved |
|---|---|---|
| A signature, a type name, a field, a defaulted method, an error variant | the crate | correct the document, same change |
| An invariant, a schema's `required` list, a tier or licence policy, what a seam is *for* | the design | change the design first, then the crate; never the reverse |

So a crate change that alters substance (removing an invariant's enforcement, relaxing a schema, adding a family) still needs the design changed first. A crate change that renames a method or adds a parameter carries the document with it.

Mechanics: a change to a public item in `v2xw-core` updates `03-interfaces.md` §1, §1.1, §1.2, §12 and §17 in the same commit, and the document's header records the date it was last reconciled. Substantive disagreements found during a reconciliation are recorded in the document as "open points for arbitration" rather than silently decided by whichever side was read first; three are recorded at the end of §1.2 as of 2026-09-18 (a node-side context that cannot reach ground truth, spans still spelled `SimTime`, and what a family trait's `tier()` means when `Model::tiers()` already answers from the card).

## D11. Arbitration of design-versus-implementation disagreements (2026-09-18)

Reconciling `03-interfaces.md` against the implemented `v2xw-core` surfaced five substantive disagreements. Resolved as follows; the code is authoritative where it wins, and the design document has been updated to match.

**1. Spans are `Duration`, not `SimTime`.** The design spelled every span as `SimTime` (`Mobility::step(dt: SimTime)`, `Phy::air_time(..) -> SimTime`). The implementation introduces a distinct `Duration` newtype. The implementation wins: an instant and a span are different things, conflating them in one `u64` invites the classic "added a timestamp to a timestamp" defect, and the compiler can catch it for free. All family-trait signatures take `Duration` for spans.

**2. Tiers come from the model card, not from a per-trait method.** The design gave each family trait its own `fn tier(&self) -> Tier`. The implementation supplies `Model::tiers() -> &[Tier]` derived from the card. The implementation wins: the card is already the single declared source of a model's metadata, and a second, independently mutable copy of the tier is exactly the drift the model-card system exists to prevent. A model serving more than one tier is also expressible this way, which the original could not do.

**3. The ground-truth firewall is strong but not compile-proof, and must say so.** `NodeView` exposes no world, no actor index, no ground-truth kinematics and no actor id, so the ordinary way of reaching ground truth is closed. But an implementor can still smuggle it through an associated type. The module documentation claimed an implementation "cannot get it, and one that tries fails to compile", which is false. The claim is corrected, and enforcement moves to where it can actually live: the conformance kit's sentinel test (`03-interfaces.md` §17, invariants I-C2 and I-T1). An honest boundary with a test is worth more than an overstated one without.

**4. Model cards are lenient to parse and strict to validate.** The published schema marks fields required that the deserialiser defaults. Keep both, deliberately: `serde` defaults make cards pleasant to write, and `ModelCard::validate` enforces the contract, including that a card carries at least one source and a validation status. A model with no cited source must fail registration, because "no black box" is a project principle and not a preference. Parsing leniency without validation strictness would silently defeat it.

**5. Recording has two encodings, and that is intentional.** The design said channels are FlatBuffers; the implementation has a serde-based `Record` trait. Both are partly right, so the rule is now explicit:
- **Snapshot channels** (keyframes, deltas) store the VWP binary frames verbatim, exactly as ADR 0008 requires, because storing the bytes that went over the wire is what makes live and replay streams provably identical.
- **Event, telemetry and metric channels** use the serde `Record` path and land in Parquet or JSONL, where a self-describing columnar format is worth far more than zero-copy.
FlatBuffers is used for neither, per the ADR 0008 amendment.

## D12. Two design-document corrections and one interface arbitration

Recorded 2026-09-19 from the Phase 1 build. Three items, found by implementing
the documents and by independently re-deriving their numbers.

### D12.1 The PSID byte-count threshold in 04-models §9.1 was wrong

The envelope derivation said "ITS-AIDs ≥ 128 take 3" COER bytes. It is 256, not
128. `Psid ::= INTEGER (0..MAX)` has a lower bound of zero, so COER encodes it as
an unsigned integer and the third byte appears at 256. Two independent
measurements agree, and the encoder was right while the document was wrong.
Corrected in place. The 93-byte overhead figure itself is confirmed exactly, by
decomposing a real signed message octet by octet.

### D12.2 A model crate cannot name the engine's event type

03-interfaces §1 says `&mut dyn Ctx` is shorthand for the fully bound
`Ctx<World = World, Actors = ActorIndex, Payload = Event>`, with the associated
types bound "at the call site". §14.3 explains why they are associated types:
the concrete event enum lives in `v2xw-engine` (D8), so naming it in the contract
crate would invert the dependency.

Implementation exposed a gap in that reasoning. A *model* crate such as
`v2xw-mobility` or `v2xw-sec` sits **below** `v2xw-engine`, so it is a call site
that cannot name `Event` either. The two crates solved it differently and
diverged:

- `v2xw-mobility` introduced a narrowed `MobCtx` trait with no un-nameable
  associated type, plus an adapter from any real `Ctx`.
- `v2xw-sec` made `CryptoBackend` and `SecurityEnvelope` generic over
  `C: Ctx + ?Sized`.

**Decision: the narrowed per-family context trait wins**, and `v2xw-sec` should
converge on it. The generic form makes the family trait non-dyn-compatible the
moment a method is generic over the context, and ADR 0007 §8 requires in-process
plug-ins to be trait objects. A narrowed trait keeps dyn-compatibility, keeps the
dependency direction right, and states exactly what each family may touch, which
is what the NodeView firewall wants anyway. The engine supplies one blanket
adapter.

Where a family genuinely needs no context at all, it takes none.

### D12.3 The VWP §3.2 vertical-axis erratum

§3.2 defines the delta as "relative to the same field in the previously delivered
frame", but the absolute vertical field is centimetres everywhere it appears
(§3.3.2, §3.4.3, §3.4.5) while the delta field is named in millimetres and the
escape threshold is expressed in millimetres. Millimetres is the only reading
consistent with the field name, with the horizontal axes and with the escape
threshold, and it is what two independent implementations now do.

Every worked example in §9 carries a zero vertical delta, so no test vector pins
the unit. That is how a tenfold error survived in the TypeScript client.

**Decision:** the delta is millimetres on all three axes, taken against the
previously transmitted quantised value, which for the vertical axis is the
transmitted centimetre value times ten. §9 owes one vector with a non-zero
vertical delta so both implementations are pinned by a test rather than by prose.
