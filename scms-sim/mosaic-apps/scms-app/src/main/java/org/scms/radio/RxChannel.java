/*
 * SPDX-License-Identifier: Apache-2.0
 * The receiver-side channel, shared by the vehicle and the road-side-unit application.
 *
 * Both SCMS receivers used to carry their own copy of the weather / NLOS / congestion loss chain
 * (ScmsBeaconApp.onMessageReceived and ScmsRsuApp.onMessageReceived), which is exactly the kind of
 * duplication that lets infrastructure evidence quietly come from a second, subtly different radio.
 * There is now one implementation and both call it.
 *
 * <h2>Ground-truth firewall</h2>
 * Every geometric quantity here comes from the back-end ORACLE (the sender's TRUE position and an
 * OPAQUE per-link key), never from {@code SignedCam.claimedX/Y}. A position-falsifying attacker
 * must not be able to steer its own reception probability: claiming a distant location would
 * otherwise invent packet loss the radio never applied, and a Sybil ghost claiming to be nearby
 * would be MORE reliably received than an honest neighbour. The oracle is consumed for channel
 * physics only -- the one value that escapes is {@link #lastRssiDbm()}, which is a quantity a real
 * receiver genuinely measures and is therefore legitimately MA-visible.
 *
 * <h2>Models</h2>
 * {@code SCMS_RADIO_MODEL=sns} (DEFAULT, unchanged behaviour): MOSAIC's SNS decides reception, and
 * this class applies the legacy weather / distance-ramp NLOS / CSMA-contention drops in the same
 * order and off the same RNG as before -- byte-for-byte the pre-Phase-2 path, and it draws no new
 * randomness anywhere.
 *
 * {@code SCMS_RADIO_MODEL=geometric} (OPT-IN): the distance-ramp NLOS heuristic is replaced by a
 * real link budget. Building footprints ({@link BuildingIndex}) classify each link LOS or NLOSb,
 * 3GPP TR 37.885 ({@link PathLoss}) gives the large-scale loss, and a Gudmundson AR(1) process
 * carried PER LINK -- keyed on the opaque true-vehicle key, so a pseudonym rotation does not
 * resample the channel -- gives correlated shadowing. The frame is delivered when
 * {@code rssi >= sensitivity}. Weather and congestion still apply on top, off the original RNG, so
 * even the geometric branch leaves the legacy draw sequence intact.
 *
 * Default link budget follows the vendored VeReMi-NextGen omnetpp.ini (20 mW = 13.01 dBm,
 * sensitivity -81 dBm, 5.9 GHz, 10 MHz), which the roadmap adopts as calibration constants. That
 * budget of 94.01 dB gives a median urban-LOS range of 294 m and a median urban-NLOSb range of
 * 26 m; with sigma = 3 dB LOS shadowing the delivery probability falls from 90% to 20% over roughly
 * 160-390 m, i.e. a gray zone far wider than the 100 m the awareness reference data demands.
 */
package org.scms.radio;

import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Random;

import org.scms.backend.ScmsBackend;

public final class RxChannel {

    // ------------------------------------------------------------------ configuration
    /** {@code sns} = today's behaviour; {@code geometric} = 3GPP TR 37.885 link budget (opt-in). */
    public static final String RADIO_MODEL = env("SCMS_RADIO_MODEL", "sns");
    public static final boolean GEOMETRIC = "geometric".equals(RADIO_MODEL);
    /** {@code urban} selects the TR 37.885 urban LOS/NLOS pair, {@code highway} the highway LOS form. */
    public static final boolean URBAN = !"highway".equals(env("SCMS_RADIO_REGIME", "urban"));
    /** Consult building footprints for the LOS/NLOSb decision (geometric model only). */
    public static final boolean USE_BUILDINGS = envI("SCMS_BUILDINGS", 1) != 0;
    /** 20 mW, the VeReMi-NextGen omnetpp.ini transmitter power. */
    public static final double TX_POWER_DBM = envD("SCMS_TX_POWER_DBM", 10.0 * Math.log10(20.0));
    public static final double RX_SENSITIVITY_DBM = envD("SCMS_RX_SENSITIVITY_DBM", -81.0);
    public static final double ANTENNA_GAIN_DBI = envD("SCMS_ANTENNA_GAIN_DBI", 0.0);
    public static final double FC_GHZ = envD("SCMS_CARRIER_GHZ", PathLoss.FC_GHZ);
    public static final double BUILDING_CELL_M = envD("SCMS_BUILDING_CELL_M", 50.0);

    /**
     * Weather attenuation of the 802.11p link, as a per-frame drop probability.
     *
     * <p>UNIFIED with the Python engine: these are exactly {@code WEATHER_RADIO_LOSS} from
     * src/scms_sim_ref/mock_pipeline/run.py:138. Before Phase 2 the Java layer carried its own
     * table (rain 0.05 / fog 0.03 / snow 0.10) which disagreed with Python's on every entry, so the
     * same {@code weather=rain} scenario produced a materially different packet-delivery ratio
     * depending on which engine generated it.
     */
    private static final Map<String, Double> WEATHER_RADIO_LOSS = Map.of(
            "clear", 0.0, "rain", 0.03, "fog", 0.02, "snow", 0.06);
    public static final double WEATHER_DROP = WEATHER_RADIO_LOSS.getOrDefault(
            env("SCMS_WEATHER", "clear"), 0.0);

    /** Legacy distance-ramp NLOS heuristic (0 = off). Ignored by the geometric model. */
    public static final double NLOS_INTENSITY = envD("SCMS_NLOS", 0.0);
    public static final int CHAN_CAPACITY = envI("SCMS_CHAN_CAPACITY", 25);
    private static final double CHAN_WINDOW_S = 0.1;

    /** ETSI reactive DCC on the modelled CBR (0 = off, the default). */
    public static final boolean DCC_ENABLED = envI("SCMS_DCC", 0) != 0;
    public static final int DCC_FRAME_BYTES = envI("SCMS_DCC_FRAME_BYTES", 300);
    public static final double DCC_DATA_RATE_MBPS = envD("SCMS_DCC_DATA_RATE_MBPS", 6.0);
    public static final double DCC_PROBE_S = envD("SCMS_DCC_PROBE_S", 0.1);
    public static final double DCC_WINDOW_S = envD("SCMS_DCC_WINDOW_S", 1.0);
    public static final double DCC_STATE_HOLD_S = envD("SCMS_DCC_STATE_HOLD_S", 1.0);
    /** 0 = CBR from PPDU airtime alone (ETSI "medium sensed busy"); 207.5 adds AIFS+backoff. */
    public static final double DCC_MAC_OVERHEAD_US = envD("SCMS_DCC_MAC_OVERHEAD_US", 0.0);

    // ------------------------------------------------------------------ shared building index
    private static volatile BuildingIndex buildings;
    private static volatile boolean buildingsResolved;
    private static volatile String buildingsNote = "not loaded";

    /**
     * Load the scenario's building footprints once per JVM. Safe (and cheap) to call from every
     * unit's {@code onStartup}; only the first call parses.
     */
    public static synchronized BuildingIndex buildings(java.io.File scenarioDir) {
        if (buildingsResolved) {
            return buildings;
        }
        if (!GEOMETRIC || !USE_BUILDINGS) {
            buildingsResolved = true;
            buildingsNote = !GEOMETRIC ? "radio_model=" + RADIO_MODEL : "SCMS_BUILDINGS=0";
            return null;
        }
        double[] off = buildingOffset();
        buildings = BuildingIndex.load(scenarioDir, BUILDING_CELL_M, off[0], off[1]);
        buildingsResolved = true;
        if (buildings == null) {
            buildingsNote = "no buildings.poly.xml found (all links classify LOS)";
            System.out.println("[RxChannel] geometric radio: " + buildingsNote);
        } else {
            buildingsNote = buildings.source();
            System.out.println("[RxChannel] geometric radio: " + buildings.ringCount()
                    + " building footprints (" + buildings.vertexCount() + " vertices) indexed on a "
                    + BUILDING_CELL_M + " m grid in " + buildings.parseMillis() + " ms from "
                    + buildings.source() + "; link budget " + fmt(linkBudgetDb())
                    + " dB -> median range LOS " + fmt(PathLoss.medianRangeM(linkBudgetDb(), URBAN, true, FC_GHZ))
                    + " m / NLOSb " + fmt(PathLoss.medianRangeM(linkBudgetDb(), URBAN, false, FC_GHZ)) + " m");
        }
        return buildings;
    }

    public static BuildingIndex buildingsOrNull() {
        return buildings;
    }

    public static String buildingsNote() {
        return buildingsNote;
    }

    public static double linkBudgetDb() {
        return TX_POWER_DBM + 2.0 * ANTENNA_GAIN_DBI - RX_SENSITIVITY_DBM;
    }

    /** {@code SCMS_BUILDING_OFFSET="dx,dy"} corrects a scenario whose net and MOSAIC offsets differ. */
    private static double[] buildingOffset() {
        String s = System.getenv("SCMS_BUILDING_OFFSET");
        if (s == null || s.isBlank()) {
            return new double[] {0.0, 0.0};
        }
        String[] p = s.trim().split("\\s*,\\s*");
        try {
            return new double[] {Double.parseDouble(p[0]), Double.parseDouble(p.length > 1 ? p[1] : "0")};
        } catch (RuntimeException ex) {
            System.err.println("[RxChannel] bad SCMS_BUILDING_OFFSET=" + s + " (want \"dx,dy\"): " + ex);
            return new double[] {0.0, 0.0};
        }
    }

    // ------------------------------------------------------------------ per-receiver state
    private final Random chanRng;
    /**
     * Channel-load meter. Constructed for EVERY receiver, not only when {@code SCMS_DCC=1}: the
     * channel busy ratio is a MEASUREMENT of the scene, and a DCC-off run that reports no CBR at all
     * cannot be compared against a DCC-on one -- the on/off delta would be a difference between a
     * number and a blank. {@link #DCC_ENABLED} decides only whether the resulting interval floor is
     * APPLIED to CAM generation ({@link #dccMinIntervalS}), never whether CBR is observed.
     */
    private final Dcc dcc;
    /** Deterministic per-receiver salt for the shadowing streams: SHA-256(label|seed|unitId). */
    private final long shadowSalt;
    /** This receiver's unit id, carried only so the per-link trace can name it. */
    private final String unitId;
    /**
     * Dedicated sampling stream for {@link LinkTrace}. Seeded exactly like the shadowing streams and
     * touched only when the trace is enabled, so switching tracing on cannot move a delivery verdict.
     */
    private final Random traceRng;

    private double chanWinStart = Double.NEGATIVE_INFINITY;
    private int chanCount;
    private int chanLoad;

    private final Map<Long, Link> links = GEOMETRIC ? new HashMap<>() : null;
    private double lastRssiDbm = Double.NaN;
    private boolean lastLos = true;
    private double lastDistM = Double.NaN;
    private double lastTxX = Double.NaN;
    private double lastTxY = Double.NaN;

    private long sensed;
    private long delivered;
    private long droppedWeather;
    private long droppedGeometric;
    private long droppedCongestion;
    private long nlosbLinks;

    /** AR(1) shadowing state for one (transmitter vehicle, this receiver) link. */
    private static final class Link {
        final Random rng;
        double z = Double.NaN;       // unit-variance shadowing state
        double txX, txY, rxX, rxY;   // endpoint positions at the previous update
        boolean havePrev;
        Link(Random rng) {
            this.rng = rng;
        }
    }

    public RxChannel(String unitId) {
        // Identical seeding to the pre-Phase-2 per-app RNG, so the legacy draw sequence is unchanged.
        this.chanRng = new Random(0x9E3779B97F4A7C15L ^ (long) unitId.hashCode());
        this.shadowSalt = ScmsBackend.streamSeed("channel-shadow", unitId);
        this.unitId = unitId;
        this.dcc = new Dcc(DCC_FRAME_BYTES, DCC_DATA_RATE_MBPS, DCC_PROBE_S, DCC_WINDOW_S,
                DCC_STATE_HOLD_S, DCC_MAC_OVERHEAD_US);
        this.traceRng = LinkTrace.enabled()
                ? new Random(ScmsBackend.streamSeed("link-trace", unitId))
                : null;
    }

    // ------------------------------------------------------------------ reception

    /**
     * Decide whether this receiver decodes a frame. Call once per received CAM, before any
     * detector runs.
     *
     * @param senderDigest the sender's pseudonym (used ONLY to look the true emitter up in the
     *                     back-end oracle; the frame's claimed content is never consulted)
     * @param t            simulation time (s)
     * @param selfX,selfY  this receiver's own position (its own state, not an oracle read)
     * @param haveSelf     false when the receiver has no resolvable position yet
     * @return true if the frame survives the channel
     */
    public boolean deliver(String senderDigest, double t, double selfX, double selfY, boolean haveSelf) {
        sensed++;
        dcc.sense(t);   // CBR counts everything the PHY hears, before any decode-side drop
        lastRssiDbm = Double.NaN;
        lastDistM = Double.NaN;
        // Sampling decision taken FIRST and off a dedicated stream, so the trace never reorders the
        // channel RNG and a traced run and an untraced one deliver exactly the same frames.
        boolean trace = traceRng != null && (LinkTrace.PROB >= 1.0 || traceRng.nextDouble() < LinkTrace.PROB);
        // 1) weather attenuation (unified table; same RNG position as before)
        if (WEATHER_DROP > 0 && chanRng.nextDouble() < WEATHER_DROP) {
            droppedWeather++;
            if (trace) {
                LinkTrace.row(t, unitId, lastDistM, "NA", lastRssiDbm, "wx",
                        selfX, selfY, lastTxX, lastTxY);
            }
            return false;
        }
        // 2) obstruction. Geometry always from the sender's TRUE position (back-end ORACLE).
        // A receiver with no resolvable position of its own (haveSelf == false: an RSU the mapping
        // gave no coordinates, or a vehicle before its first mobility update) has no link geometry
        // to evaluate, so the frame passes. That is the same degradation the legacy NLOS stage
        // already applied, and it fails OPEN -- never toward using the claimed position instead.
        if (GEOMETRIC) {
            if (haveSelf && !geometricDeliver(senderDigest, t, selfX, selfY)) {
                droppedGeometric++;
                if (trace) {
                    LinkTrace.row(t, unitId, lastDistM, lastLos ? "LOS" : "NLOSb", lastRssiDbm,
                            "geom", selfX, selfY, lastTxX, lastTxY);
                }
                return false;
            }
        } else if (NLOS_INTENSITY > 0 && haveSelf) {
            double[] txTrue = ScmsBackend.instance().truePositionOf(senderDigest);
            if (txTrue != null) {
                double dist = Math.hypot(txTrue[0] - selfX, txTrue[1] - selfY);
                double pn = NLOS_INTENSITY * Math.min(1.0, Math.max(0.0, (dist - 150.0) / 300.0));
                if (pn > 0 && chanRng.nextDouble() < pn) {
                    droppedGeometric++;
                    return false;
                }
            }
        }
        // 3) CSMA/CA contention loss on the modelled 100 ms channel load
        if (t - chanWinStart >= CHAN_WINDOW_S) {
            chanWinStart = t;
            chanLoad = chanCount;
            chanCount = 0;
        }
        chanCount++;
        if (CHAN_CAPACITY > 0 && chanLoad > CHAN_CAPACITY) {
            double pDrop = Math.min(0.95, (double) (chanLoad - CHAN_CAPACITY) / CHAN_CAPACITY);
            if (chanRng.nextDouble() < pDrop) {
                droppedCongestion++;
                if (trace) {
                    LinkTrace.row(t, unitId, lastDistM, linkState(), lastRssiDbm, "cong",
                            selfX, selfY, lastTxX, lastTxY);
                }
                return false;
            }
        }
        delivered++;
        if (trace) {
            LinkTrace.row(t, unitId, lastDistM, linkState(), lastRssiDbm, "ok",
                    selfX, selfY, lastTxX, lastTxY);
        }
        return true;
    }

    /** LOS/NLOSb label for the trace; {@code NA} outside the geometric model, where none was taken. */
    private String linkState() {
        return !GEOMETRIC || Double.isNaN(lastDistM) ? "NA" : (lastLos ? "LOS" : "NLOSb");
    }

    /**
     * 3GPP TR 37.885 link budget with a building-derived LOS/NLOSb state and AR(1) shadowing.
     *
     * <p>All randomness comes from a dedicated stream keyed on
     * {@code (scenario seed, opaque true-vehicle key, this receiver)} -- never the global channel
     * RNG -- so switching the model on cannot perturb the legacy draw sequence.
     */
    private boolean geometricDeliver(String senderDigest, double t, double rxX, double rxY) {
        ScmsBackend backend = ScmsBackend.instance();
        double[] txTrue = backend.truePositionOf(senderDigest);
        if (txTrue == null) {
            return true;   // emitter not on the oracle yet: never fall back to the CLAIMED position
        }
        BuildingIndex bi = buildings;
        boolean los = true;
        if (bi != null) {
            bi.probe(rxX, rxY);
            los = !bi.blocked(txTrue[0], txTrue[1], rxX, rxY);
            if (!los) {
                nlosbLinks++;
            }
        }
        double d = Math.hypot(txTrue[0] - rxX, txTrue[1] - rxY);
        lastDistM = d;
        lastTxX = txTrue[0];
        lastTxY = txTrue[1];
        double pl;
        if (!URBAN) {
            pl = PathLoss.highwayLos(d, FC_GHZ);
        } else if (los) {
            pl = PathLoss.urbanLos(d, FC_GHZ);
        } else {
            pl = PathLoss.urbanNlos(d, FC_GHZ);
        }
        // Keyed on (scenario seed, opaque TRUE-vehicle key, this receiver) -- never on the digest, so
        // a pseudonym rotation continues the SAME shadowing process instead of resampling it.
        Link link = links.computeIfAbsent(backend.channelLinkKey(senderDigest),
                k -> new Link(new Random(k ^ shadowSalt)));
        double shadow = shadowDb(link, los, txTrue[0], txTrue[1], rxX, rxY);
        lastLos = los;
        lastRssiDbm = TX_POWER_DBM + 2.0 * ANTENNA_GAIN_DBI - pl - shadow;
        return lastRssiDbm >= RX_SENSITIVITY_DBM;
    }

    /**
     * Gudmundson AR(1) shadowing carried per link: {@code z <- rho*z + sqrt(1-rho^2)*N(0,1)} with
     * {@code rho = exp(-dd / d_corr)}, where {@code dd} is how far the two endpoints moved since the
     * last frame on this link. Keeping a unit-variance state and scaling by sigma at use time means a
     * LOS<->NLOSb flip changes the magnitude (3 vs 4 dB) without discontinuously resampling.
     *
     * <p>The old model drew i.i.d. per step AND keyed the draw on the CERT DIGEST, so a pseudonym
     * rotation resampled the channel. Here the key is the opaque true-vehicle key, which is stable
     * across rotation and identical for a Sybil ghost and its puppeteer -- both are the same radio.
     */
    private double shadowDb(Link link, boolean los, double txX, double txY, double rxX, double rxY) {
        double sigma = PathLoss.shadowSigmaDb(los);
        if (Double.isNaN(link.z)) {
            link.z = link.rng.nextGaussian();
        } else {
            double dd = link.havePrev
                    ? Math.hypot(txX - link.txX, txY - link.txY) + Math.hypot(rxX - link.rxX, rxY - link.rxY)
                    : Double.POSITIVE_INFINITY;
            double rho = Math.exp(-dd / PathLoss.decorrelationM(los));
            link.z = rho * link.z + Math.sqrt(Math.max(0.0, 1.0 - rho * rho)) * link.rng.nextGaussian();
        }
        link.txX = txX;
        link.txY = txY;
        link.rxX = rxX;
        link.rxY = rxY;
        link.havePrev = true;
        return sigma * link.z;
    }

    /**
     * RSSI (dBm) of the frame most recently passed to {@link #deliver}, or NaN outside the geometric
     * model. This is the extension point for the planned {@code rssi_dbm} evidence observable: it is
     * a quantity a real receiver genuinely measures, so it is legitimately MA-VISIBLE even though it
     * is computed from true geometry, and it is exactly what makes an RSSI-vs-claimed-distance
     * detector possible (a Sybil ghost inherits its puppeteer's true-position RSSI).
     *
     * <p>NOT yet written into {@code ma_reports}, and the reason is a trap worth recording: the
     * colluding-attacker path files reports through {@code ScmsBackend.maybeCollude} WITHOUT having
     * received a frame. If honest reports carried an RSSI and fabricated ones did not, "rssi_dbm is
     * absent" would be a perfect, unintended oracle for a false accusation -- the collusion attack
     * would become trivially detectable for entirely the wrong reason. Landing this field requires
     * the collusion path to synthesise a plausible RSSI first.
     */
    public double lastRssiDbm() {
        return lastRssiDbm;
    }

    /** LOS state of the most recent geometric evaluation. ORACLE-derived -- diagnostics and channel
     *  physics only; it must never reach a report or a feature. */
    public boolean lastLos() {
        return lastLos;
    }

    // ------------------------------------------------------------------ DCC (transmit side)

    /**
     * The reactive-DCC CAM interval floor for this station right now, or 0 when DCC is off.
     *
     * <p>The meter runs either way -- {@link Dcc#currentMinIntervalS} is what latches the DCC state
     * and accumulates the CBR statistics, so calling it unconditionally is what makes a DCC-off run
     * report the same CBR observable a DCC-on run reports. Only the returned FLOOR is gated: with
     * {@code SCMS_DCC=0} this returns 0.0 and {@code ScmsBeaconApp} keeps the bare ETSI
     * T_GenCamMin, exactly as before.
     */
    public double dccMinIntervalS(double t) {
        double floorS = dcc.currentMinIntervalS(t);
        return DCC_ENABLED ? floorS : 0.0;
    }

    public double dccCbr() {
        return dcc.currentCbr();
    }

    public double dccRateHz() {
        return dcc.currentRateHz();
    }

    public void dccNoteAllowed() {
        dcc.noteAllowed();
    }

    public void dccNoteSuppressed() {
        dcc.noteSuppressed();
    }

    /** Push this receiver's channel counters into the back-end run totals (call at shutdown). */
    public void publishStats() {
        ScmsBackend backend = ScmsBackend.instance();
        backend.noteChannel(sensed, delivered, droppedWeather, droppedGeometric,
                droppedCongestion, nlosbLinks,
                dcc.allowedCount(), dcc.suppressedCount(),
                dcc.cbrSamples(), dcc.cbrSum(), dcc.cbrMax());
        // The footprint index is a JVM singleton, so its live counters and the projection-alignment
        // verdict are global; every receiver publishes the same snapshot and the last one wins.
        BuildingIndex bi = buildings;
        if (bi != null) {
            backend.noteChannelIndex(bi.stats());
        }
    }

    // ------------------------------------------------------------------ manifest

    /** Radio/DCC knobs as resolved by this JVM, for manifest.effective_params (replay parity). */
    public static Map<String, Object> params() {
        Map<String, Object> p = new LinkedHashMap<>();
        p.put("SCMS_RADIO_MODEL", RADIO_MODEL);
        p.put("SCMS_RADIO_REGIME", URBAN ? "urban" : "highway");
        p.put("SCMS_CHAN_CAPACITY", CHAN_CAPACITY);
        p.put("SCMS_NLOS", NLOS_INTENSITY);
        p.put("SCMS_WEATHER_RADIO_LOSS", WEATHER_DROP);
        p.put("SCMS_DCC", DCC_ENABLED);
        // The CBR meter's constants are published whether or not DCC acts on them: with SCMS_DCC=0
        // counts.dcc still carries a measured cbr_mean/cbr_max, and a reader has to be able to see
        // the airtime and window that number was computed with.
        p.put("SCMS_DCC_FRAME_BYTES", DCC_FRAME_BYTES);
        p.put("SCMS_DCC_DATA_RATE_MBPS", DCC_DATA_RATE_MBPS);
        p.put("SCMS_DCC_PROBE_S", DCC_PROBE_S);
        p.put("SCMS_DCC_WINDOW_S", DCC_WINDOW_S);
        p.put("SCMS_DCC_STATE_HOLD_S", DCC_STATE_HOLD_S);
        p.put("SCMS_DCC_MAC_OVERHEAD_US", DCC_MAC_OVERHEAD_US);
        p.put("dcc_frame_airtime_s", round6(Dcc.airtimeSeconds(DCC_FRAME_BYTES, DCC_DATA_RATE_MBPS)
                + DCC_MAC_OVERHEAD_US * 1e-6));
        p.put("dcc_reference", "ETSI TS 102 687 reactive DCC (refdata/etsi_cam_dcc.json,"
                + " refdata/phy_80211p_profile.json)"
                + (DCC_ENABLED ? "" : " -- MEASURED ONLY, the rate floor is not applied"));
        if (LinkTrace.enabled()) {
            p.put("SCMS_LINK_TRACE", LinkTrace.path());
            p.put("SCMS_LINK_TRACE_PROB", LinkTrace.PROB);
        }
        if (GEOMETRIC) {
            p.put("SCMS_BUILDINGS", USE_BUILDINGS);
            p.put("SCMS_TX_POWER_DBM", round3(TX_POWER_DBM));
            p.put("SCMS_RX_SENSITIVITY_DBM", RX_SENSITIVITY_DBM);
            p.put("SCMS_ANTENNA_GAIN_DBI", ANTENNA_GAIN_DBI);
            p.put("SCMS_CARRIER_GHZ", FC_GHZ);
            p.put("SCMS_BUILDING_CELL_M", BUILDING_CELL_M);
            p.put("pathloss_reference", "3GPP TR 37.885 (refdata/pathloss_3gpp_tr37885.json)");
            p.put("link_budget_db", round3(linkBudgetDb()));
            p.put("median_range_los_m", round3(PathLoss.medianRangeM(linkBudgetDb(), URBAN, true, FC_GHZ)));
            p.put("median_range_nlosb_m", round3(PathLoss.medianRangeM(linkBudgetDb(), URBAN, false, FC_GHZ)));
            // Static description only: effective_params is snapshotted at start-up, so the live
            // query counters belong in counts.channel.buildings (written at shutdown) instead.
            BuildingIndex bi = buildings;
            p.put("buildings", bi == null ? buildingsNote : bi.describe());
        }
        return p;
    }

    // ------------------------------------------------------------------ helpers
    private static String env(String n, String d) {
        String e = System.getenv(n);
        return (e != null && !e.isBlank()) ? e.trim().toLowerCase(java.util.Locale.ROOT) : d;
    }

    private static int envI(String n, int d) {
        String e = System.getenv(n);
        try {
            return (e != null && !e.isBlank()) ? Integer.parseInt(e.trim()) : d;
        } catch (NumberFormatException ex) {
            return d;
        }
    }

    private static double envD(String n, double d) {
        String e = System.getenv(n);
        try {
            return (e != null && !e.isBlank()) ? Double.parseDouble(e.trim()) : d;
        } catch (NumberFormatException ex) {
            return d;
        }
    }

    private static String fmt(double v) {
        return String.format(java.util.Locale.ROOT, "%.1f", v);
    }

    private static double round3(double v) {
        return Math.round(v * 1000.0) / 1000.0;
    }

    private static double round6(double v) {
        return Math.round(v * 1e6) / 1e6;
    }
}
