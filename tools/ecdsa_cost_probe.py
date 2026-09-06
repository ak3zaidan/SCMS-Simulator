"""What real ECDSA costs, per primitive, so a per-run bill can be built rather than guessed.

Three layers are timed separately, because they are three different decisions a user makes:

1. the **raw P-256 primitives** (`scms_core.ecdsa_p256`) -- sign and verify, on the exact key and
   message shapes the engine uses;
2. the **engine's own paths** (`SecurityLayer.sign` / `.verify`), which add TS 103 097 signer
   alternation, header construction and certificate/CRL checking around the primitive;
3. **butterfly provisioning** (`ScmsProvisioning.provision`), which is paid once per device at
   construction and never again.

Everything is reported as a rate AND as the bill for a stated fleet-hour, so "what would 1188
vehicles for an hour cost" is arithmetic over measured rates rather than an extrapolated run.

    python tools/ecdsa_cost_probe.py --fleet 1188 --hours 1 --json C:/Temp/pstack/ecdsa.json
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
_SRC = os.path.join(os.path.dirname(_HERE), "src")
if _SRC not in sys.path:
    sys.path.insert(0, _SRC)

from scms_sim_ref.scms_core import ecdsa_p256 as EC                     # noqa: E402
from scms_sim_ref.scms_core.engine_security import SecurityLayer        # noqa: E402
from scms_sim_ref.scms_core.linkage import DeviceLinkageContext         # noqa: E402


def _derive(label: str, n: int) -> bytes:
    """A deterministic key-derivation oracle with the shape the layer expects."""
    out, i = b"", 0
    while len(out) < n:
        out += hashlib.sha256(f"{label}|{i}".encode()).digest()
        i += 1
    return out[:n]


def _timed(fn, n: int, warmup: int = 50) -> dict:
    for _ in range(warmup):
        fn(0)
    t0 = time.perf_counter()
    for i in range(n):
        fn(i)
    dt = time.perf_counter() - t0
    return {"n": n, "wall_s": round(dt, 6), "per_op_us": round(dt / n * 1e6, 3),
            "ops_per_s": round(n / dt, 1)}


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--n", type=int, default=3000)
    p.add_argument("--fleet", type=int, default=1188, help="vehicles in the costed fleet-hour")
    p.add_argument("--hours", type=float, default=1.0)
    p.add_argument("--cam-rate-hz", type=float, default=10.0,
                   help="CAM rate the bill assumes; pass the MEASURED rate for the rules arm")
    p.add_argument("--fanout", type=float, default=14.14,
                   help="delivered CAMs per transmitted frame (the logical/computed ratio)")
    p.add_argument("--json", dest="json_out", default="")
    a = p.parse_args(argv)

    out: dict = {"signing_mode": EC.SIGNING_MODE, "deterministic": EC.DETERMINISTIC,
                 "python": sys.version.split()[0]}

    # 1. raw primitives -------------------------------------------------------------------- #
    sk = EC.SigningKey(int.from_bytes(_derive("bench", 32), "big") % (2 ** 255) or 7)
    vk = sk.public_key()
    msgs = [hashlib.sha256(f"m{i}".encode()).digest() * 4 for i in range(256)]   # 128 B, ~a PDU
    sigs = [sk.sign(m) for m in msgs]
    out["raw_sign"] = _timed(lambda i: sk.sign(msgs[i % 256]), a.n)
    out["raw_verify"] = _timed(lambda i: vk.verify(msgs[i % 256], sigs[i % 256]), a.n)
    out["signature_bytes"] = len(sigs[0])

    # 2. the engine's own paths ------------------------------------------------------------- #
    def _ctx(lbl: str) -> DeviceLinkageContext:
        return DeviceLinkageContext(0x0001, 0x0002, _derive(f"ls1:{lbl}", 16),
                                    _derive(f"ls2:{lbl}", 16))

    sec = SecurityLayer(_derive)
    # `rotate_period_s = 0` -- the InTAS bench's own setting -- gives ONE certificate per device,
    # which is what the measured runs paid for. The 20-cert row beside it is a rotating fleet.
    t0 = time.perf_counter()
    creds = sec.provision("veh_000", _ctx("veh_000"), [(0, 0, 0.0, 400.0)])
    out["provision_one_device_1cert_s"] = round(time.perf_counter() - t0, 6)
    out["certificates_per_device"] = len(creds.device.credentials)

    for n_certs in (1, 20):
        wins = [(j // 20, j % 20, j * 300.0, (j + 1) * 300.0) for j in range(n_certs)]
        n_prov = 40
        t0 = time.perf_counter()
        for k in range(1, n_prov + 1):
            lbl = f"v{n_certs}_{k:03d}"
            sec.provision(lbl, _ctx(lbl), wins)
        dt = time.perf_counter() - t0
        out[f"provision_{n_certs}cert"] = {
            "n_devices": n_prov, "certs_per_device": n_certs, "wall_s": round(dt, 6),
            "per_device_s": round(dt / n_prov, 6), "devices_per_s": round(n_prov / dt, 2)}
    out["provision"] = out["provision_1cert"]

    digest = creds.device.digests[0]
    payload = b"\x01" * 134
    out["layer_sign"] = _timed(
        lambda i: sec.sign("veh_000", digest, payload + bytes([i % 251]), 1.0 + i * 0.1), a.n // 3)
    # A GENUINELY cold verify: N DISTINCT signed messages, each verified EXACTLY ONCE, so no call
    # can hit the memo. Signing them is outside the timed region. (A first pass of this probe reused
    # 256 messages cyclically and measured 81 k verify/s -- three times the raw primitive -- which is
    # the cache being timed, not the curve.)
    n_cold = max(200, a.n // 3)
    cold = [sec.sign("veh_000", digest, payload + i.to_bytes(4, "big"), 5.0 + i * 1e-4)
            for i in range(n_cold)]
    t0 = time.perf_counter()
    for sb_i in cold:
        sec.verify(sb_i, 5.0)
    dt = time.perf_counter() - t0
    out["layer_verify_cold"] = {"n": n_cold, "wall_s": round(dt, 6),
                                "per_op_us": round(dt / n_cold * 1e6, 3),
                                "ops_per_s": round(n_cold / dt, 1)}
    sb = cold[0]
    out["layer_verify_cached"] = _timed(lambda i: sec.verify(sb, 5.0), a.n)
    out["verification_cache"] = sec.verifier.stats()

    # 3. the fleet-hour bill ---------------------------------------------------------------- #
    frames = a.fleet * a.cam_rate_hz * 3600.0 * a.hours
    sign_s = frames * out["layer_sign"]["per_op_us"] / 1e6
    ver_computed_s = frames * out["layer_verify_cold"]["per_op_us"] / 1e6
    ver_logical_s = frames * a.fanout * out["layer_verify_cold"]["per_op_us"] / 1e6
    prov_s = a.fleet * out["provision"]["per_device_s"]
    out["fleet_hour"] = {
        "fleet": a.fleet, "hours": a.hours, "cam_rate_hz": a.cam_rate_hz,
        "frames": int(frames), "fanout": a.fanout,
        "delivered_links": int(frames * a.fanout),
        "provisioning_s": round(prov_s, 1),
        "signing_s": round(sign_s, 1),
        "verification_s_computed_with_cache": round(ver_computed_s, 1),
        "verification_s_logical_no_cache": round(ver_logical_s, 1),
        "total_s_with_cache": round(prov_s + sign_s + ver_computed_s, 1),
        "total_s_no_cache": round(prov_s + sign_s + ver_logical_s, 1),
        "realtime_factor_with_cache": round((prov_s + sign_s + ver_computed_s)
                                            / (3600.0 * a.hours), 3),
        "cores_needed_for_realtime_no_cache": round(ver_logical_s / (3600.0 * a.hours), 2),
    }
    txt = json.dumps(out, indent=1, sort_keys=True)
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(txt + "\n")
    print(txt)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
