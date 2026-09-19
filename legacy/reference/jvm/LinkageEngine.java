/*
 * SPDX-License-Identifier: Apache-2.0
 * SCMS linkage-value engine (Java port of src/scms_sim_ref/scms_core/linkage.py).
 * Faithful to CAMP SCP2 / Brecht et al. Cross-validated byte-for-byte against the
 * Python reference (see main() self-test vectors). Pure JDK crypto — no MOSAIC deps.
 */
package org.scms.crypto;

import java.security.MessageDigest;
import java.util.Arrays;
import javax.crypto.Cipher;
import javax.crypto.spec.SecretKeySpec;

public final class LinkageEngine {

    public static final int LA_ID_BYTES = 2;
    public static final int LS_BYTES = 16;
    public static final int PLV_BYTES = 9;

    private LinkageEngine() {
    }

    private static byte[] laIdBytes(int laId) {
        if (laId < 0 || laId >= (1 << (8 * LA_ID_BYTES))) {
            throw new IllegalArgumentException("la_id must be 16-bit: " + laId);
        }
        return new byte[] {(byte) (laId >>> 8), (byte) laId};
    }

    /** ls_x(i) from ls_x(i-1): trunc128( SHA-256( la_id || ls || 0^112 ) ). */
    public static byte[] linkageSeedNext(int laId, byte[] lsPrev) {
        if (lsPrev.length != LS_BYTES) {
            throw new IllegalArgumentException("linkage seed must be 16 bytes");
        }
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-256");
            md.update(laIdBytes(laId));
            md.update(lsPrev);
            md.update(new byte[32 - LA_ID_BYTES - LS_BYTES]); // 0^112
            return Arrays.copyOf(md.digest(), LS_BYTES);
        } catch (Exception e) {
            throw new IllegalStateException(e);
        }
    }

    /** ls_x(i): hash the initial seed forward i times. */
    public static byte[] linkageSeedAt(int laId, byte[] ls0, int i) {
        if (i < 0) {
            throw new IllegalArgumentException("period index i must be >= 0");
        }
        byte[] ls = ls0;
        for (int k = 0; k < i; k++) {
            ls = linkageSeedNext(laId, ls);
        }
        return ls;
    }

    /** plv_x(i,j): AES-128 Davies-Meyer over (la_id || j || 0^80), truncated to 72 bits. */
    public static byte[] preLinkageValue(int laId, byte[] lsI, int j) {
        if (lsI.length != LS_BYTES) {
            throw new IllegalArgumentException("linkage seed must be 16 bytes");
        }
        byte[] block = new byte[16];
        byte[] la = laIdBytes(laId);
        block[0] = la[0];
        block[1] = la[1];
        block[2] = (byte) (j >>> 24);
        block[3] = (byte) (j >>> 16);
        block[4] = (byte) (j >>> 8);
        block[5] = (byte) j;
        // bytes 6..15 remain zero (0^80)
        try {
            Cipher c = Cipher.getInstance("AES/ECB/NoPadding");
            c.init(Cipher.ENCRYPT_MODE, new SecretKeySpec(lsI, "AES"));
            byte[] enc = c.doFinal(block);
            byte[] dm = new byte[16];
            for (int k = 0; k < 16; k++) {
                dm[k] = (byte) (enc[k] ^ block[k]);
            }
            return Arrays.copyOf(dm, PLV_BYTES);
        } catch (Exception e) {
            throw new IllegalStateException(e);
        }
    }

    /** lv(i,j) = plv1 XOR plv2. In the real system only the PCA computes this. */
    public static byte[] linkageValue(byte[] plv1, byte[] plv2) {
        byte[] out = new byte[PLV_BYTES];
        for (int k = 0; k < PLV_BYTES; k++) {
            out[k] = (byte) (plv1[k] ^ plv2[k]);
        }
        return out;
    }

    /** Per-device linkage material (colocated only for the reference engine/oracle). */
    public static final class Device {
        public final int laId1;
        public final int laId2;
        public final byte[] ls1Zero;
        public final byte[] ls2Zero;

        public Device(int laId1, int laId2, byte[] ls1Zero, byte[] ls2Zero) {
            this.laId1 = laId1;
            this.laId2 = laId2;
            this.ls1Zero = ls1Zero;
            this.ls2Zero = ls2Zero;
        }

        public byte[] linkageValueFor(int i, int j) {
            byte[] plv1 = preLinkageValue(laId1, linkageSeedAt(laId1, ls1Zero, i), j);
            byte[] plv2 = preLinkageValue(laId2, linkageSeedAt(laId2, ls2Zero, i), j);
            return linkageValue(plv1, plv2);
        }
    }

    /** A CRL entry that revokes one device from period i forward (forward-only privacy). */
    public static final class CrlEntry {
        public final int i;
        public final int laId1;
        public final int laId2;
        public final byte[] ls1I;
        public final byte[] ls2I;
        public final int jmax;

        public CrlEntry(int i, int laId1, int laId2, byte[] ls1I, byte[] ls2I, int jmax) {
            this.i = i;
            this.laId1 = laId1;
            this.laId2 = laId2;
            this.ls1I = ls1I;
            this.ls2I = ls2I;
            this.jmax = jmax;
        }

        public static CrlEntry fromDevice(Device d, int i, int jmax) {
            return new CrlEntry(i, d.laId1, d.laId2,
                    linkageSeedAt(d.laId1, d.ls1Zero, i), linkageSeedAt(d.laId2, d.ls2Zero, i), jmax);
        }

        public boolean matches(int certI, int certJ, byte[] certLv) {
            if (certI < i || certJ < 0 || certJ >= jmax) {
                return false;
            }
            byte[] ls1 = linkageSeedAt(laId1, ls1I, certI - i);
            byte[] ls2 = linkageSeedAt(laId2, ls2I, certI - i);
            byte[] lv = linkageValue(preLinkageValue(laId1, ls1, certJ), preLinkageValue(laId2, ls2, certJ));
            return Arrays.equals(lv, certLv);
        }
    }

    // ------------------------------------------------------------------ helpers
    public static String hex(byte[] b) {
        StringBuilder sb = new StringBuilder(b.length * 2);
        for (byte x : b) {
            sb.append(Character.forDigit((x >> 4) & 0xF, 16)).append(Character.forDigit(x & 0xF, 16));
        }
        return sb.toString();
    }

    public static byte[] fromHex(String s) {
        byte[] out = new byte[s.length() / 2];
        for (int i = 0; i < out.length; i++) {
            out[i] = (byte) Integer.parseInt(s.substring(2 * i, 2 * i + 2), 16);
        }
        return out;
    }

    public static byte[] repeat(int value, int n) {
        byte[] b = new byte[n];
        Arrays.fill(b, (byte) value);
        return b;
    }

    // ---------------------------------------------- self-test vs Python reference
    public static void main(String[] args) {
        int fails = 0;
        Device dev = new Device(1, 2, repeat(1, 16), repeat(2, 16));

        fails += check("ls1_5", hex(linkageSeedAt(1, repeat(1, 16), 5)), "0c482b59dd24d31a9f657f7facf71678");
        fails += check("plv1_5_3", hex(preLinkageValue(1, linkageSeedAt(1, repeat(1, 16), 5), 3)), "e863c6e955e034483c");
        fails += check("plv2_5_3", hex(preLinkageValue(2, linkageSeedAt(2, repeat(2, 16), 5), 3)), "1fb1b4a3a76b7ccd9c");
        fails += check("lv_5_3", hex(dev.linkageValueFor(5, 3)), "f7d2724af28b4885a0");
        fails += check("lv_10_0", hex(dev.linkageValueFor(10, 0)), "8d30464422b50365dd");

        CrlEntry e10 = CrlEntry.fromDevice(dev, 10, 20);
        fails += check("ls1_10", hex(e10.ls1I), "8080aac3e5a18e17cc8a8a61e889dc53");
        fails += check("ls2_10", hex(e10.ls2I), "0cb50072150df55017e12b09aa5e03f0");
        fails += check("match_10_0", String.valueOf(e10.matches(10, 0, dev.linkageValueFor(10, 0))), "true");
        fails += check("match_12_0", String.valueOf(e10.matches(12, 0, dev.linkageValueFor(12, 0))), "true");
        fails += check("match_9_0", String.valueOf(e10.matches(9, 0, dev.linkageValueFor(9, 0))), "false");

        if (fails == 0) {
            System.out.println("LinkageEngine self-test: ALL VECTORS MATCH the Python reference.");
        } else {
            System.out.println("LinkageEngine self-test: " + fails + " MISMATCH(ES).");
            System.exit(1);
        }
    }

    private static int check(String name, String got, String expected) {
        boolean ok = got.equals(expected);
        System.out.println((ok ? "  ok  " : " FAIL ") + name + " = " + got + (ok ? "" : " (expected " + expected + ")"));
        return ok ? 0 : 1;
    }
}
