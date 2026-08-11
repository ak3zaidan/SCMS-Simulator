/*
 * SPDX-License-Identifier: Apache-2.0
 * In-JVM SCMS back-end for the MOSAIC layer (v4 — full entity model).
 *
 * Orchestrates the full Security Credential Management System, modelled as DISTINCT
 * entities with real trust boundaries (see org.scms.entities.Scms): DCM + ECA
 * (enrollment), RA + PCA + LA1 + LA2 (provisioning), MA + CRLG + CRL Store (enforcement),
 * LOP (privacy proxy), and the Root CA / ICA / PG / Electors trust anchors. This class is
 * the deterministic coordinator + dataset writer; the SCMS state lives inside the entities,
 * so the key property holds structurally: the MA never receives a true identity — it drives
 * resolution through PCA -> LA1/LA2 -> RA and only ever gets forward seeds + an opaque handle.
 *
 * Detection is distributed across the vehicle apps over MOSAIC's real AdHoc radio; this
 * back-end ingests reports (via the LOP), correlates (MA), resolves + revokes + issues a CRL,
 * and writes the trust-separated dataset (ma/*, ground_truth/*, manifest.json) once at JVM exit.
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
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Random;
import java.util.TreeMap;

import org.scms.attacks.AttackLib;
import org.scms.crypto.LinkageEngine;
import org.scms.entities.Scms;

public final class ScmsBackend {

    static final long MASTER_SEED = resolveSeed();
    static final int JMAX = envInt("SCMS_JMAX", 20);
    static final int REPORT_THRESHOLD_K = envInt("SCMS_REPORT_K", Integer.getInteger("scms.k", 3));
    static final int ATTACKER_PCT = envInt("SCMS_ATTACKER_PCT", Integer.getInteger("scms.attackerPct", 20));
    static final int FREEZE_AFTER_UPDATES = envInt("SCMS_FREEZE_UPDATES", 5);
    static final double CONST_OFFSET_M = envDouble("SCMS_OFFSET_M", 1500.0);
    static final double REPORT_PROB = envDouble("SCMS_REPORT_PROB", 0.9);
    static final double CRL_PROP_DELAY_S = envDouble("SCMS_CRL_DELAY", 2.0);
    static final String OUT_DIR = resolveOutDir();

    // Config is read from environment variables first (so the GUI can change it without a
    // recompile and with no JVM "Picked up ..." banner), then -D system properties, then defaults.
    private static long resolveSeed() {
        String e = System.getenv("SCMS_SEED");
        return (e != null && !e.isBlank()) ? Long.parseLong(e.trim()) : Long.getLong("scms.seed", 20260809L);
    }

    private static String resolveOutDir() {
        String e = System.getenv("SCMS_OUT_DIR");
        return (e != null && !e.isBlank()) ? e
                : System.getProperty("scms.outDir", "C:\\Users\\Administrator\\SCMS-Simulator\\datasets\\mosaic_poc");
    }

    private static int envInt(String name, int dflt) {
        String e = System.getenv(name);
        try {
            return (e != null && !e.isBlank()) ? Integer.parseInt(e.trim()) : dflt;
        } catch (NumberFormatException ex) {
            return dflt;
        }
    }

    private static double envDouble(String name, double dflt) {
        String e = System.getenv(name);
        try {
            return (e != null && !e.isBlank()) ? Double.parseDouble(e.trim()) : dflt;
        } catch (NumberFormatException ex) {
            return dflt;
        }
    }

    private static final ScmsBackend INSTANCE = new ScmsBackend();

    public static ScmsBackend instance() {
        return INSTANCE;
    }

    public static final class Cred {
        public final String certDigest;
        public final int iPeriod;
        public final int jIndex;
        public final String linkageValueHex;
        public final boolean attacker;
        public final String attackType;
        Cred(String certDigest, int iPeriod, int jIndex, String linkageValueHex, boolean attacker, String attackType) {
            this.certDigest = certDigest;
            this.iPeriod = iPeriod;
            this.jIndex = jIndex;
            this.linkageValueHex = linkageValueHex;
            this.attacker = attacker;
            this.attackType = attackType;
        }
    }

    private final Random rng = new Random(MASTER_SEED);
    private final Gson gson = new GsonBuilder().serializeSpecialFloatingPointValues().create();
    private final Scms scms = new Scms();
    private final List<String> attacksEnabled = AttackLib.enabled();
    private final AttackLib.Cfg attackCfg = new AttackLib.Cfg();

    private static final class Dev {
        String unitId, certDigest, requestHash, lvHex;
        int i, j;
        byte[] lv;
        LinkageEngine.Device link;
        boolean attacker;
        String attackType = "none";
        AttackLib.State atk = null;
        List<String> ghosts = null;   // Sybil: extra identities mapped back to this device
    }

    private final Map<String, Dev> devByUnit = new HashMap<>();
    private final Map<String, Dev> devByDigest = new HashMap<>();
    private final Map<String, Double> firstSeen = new HashMap<>();
    private final Map<String, Double> lastSeen = new HashMap<>();

    private final List<Map<String, Object>> maReports = new ArrayList<>();
    private final List<Map<String, Object>> maInvest = new ArrayList<>();
    private final List<Map<String, Object>> maCrl = new ArrayList<>();
    private final List<Map<String, Object>> maCertStatus = new ArrayList<>();
    private final List<Map<String, Object>> gtVeh = new ArrayList<>();
    private final List<Map<String, Object>> gtId = new ArrayList<>();
    private final List<Map<String, Object>> gtEnroll = new ArrayList<>();
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
        byte[] ls1 = Arrays.copyOf(sha("ls1|" + MASTER_SEED + "|" + unitId), 16);
        byte[] ls2 = Arrays.copyOf(sha("ls2|" + MASTER_SEED + "|" + unitId), 16);
        d.link = new LinkageEngine.Device(1, 2, ls1, ls2);
        d.i = 0;
        d.j = (sha("j|" + unitId)[0] & 0xff) % JMAX;
        d.lv = d.link.linkageValueFor(d.i, d.j);   // PCA computes lv = plv1 XOR plv2
        d.lvHex = hex(d.lv, d.lv.length);
        d.certDigest = hex(sha("cert|" + MASTER_SEED + "|" + unitId), 8);
        d.requestHash = hex(sha("req|" + MASTER_SEED + "|" + unitId), 8);
        d.attacker = ((sha("role|" + MASTER_SEED + "|" + unitId)[0] & 0xff) * 100 / 256) < ATTACKER_PCT;
        if (d.attacker) {
            d.attackType = AttackLib.assign(unitId, MASTER_SEED, attacksEnabled);
            d.atk = new AttackLib.State(MASTER_SEED * 1000003L + unitId.hashCode());
        }

        // SCMS provisioning across the distinct entities (trust boundaries preserved).
        scms.dcm.attest(unitId);
        String enrollmentId = scms.eca.issue(unitId);
        String laH1 = "lc1:" + unitId;
        String laH2 = "lc2:" + unitId;
        int laId1 = 0x0001;
        int laId2 = 0x0002;
        scms.la1.register(laH1, ls1, laId1);
        scms.la2.register(laH2, ls2, laId2);
        scms.pca.issue(d.certDigest, d.requestHash, d.i, d.j, laH1, laH2);
        scms.ra.bind(d.requestHash, enrollmentId);   // RA is the only request->identity mapping

        devByUnit.put(unitId, d);
        devByDigest.put(d.certDigest, d);
        if (d.attacker && "Sybil".equals(d.attackType)) {
            d.ghosts = new ArrayList<>();
            for (int k = 0; k < attackCfg.sybilGhosts; k++) {
                String g = hex(sha("ghost|" + MASTER_SEED + "|" + unitId + "|" + k), 8);
                d.ghosts.add(g);
                devByDigest.put(g, d);   // ghost identities resolve to the same attacker (for labeling)
                gtId.add(gtRow("true_vehicle_id", unitId, "pseudonym_cert_digest", g, "i_period", d.i));
            }
        }
        gtVeh.add(gtRow("true_vehicle_id", unitId, "is_attacker", d.attacker, "attacker_role", d.attackType));
        gtId.add(gtRow("true_vehicle_id", unitId, "pseudonym_cert_digest", d.certDigest, "i_period", d.i));
        gtEnroll.add(gtRow("true_vehicle_id", unitId, "enrollment_cert", enrollmentId,
                "device_type", scms.dcm.deviceType(unitId), "eca_id", scms.eca.id));
        if (d.attacker) {
            gtAtk.add(gtRow("attack_id", "atk_" + unitId, "true_vehicle_id", unitId, "attack_type", d.attackType));
        }
    }

    public synchronized Cred getCredential(String unitId) {
        register(unitId);
        Dev d = devByUnit.get(unitId);
        return new Cred(d.certDigest, d.i, d.j, d.lvHex, d.attacker, d.attackType);
    }

    /** Compute the claimed CAM (content + timing/flood/sybil flags) for one broadcast. */
    public synchronized AttackLib.Claim claim(String unitId, int sendCount, double x, double y,
                                              double speed, double heading, long tNs) {
        Dev d = devByUnit.get(unitId);
        if (d == null) {
            register(unitId);
            d = devByUnit.get(unitId);
        }
        if (!d.attacker) {
            AttackLib.Claim c = new AttackLib.Claim();
            c.x = x; c.y = y; c.speed = speed; c.heading = heading; c.genTimeNs = tNs;
            return c;
        }
        return AttackLib.compute(d.attackType, d.atk, sendCount, x, y, speed, heading, tNs, attackCfg);
    }

    /** Sybil ghost cert digests for an attacker (empty for everyone else). */
    public synchronized List<String> ghostDigests(String unitId) {
        Dev d = devByUnit.get(unitId);
        return (d != null && d.ghosts != null) ? d.ghosts : java.util.Collections.emptyList();
    }

    public synchronized void onCamSent(String certDigest, double t) {
        firstSeen.putIfAbsent(certDigest, t);
        lastSeen.put(certDigest, t);
    }

    public synchronized boolean isRevoked(String subjectDigest, double t) {
        return scms.crlStore.enforced(subjectDigest, t, CRL_PROP_DELAY_S);
    }

    // -------------------------------------------------- report ingestion (LOP -> RA -> MA)
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
        scms.lop.forward(rep.certDigest, subj.certDigest);   // LOP strips network identifiers
        reportCounter++;
        String rid = String.format("rpt_%05d", reportCounter);
        maReports.add(maRow("report_id", rid, "ingest_time", round3(t), "detection_time", round3(t),
                "reporter_cert_digest", reporterDigest, "subject_cert_digest", subjectDigest,
                "reason_codes", List.of(reasonCode),
                "detector_score", round3(score), "sig_valid", true, "cert_crl_status", "active"));
        gtRepLbl.add(gtRow("report_id", rid, "reporter_true_id", rep.unitId, "subject_true_id", subj.unitId,
                "report_correctness", subj.attacker ? "correct" : "false_positive"));
        int distinct = scms.ma.addReporter(subjectDigest, reporterDigest);
        if (!scms.crlStore.isRevoked(subj.certDigest) && distinct >= REPORT_THRESHOLD_K) {
            resolveAndRevoke(subj, t);
        }
    }

    /** MA investigation across entities: PCA -> LA1/LA2 (seeds) -> RA (blacklist) -> CRLG -> CRL Store. */
    private void resolveAndRevoke(Dev subj, double t) {
        caseCounter++;
        String caseId = String.format("case_%04d", caseCounter);
        Scms.Prov p = scms.pca.resolve(subj.certDigest);          // opaque record: no identity
        byte[] ls1i = scms.la1.seedAt(p.laHandle1, p.i);          // forward seeds from the two LAs
        byte[] ls2i = scms.la2.seedAt(p.laHandle2, p.i);
        int laId1 = scms.la1.laId(p.laHandle1);
        int laId2 = scms.la2.laId(p.laHandle2);
        scms.ra.blacklistByRequest(p.requestHash);               // enrollment identity stays inside the RA
        LinkageEngine.CrlEntry entry = scms.crlg.issue(p.i, laId1, laId2, ls1i, ls2i, JMAX);
        if (!entry.matches(subj.i, subj.j, subj.lv)) {
            throw new IllegalStateException("CRL entry failed to revoke its target device");
        }
        scms.crlStore.publish(subj.certDigest, t);
        int reporters = scms.ma.distinctReporters(subj.certDigest);
        maInvest.add(maRow("case_id", caseId, "opened_time", round3(t), "trigger", "report_threshold",
                "num_distinct_reporters", reporters, "linkage_result", "same", "identity_resolved", true,
                "revocation_decision", "revoke", "resolved_case_handle", hex(sha(caseId), 6)));
        maCrl.add(maRow("crl_id", String.format("crl_%04d", caseCounter), "issue_time", round3(t),
                "entry_type", "seed", "num_entries", scms.crlg.size()));
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
                Double rt = scms.crlStore.revocationTime(d.certDigest);
                maCertStatus.add(maRow("cert_digest", d.certDigest,
                        "first_seen", firstSeen.getOrDefault(d.certDigest, 0.0),
                        "last_seen", lastSeen.getOrDefault(d.certDigest, 0.0),
                        "issuing_pca", scms.pca.id, "crl_status", rt != null ? "revoked" : "active",
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
            digests.put("ground_truth/gt_enrollment.jsonl", writeJsonl(gt.resolve("gt_enrollment.jsonl"), sortBy(gtEnroll, "true_vehicle_id")));
            digests.put("ground_truth/gt_attacks.jsonl", writeJsonl(gt.resolve("gt_attacks.jsonl"), sortBy(gtAtk, "attack_id")));
            digests.put("ground_truth/gt_report_labels.jsonl", writeJsonl(gt.resolve("gt_report_labels.jsonl"), sortBy(gtRepLbl, "report_id")));
            digests.put("ground_truth/gt_linkage_revocation.jsonl", writeJsonl(gt.resolve("gt_linkage_revocation.jsonl"), sortBy(gtRev, "true_vehicle_id")));

            MessageDigest all = MessageDigest.getInstance("SHA-256");
            for (Map.Entry<String, String> en : digests.entrySet()) {
                all.update(en.getKey().getBytes(StandardCharsets.UTF_8));
                all.update(en.getValue().getBytes(StandardCharsets.UTF_8));
            }
            Map<String, Object> manifest = new LinkedHashMap<>();
            manifest.put("dataset_version", "0.3.0");
            manifest.put("generator", "scms_sim_ref (MOSAIC layer, full-entity back-end v4)");
            manifest.put("seed", MASTER_SEED);
            manifest.put("scms_entities", Scms.ENTITY_NAMES);
            Map<String, Object> cfg = new LinkedHashMap<>();
            cfg.put("reception", "MOSAIC AdHoc ITS-G5 CCH via SNS (range/delay)");
            cfg.put("report_threshold_k", REPORT_THRESHOLD_K);
            cfg.put("attacker_pct", ATTACKER_PCT);
            cfg.put("report_prob", REPORT_PROB);
            cfg.put("jmax", JMAX);
            cfg.put("attacks_enabled", attacksEnabled);
            manifest.put("config", cfg);
            Map<String, Object> counts = new LinkedHashMap<>();
            counts.put("vehicles", devByUnit.size());
            counts.put("reports", maReports.size());
            counts.put("investigations", maInvest.size());
            counts.put("revoked", scms.crlg.size());
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
                    + " revoked=" + scms.crlg.size() + ")");
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
