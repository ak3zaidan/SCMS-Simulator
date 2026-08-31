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
    public static final double WEATHER_SENSOR_MULT = weatherSensorMult();

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

    /** Version of the MOSAIC application layer, recorded in the manifest (jar name is unchanged). */
    public static final String APP_VERSION = "0.1.0+realism.p2";

    // ---------------------------------------------------------------- Phase-1 realism (VeReMi-NextGen ports)
    // Every knob below is OPT-IN and defaults to the pre-port behaviour, so an existing seed still
    // reproduces its dataset byte-for-byte (no extra RNG draws are taken on the default path).
    /** builtin = the OU-correlated GNSS model in claim(); nextgen = VeReMi-NextGen SensorErrorModel. */
    public static final String SENSOR_MODEL = envStr("SCMS_SENSOR_MODEL", "builtin");
    public static final double SENSOR_POS_ERR_M = envDouble("SCMS_SENSOR_POS_ERR_M", 5.0);
    public static final double SENSOR_SPEED_ERR = envDouble("SCMS_SENSOR_SPEED_ERR", 0.00016);
    public static final double SENSOR_HEAD_ERR_DEG = envDouble("SCMS_SENSOR_HEAD_ERR_DEG", 20.0);
    public static final double SENSOR_POS_SIGMA_FRAC = envDouble("SCMS_SENSOR_POS_SIGMA_FRAC", 0.03);
    public static final double SENSOR_HEAD_DECAY = envDouble("SCMS_SENSOR_HEAD_DECAY", 0.1);
    /** Per-vehicle driver profiles (10/80/10 aggressive/normal/passive) via requestVehicleParametersUpdate. */
    public static final boolean DRIVER_PROFILES = envInt("SCMS_DRIVER_PROFILES", 0) != 0;
    public static final double DRIVER_AGGRESSIVE_FRAC = envDouble("SCMS_DRIVER_AGGRESSIVE_PCT", 10.0) / 100.0;
    public static final double DRIVER_PASSIVE_FRAC = envDouble("SCMS_DRIVER_PASSIVE_PCT", 10.0) / 100.0;
    /** period = fixed ROTATE_PERIOD_S; distance = NextGen's privacy model (distance, then distance+time). */
    public static final String PSEUDONYM_POLICY = envStr("SCMS_PSEUDONYM_POLICY", "period");
    static final double PSN_DIST_MIN_M = envDouble("SCMS_PSN_DIST_MIN_M", 800.0);
    static final double PSN_DIST_MAX_M = envDouble("SCMS_PSN_DIST_MAX_M", 1500.0);
    static final double PSN_TIME_MIN_S = envDouble("SCMS_PSN_TIME_MIN_S", 120.0);
    static final double PSN_TIME_MAX_S = envDouble("SCMS_PSN_TIME_MAX_S", 360.0);

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

    private static String envStr(String name, String dflt) {
        String e = System.getenv(name);
        return (e != null && !e.isBlank()) ? e.trim().toLowerCase(java.util.Locale.ROOT) : dflt;
    }

    /**
     * Deterministic 64-bit seed for a per-vehicle RNG stream, keyed on the SCENARIO SEED and the
     * vehicle id — never on wall-clock time or object identity. SHA-256 based, so it is stable
     * across JVM versions and platforms (String.hashCode mixes are only spec-stable, and collide).
     */
    public static long streamSeed(String label, String unitId) {
        byte[] h = sha(label + "|" + MASTER_SEED + "|" + unitId);
        long v = 0;
        for (int k = 0; k < 8; k++) {
            v = (v << 8) | (h[k] & 0xffL);
        }
        return v;
    }

    /** Deterministic U[0,1) keyed on (label, scenario seed, vehicle id): stateless, order-independent. */
    public static double keyedUniform(String label, String unitId) {
        return (streamSeed(label, unitId) >>> 11) * 0x1.0p-53;
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
        String driverProfile;                      // DriverProfile name (null unless DRIVER_PROFILES)
        // NextGen-style distance/time pseudonym policy (sim time only — see rotateByDistance)
        double psnOdoRef = Double.NaN;             // odometer reading at the last change
        double psnLastChangeT = 0;                 // SIM time of the last change
        boolean psnFirstChange = true;
        double psnDistM, psnTimeS;
        double gpsQ = 1.0;                          // per-vehicle GNSS quality factor (heterogeneous)
        boolean revoked = false;                   // vehicle-level revocation (covers all pseudonyms)
        double revokeT = -1;
        boolean isRsu = false;                     // static infrastructure receiver, never a vehicle
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
    private volatile java.io.File scenarioDir;   // set once by the app (MOSAIC configuration path)
    private final Map<String, Object> appParams = new TreeMap<>();   // app-layer effective knobs
    private double lastLiveWriteT = -1e9;
    private final List<String> victims = new ArrayList<>();          // benign collusion targets
    private final Map<String, double[]> subjAgg = new HashMap<>();   // subject -> {firstT, lastT}
    private final Map<String, java.util.Set<Long>> subjSeconds = new HashMap<>();   // subject -> distinct report seconds
    private final Map<String, java.util.Set<String>> trustedReporters = new HashMap<>();  // subject veh -> trusted reporter vehs
    // Channel / DCC run totals, aggregated from every receiver at shutdown (manifest diagnostics).
    private long chSensed, chDelivered, chDropWeather, chDropObstruction, chDropCongestion, chNlosb;
    private long dccAllowedCams, dccSuppressedCams, dccCbrSamples;
    private double dccCbrSum, dccCbrMax;
    private Map<String, Object> channelIndexStats;

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

    /**
     * Provision a ROAD-SIDE UNIT as an always-trusted reporter (see {@link org.scms.app.ScmsRsuApp}).
     *
     * <p>Deliberately NOT {@link #register}: an RSU is infrastructure, not a vehicle. It never draws
     * an attacker/faulty/colluder role, never beacons, and gets no ``gt_vehicle`` /
     * ``gt_identity_map`` / ``gt_enrollment`` row. That absence is exactly the signal the downstream
     * feature builder uses -- ``featurize._is_rsu_reporter`` classifies a reporter cert that the
     * oracle identity map does not know as infrastructure -- so RSU evidence lands on an opaque
     * ``rsu_*`` graph node with no identity anywhere near it. The Dev entry exists only so
     * {@link #onDetection} can resolve the reporter and apply the ordinary trust bookkeeping.
     *
     * <p>An RSU is enrolled with the PCA under its own request hash so its certificate is a real
     * SCMS credential, but it carries no linkage seeds: infrastructure is not pseudonymous, and
     * there is nothing for the LAs to link.
     */
    public synchronized Cred rsuCredential(String unitId) {
        Dev d = devByUnit.get(unitId);
        if (d == null) {
            d = new Dev();
            d.unitId = unitId;
            d.isRsu = true;
            d.i = 0;
            d.j = 0;
            d.certDigest = hex(sha("rsucert|" + MASTER_SEED + "|" + unitId), 8);
            d.requestHash = hex(sha("rsureq|" + MASTER_SEED + "|" + unitId), 8);
            d.lvHex = "";
            d.nextRotateT = Double.MAX_VALUE;      // infrastructure does not rotate pseudonyms
            scms.dcm.attest(unitId);
            String enrollmentId = scms.eca.issue(unitId);
            scms.pca.issue(d.certDigest, d.requestHash, d.i, d.j, "lc1:" + unitId, "lc2:" + unitId);
            scms.ra.bind(d.requestHash, enrollmentId);
            devByUnit.put(unitId, d);
            devByDigest.put(d.certDigest, d);
        }
        return new Cred(d.certDigest, d.i, d.j, d.lvHex, false, "none");
    }

    /** Road-side units provisioned this run (manifest counts; excluded from the vehicle count). */
    private synchronized int rsuCount() {
        int n = 0;
        for (Dev d : devByUnit.values()) {
            if (d.isRsu) {
                n++;
            }
        }
        return n;
    }

    /** Current pseudonym for a vehicle's next beacon, rotating it if the lifetime has elapsed. */
    public synchronized Cred beaconCred(String unitId, long tNs) {
        return beaconCred(unitId, tNs, Double.NaN);
    }

    /**
     * As {@link #beaconCred(String, long)}, but also accepting the vehicle's odometer (m) so the
     * NextGen distance-based privacy policy can be evaluated (ignored by the default period policy).
     */
    public synchronized Cred beaconCred(String unitId, long tNs, double odometerM) {
        Dev d = devByUnit.get(unitId);
        if (d == null) {
            register(unitId);
            d = devByUnit.get(unitId);
        }
        double t = tNs / 1e9;
        if ("distance".equals(PSEUDONYM_POLICY)) {
            rotateByDistance(d, t, odometerM);
        } else {
            while (ROTATE_PERIOD_S > 0 && t >= d.nextRotateT) {
                rotate(d);
                d.nextRotateT += ROTATE_PERIOD_S;
            }
        }
        return new Cred(d.certDigest, d.i, d.j, d.lvHex, d.attacker, d.attackType);
    }

    /**
     * VeReMi-NextGen's privacy-realistic pseudonym change model (VehicleCamSendingApp.java:172-200):
     * the FIRST change happens after D ~ U(800, 1500) m have been driven; every later change needs
     * BOTH that distance again AND T ~ U(120, 360) s of elapsed time.
     *
     * <p>Upstream measures the elapsed time with {@code LocalDateTime.now()} — wall clock — so the
     * policy fires on how long the SIMULATOR ran, not on how long the vehicle drove: at 100x real
     * time no vehicle ever reaches the time criterion, and no run is reproducible. Here the timer is
     * MOSAIC simulation time, and D/T are drawn from SHA-256(seed|vehicle|cycle) instead of
     * {@code Math.random()}, so the whole policy is a pure function of (scenario seed, vehicle id).
     */
    private void rotateByDistance(Dev d, double t, double odometerM) {
        if (Double.isNaN(odometerM)) {
            return;   // no odometer available (e.g. a unit that never reported vehicle data)
        }
        if (Double.isNaN(d.psnOdoRef)) {
            d.psnOdoRef = odometerM;
            d.psnLastChangeT = t;
            d.psnDistM = PSN_DIST_MIN_M + (PSN_DIST_MAX_M - PSN_DIST_MIN_M)
                    * keyedUniform("psn-dist", d.unitId);
            d.psnTimeS = PSN_TIME_MIN_S + (PSN_TIME_MAX_S - PSN_TIME_MIN_S)
                    * keyedUniform("psn-time|0", d.unitId);
        }
        if (odometerM - d.psnOdoRef <= d.psnDistM) {
            return;
        }
        if (!d.psnFirstChange && (t - d.psnLastChangeT) <= d.psnTimeS) {
            return;   // distance reached, but the privacy dwell time has not elapsed yet
        }
        rotate(d);
        d.psnFirstChange = false;
        d.psnOdoRef = odometerM;
        d.psnLastChangeT = t;
        d.psnTimeS = PSN_TIME_MIN_S + (PSN_TIME_MAX_S - PSN_TIME_MIN_S)
                * keyedUniform("psn-time|" + d.rotCount, d.unitId);
    }

    /**
     * ORACLE channel oracle: the TRUE position of the vehicle behind a pseudonym (Sybil ghosts
     * resolve to their puppeteer, which is where the frame is physically emitted from).
     *
     * <p>This exists for RADIO PHYSICS ONLY — LOS/NLOS geometry, path loss, ranging — where the
     * receiver-side model must not be steered by attacker-controlled content. It must never reach a
     * misbehaviour report, an ma/* row, or any feature: those stay derived from the CLAIMED state.
     *
     * @return {trueX, trueY} in projected metres, or null before the vehicle's first beacon
     */
    public synchronized double[] truePositionOf(String certDigest) {
        Dev d = devByDigest.get(certDigest);
        if (d == null || d.lastSeenT < 0) {
            return null;
        }
        return new double[] {d.lastX, d.lastY};
    }

    /**
     * OPAQUE, stable key for the RADIO LINK behind a pseudonym — the identifier the channel model
     * keys its per-link shadowing process on.
     *
     * <p>It is deliberately NOT the vehicle id. The channel needs an identity that survives
     * pseudonym rotation (otherwise every rotation resamples the channel — the correctness bug the
     * per-step, digest-keyed shadowing draw had) and that maps a Sybil ghost onto its puppeteer
     * (they are one radio). Handing the application layer the true vehicle id would satisfy both and
     * put a real identity one careless line away from a misbehaviour report, so what comes back is
     * SHA-256("linkkey" | scenario seed | unit id) truncated to 64 bits: enough to key an RNG stream
     * and a hash map, and useless as an identity.
     *
     * @return the link key, or 0 before the vehicle's first beacon (no oracle entry yet)
     */
    public synchronized long channelLinkKey(String certDigest) {
        Dev d = devByDigest.get(certDigest);
        if (d == null) {
            return 0L;
        }
        return streamSeed("linkkey", d.unitId);
    }

    /**
     * Aggregate one receiver's channel counters into the run totals (manifest {@code counts.channel}).
     * Diagnostics only: no dataset row depends on them.
     */
    public synchronized void noteChannel(long sensed, long delivered, long dropWeather,
                                         long dropObstruction, long dropCongestion, long nlosbLinks,
                                         long dccAllowed, long dccSuppressed,
                                         long cbrSamples, double cbrSum, double cbrMax) {
        chSensed += sensed;
        chDelivered += delivered;
        chDropWeather += dropWeather;
        chDropObstruction += dropObstruction;
        chDropCongestion += dropCongestion;
        chNlosb += nlosbLinks;
        dccAllowedCams += dccAllowed;
        dccSuppressedCams += dccSuppressed;
        dccCbrSamples += cbrSamples;
        dccCbrSum += cbrSum;
        dccCbrMax = Math.max(dccCbrMax, cbrMax);
    }

    /** Final LOS/NLOSb index counters + projection-alignment verdict (manifest diagnostics). */
    public synchronized void noteChannelIndex(Map<String, Object> stats) {
        if (stats != null && !stats.isEmpty()) {
            channelIndexStats = stats;
        }
    }

    /** Records the driver profile a vehicle applied (ground truth + manifest fleet composition). */
    public synchronized void noteDriverProfile(String unitId, String profile) {
        Dev d = devByUnit.get(unitId);
        if (d == null) {
            register(unitId);
            d = devByUnit.get(unitId);
        }
        d.driverProfile = profile;
    }

    /** Scenario directory (parent of scenario_config.json), used to locate the input-hash manifest. */
    public void noteScenarioDir(java.io.File f) {
        if (f == null || scenarioDir != null) {
            return;
        }
        scenarioDir = f.isDirectory() ? f : f.getParentFile();
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
        return claim(unitId, sendCount, x, y, speed, heading, tNs, null);
    }

    /**
     * As {@link #claim(String, int, double, double, double, double, long)}, but with the sender's own
     * MEASURED state supplied by the app-side VeReMi-NextGen sensor model (SCMS_SENSOR_MODEL=nextgen).
     * The TRUE state is still passed in x/y/speed/heading, so the ground-truth tables and the
     * "falsified" test keep comparing the attack against the honest measurement, never against noise.
     * When {@code measured} is null the built-in OU/GNSS model runs exactly as before (identical RNG
     * draw order — the default path is unchanged, bit for bit).
     */
    public synchronized AttackLib.Claim claim(String unitId, int sendCount, double x, double y,
                                              double speed, double heading, long tNs,
                                              org.scms.realism.SensorErrorModel.Sample measured) {
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
        double mx;
        double my;
        // 2-D 95% position-confidence radius: sqrt(-2 ln 0.05) ≈ 2.448 × per-axis sigma
        // (the 1-D z-score 1.96 would under-cover a 2-D error — a real calibration fix).
        // TRANSMITTED COARSELY (SensorErrorModel.quantisedConfidence, applied below): a raw radius is
        // a monotone function of this vehicle's own 256-level gpsQ, i.e. a near-unique value that is
        // stable across pseudonym rotations, and it reaches the MA as ma_reports.subject_pos_confidence
        // and ml/report_features.csv's pos_confidence. That is a linkage key, not an accuracy report.
        double conf;
        if (measured == null) {
            double a = Math.exp(-GPS_THETA * dt);                 // OU bias update over dt
            double qv = bias * Math.sqrt(Math.max(0.0, 1 - a * a));
            d.sBiasX = a * d.sBiasX + qv * d.sRng.nextGaussian();
            d.sBiasY = a * d.sBiasY + qv * d.sRng.nextGaussian();
            mx = x + d.sBiasX + sigma * d.sRng.nextGaussian();
            my = y + d.sBiasY + sigma * d.sRng.nextGaussian();
            conf = 2.448 * Math.sqrt(sigma * sigma + bias * bias);
        } else {
            // NextGen sensor model: the app already produced the measurement (correlated GNSS bias,
            // relative speed error, speed-decaying heading error) from the TRUE state. The outlier /
            // degradation / fault machinery below still applies — those are hardware pathologies the
            // upstream model does not have, and they are what makes benign false positives realistic.
            mx = measured.x;
            my = measured.y;
            conf = measured.posConf;   // already weather-scaled: the app sizes the model by WEATHER_SENSOR_MULT
        }
        // one choke point for every producer of a transmitted confidence (idempotent, so the
        // already-quantised NextGen value passes through unchanged)
        conf = org.scms.realism.SensorErrorModel.quantisedConfidence(conf);
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
        double mspeed;
        double mheading;
        if (measured == null) {
            mspeed = Math.max(0.0, speed + SPEED_SIGMA_MS * d.sRng.nextGaussian());
            mheading = ((heading + HEADING_SIGMA_DEG * d.sRng.nextGaussian()) % 360 + 360) % 360;
        } else {
            mspeed = Math.max(0.0, measured.speed);
            mheading = ((measured.heading % 360) + 360) % 360;
        }
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
            // ADR 0002: the TRUE kinematics are written out, not reconstructed downstream. The
            // harness used to differentiate true_x/true_y twice to recover acceleration, and a
            // single-sample lane change (~3.2 m of lateral displacement at constant speed) then
            // showed up as hundreds of m/s^2. The simulator knows the exact values at emission time.
            //
            // Units and convention, matching everything else on the MOSAIC path:
            //   true_speed   m/s
            //   true_heading degrees in [0, 360), 0 = NORTH, increasing CLOCKWISE
            // which is SUMO's / MOSAIC's VehicleData.getHeading() convention, the same one the
            // claimed heading uses, the same one AttackLib's along-road offsets assume
            // (x + d*sin(h), y + d*cos(h), AttackLib.java:370-372) and the same one
            // CamDetector.headingInconsistency compares against (atan2(dx, dy), CamDetector.java:143).
            // ORACLE ONLY: both keys are in schemas.records.FORBIDDEN_FEATURE_KEYS.
            Map<String, Object> em = gtRow("emit_id", String.format(java.util.Locale.ROOT, "emt_%08d", gtEmit.size()), "t", round3(t),
                    "true_vehicle_id", unitId,
                    "true_x", round3(x), "true_y", round3(y),
                    "true_speed", round3(speed), "true_heading", round3(norm360(heading)),
                    "claimed_x", round3(c.x), "claimed_y", round3(c.y), "claimed_speed", round3(c.speed),
                    "pos_conf", round3(conf),
                    "is_attacker", d.attacker, "is_faulty", d.faulty, "falsified", falsified);
            if (measured != null) {
                // ORACLE-only: the honest MEASUREMENT (sensor error applied, attack not yet) — lets a
                // message-level benchmark separate GNSS/odometry error from attack magnitude. Only
                // present when the NextGen sensor model is on, so default datasets are unchanged.
                em.put("measured_x", round3(mx));
                em.put("measured_y", round3(my));
                em.put("measured_speed", round3(mspeed));
            }
            gtEmit.add(em);
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
            // ORACLE fleet composition: which driver profile each vehicle actually applied. Only
            // emitted when the profiles are on, so datasets generated without them are unchanged.
            if (DRIVER_PROFILES) {
                for (Map<String, Object> row : gtVeh) {
                    Dev d = devByUnit.get(row.get("true_vehicle_id"));
                    row.put("driver_profile", (d != null && d.driverProfile != null) ? d.driverProfile : "UNKNOWN");
                }
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
            manifest.put("app_version", APP_VERSION);
            // ADR 0002: gt_emissions_sample gained true_speed / true_heading, so the ground-truth
            // record shape changed and every pinned digest for this engine moves with it. The
            // increment is the versioned, deliberate half of the determinism contract — the
            // property is unchanged, the value it is asserted against is not.
            manifest.put("schema_versions", Map.of("ma_visible", 1, "ground_truth", 2));
            manifest.put("seed", MASTER_SEED);
            manifest.put("scms_entities", Scms.ENTITY_NAMES);
            Map<String, Object> inputs = inputManifest();
            Map<String, Object> cfg = new LinkedHashMap<>();
            cfg.put("reception", "MOSAIC AdHoc ITS-G5 CCH via SNS (range/delay)");
            // Analysis constants datagen.realism_bench needs to score a MOSAIC dataset without being
            // told the scenario by hand: the emission-sampling fraction (below 1.0 most traffic
            // metrics are unscoreable), the SNS single-hop radius and the acceptanceRangeThreshold
            // normaliser (both drive the link-distance reconstruction), and the road-network class
            // that selects the reference speed/headway bands. road_network comes from the generator's
            // scms_inputs.json params block (SCMS_ROAD_NETWORK overrides), never guessed here.
            cfg.put("emit_sample_prob", EMIT_SAMPLE);
            cfg.put("radio_range_m", envDouble("SCMS_RADIO_RANGE", 709.4));
            cfg.put("art_max_m", envDouble("SCMS_ART_MAX_M", 1000.0));
            String roadNetwork = inputParam(inputs, "road_network", envStr("SCMS_ROAD_NETWORK", ""));
            if (!roadNetwork.isEmpty()) {
                cfg.put("road_network", roadNetwork);
                cfg.put("regime", inputParam(inputs, "regime",
                        "linear".equals(roadNetwork) ? "highway" : "urban"));
            }
            cfg.put("sensor_model", SENSOR_MODEL);
            cfg.put("driver_profiles", DRIVER_PROFILES);
            cfg.put("pseudonym_policy", PSEUDONYM_POLICY);
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
            // REPLAY PARITY: every knob this JVM actually resolved (defaults included) plus the raw
            // SCMS_* environment as it was set, so a run can be reproduced from the manifest alone.
            manifest.put("effective_params", effectiveParams());
            manifest.put("env_scms", rawScmsEnv());
            if (inputs != null) {
                manifest.put("inputs", inputs);   // scenario input files + their sha256 (see gen_scenario)
            }
            int nRsu = rsuCount();
            Map<String, Object> counts = new LinkedHashMap<>();
            counts.put("vehicles", devByUnit.size() - nRsu);   // RSUs are infrastructure, not vehicles
            if (nRsu > 0) {
                counts.put("rsus", nRsu);
            }
            counts.put("reports", maReports.size());
            counts.put("investigations", maInvest.size());
            counts.put("revoked", scms.crlg.size());
            if (DRIVER_PROFILES) {
                Map<String, Integer> profiles = new TreeMap<>();
                for (Dev d : devByUnit.values()) {
                    if (d.isRsu) {
                        continue;                    // infrastructure has no driver
                    }
                    String p = (d.driverProfile != null) ? d.driverProfile : "UNKNOWN";
                    profiles.merge(p, 1, Integer::sum);
                }
                counts.put("driver_profiles", profiles);
            }
            // Channel diagnostics: how many frames the receivers actually sensed and where the
            // losses went. Only emitted when something beyond the default SNS path was active, so a
            // pre-Phase-2 run's manifest is unchanged.
            if (chSensed > 0 && (chDropWeather + chDropObstruction + chDropCongestion + chNlosb) > 0) {
                Map<String, Object> ch = new LinkedHashMap<>();
                ch.put("frames_sensed", chSensed);
                ch.put("frames_delivered", chDelivered);
                ch.put("delivery_ratio", round3((double) chDelivered / chSensed));
                ch.put("dropped_weather", chDropWeather);
                ch.put("dropped_obstruction", chDropObstruction);
                ch.put("dropped_congestion", chDropCongestion);
                ch.put("nlosb_links", chNlosb);
                if (channelIndexStats != null) {
                    ch.put("buildings", channelIndexStats);
                }
                counts.put("channel", ch);
            }
            if (dccAllowedCams + dccSuppressedCams > 0 || dccCbrSamples > 0) {
                Map<String, Object> dc = new LinkedHashMap<>();
                dc.put("cams_allowed", dccAllowedCams);
                dc.put("cams_suppressed", dccSuppressedCams);
                dc.put("cbr_samples", dccCbrSamples);
                dc.put("cbr_mean", dccCbrSamples > 0 ? round3(dccCbrSum / dccCbrSamples) : 0.0);
                dc.put("cbr_max", round3(dccCbrMax));
                counts.put("dcc", dc);
            }
            manifest.put("counts", counts);
            manifest.put("data_digest_sha256", hex(all.digest(), 32));
            manifest.put("outputs", digests);
            // Standards profile, corrected 2026-08-30 to match the Python engine's
            // run.py STANDARDS_PROFILE. The claim `cert: IEEE 1609.2` was NOT SUPPORTABLE and is
            // withdrawn: there is no 1609.2 certificate structure here, and NO SIGNING happens --
            // SignedCam carries a boolean and this backend passes a literal `true`. HashedId8 IS
            // real, so the claim is downgraded to an identifier-only claim naming it. `linkage`
            // is KEPT: LinkageEngine implements CAMP SCP2 for real. Two claims are STRONGER on
            // this engine than on the Python one and are stated separately rather than hidden
            // inside a vague "messaging": the EN 302 637-2 / TS 103 900 CAM GENERATION RULES
            // (ScmsBeaconApp) and TS 102 687 reactive DCC (Dcc.java) are genuinely implemented --
            // the message STRUCTURE and its ASN.1 encoding are not.
            // manifest.json is excluded from data_digest_sha256, so this moves zero digests.
            Map<String, String> stdProfile = new LinkedHashMap<>();
            stdProfile.put("linkage", "CAMP SCP2 -- implemented and enforced (crypto/LinkageEngine.java)");
            stdProfile.put("cert", "HashedId8 identifiers per IEEE 1609.2 6.4.3; NOT a 1609.2 certificate profile");
            stdProfile.put("security_envelope", "none -- SignedCam's signature flag is a simulated boolean; "
                    + "no signature is computed or verified");
            stdProfile.put("message", "engine-private CAM DTO over ITS-G5; no ASN.1 encoding. "
                    + "ETSI EN 302 637-2 / TS 103 900 GENERATION RULES are implemented "
                    + "(app/ScmsBeaconApp.java); the message STRUCTURE is not.");
            stdProfile.put("congestion_control", "ETSI TS 102 687 reactive DCC implemented "
                    + "(radio/Dcc.java); opt-in");
            stdProfile.put("report", "ETSI TS 103 759 V2.2.1: partial field-name correspondence only; "
                    + "not encoded, not signed, and carrying no v2xPduEvidence");
            manifest.put("standards_profile", stdProfile);
            Files.write(Paths.get(OUT_DIR, "manifest.json"),
                    (new GsonBuilder().setPrettyPrinting().create().toJson(manifest) + "\n")
                            .getBytes(StandardCharsets.UTF_8));
            System.out.println("[ScmsBackend] wrote dataset to " + OUT_DIR
                    + " (vehicles=" + (devByUnit.size() - nRsu)
                    + (nRsu > 0 ? " rsus=" + nRsu : "")
                    + " reports=" + maReports.size()
                    + " revoked=" + scms.crlg.size() + ")");
        } catch (Exception ex) {
            ex.printStackTrace();
        }
    }

    // ------------------------------------------------------- manifest parity (replayable runs)

    /** App-layer knobs, pushed once by ScmsBeaconApp so the manifest records the WHOLE Java config. */
    public void noteAppParams(Map<String, Object> params) {
        if (params == null) {
            return;
        }
        synchronized (appParams) {
            if (appParams.isEmpty()) {
                appParams.putAll(params);
            }
        }
    }

    /**
     * Every parameter this JVM actually resolved — defaults included — keyed by the environment
     * variable that sets it, so `manifest.effective_params` alone is enough to replay the run.
     */
    private Map<String, Object> effectiveParams() {
        Map<String, Object> p = new TreeMap<>();
        p.put("SCMS_SEED", MASTER_SEED);
        p.put("SCMS_JMAX", JMAX);
        p.put("SCMS_REPORT_K", REPORT_THRESHOLD_K);
        p.put("SCMS_ATTACKER_PCT", ATTACKER_PCT);
        p.put("SCMS_FAULTY_PCT", FAULTY_PCT);
        p.put("SCMS_REPORT_PROB", REPORT_PROB);
        p.put("SCMS_CRL_DELAY", CRL_PROP_DELAY_S);
        p.put("SCMS_INGEST_DELAY", INGEST_DELAY_S);
        p.put("SCMS_EMIT_SAMPLE", EMIT_SAMPLE);
        p.put("SCMS_MIN_SECONDS", REVOKE_MIN_SECONDS);
        p.put("SCMS_PERSIST_S", REVOKE_PERSIST_S);
        p.put("SCMS_MA_DEFENSE", MA_DEFENSE);
        p.put("SCMS_REPUTATION_MAX", REPUTATION_MAX);
        p.put("SCMS_REPORT_BUDGET", REPORT_BUDGET);
        p.put("SCMS_ROTATE_PERIOD", ROTATE_PERIOD_S);
        p.put("SCMS_COLLUDE_PCT", COLLUDE_PCT);
        p.put("SCMS_VICTIM_PCT", VICTIM_PCT);
        p.put("SCMS_FALSE_REPORT_INTERVAL", FALSE_REPORT_INTERVAL_S);
        p.put("SCMS_GPS_SIGMA", GPS_SIGMA_M);
        p.put("SCMS_GPS_BIAS", GPS_BIAS_M);
        p.put("SCMS_GPS_THETA", GPS_THETA);
        p.put("SCMS_SPEED_SIGMA", SPEED_SIGMA_MS);
        p.put("SCMS_HEADING_SIGMA", HEADING_SIGMA_DEG);
        p.put("SCMS_GPS_OUTLIER_RATE", GPS_OUTLIER_RATE);
        p.put("SCMS_GPS_OUTLIER_M", GPS_OUTLIER_M);
        p.put("SCMS_GPS_DEGRADE_RATE", GPS_DEGRADE_RATE);
        p.put("SCMS_GPS_DEGRADE_FACTOR", GPS_DEGRADE_FACTOR);
        p.put("SCMS_WEATHER", envStr("SCMS_WEATHER", "clear"));
        // Set by the generator, read by the SNS federate rather than by this JVM -- recorded here
        // because the manifest is the replay contract and a run is not reproducible without them.
        p.put("SCMS_RADIO_RANGE", envDouble("SCMS_RADIO_RANGE", 709.4));
        p.put("SCMS_RADIO_LOSS", envDouble("SCMS_RADIO_LOSS", 0.0));
        p.put("SCMS_SENSOR_MODEL", SENSOR_MODEL);
        p.put("SCMS_SENSOR_POS_ERR_M", SENSOR_POS_ERR_M);
        p.put("SCMS_SENSOR_SPEED_ERR", SENSOR_SPEED_ERR);
        p.put("SCMS_SENSOR_HEAD_ERR_DEG", SENSOR_HEAD_ERR_DEG);
        p.put("SCMS_SENSOR_POS_SIGMA_FRAC", SENSOR_POS_SIGMA_FRAC);
        p.put("SCMS_SENSOR_HEAD_DECAY", SENSOR_HEAD_DECAY);
        p.put("SCMS_DRIVER_PROFILES", DRIVER_PROFILES);
        p.put("SCMS_DRIVER_AGGRESSIVE_PCT", DRIVER_AGGRESSIVE_FRAC * 100.0);
        p.put("SCMS_DRIVER_PASSIVE_PCT", DRIVER_PASSIVE_FRAC * 100.0);
        p.put("SCMS_PSEUDONYM_POLICY", PSEUDONYM_POLICY);
        p.put("SCMS_PSN_DIST_MIN_M", PSN_DIST_MIN_M);
        p.put("SCMS_PSN_DIST_MAX_M", PSN_DIST_MAX_M);
        p.put("SCMS_PSN_TIME_MIN_S", PSN_TIME_MIN_S);
        p.put("SCMS_PSN_TIME_MAX_S", PSN_TIME_MAX_S);
        p.putAll(attackCfg.effective());
        synchronized (appParams) {
            p.putAll(appParams);
        }
        return p;
    }

    /** The SCMS_* environment as the launcher actually set it (output path excluded: machine-local). */
    private static Map<String, String> rawScmsEnv() {
        Map<String, String> env = new TreeMap<>();
        for (Map.Entry<String, String> e : System.getenv().entrySet()) {
            if (e.getKey().startsWith("SCMS_") && !"SCMS_OUT_DIR".equals(e.getKey())) {
                env.put(e.getKey(), e.getValue());
            }
        }
        return env;
    }

    /**
     * Scenario input-file hashes, read from a JSON side-car the scenario generator writes next to
     * scenario_config.json (or pointed at by SCMS_INPUTS_JSON). Contract (schema "scms.inputs/1"):
     *
     * <pre>{ "schema": "scms.inputs/1", "scenario_key": "...", "generator": "...",
     *   "params": {...}, "tool_versions": {...},
     *   "inputs": { "sumo/ingolstadt.net.xml": "&lt;sha256&gt;", ... } }</pre>
     *
     * The list form {@code "inputs": [{"path": ..., "sha256": ...}, ...]} is accepted too. Absent
     * file -> the key is simply omitted from the manifest, so nothing depends on the generator side.
     */
    private Map<String, Object> inputManifest() {
        for (Path p : inputManifestCandidates()) {
            try {
                if (p == null || !Files.isRegularFile(p)) {
                    continue;
                }
                byte[] raw = Files.readAllBytes(p);
                Map<?, ?> parsed = new Gson().fromJson(new String(raw, StandardCharsets.UTF_8), Map.class);
                if (parsed == null) {
                    continue;
                }
                Map<String, Object> out = new LinkedHashMap<>();
                out.put("source", p.toAbsolutePath().normalize().toString().replace('\\', '/'));
                out.put("inputs_file_sha256", sha256Hex(raw));
                for (String k : new String[] {"schema", "scenario_key", "generator", "generated_utc",
                        "params", "tool_versions"}) {
                    Object v = parsed.get(k);
                    if (v != null) {
                        out.put(k, v);
                    }
                }
                out.put("files", inputFileHashes(parsed.get("inputs")));
                return out;
            } catch (Exception ex) {
                System.err.println("[ScmsBackend] input manifest unreadable (" + p + "): " + ex);
            }
        }
        return null;
    }

    /**
     * One value out of the generator side-car's ``params`` block, as a string.
     *
     * <p>Used for the scenario facts the Java layer cannot know but a downstream consumer needs --
     * chiefly ``road_network``, which the generator derives from the actual SUMO net (see
     * mapgen.road_network_token) and which datagen.realism_bench turns into its reference-band
     * regime. Falls back to ``dflt`` when there is no side-car or no such key.
     */
    private static String inputParam(Map<String, Object> inputs, String key, String dflt) {
        Object params = (inputs == null) ? null : inputs.get("params");
        if (params instanceof Map) {
            Object v = ((Map<?, ?>) params).get(key);
            if (v != null && !String.valueOf(v).isBlank()) {
                return String.valueOf(v).trim();
            }
        }
        return dflt == null ? "" : dflt.trim();
    }

    private List<Path> inputManifestCandidates() {
        List<Path> cands = new ArrayList<>();
        String explicit = System.getenv("SCMS_INPUTS_JSON");
        if (explicit != null && !explicit.isBlank()) {
            cands.add(Paths.get(explicit.trim()));
        }
        java.io.File dir = scenarioDir;
        if (dir != null) {
            // MOSAIC hands the app its <scenario>/application directory, so also look one and two
            // levels up (the scenario root, where scenario_config.json lives).
            Path base = dir.toPath().toAbsolutePath().normalize();
            for (int up = 0; up < 3 && base != null; up++) {
                cands.add(base.resolve("scms_inputs.json"));
                base = base.getParent();
            }
        }
        return cands;
    }

    /** Normalise either {"path": "sha"} or [{"path":..,"sha256":..}] into a sorted path -> sha map. */
    private static Map<String, String> inputFileHashes(Object inputs) {
        Map<String, String> files = new TreeMap<>();
        if (inputs instanceof Map) {
            for (Map.Entry<?, ?> e : ((Map<?, ?>) inputs).entrySet()) {
                files.put(String.valueOf(e.getKey()), String.valueOf(e.getValue()));
            }
        } else if (inputs instanceof List) {
            for (Object o : (List<?>) inputs) {
                if (o instanceof Map) {
                    Map<?, ?> m = (Map<?, ?>) o;
                    Object path = m.get("path");
                    Object sha = m.get("sha256");
                    if (path != null && sha != null) {
                        files.put(String.valueOf(path), String.valueOf(sha));
                    }
                }
            }
        }
        return files;
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

    /**
     * Write one JSONL table and return its SHA-256, STREAMING row by row.
     *
     * <p>The previous form built the whole file as a single {@code StringBuilder} and then took a
     * second full copy as a byte array — roughly 6x the file size resident at once, all of it in the
     * JVM shutdown hook where there is no chance to recover. A run with heavy DoS flooding
     * (SCMS_FLOOD_BURST high, many attackers) produces enough report rows to exhaust the default
     * heap there, and the dataset is lost after the simulation has already completed. Digesting the
     * same bytes on the way to disk keeps memory flat and produces byte-identical output.
     */
    private String writeJsonl(Path path, List<Map<String, Object>> rows) throws IOException {
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-256");
            try (java.io.OutputStream out = new java.io.BufferedOutputStream(
                    Files.newOutputStream(path), 1 << 16)) {
                for (Map<String, Object> r : rows) {
                    byte[] line = (gson.toJson(r) + "\n").getBytes(StandardCharsets.UTF_8);
                    md.update(line);
                    out.write(line);
                }
            }
            return hex(md.digest(), 32);
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new IllegalStateException(e);
        }
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

    /** Heading into [0, 360) (0 = North, clockwise — SUMO/MOSAIC convention). */
    private static double norm360(double a) {
        double v = a % 360.0;
        return v < 0 ? v + 360.0 : v;
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
