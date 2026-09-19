"""Extract concrete acceptance vectors from the legacy Python SCMS core.

Every value the legacy tests assert on is recomputed here and printed as JSON.
Where a legacy test uses os.urandom, a fixed seed is substituted and pinned.
"""
import json, sys, hashlib
sys.path.insert(0, "/Users/ahmedzaidan/Developer/SCMS-Simulator/legacy")

from cryptography.hazmat.primitives.asymmetric import ec as cec
from scms_sim_ref.scms_core import butterfly as bf, ec, linkage as lk
from scms_sim_ref.scms_core.linkage import CrlLinkageEntry, DeviceLinkageContext

def h(n):  # 32-byte big-endian hex of an int
    return format(n, "064x")

def pt(p):
    return None if p is None else {"x": h(p[0]), "y": h(p[1])}

def lib_pub(d):
    k = cec.derive_private_key(d, cec.SECP256R1())
    n = k.public_key().public_numbers()
    return (n.x, n.y)

out = {}

# ---------- ec.py ----------
out["curve"] = {"p": h(ec.P), "a": h(ec.A), "b": h(ec.B), "n": h(ec.N), "g": pt(ec.G)}
out["scalar_mult_vs_library"] = []
for d in (1, 2, 3, 7, 12345, 2**128 + 1, ec.N - 1):
    mine, lib = ec.scalar_mult(d), lib_pub(d)
    assert mine == lib, d
    out["scalar_mult_vs_library"].append({"d": h(d), "point": pt(mine)})
d1, d2 = 111111, 987654321
out["homomorphism"] = {
    "d1": h(d1), "d2": h(d2),
    "sum": h((d1 + d2) % ec.N),
    "lhs": pt(ec.add(ec.scalar_mult(d1), ec.scalar_mult(d2))),
    "rhs": pt(ec.scalar_mult((d1 + d2) % ec.N)),
    "lib": pt(lib_pub((d1 + d2) % ec.N)),
}

# ---------- butterfly ----------
CK = bytes(range(16))
out["f1"] = [{"i": i, "j": j, "v": h(bf.f1(CK, i, j))} for i in range(3) for j in range(5)]
out["f2"] = [{"i": i, "j": j, "v": h(bf.f2(CK, i, j))} for i in range(3) for j in range(5)]
out["f_key_hex"] = CK.hex()

SEED = bytes(range(64))
cat = bf.new_caterpillar(SEED)
out["caterpillar"] = {
    "seed": SEED.hex(), "a": h(cat.a), "p": h(cat.p),
    "ck": cat.ck.hex(), "ek": cat.ek.hex(),
    "A": pt(cat.A), "P": pt(cat.P),
}
rows = []
for i in range(2):
    for j in range(4):
        B, Q = bf.ra_cocoon_keys(cat.A, cat.P, cat.ck, cat.ek, i, j)
        c = 0x1234567890ABCDEF + i * 7 + j
        certified, C = bf.pca_certify_explicit(B, c)
        d_priv = bf.device_signing_private(cat, i, j, c)
        q_priv = bf.device_encryption_private(cat, i, j)
        assert ec.scalar_mult(d_priv) == certified
        assert ec.scalar_mult(q_priv) == Q
        assert B != cat.A and Q != cat.P
        rows.append({"i": i, "j": j, "B": pt(B), "Q": pt(Q), "c": h(c),
                     "certified": pt(certified), "C": pt(C),
                     "d_priv": h(d_priv), "q_priv": h(q_priv)})
out["butterfly_identity"] = rows

SEED9 = bytes([9] * 64)
cat9 = bf.new_caterpillar(SEED9)
B9, Q9 = bf.ra_cocoon_keys(cat9.A, cat9.P, cat9.ck, cat9.ek, 0, 0)
cert9, C9 = bf.pca_certify_explicit(B9, 424242)
out["ra_cannot_predict"] = {
    "seed": SEED9.hex(), "a": h(cat9.a), "p": h(cat9.p),
    "ck": cat9.ck.hex(), "ek": cat9.ek.hex(),
    "A": pt(cat9.A), "P": pt(cat9.P),
    "B": pt(B9), "Q": pt(Q9), "c": h(424242),
    "certified": pt(cert9), "C": pt(C9),
    "equal": B9 == cert9,
}

# ---------- linkage ----------
LS1, LS2 = b"\x01" * 16, b"\x02" * 16
LA1, LA2 = 0x0001, 0x0002
dev = DeviceLinkageContext(la_id1=LA1, la_id2=LA2, ls1_0=LS1, ls2_0=LS2)
out["linkage_widths"] = {"LA_ID_BYTES": lk.LA_ID_BYTES, "LS_BYTES": lk.LS_BYTES,
                         "J_BYTES": lk.J_BYTES, "PLV_BYTES": lk.PLV_BYTES}
out["device"] = {"la_id1": LA1, "la_id2": LA2, "ls1_0": LS1.hex(), "ls2_0": LS2.hex()}
out["seed_chain_la1"] = [lk.linkage_seed_at(LA1, LS1, i).hex() for i in range(12)]
out["seed_chain_la2"] = [lk.linkage_seed_at(LA2, LS2, i).hex() for i in range(12)]
out["plv1_ls0_j0"] = lk.pre_linkage_value(LA1, LS1, 0).hex()
out["plv_grid"] = [
    {"i": i, "j": j,
     "plv1": lk.pre_linkage_value(LA1, lk.linkage_seed_at(LA1, LS1, i), j).hex(),
     "plv2": lk.pre_linkage_value(LA2, lk.linkage_seed_at(LA2, LS2, i), j).hex(),
     "lv": dev.linkage_value_for(i, j).hex()}
    for i in range(3) for j in range(5)
]
out["lv_5_3"] = dev.linkage_value_for(5, 3).hex()
out["lv_7_2"] = dev.linkage_value_for(7, 2).hex()
out["plv_5_3"] = {
    "plv1": lk.pre_linkage_value(LA1, lk.linkage_seed_at(LA1, LS1, 5), 3).hex(),
    "plv2": lk.pre_linkage_value(LA2, lk.linkage_seed_at(LA2, LS2, 5), 3).hex(),
}

REVOKE_I = 10
entry = CrlLinkageEntry.from_device(dev, i=REVOKE_I, jmax=20)
out["crl_entry"] = {"i": entry.i, "la_id1": entry.la_id1, "la_id2": entry.la_id2,
                    "ls1_i": entry.ls1_i.hex(), "ls2_i": entry.ls2_i.hex(),
                    "jmax": entry.jmax}
matches = []
for cert_i in (0, 1, 5, 9, REVOKE_I, REVOKE_I + 1, REVOKE_I + 5):
    for j in (0, 7, 19, 20):
        lv = dev.linkage_value_for(cert_i, j)
        matches.append({"cert_i": cert_i, "cert_j": j, "lv": lv.hex(),
                        "matches": entry.matches(cert_i, j, lv)})
out["crl_matches"] = matches
# wrong-lv rejection
out["crl_wrong_lv"] = entry.matches(REVOKE_I, 0, bytes(9))

# other device (os.urandom replaced by pinned seeds)
OTHER1 = hashlib.sha256(b"other-la1").digest()[:16]
OTHER2 = hashlib.sha256(b"other-la2").digest()[:16]
other = DeviceLinkageContext(la_id1=LA1, la_id2=LA2, ls1_0=OTHER1, ls2_0=OTHER2)
entry4 = CrlLinkageEntry.from_device(dev, i=4)
out["other_device"] = {"ls1_0": OTHER1.hex(), "ls2_0": OTHER2.hex()}
out["crl_other_device"] = {
    "entry_i": 4, "ls1_i": entry4.ls1_i.hex(), "ls2_i": entry4.ls2_i.hex(),
    "jmax": entry4.jmax,
    "rows": [{"cert_i": ci, "cert_j": j, "lv": other.linkage_value_for(ci, j).hex(),
              "matches": entry4.matches(ci, j, other.linkage_value_for(ci, j))}
             for ci in range(4, 9) for j in range(3)],
}
entry2 = CrlLinkageEntry.from_device(dev, i=2)
out["crl_contains"] = {
    "entry_i": 2, "ls1_i": entry2.ls1_i.hex(), "ls2_i": entry2.ls2_i.hex(),
    "victim_lv_3_1": dev.linkage_value_for(3, 1).hex(),
    "victim_hit": lk.crl_contains([entry2], 3, 1, dev.linkage_value_for(3, 1)),
    "other_lv_3_1": other.linkage_value_for(3, 1).hex(),
    "other_hit": lk.crl_contains([entry2], 3, 1, other.linkage_value_for(3, 1)),
}
entry0 = CrlLinkageEntry.from_device(dev, i=0, jmax=20)
out["out_of_range_j"] = {
    "entry_i": 0, "ls1_i": entry0.ls1_i.hex(), "ls2_i": entry0.ls2_i.hex(),
    "lv_0_0": dev.linkage_value_for(0, 0).hex(),
    "matches_j20": entry0.matches(0, 20, dev.linkage_value_for(0, 0)),
}
# distinctness counts the legacy tests assert
out["distinct_counts"] = {
    "f1_15": len({bf.f1(CK, i, j) for i in range(3) for j in range(5)}),
    "lv_15": len({dev.linkage_value_for(i, j) for i in range(3) for j in range(5)}),
}
json.dump(out, sys.stdout, indent=1)
