# Retained reference material from the retired MOSAIC/Java layer

`01-inventory.md` §3.7 retires `scms-sim/` — a second, diverging implementation
with hard-coded `C:\Users\Administrator` paths, bundled `.exe` binaries and NTFS
junctions, which ran from no fresh clone on any operating system. It explicitly
names three pieces worth keeping as reference, and only those three are here.
Nothing in this directory is built, tested or imported; it exists to be read.

## `jvm/LinkageEngine.java` (204 lines)

A verified port of the frozen Python `linkage.py`, agreeing with it on ten
shared test vectors. Keep as a reference implementation for anyone integrating
a JVM stack against our linkage values, and as an independent check on the
Rust implementation in `v2xw-sec`.

## `jvm/ScmsBeaconApp.java` (425 lines)

Lines 140–154 hold a correct ETSI EN 302 637-2 CAM generation-rule block:
trigger when the position changes by more than 4 m, the heading by more than
4°, or the speed by more than 0.5 m/s, floored at `T_GenCamMin` and
heart-beating at `T_GenCamMax`. Cite it when writing the CAM generator.

It also carries the alpha-beta tracker gains and normalised-residual gate the
old detectors used, which are a useful cross-check when the Phase 2 detectors
are calibrated. Note that its `SignedCam.sigValid` was always true, so nothing
here is evidence about signature behaviour.

## `sumo/mapgen.py` (424 lines)

Usable `netgenerate` (grid, spider, random), `netconvert` and `randomTrips.py`
invocations with their timeouts and parameters. Keep for the optional SUMO
mobility tier (ADR 0005). The Windows `.exe` paths in it are not portable and
must be reworked, not copied.

## Deliberately not kept

`ScmsBackend`, `AttackLib` (32 bases x profiles = 355 variants), the `Scms`
stubs, `SignedCam` (signature validity hard-coded true), and the four
PowerShell drivers. The `third_party/veremi-nextgen` submodule is also removed:
it was never checked out, and VeReMi compatibility is an exporter format in the
new design (`08-measurement-and-data.md` §5.2), not a dependency.
