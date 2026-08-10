/*
 * SPDX-License-Identifier: Apache-2.0
 * In-JVM SCMS back-end for the MOSAIC layer (v2).
 *
 * A single-host MOSAIC run executes all application instances in one sequential JVM,
 * so a static singleton back-end is deterministic and safe. It composites the
 * RA/PCA/LA1/LA2/MA/CRLG roles for the simulation while preserving the key trust
 * property: MA-visible investigation records NEVER contain a true identity — only
 * pseudonym-certificate digests and an opaque case handle. True identities live only
 * in the ground-truth (ORACLE) plane.
 *
 * v2 change: message reception + detection are now DISTRIBUTED across the vehicle apps
 * over MOSAIC's real AdHoc radio; this back-end provisions credentials, defines the
 * attacker's claimed position, ingests reports via {@link #onDetection}, correlates,
 * resolves identity with the cross-validated {@link org.scms.crypto.LinkageEngine}
 * (CAMP SCP2), revokes, and issues/enforces a CRL. Outputs are written once via a JVM
 * shutdown hook:
 *   <outDir>/ma/*.jsonl (MA/PUBLIC — safe for features), <outDir>/ground_truth/*.jsonl
 *   (ORACLE — labels only), <outDir>/manifest.json (seed, config, per-file sha256, digest).
 */
package org.scms.backend;

import com.google.gson.GsonBuilder;
import com.google.gson.Gson;
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

    static final long MASTER_SEED = resolveSeed();
    static final int JMAX = 20;
    static final int REPORT_THRESHOLD_K = Integer.getInteger("scms.k", 3);
    static final int ATTACKER_PCT = Integer.getInteger("scms.attackerPct", 20);
    static final int FREEZE_AFTER_UPDATES = 5;
    static final double CONST_OFFSET_M = 1500.0;   // ConstPosOffset shift (caught by the ART detector)
    static final double REPORT_PROB = 0.9;
    static final double CRL_PROP_DELAY_S = 2.0;
    static final String OUT_DIR = resolveOutDir();

    // Config is read from environment variables first (no JVM "Picked up ..." banner on stderr),
    // then -D system properties, then a default. run.ps1 sets SCMS_OUT_DIR / SCMS_SEED.
    private static long resolveSeed() {
        String e = System.getenv("SCMS_SEED");
        return (e != null && !e.isBlank()) ? Long.parseLong(e.trim()) : Long.getLong("scms.seed", 20260809L);
    }

    private static String resolveOutDir() {
        String e = System.getenv("SCMS_OUT_DIR");
        return (e != null && !e.isBlank()) ? e
                : System.getProperty("scms.outDir", "C:\\Users\\Administrator\\SCMS-Simulator\\datasets\\mosaic_poc");
    }

    private static final ScmsBackend INSTANCE = new ScmsBackend();

    public static ScmsBackend instance() {
        return INSTANCE;
    }

    /** Credential handed to a vehicle app so it can build/sign its CAM. */
    public static final class Cred {
        public final String certDigest;
        public final int iPeriod;
        public final int jIndex;
        public final String linkageValueHex;
        public final boolean attacker;

        Cred(String certDigest, int iPeriod, int jIndex, String linkageValueHex, boolean attacker) {
            this.certDigest = certDigest;
            this.iPeriod = iPeriod;
            this.jIndex = jIndex;
            this.linkageValueHex = linkageValueHex;
            this.attacker = attacker;
        }
    }

    private final Random rng = new Random(MASTER_SEED);
    private final Gson gson = new GsonBuilder().serializeSpecialFloatingPointValues().create();

    private static final class Dev {
        String unitId, certDigest, requestHash, lvHex;
        int i, j;
        byte[] lv;
        LinkageEngine.Device link;
        boolean attacker;
        String attackType = "none";   // ConstPos | ConstPosOffset
        double[] frozen = null;
    }

    private final Map<String, Dev> devByUnit = new HashMap<>();
    private final Map<String, Dev> devByDigest = new HashMap<>();
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

    // -------------------------------------------------------------- provisioning
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
        d.lvHex = hex(d.lv, d.lv.length);
        d.certDigest = hex(sha("cert|" + MASTER_SEED + "|" + unitId), 8);
        d.requestHash = hex(sha("req|" + MASTER_SEED + "|" + unitId), 8);
        d.attacker = ((sha("role|" + MASTER_SEED + "|" + unitId)[0] & 0xff) * 100 / 256) < ATTACKER_PCT;
        if (d.attacker) {
            d.attackType = ((sha("atktype|" + unitId)[0] & 0xff) % 2 == 0) ? "ConstPos" : "ConstPosOffset";
        }
        devByUnit.put(unitId, d);
        devByDigest.put(d.certDigest, d);
        gtVeh.add(gtRow("true_vehicle_id", unitId, "is_attacker", d.attacker, "attacker_role", d.attackType));
        gtId.add(gtRow("true_vehicle_id", unitId, "pseudonym_cert_digest", d.certDigest, "i_period", d.i));
        if (d.attacker) {
            gtAtk.add(gtRow("attack_id", "atk_" + unitId, "true_vehicle_id", unitId, "attack_type", d.attackType));
        }
    }

    public synchronized Cred getCredential(String unitId) {
        register(unitId);
        Dev d = devByUnit.get(unitId);
        return new Cred(d.certDigest, d.i, d.j, d.lvHex, d.attacker);
    }

    /** Claimed position a vehicle broadcasts: attacker freezes (ConstPos); others tell the truth. */
    public synchronized double[] claimedPosition(String unitId, int updateCount, double x, double y) {
        Dev d = devByUnit.get(unitId);
        if (d == null) {
            register(unitId);
            d = devByUnit.get(unitId);
        }
        if (d.attacker && "ConstPosOffset".equals(d.attackType)) {
            return new double[] {x + CONST_OFFSET_M, y + CONST_OFFSET_M};   // claim a shifted position
        }
        if (d.attacker && "ConstPos".equals(d.attackType)) {
            if (updateCount == FREEZE_AFTER_UPDATES) {
                d.frozen = new double[] {x, y};
            }
            if (d.frozen != null) {
                return new double[] {d.frozen[0], d.frozen[1]};
            }
        }
        return new double[] {x, y};
    }

    public synchronized void onCamSent(String certDigest, double t) {
        firstSeen.putIfAbsent(certDigest, t);
        lastSeen.put(certDigest, t);
    }

    public synchronized boolean isRevoked(String subjectDigest, double t) {
        Double rt = revocationTime.get(subjectDigest);
        return rt != null && t >= rt + CRL_PROP_DELAY_S;
    }

    // -------------------------------------------------- report ingestion (MA)
    public synchronized void onDetection(String reporterDigest, String subjectDigest, double score, double t,
                                         String reasonCode) {
        if (rng.nextDouble() > REPORT_PROB) {
            return; // suppression / loss on the report channel
        }
        Dev subj = devByDigest.get(subjectDigest);
        Dev rep = devByDigest.get(reporterDigest);
        if (subj == null || rep == null) {
            return;
        }
        reportCounter++;
        String rid = String.format("rpt_%05d", reportCounter);
        maReports.add(maRow("report_id", rid, "ingest_time", round3(t), "detection_time", round3(t),
                "reporter_cert_digest", rep.certDigest, "subject_cert_digest", subj.certDigest,
                "reason_codes", List.of(reasonCode),
                "detector_score", round3(score), "sig_valid", true, "cert_crl_status", "active"));
        gtRepLbl.add(gtRow("report_id", rid, "reporter_true_id", rep.unitId, "subject_true_id", subj.unitId,
                "report_correctness", subj.attacker ? "correct" : "false_positive"));
        reportersBySubject.computeIfAbsent(subj.certDigest, k -> new HashSet<>()).add(rep.certDigest);
        if (!revocationTime.containsKey(subj.certDigest)
                && reportersBySubject.get(subj.certDigest).size() >= REPORT_THRESHOLD_K) {
            resolveAndRevoke(subj, t);
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
        maInvest.add(maRow("case_id", caseId, "opened_time", round3(t), "trigger", "report_threshold",
                "num_distinct_reporters", reporters, "linkage_result", "same", "identity_resolved", true,
                "revocation_decision", "revoke", "resolved_case_handle", hex(sha(caseId), 6)));
        maCrl.add(maRow("crl_id", String.format("crl_%04d", caseCounter), "issue_time", round3(t),
                "entry_type", "seed", "num_entries", crl.size()));
        gtRev.add(gtRow("true_vehicle_id", subj.unitId, "should_have_been_revoked", subj.attacker,
                "true_revocation_time", round3(t)));
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
            manifest.put("dataset_version", "0.2.0");
            manifest.put("generator", "scms_sim_ref (MOSAIC layer, in-JVM back-end v2, real AdHoc reception)");
            manifest.put("seed", MASTER_SEED);
            Map<String, Object> cfg = new LinkedHashMap<>();
            cfg.put("reception", "MOSAIC AdHoc ITS-G5 CCH via SNS (range/delay)");
            cfg.put("report_threshold_k", REPORT_THRESHOLD_K);
            cfg.put("attacker_pct", ATTACKER_PCT);
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
                    "cert", "IEEE 1609.2", "linkage", "CAMP SCP2", "messaging", "ETSI CAM over ITS-G5"));
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
