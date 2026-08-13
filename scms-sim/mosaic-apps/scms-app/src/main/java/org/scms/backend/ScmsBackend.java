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
    static final int JMAX = Math.max(1, envInt("SCMS_JMAX", 20));   // >=1: avoids modulo-by-zero
    static final int REPORT_THRESHOLD_K = envInt("SCMS_REPORT_K", Integer.getInteger("scms.k", 3));
    static final int ATTACKER_PCT = envInt("SCMS_ATTACKER_PCT", Integer.getInteger("scms.attackerPct", 20));
    static final int FREEZE_AFTER_UPDATES = envInt("SCMS_FREEZE_UPDATES", 5);
    static final double CONST_OFFSET_M = envDouble("SCMS_OFFSET_M", 1500.0);
    static final double REPORT_PROB = envDouble("SCMS_REPORT_PROB", 0.9);
    static final double CRL_PROP_DELAY_S = envDouble("SCMS_CRL_DELAY", 2.0);
    static final double LIVE_INTERVAL_S = envDouble("SCMS_LIVE_INTERVAL", 1.0);
    // On-board sensor error (realistic GNSS/odometry): a temporally-correlated bias (OU) plus
    // per-sample white noise, so honest vehicles broadcast MEASURED, not perfect, state.
    static final double GPS_SIGMA_M = envDouble("SCMS_GPS_SIGMA", 1.5);   // white position noise sd (m)
    static final double GPS_BIAS_M = envDouble("SCMS_GPS_BIAS", 3.0);     // correlated bias sd (m)
    static final double GPS_THETA = envDouble("SCMS_GPS_THETA", 0.1);     // bias mean-reversion (1/s)
    static final double SPEED_SIGMA_MS = envDouble("SCMS_SPEED_SIGMA", 0.5);
    static final double HEADING_SIGMA_DEG = envDouble("SCMS_HEADING_SIGMA", 2.0);
    // Rare heavy-tailed GNSS outliers (multipath / urban canyon): occasional large position
    // spikes, the realistic source of transient false detections. Rate is per-SECOND so it is
    // independent of the CAM rate; a per-CAM probability is derived as rate*dt.
    static final double GPS_OUTLIER_RATE = envDouble("SCMS_GPS_OUTLIER_RATE", 0.008);
    static final double GPS_OUTLIER_M = envDouble("SCMS_GPS_OUTLIER_M", 45.0);
    // Transient GPS degradation (urban canyon / tunnel): benign vehicles occasionally enter a
    // multi-second bad-GPS burst — a realistic source of hard, benign false positives.
    static final double GPS_DEGRADE_RATE = envDouble("SCMS_GPS_DEGRADE_RATE", 0.004);   // per second
    static final double GPS_DEGRADE_FACTOR = envDouble("SCMS_GPS_DEGRADE_FACTOR", 5.0);
    static final double GPS_DEGRADE_MIN_S = 3.0, GPS_DEGRADE_MAX_S = 8.0;
    // Weather (SCMS_WEATHER = clear|rain|fog|snow) degrades sensors (and radio, in the app).
    static final double WEATHER_SENSOR_MULT = weatherSensorMult();

    private static double weatherSensorMult() {
        String w = System.getenv("SCMS_WEATHER");
        if (w == null) {
            return 1.0;
        }
        switch (w.toLowerCase()) {
            case "rain": return 1.5;
            case "fog":  return 2.5;
            case "snow": return 2.0;
            default:     return 1.0;
        }
    }
    // MA revocation requires SUSTAINED, corroborated evidence (not a transient spike): K distinct
    // reporters AND reports in at least this many distinct 1-second intervals spanning >= PERSIST_S.
    // Counting distinct seconds (not raw reports) stops one multi-witness GPS spike from looking
    // persistent; the threshold sits above the tail of the benign outlier count so honest vehicles
    // clear it while continuously-misbehaving attackers do not.
    static final int REVOKE_MIN_SECONDS = envInt("SCMS_MIN_SECONDS", 8);
    static final double REVOKE_PERSIST_S = envDouble("SCMS_PERSIST_S", 5.0);
    // Collusion-robust MA: only reports from TRUSTED reporters count toward revocation. A reporter
    // that is itself heavily reported (or revoked) is discounted — so a colluding group that also
    // misbehaves loses its ability to frame a benign victim. On by default (a real MA weighs trust);
    // a no-op when there is no collusion (honest reporters are never suspicious).
    static final boolean MA_DEFENSE = envInt("SCMS_MA_DEFENSE", 1) != 0;
    static final int REPUTATION_MAX = envInt("SCMS_REPUTATION_MAX", 6);   // reports-against before a reporter is distrusted
    static final int REPORT_BUDGET = envInt("SCMS_REPORT_BUDGET", 25);    // reports a reporter may file before rate-limited
    // Pseudonym rotation: each vehicle changes pseudonym certificate every ROTATE_PERIOD_S (a new
    // digest under the SAME LA linkage seeds, so the MA can still link+revoke it). 0 disables.
    static final double ROTATE_PERIOD_S = envDouble("SCMS_ROTATE_PERIOD", 90.0);
    // A fraction of vehicles are FAULTY (malfunctioning sensor), not malicious: they look anomalous
    // but are not attacks, so a detector must tell faults from attacks. And a per-message ground-truth
    // sample records true-vs-claimed content for message-level benchmarks.
    static final int FAULTY_PCT = envInt("SCMS_FAULTY_PCT", 5);
    static final double EMIT_SAMPLE = envDouble("SCMS_EMIT_SAMPLE", 0.02);
    // Collusion / false-accusation attack on the MA's integrity: a fraction of attackers coordinate
    // to file FALSE misbehaviour reports against a small set of benign victims. Enough colluders
    // can frame an innocent vehicle — a realistic SCMS threat the dataset must contain.
    static final int COLLUDE_PCT = envInt("SCMS_COLLUDE_PCT", 0);    // of attackers (0 = off by default)
    static final int VICTIM_PCT = envInt("SCMS_VICTIM_PCT", 3);      // of benign vehicles
    static final double FALSE_REPORT_INTERVAL_S = envDouble("SCMS_FALSE_REPORT_INTERVAL", 2.0);
    static final double INGEST_DELAY_S = envDouble("SCMS_INGEST_DELAY", 0.15);   // MA report-channel latency
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
        boolean faulty;                // malfunctioning sensor (anomalous but not malicious)
        boolean colluder;              // files false accusations against benign victims
        boolean victim;                // benign target of a collusion campaign
        double lastFalseReportT = -1e9;
        int reportsReceived;           // reports filed AGAINST this vehicle (reporter-reputation)
        int reportsFiled;              // reports this vehicle has filed (rate-limit / spray detection)
        int faultMode;                 // 0 = extra outliers, 1 = constant bias, 2 = slow drift
        double faultAngle, faultMag;   // fault direction + magnitude
        double driftAccum;             // slow-drift fault accumulator
        double degradeUntil = -1;      // GPS-degradation burst end time
        double onsetT = -1;            // attack onset time (first falsified message)
        String attackType = "none";
        AttackLib.State atk = null;
        List<String> ghosts = null;   // Sybil: extra identities mapped back to this device
        double lastX, lastY, lastSeenT = -1;   // true position, for the live map
        double sBiasX, sBiasY, sensorLastT = -1;   // OU-correlated sensor bias state
        java.util.Random sRng;                     // per-vehicle sensor-noise RNG (seeded)
        double nextRotateT = Double.MAX_VALUE;     // next pseudonym rotation time
        int rotCount = 0;
        double gpsQ = 1.0;                          // per-vehicle GNSS quality factor (heterogeneous)
        boolean revoked = false;                   // vehicle-level revocation (covers all pseudonyms)
        double revokeT = -1;
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
    private final List<Map<String, Object>> gtEmit = new ArrayList<>();   // per-message GT sample
    private final Random emitRng = new Random(MASTER_SEED * 104729L);

    private int reportCounter = 0;
    private int caseCounter = 0;
    private boolean written = false;
    private double lastLiveWriteT = -1e9;
    private final List<String> victims = new ArrayList<>();          // benign collusion targets
    private final Map<String, double[]> subjAgg = new HashMap<>();   // subject -> {firstT, lastT}
    private final Map<String, java.util.Set<Long>> subjSeconds = new HashMap<>();   // subject -> distinct report seconds
    private final Map<String, java.util.Set<String>> trustedReporters = new HashMap<>();  // subject veh -> trusted reporter vehs

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
            d.colluder = ((sha("collude|" + MASTER_SEED + "|" + unitId)[0] & 0xff) * 100 / 256) < COLLUDE_PCT;
        } else {
            d.faulty = ((sha("fault|" + MASTER_SEED + "|" + unitId)[0] & 0xff) * 100 / 256) < FAULTY_PCT;
            if (d.faulty) {
                byte[] fh = sha("faultmode|" + MASTER_SEED + "|" + unitId);
                d.faultMode = (fh[0] & 0xff) % 3;
                d.faultAngle = ((fh[1] & 0xff) / 255.0) * 2 * Math.PI;
                d.faultMag = 20.0 + (fh[2] & 0xff) / 255.0 * 30.0;   // 20..50 m constant-bias fault
            }
            d.victim = ((sha("victim|" + MASTER_SEED + "|" + unitId)[0] & 0xff) * 100 / 256) < VICTIM_PCT;
            if (d.victim) {
                victims.add(unitId);
            }
        }
        // heterogeneous GNSS quality per vehicle (0.4 good .. ~2.2 poor, skewed to typical), so the
        // broadcast position confidence VARIES and is an informative feature (not a constant).
        double qr = (sha("gpsq|" + MASTER_SEED + "|" + unitId)[0] & 0xff) / 255.0;
        d.gpsQ = 0.4 + 1.8 * Math.pow(qr, 1.6);

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

        d.nextRotateT = (ROTATE_PERIOD_S > 0) ? ROTATE_PERIOD_S : Double.MAX_VALUE;
        devByUnit.put(unitId, d);
        devByDigest.put(d.certDigest, d);
        if (d.attacker && "Sybil".equals(AttackLib.baseOf(d.attackType))) {
            d.ghosts = new ArrayList<>();
            int nGhosts = AttackLib.ghostCount(d.attackType, attackCfg);
            for (int k = 0; k < nGhosts; k++) {
                String g = hex(sha("ghost|" + MASTER_SEED + "|" + unitId + "|" + k), 8);
                d.ghosts.add(g);
                devByDigest.put(g, d);   // ghost identities resolve to the same attacker (for labeling)
                gtId.add(gtRow("true_vehicle_id", unitId, "pseudonym_cert_digest", g, "i_period", d.i));
            }
        }
        gtVeh.add(gtRow("true_vehicle_id", unitId, "is_attacker", d.attacker, "is_faulty", d.faulty,
                "attacker_role", d.attackType));
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

    /** Current pseudonym for a vehicle's next beacon, rotating it if the lifetime has elapsed. */
    public synchronized Cred beaconCred(String unitId, long tNs) {
        Dev d = devByUnit.get(unitId);
        if (d == null) {
            register(unitId);
            d = devByUnit.get(unitId);
        }
        double t = tNs / 1e9;
        while (ROTATE_PERIOD_S > 0 && t >= d.nextRotateT) {
            rotate(d);
            d.nextRotateT += ROTATE_PERIOD_S;
        }
        return new Cred(d.certDigest, d.i, d.j, d.lvHex, d.attacker, d.attackType);
    }

    /** Roll the vehicle to a fresh pseudonym under the SAME LA linkage seeds (privacy), so the
     *  MA can still link it on investigation; a revoked vehicle's new pseudonym stays revoked. */
    private void rotate(Dev d) {
        d.rotCount++;
        d.j = (d.j + 1) % JMAX;
        if (d.j == 0) {
            d.i++;
        }
        d.lv = d.link.linkageValueFor(d.i, d.j);
        d.lvHex = hex(d.lv, d.lv.length);
        d.certDigest = hex(sha("cert|" + MASTER_SEED + "|" + d.unitId + "|rot" + d.rotCount), 8);
        devByDigest.put(d.certDigest, d);   // new pseudonym resolves to the same vehicle
        scms.pca.issue(d.certDigest, d.requestHash, d.i, d.j, "lc1:" + d.unitId, "lc2:" + d.unitId);
        gtId.add(gtRow("true_vehicle_id", d.unitId, "pseudonym_cert_digest", d.certDigest, "i_period", d.i));
    }

    /** Compute the claimed CAM (content + timing/flood/sybil flags) for one broadcast. */
    public synchronized AttackLib.Claim claim(String unitId, int sendCount, double x, double y,
                                              double speed, double heading, long tNs) {
        Dev d = devByUnit.get(unitId);
        if (d == null) {
            register(unitId);
            d = devByUnit.get(unitId);
        }
        double t = tNs / 1e9;
        d.lastX = x; d.lastY = y; d.lastSeenT = t;   // TRUE position (ground truth + live map)
        maybeWriteLive(t);
        // sensor model: what this vehicle MEASURES of its own state (GNSS/odometry error).
        if (d.sRng == null) {
            d.sRng = new java.util.Random(MASTER_SEED * 7919L + unitId.hashCode());
        }
        double dt = (d.sensorLastT < 0) ? 0.1 : Math.max(1e-3, Math.min(10.0, t - d.sensorLastT));
        d.sensorLastT = t;
        double q = d.gpsQ * WEATHER_SENSOR_MULT;              // per-vehicle GNSS quality × weather
        // transient degradation burst (canyon/tunnel) — elevated error for a few seconds
        if (t < d.degradeUntil) {
            q *= GPS_DEGRADE_FACTOR;
        } else if (d.sRng.nextDouble() < GPS_DEGRADE_RATE * dt) {
            d.degradeUntil = t + GPS_DEGRADE_MIN_S + d.sRng.nextDouble() * (GPS_DEGRADE_MAX_S - GPS_DEGRADE_MIN_S);
            q *= GPS_DEGRADE_FACTOR;
        }
        double sigma = GPS_SIGMA_M * q;
        double bias = GPS_BIAS_M * q;
        double a = Math.exp(-GPS_THETA * dt);                 // OU bias update over dt
        double qv = bias * Math.sqrt(Math.max(0.0, 1 - a * a));
        d.sBiasX = a * d.sBiasX + qv * d.sRng.nextGaussian();
        d.sBiasY = a * d.sBiasY + qv * d.sRng.nextGaussian();
        double mx = x + d.sBiasX + sigma * d.sRng.nextGaussian();
        double my = y + d.sBiasY + sigma * d.sRng.nextGaussian();
        // Faulty units glitch more often (a malfunctioning sensor, not an attack).
        double outRate = (d.faulty && d.faultMode == 0 ? GPS_OUTLIER_RATE * 6.0 : GPS_OUTLIER_RATE) * q;
        if (d.sRng.nextDouble() < Math.min(0.6, outRate * dt)) {   // rare multipath spike
            double ang = d.sRng.nextDouble() * 2 * Math.PI;
            double mag = GPS_OUTLIER_M * (0.5 + d.sRng.nextDouble());
            mx += Math.cos(ang) * mag; my += Math.sin(ang) * mag;
        }
        // sustained sensor faults (malfunction, not malicious): a persistent constant bias or a
        // slowly-growing drift — anomalous but self-consistent, so hard to tell from a subtle attack.
        if (d.faulty && d.faultMode == 1) {
            mx += Math.cos(d.faultAngle) * d.faultMag; my += Math.sin(d.faultAngle) * d.faultMag;
        } else if (d.faulty && d.faultMode == 2) {
            d.driftAccum = Math.min(60.0, d.driftAccum + 0.2);   // grows ~0.2 m per CAM, capped
            mx += Math.cos(d.faultAngle) * d.driftAccum; my += Math.sin(d.faultAngle) * d.driftAccum;
        }
        double mspeed = Math.max(0.0, speed + SPEED_SIGMA_MS * d.sRng.nextGaussian());
        double mheading = ((heading + HEADING_SIGMA_DEG * d.sRng.nextGaussian()) % 360 + 360) % 360;
        // 2-D 95% position-confidence radius: sqrt(-2 ln 0.05) ≈ 2.448 × per-axis sigma
        // (the 1-D z-score 1.96 would under-cover a 2-D error — a real calibration fix).
        double conf = 2.448 * Math.sqrt(sigma * sigma + bias * bias);
        AttackLib.Claim c;
        if (!d.attacker) {
            c = new AttackLib.Claim();
            c.x = mx; c.y = my; c.speed = mspeed; c.heading = mheading; c.genTimeNs = tNs;
        } else {
            c = AttackLib.compute(d.attackType, d.atk, sendCount, mx, my, mspeed, mheading, tNs, attackCfg);
        }
        c.posConf = conf;
        // circular heading delta in [0,180]; pure-heading attacks (ReversedHeading, RandomHeading,
        // PerpendicularHeading, HeadingOffset) leave x/y/speed/genTime untouched, so without this
        // term the whole heading family was labelled falsified=false and never got an onset stamp.
        double dHeading = Math.abs(((c.heading - mheading + 540.0) % 360.0) - 180.0);
        boolean falsified = d.attacker && (Math.hypot(c.x - mx, c.y - my) > 1.0
                || Math.abs(c.speed - mspeed) > 1.0 || dHeading > 5.0
                || c.genTimeNs != tNs || c.flood || c.sybilGhosts > 0);
        if (falsified && d.onsetT < 0) {
            d.onsetT = t;    // attack ONSET: first time this attacker actually falsified content
        }
        // per-message ground-truth sample (true vs claimed) for message-level benchmarks
        if (emitRng.nextDouble() < EMIT_SAMPLE) {
            gtEmit.add(gtRow("emit_id", String.format(java.util.Locale.ROOT, "emt_%08d", gtEmit.size()), "t", round3(t),
                    "true_vehicle_id", unitId,
                    "true_x", round3(x), "true_y", round3(y),
                    "claimed_x", round3(c.x), "claimed_y", round3(c.y), "claimed_speed", round3(c.speed),
                    "pos_conf", round3(conf),
                    "is_attacker", d.attacker, "is_faulty", d.faulty, "falsified", falsified));
        }
        return c;
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
        // Vehicle-level: the CRL carries the LA linkage seed, so ALL of a revoked vehicle's
        // pseudonyms (past and future rotations) are enforced once propagation delay has elapsed.
        Dev d = devByDigest.get(subjectDigest);
        if (d != null && d.revoked && t >= d.revokeT + CRL_PROP_DELAY_S) {
            return true;
        }
        return scms.crlStore.enforced(subjectDigest, t, CRL_PROP_DELAY_S);
    }

    /** Colluding attackers coordinate to file FALSE accusations against benign victims (an attack on
     *  the MA's integrity). Enough distinct colluders on one victim can frame it. Called each beacon. */
    public synchronized void maybeCollude(String unitId, long tNs) {
        Dev c = devByUnit.get(unitId);
        if (c == null || !c.colluder || victims.isEmpty()) {
            return;
        }
        double t = tNs / 1e9;
        if (t - c.lastFalseReportT < FALSE_REPORT_INTERVAL_S) {
            return;
        }
        c.lastFalseReportT = t;
        // colluders converge on the SAME victim at a given time, so K distinct colluders pile onto one.
        int idx = (int) Math.floorMod((long) (t / FALSE_REPORT_INTERVAL_S), victims.size());
        Dev v = devByUnit.get(victims.get(idx));
        if (v != null) {
            // use a PLAUSIBLE detector reason + fingerprint so the false report is indistinguishable
            // from a real one at the MA level — detecting it needs behavioural/graph signals.
            Map<String, Double> det = new HashMap<>();
            det.put("positionSpeedInconsistency", 2.0);
            onDetection(c.certDigest, v.certDigest, 30.0, t, "positionSpeedInconsistency", 6.5, 2.0, det);
        }
    }

    // -------------------------------------------------- report ingestion (LOP -> RA -> MA)
    public synchronized void onDetection(String reporterDigest, String subjectDigest, double score, double t,
                                         String reasonCode, double subjectPosConf, double scoreNorm,
                                         Map<String, Double> detNorms) {
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
        String rid = String.format(java.util.Locale.ROOT, "rpt_%05d", reportCounter);
        double ingestDelay = INGEST_DELAY_S * (0.5 + rng.nextDouble());   // realistic report-channel latency
        Map<String, Object> row = maRow("report_id", rid, "ingest_time", round3(t + ingestDelay),
                "detection_time", round3(t),
                "reporter_cert_digest", reporterDigest, "subject_cert_digest", subjectDigest,
                "reason_codes", List.of(reasonCode),
                "detector_score", round3(score), "detector_score_norm", round3(scoreNorm),
                "subject_pos_confidence", round3(subjectPosConf),
                "sig_valid", true, "cert_crl_status", "active");
        if (detNorms != null) {   // full multi-detector fusion fingerprint for this report
            for (Map.Entry<String, Double> en : detNorms.entrySet()) {
                row.put("detnorm_" + en.getKey(), round3(en.getValue()));
            }
        }
        maReports.add(row);
        String correctness = subj.attacker ? "correct"
                : (rep.colluder ? "malicious_false_report"       // a coordinated false accusation
                : (subj.faulty ? "faulty_detection" : "false_positive"));
        gtRepLbl.add(gtRow("report_id", rid, "reporter_true_id", rep.unitId, "subject_true_id", subj.unitId,
                "report_correctness", correctness));
        int distinctAll = scms.ma.addReporter(subjectDigest, reporterDigest);
        subj.reportsReceived++;
        rep.reportsFiled++;
        // reporter-reputation defense: a report only counts if the reporter is trusted — not revoked,
        // not itself heavily reported, and not spraying accusations beyond a sane report budget
        // (rate-limiting). This blunts collusion campaigns that flood false reports at a victim.
        boolean repTrusted = !MA_DEFENSE || (!rep.revoked && rep.reportsReceived < REPUTATION_MAX
                && rep.reportsFiled <= REPORT_BUDGET);
        java.util.Set<String> trusted = trustedReporters.computeIfAbsent(subj.unitId, k -> new java.util.HashSet<>());
        if (repTrusted) {
            trusted.add(rep.unitId);
        }
        int distinct = MA_DEFENSE ? trusted.size() : distinctAll;
        // Persistence and distinct-second evidence aggregate over the SAME unit as the reporter set
        // (subj.unitId), so a Sybil attacker cannot split evidence across ghost pseudonyms to keep any
        // single digest below the persistence gate while the reporter count builds per vehicle. (This
        // matches the K-distinct-reporters key; keying persistence per digest instead let ghosts evade.)
        double[] ag = subjAgg.computeIfAbsent(subj.unitId, k -> new double[] {t, t});
        ag[1] = t;                                               // lastT (firstT fixed)
        java.util.Set<Long> secs = subjSeconds.computeIfAbsent(subj.unitId, k -> new java.util.HashSet<>());
        secs.add((long) Math.floor(t));
        boolean sustained = secs.size() >= REVOKE_MIN_SECONDS && (ag[1] - ag[0]) >= REVOKE_PERSIST_S;
        if (!subj.revoked && distinct >= REPORT_THRESHOLD_K && sustained) {
            resolveAndRevoke(subj, t);
        }
    }

    /** MA investigation across entities: PCA -> LA1/LA2 (seeds) -> RA (blacklist) -> CRLG -> CRL Store. */
    private void resolveAndRevoke(Dev subj, double t) {
        caseCounter++;
        String caseId = String.format(java.util.Locale.ROOT, "case_%04d", caseCounter);
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
        subj.revoked = true; subj.revokeT = t;   // vehicle-level: covers all pseudonyms (LA linkage)
        int reporters = scms.ma.distinctReporters(subj.certDigest);
        maInvest.add(maRow("case_id", caseId, "opened_time", round3(t), "trigger", "report_threshold",
                "num_distinct_reporters", reporters, "linkage_result", "same", "identity_resolved", true,
                "revocation_decision", "revoke", "resolved_case_handle", hex(sha(caseId), 6)));
        maCrl.add(maRow("crl_id", String.format(java.util.Locale.ROOT, "crl_%04d", caseCounter), "issue_time", round3(t),
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
            // stamp each attacker's onset time (first falsified message) for detection-latency eval
            for (Map<String, Object> row : gtAtk) {
                Dev d = devByUnit.get(row.get("true_vehicle_id"));
                row.put("attack_onset_time", (d != null && d.onsetT >= 0) ? round3(d.onsetT) : null);
            }
            // one row per pseudonym the MA observed (rotation + ghosts), with vehicle-level
            // revocation status — the LA linkage revokes every pseudonym of a caught vehicle.
            for (Map.Entry<String, Dev> e : devByDigest.entrySet()) {
                String dig = e.getKey();
                Dev d = e.getValue();
                if (!firstSeen.containsKey(dig)) {
                    continue;   // only pseudonyms actually observed on-air are MA subjects
                }
                Double rt = d.revoked ? round3(d.revokeT) : null;
                maCertStatus.add(maRow("cert_digest", dig,
                        "first_seen", firstSeen.getOrDefault(dig, 0.0),
                        "last_seen", lastSeen.getOrDefault(dig, 0.0),
                        "issuing_pca", scms.pca.id, "crl_status", d.revoked ? "revoked" : "active",
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
            digests.put("ground_truth/gt_emissions_sample.jsonl", writeJsonl(gt.resolve("gt_emissions_sample.jsonl"), sortBy(gtEmit, "emit_id")));

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
            cfg.put("attack_variants_enabled", attacksEnabled.size());
            java.util.Set<String> bases = new java.util.TreeSet<>();
            for (String vName : attacksEnabled) {
                bases.add(AttackLib.baseOf(vName));
            }
            cfg.put("attack_bases_enabled", new ArrayList<>(bases));
            cfg.put("attack_variants_total", AttackLib.CATALOG.size());
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

    /** Throttled snapshot of active vehicles for the live dashboard map (state: 0 benign,
     *  1 attacker, 2 reported, 3 revoked). Best-effort; not part of the dataset. */
    private void maybeWriteLive(double t) {
        if (t - lastLiveWriteT < LIVE_INTERVAL_S) {
            return;
        }
        lastLiveWriteT = t;
        try {
            List<double[]> vs = new ArrayList<>();
            for (Dev d : devByUnit.values()) {
                if (d.lastSeenT < 0 || t - d.lastSeenT > 3.0) {
                    continue;
                }
                int s = 0;
                if (scms.crlStore.isRevoked(d.certDigest)) {
                    s = 3;
                } else if (scms.ma.distinctReporters(d.certDigest) > 0) {
                    s = 2;
                } else if (d.attacker) {
                    s = 1;
                }
                vs.add(new double[] {round3(d.lastX), round3(d.lastY), s});
            }
            Map<String, Object> snap = new LinkedHashMap<>();
            snap.put("t", round3(t));
            snap.put("n", vs.size());
            snap.put("vehicles", vs);
            Files.createDirectories(Paths.get(OUT_DIR));
            Path tmp = Paths.get(OUT_DIR, "live_state.json.tmp");
            Files.write(tmp, gson.toJson(snap).getBytes(StandardCharsets.UTF_8));
            Files.move(tmp, Paths.get(OUT_DIR, "live_state.json"),
                    java.nio.file.StandardCopyOption.REPLACE_EXISTING);
        } catch (Exception ex) {
            // live view is best-effort
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
