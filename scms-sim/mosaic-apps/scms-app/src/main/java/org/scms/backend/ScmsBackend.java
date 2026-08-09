/*
 * SPDX-License-Identifier: Apache-2.0
 * In-JVM SCMS back-end for the MOSAIC layer (v1).
 *
 * A single-host MOSAIC run executes all application instances in one, sequential
 * JVM, so a static singleton back-end is deterministic and safe. It composites the
 * RA/PCA/LA1/LA2/MA/CRLG roles for the simulation while preserving the key trust
 * property: the MA-visible investigation records NEVER contain a true identity —
 * only pseudonym-certificate digests and an opaque case handle. True identities
 * live solely in the ground-truth (ORACLE) plane.
 *
 * v1 models message reception + detection centrally, driven by REAL SUMO mobility.
 * Real ETSI AdHoc CAM reception (range/loss/latency via MOSAIC) is the next increment;
 * the app<->backend seam here is designed so that swap is localized.
 *
 * Revocation uses the cross-validated {@link org.scms.crypto.LinkageEngine} (CAMP SCP2).
 * Outputs (written once via a JVM shutdown hook, so they survive normal sim end):
 *   <outDir>/ma/*.jsonl            (MA / PUBLIC — safe for ML features)
 *   <outDir>/ground_truth/*.jsonl  (ORACLE — labels/eval only)
 *   <outDir>/manifest.json         (seed, config, per-file sha256, data digest)
 */
package org.scms.backend;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Random;
import java.util.Set;
import java.util.TreeMap;

import org.scms.crypto.LinkageEngine;

public final class ScmsBackend {

    // ---- configuration (overridable via -Dscms.*) ----
    static final long MASTER_SEED = Long.getLong("scms.seed", 20260809L);
    static final int JMAX = 20;
    static final int REPORT_THRESHOLD_K = Integer.getInteger("scms.k", 3);
    static final int ATTACKER_PCT = Integer.getInteger("scms.attackerPct", 20);
    static final int FREEZE_AFTER_UPDATES = 5;
    static final double COMM_RANGE_M = Double.parseDouble(System.getProperty("scms.range", "300"));
    static final double CONSISTENCY_THRESH_M = 5.0;
    static final double REPORT_PROB = 0.9;
    static final double CRL_PROP_DELAY_S = 2.0;
    static final String OUT_DIR = System.getProperty("scms.outDir",
            "C:\\Users\\Administrator\\SCMS-Simulator\\datasets\\mosaic_poc");

    private static final ScmsBackend INSTANCE = new ScmsBackend();

    public static ScmsBackend instance() {
        return INSTANCE;
    }

    private final Random rng = new Random(MASTER_SEED);
    private final Gson gson = new GsonBuilder().serializeSpecialFloatingPointValues().create();

    private static final class Dev {
        String unitId, certDigest, requestHash;
        int i, j;
        byte[] lv;
        LinkageEngine.Device link;
        boolean attacker;
        int updates = 0;
        double[] frozen = null;
    }

    private final Map<String, Dev> devByUnit = new HashMap<>();
    private final TreeMap<String, double[]> truePos = new TreeMap<>();     // sorted => deterministic reception order
    private final Map<String, double[]> lastClaimedPair = new HashMap<>(); // "rx|txDigest" -> {x,y,t}
    private final Map<String, Set<String>> reportersBySubject = new HashMap<>();
    private final Map<String, Double> revocationTime = new HashMap<>();
    private final Map<String, Double> firstSeen = new HashMap<>();
    private final Map<String, Double> lastSeen = new HashMap<>();
    private final List<LinkageEngine.CrlEntry> crl = new ArrayList<>();

    private final List<Map<String, Object>> maReports = new ArrayList<>();
    private final List<Map<String, Object>> maInvest = new ArrayList<>();
    private final List<Map<String, Object>> maCrl = new ArrayList<>();
    private final List<Map<String, Object>> maCertStatus = new ArrayList<>();
    private final List<Map<String, Object>> gtVeh = new ArrayList<>();
    private final List<Map<String, Object>> gtId = new ArrayList<>();
    private final List<Map<String, Object>> gtAtk = new ArrayList<>();
    private final List<Map<String, Object>> gtRepLbl = new ArrayList<>();
    private final List<Map<String, Object>> gtRev = new ArrayList<>();

    private int reportCounter = 0;
    private int caseCounter = 0;
    private boolean written = false;

    private ScmsBackend() {
        Runtime.getRuntime().addShutdownHook(new Thread(this::writeOutputs));
    }

    // ---------------------------------------------------------------- lifecycle
    public synchronized void register(String unitId) {
        if (devByUnit.containsKey(unitId)) {
            return;
        }
        Dev d = new Dev();
        d.unitId = unitId;
        d.link = new LinkageEngine.Device(1, 2,
                Arrays.copyOf(sha("ls1|" + MASTER_SEED + "|" + unitId), 16),
                Arrays.copyOf(sha("ls2|" + MASTER_SEED + "|" + unitId), 16));
        d.i = 0;
        d.j = (sha("j|" + unitId)[0] & 0xff) % JMAX;
        d.lv = d.link.linkageValueFor(d.i, d.j);
        d.certDigest = hex(sha("cert|" + MASTER_SEED + "|" + unitId), 8);
        d.requestHash = hex(sha("req|" + MASTER_SEED + "|" + unitId), 8);
        d.attacker = ((sha("role|" + MASTER_SEED + "|" + unitId)[0] & 0xff) * 100 / 256) < ATTACKER_PCT;
        devByUnit.put(unitId, d);
        gtVeh.add(gtRow("true_vehicle_id", unitId, "is_attacker", d.attacker,
                "attacker_role", d.attacker ? "ConstPos" : "none"));
        gtId.add(gtRow("true_vehicle_id", unitId, "pseudonym_cert_digest", d.certDigest, "i_period", d.i));
        if (d.attacker) {
            gtAtk.add(gtRow("attack_id", "atk_" + unitId, "true_vehicle_id", unitId, "attack_type", "ConstPos"));
        }
    }

    /** Called from the vehicle app on every SUMO-driven update. */
    public synchronized void onVehicleUpdate(String unitId, long simTimeNs, double x, double y,
                                             double speed, double heading) {
        double t = simTimeNs / 1e9;
        Dev tx = devByUnit.get(unitId);
        if (tx == null) {
            register(unitId);
            tx = devByUnit.get(unitId);
        }
        tx.updates++;
        truePos.put(unitId, new double[] {x, y});
        firstSeen.putIfAbsent(tx.certDigest, t);
        lastSeen.put(tx.certDigest, t);

        // Claimed state: attacker freezes its position (ConstPos) while still claiming its speed.
        double cx = x, cy = y, cs = speed;
        if (tx.attacker) {
            if (tx.updates == FREEZE_AFTER_UPDATES) {
                tx.frozen = new double[] {x, y};
            }
            if (tx.frozen != null) {
                cx = tx.frozen[0];
                cy = tx.frozen[1];
            }
        }

        for (Map.Entry<String, double[]> e : truePos.entrySet()) {
            String rxUnit = e.getKey();
            if (rxUnit.equals(unitId)) {
                continue;
            }
            double[] rp = e.getValue();
            if (Math.hypot(rp[0] - x, rp[1] - y) > COMM_RANGE_M) {
                continue; // out of range: no reception
            }
            Double rt = revocationTime.get(tx.certDigest);
            if (rt != null && t >= rt + CRL_PROP_DELAY_S) {
                continue; // ENFORCEMENT: revoked cert dropped after CRL propagation
            }
            String key = rxUnit + "|" + tx.certDigest;
            double[] prev = lastClaimedPair.get(key);
            lastClaimedPair.put(key, new double[] {cx, cy, t});
            if (prev == null) {
                continue;
            }
            double moved = Math.hypot(cx - prev[0], cy - prev[1]);
            double inconsistency = Math.abs(moved - cs * (t - prev[2]));
            if (inconsistency <= CONSISTENCY_THRESH_M) {
                continue;
            }
            if (rng.nextDouble() > REPORT_PROB) {
                continue; // suppression / loss
            }
            Dev rx = devByUnit.get(rxUnit);
            if (rx == null) {
                continue;
            }
            reportCounter++;
            String rid = String.format("rpt_%05d", reportCounter);
            maReports.add(maRow("report_id", rid, "ingest_time", t, "detection_time", t,
                    "reporter_cert_digest", rx.certDigest, "subject_cert_digest", tx.certDigest,
                    "reason_codes", List.of("positionSpeedConsistency"),
                    "detector_score", round3(inconsistency), "sig_valid", true, "cert_crl_status", "active"));
            gtRepLbl.add(gtRow("report_id", rid, "reporter_true_id", rxUnit, "subject_true_id", unitId,
                    "report_correctness", tx.attacker ? "correct" : "false_positive"));
            reportersBySubject.computeIfAbsent(tx.certDigest, k -> new HashSet<>()).add(rx.certDigest);

            if (!revocationTime.containsKey(tx.certDigest)
                    && reportersBySubject.get(tx.certDigest).size() >= REPORT_THRESHOLD_K) {
                resolveAndRevoke(tx, t);
            }
        }
    }

    /** MA investigation: real two-LA linkage resolution + revocation. MA never learns the true id. */
    private void resolveAndRevoke(Dev subj, double t) {
        caseCounter++;
        String caseId = String.format("case_%04d", caseCounter);
        LinkageEngine.CrlEntry entry = LinkageEngine.CrlEntry.fromDevice(subj.link, subj.i, JMAX);
        if (!entry.matches(subj.i, subj.j, subj.lv)) {
            throw new IllegalStateException("CRL entry failed to revoke its target device");
        }
        crl.add(entry);
        revocationTime.put(subj.certDigest, t);
        int reporters = reportersBySubject.get(subj.certDigest).size();
        maInvest.add(maRow("case_id", caseId, "opened_time", t, "trigger", "report_threshold",
                "num_distinct_reporters", reporters, "linkage_result", "same", "identity_resolved", true,
                "revocation_decision", "revoke", "resolved_case_handle", hex(sha(caseId), 6)));
        maCrl.add(maRow("crl_id", String.format("crl_%04d", caseCounter), "issue_time", t,
                "entry_type", "seed", "num_entries", crl.size()));
        gtRev.add(gtRow("true_vehicle_id", subj.unitId, "should_have_been_revoked", subj.attacker,
                "true_revocation_time", t));
    }

    // ------------------------------------------------------------------- output
    private synchronized void writeOutputs() {
        if (written) {
            return;
        }
        written = true;
        try {
            for (Dev d : devByUnit.values()) {
                Double rt = revocationTime.get(d.certDigest);
                maCertStatus.add(maRow("cert_digest", d.certDigest,
                        "first_seen", firstSeen.getOrDefault(d.certDigest, 0.0),
                        "last_seen", lastSeen.getOrDefault(d.certDigest, 0.0),
                        "issuing_pca", "PCA-1", "crl_status", rt != null ? "revoked" : "active",
                        "revocation_time", rt));
            }
            Path ma = Paths.get(OUT_DIR, "ma");
            Path gt = Paths.get(OUT_DIR, "ground_truth");
            Files.createDirectories(ma);
            Files.createDirectories(gt);
            Map<String, String> digests = new TreeMap<>();
            digests.put("ma/ma_reports.jsonl", writeJsonl(ma.resolve("ma_reports.jsonl"), sortBy(maReports, "report_id")));
            digests.put("ma/ma_investigations.jsonl", writeJsonl(ma.resolve("ma_investigations.jsonl"), sortBy(maInvest, "case_id")));
            digests.put("ma/ma_crl_events.jsonl", writeJsonl(ma.resolve("ma_crl_events.jsonl"), sortBy(maCrl, "crl_id")));
            digests.put("ma/ma_cert_status.jsonl", writeJsonl(ma.resolve("ma_cert_status.jsonl"), sortBy(maCertStatus, "cert_digest")));
            digests.put("ground_truth/gt_vehicle.jsonl", writeJsonl(gt.resolve("gt_vehicle.jsonl"), sortBy(gtVeh, "true_vehicle_id")));
            digests.put("ground_truth/gt_identity_map.jsonl", writeJsonl(gt.resolve("gt_identity_map.jsonl"), sortBy(gtId, "pseudonym_cert_digest")));
            digests.put("ground_truth/gt_attacks.jsonl", writeJsonl(gt.resolve("gt_attacks.jsonl"), sortBy(gtAtk, "attack_id")));
            digests.put("ground_truth/gt_report_labels.jsonl", writeJsonl(gt.resolve("gt_report_labels.jsonl"), sortBy(gtRepLbl, "report_id")));
            digests.put("ground_truth/gt_linkage_revocation.jsonl", writeJsonl(gt.resolve("gt_linkage_revocation.jsonl"), sortBy(gtRev, "true_vehicle_id")));

            MessageDigest all = MessageDigest.getInstance("SHA-256");
            for (Map.Entry<String, String> en : digests.entrySet()) {
                all.update(en.getKey().getBytes(StandardCharsets.UTF_8));
                all.update(en.getValue().getBytes(StandardCharsets.UTF_8));
            }
            Map<String, Object> manifest = new LinkedHashMap<>();
            manifest.put("dataset_version", "0.1.0");
            manifest.put("generator", "scms_sim_ref (MOSAIC layer, in-JVM back-end v1)");
            manifest.put("seed", MASTER_SEED);
            Map<String, Object> cfg = new LinkedHashMap<>();
            cfg.put("report_threshold_k", REPORT_THRESHOLD_K);
            cfg.put("attacker_pct", ATTACKER_PCT);
            cfg.put("comm_range_m", COMM_RANGE_M);
            cfg.put("consistency_threshold_m", CONSISTENCY_THRESH_M);
            cfg.put("report_prob", REPORT_PROB);
            cfg.put("jmax", JMAX);
            manifest.put("config", cfg);
            Map<String, Object> counts = new LinkedHashMap<>();
            counts.put("vehicles", devByUnit.size());
            counts.put("reports", maReports.size());
            counts.put("investigations", maInvest.size());
            counts.put("revoked", crl.size());
            manifest.put("counts", counts);
            manifest.put("data_digest_sha256", hex(all.digest(), 32));
            manifest.put("outputs", digests);
            manifest.put("standards_profile", Map.of("report", "ETSI TS 103 759 (shape)",
                    "cert", "IEEE 1609.2", "linkage", "CAMP SCP2"));
            Files.write(Paths.get(OUT_DIR, "manifest.json"),
                    (new GsonBuilder().setPrettyPrinting().create().toJson(manifest) + "\n")
                            .getBytes(StandardCharsets.UTF_8));
            System.out.println("[ScmsBackend] wrote dataset to " + OUT_DIR
                    + " (vehicles=" + devByUnit.size() + " reports=" + maReports.size()
                    + " revoked=" + crl.size() + ")");
        } catch (Exception ex) {
            ex.printStackTrace();
        }
    }

    private String writeJsonl(Path path, List<Map<String, Object>> rows) throws IOException {
        StringBuilder sb = new StringBuilder();
        for (Map<String, Object> r : rows) {
            sb.append(gson.toJson(r)).append('\n');
        }
        byte[] bytes = sb.toString().getBytes(StandardCharsets.UTF_8);
        Files.write(path, bytes);
        return sha256Hex(bytes);
    }

    // ------------------------------------------------------------------ helpers
    private static Map<String, Object> map(String vis, Object[] kv) {
        LinkedHashMap<String, Object> m = new LinkedHashMap<>();
        m.put("_visibility", vis);
        for (int i = 0; i < kv.length; i += 2) {
            m.put((String) kv[i], kv[i + 1]);
        }
        return m;
    }

    private static Map<String, Object> maRow(Object... kv) {
        return map("MA", kv);
    }

    private static Map<String, Object> gtRow(Object... kv) {
        return map("ORACLE", kv);
    }

    private static List<Map<String, Object>> sortBy(List<Map<String, Object>> rows, String key) {
        List<Map<String, Object>> copy = new ArrayList<>(rows);
        copy.sort(Comparator.comparing(m -> String.valueOf(m.get(key))));
        return copy;
    }

    private static double round3(double v) {
        return Math.round(v * 1000.0) / 1000.0;
    }

    private static byte[] sha(String s) {
        try {
            return MessageDigest.getInstance("SHA-256").digest(s.getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) {
            throw new IllegalStateException(e);
        }
    }

    private static String sha256Hex(byte[] b) {
        try {
            return hex(MessageDigest.getInstance("SHA-256").digest(b), 32);
        } catch (Exception e) {
            throw new IllegalStateException(e);
        }
    }

    private static String hex(byte[] b, int n) {
        StringBuilder sb = new StringBuilder(n * 2);
        for (int k = 0; k < n; k++) {
            sb.append(String.format("%02x", b[k] & 0xff));
        }
        return sb.toString();
    }
}
