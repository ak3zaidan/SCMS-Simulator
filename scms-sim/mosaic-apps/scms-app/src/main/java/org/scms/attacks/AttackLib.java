/*
 * SPDX-License-Identifier: Apache-2.0
 * Attack library — the misbehaviour taxonomy and per-CAM content falsification.
 *
 * Two layers:
 *   1. BASE BEHAVIOURS (~30) — the distinct falsification mechanisms, grouped into families:
 *        position: ConstPos, ConstPosOffset, ConstPosOffsetX/Y, RandomPos, RandomPosOffset,
 *                  Teleport, SineWavePos
 *        speed:    ConstSpeedZero, ConstSpeedOffset, ConstSpeedHigh, RandomSpeed,
 *                  RandomSpeedOffset, StopAndGo
 *        heading:  ReversedHeading, RandomHeading, PerpendicularHeading, HeadingOffset
 *        combined: Disruptive, PosSpeedInconsistent, PosHeadingInconsistent, EventualStop
 *        timing:   DataReplay, DelayedMessages, OutOfOrder, DoS, DoSRandom
 *        identity: Sybil
 *   2. A VARIANT MATRIX — each base is composed with a temporal PROFILE
 *        (constant / gradual / intermittent / delayed) and a magnitude TIER
 *        (small / med / large / huge). Enumerating the matrix yields a deterministic
 *        catalogue of ~300 concrete, distinct attack variants (see CATALOG).
 *
 * Config-driven via env SCMS_ATTACKS = "all" (default) or a comma list of families
 * ("position"), bases ("ConstPosOffset"), or exact variant names
 * ("ConstPosOffset/gradual/large"). Each attacker is deterministically assigned one enabled
 * variant. compute() returns the claimed CAM content (plus timing/flood/sybil flags) for one
 * broadcast; per-attacker mutable state (frozen position, replay snapshot, RNG) lives in
 * State so runs stay reproducible.
 */
package org.scms.attacks;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Random;

public final class AttackLib {

    // --- base behaviours, by family ---
    static final String[] FAM_POSITION = {
            "ConstPos", "ConstPosOffset", "ConstPosOffsetX", "ConstPosOffsetY",
            "RandomPos", "RandomPosOffset", "Teleport", "SineWavePos"};
    static final String[] FAM_SPEED = {
            "ConstSpeedZero", "ConstSpeedOffset", "ConstSpeedHigh",
            "RandomSpeed", "RandomSpeedOffset", "StopAndGo"};
    static final String[] FAM_HEADING = {
            "ReversedHeading", "RandomHeading", "PerpendicularHeading", "HeadingOffset"};
    static final String[] FAM_COMBINED = {
            "Disruptive", "PosSpeedInconsistent", "PosHeadingInconsistent", "EventualStop"};
    static final String[] FAM_TIMING = {
            "DataReplay", "DelayedMessages", "OutOfOrder", "DoS", "DoSRandom"};
    static final String[] FAM_IDENTITY = {"Sybil"};

    /** Every base behaviour (for the GUI picker and family/base expansion). */
    public static final List<String> BASES = new ArrayList<>();
    /** Family name -> its base behaviours. */
    public static final Map<String, List<String>> FAMILIES = new LinkedHashMap<>();
    static final Map<String, String> BASE_FAMILY = new LinkedHashMap<>();

    // --- variant-matrix axes ---
    // Bases whose magnitude scales meaningfully: full profile x tier matrix.
    static final String[] MAG_BASES = {
            "ConstPosOffset", "ConstPosOffsetX", "ConstPosOffsetY", "RandomPos", "RandomPosOffset",
            "ConstSpeedOffset", "RandomSpeed", "RandomSpeedOffset", "HeadingOffset",
            "PosSpeedInconsistent", "PosHeadingInconsistent", "Disruptive", "DoSRandom",
            "Teleport", "SineWavePos"};
    static final String[] PROFILES_MAG = {"constant", "gradual", "intermittent", "delayed"};
    static final String[] TIERS_MAG = {"small", "med", "large", "huge"};
    // Structural bases (mechanism is temporal, not a scalar): profile only, one tier.
    static final String[] STRUCT_BASES = {
            "ConstPos", "EventualStop", "StopAndGo", "ConstSpeedZero", "ConstSpeedHigh",
            "ReversedHeading", "RandomHeading", "PerpendicularHeading", "DataReplay",
            "OutOfOrder", "DoS"};
    static final String[] PROFILES_STRUCT = {"constant", "delayed", "intermittent"};
    // Structural bases carrying a scalar (ghost count / delay length): profile x 3 tiers.
    static final String[] STRUCT_SCALAR_BASES = {"Sybil", "DelayedMessages"};
    static final String[] TIERS_SCALAR = {"small", "med", "large"};

    /** Ordered catalogue of every concrete variant name. */
    public static final List<String> CATALOG = new ArrayList<>();
    private static final Map<String, Variant> RESOLVED = new LinkedHashMap<>();

    static {
        for (Object[] fam : new Object[][] {
                {"position", FAM_POSITION}, {"speed", FAM_SPEED}, {"heading", FAM_HEADING},
                {"combined", FAM_COMBINED}, {"timing", FAM_TIMING}, {"identity", FAM_IDENTITY}}) {
            String fname = (String) fam[0];
            List<String> bases = List.of((String[]) fam[1]);
            FAMILIES.put(fname, bases);
            for (String b : bases) {
                BASES.add(b);
                BASE_FAMILY.put(b, fname);
            }
        }
        for (String b : MAG_BASES) {
            for (String p : PROFILES_MAG) {
                for (String t : TIERS_MAG) {
                    add(b, p, t);
                }
            }
        }
        for (String b : STRUCT_BASES) {
            for (String p : PROFILES_STRUCT) {
                add(b, p, "std");
            }
        }
        for (String b : STRUCT_SCALAR_BASES) {
            for (String p : PROFILES_STRUCT) {
                for (String t : TIERS_SCALAR) {
                    add(b, p, t);
                }
            }
        }
    }

    private static void add(String base, String profile, String tier) {
        String name = base + "/" + profile + "/" + tier;
        Variant v = new Variant(name, base, BASE_FAMILY.getOrDefault(base, "other"), profile, tier);
        CATALOG.add(name);
        RESOLVED.put(name, v);
    }

    private AttackLib() {
    }

    /** Resolve a variant (or bare base name) to its parsed Variant. */
    public static Variant resolve(String name) {
        Variant v = RESOLVED.get(name);
        if (v != null) {
            return v;
        }
        // Accept a bare base name (legacy) -> its canonical constant/med|std variant.
        String tier = arrayHas(MAG_BASES, name) ? "med" : "std";
        String canon = name + "/constant/" + tier;
        v = RESOLVED.get(canon);
        if (v != null) {
            return v;
        }
        return new Variant(name, name, BASE_FAMILY.getOrDefault(name, "other"), "constant", "med");
    }

    public static String baseOf(String name) {
        return resolve(name).base;
    }

    /** Enabled variant names from env SCMS_ATTACKS ("all", family, base, or exact variant). */
    public static List<String> enabled() {
        String e = System.getenv("SCMS_ATTACKS");
        if (e == null || e.isBlank() || e.equalsIgnoreCase("all")) {
            return CATALOG;
        }
        List<String> out = new ArrayList<>();
        for (String raw : e.split(",")) {
            String tok = raw.trim();
            if (tok.isEmpty()) {
                continue;
            }
            if (FAMILIES.containsKey(tok.toLowerCase())) {
                List<String> bases = FAMILIES.get(tok.toLowerCase());
                for (String name : CATALOG) {
                    if (bases.contains(resolve(name).base)) {
                        out.add(name);
                    }
                }
            } else if (RESOLVED.containsKey(tok)) {
                out.add(tok);   // exact variant
            } else {
                for (String name : CATALOG) {   // a base name -> all its variants
                    if (resolve(name).base.equalsIgnoreCase(tok)) {
                        out.add(name);
                    }
                }
            }
        }
        return out.isEmpty() ? CATALOG : out;
    }

    /** Deterministically assign one enabled variant to an attacker. */
    public static String assign(String unitId, long seed, List<String> en) {
        int idx = Math.floorMod(hash("atktype|" + seed + "|" + unitId), en.size());
        return en.get(idx);
    }

    private static boolean arrayHas(String[] a, String s) {
        for (String x : a) {
            if (x.equals(s)) {
                return true;
            }
        }
        return false;
    }

    private static int hash(String s) {
        int h = 0x811c9dc5;
        for (byte b : s.getBytes(StandardCharsets.UTF_8)) {
            h ^= (b & 0xff);
            h *= 0x01000193;
        }
        return h;
    }

    /** A parsed variant: base behaviour + temporal profile + magnitude tier. */
    public static final class Variant {
        public final String name, base, family, profile, tier;
        public final double scale;

        Variant(String name, String base, String family, String profile, String tier) {
            this.name = name; this.base = base; this.family = family;
            this.profile = profile; this.tier = tier;
            switch (tier) {
                case "small": this.scale = 0.4; break;
                case "large": this.scale = 2.5; break;
                case "huge":  this.scale = 6.0; break;
                default:      this.scale = 1.0; break;   // med / std
            }
        }
    }

    /** Tunable attack magnitudes + temporal-profile parameters (from env, with defaults). */
    public static final class Cfg {
        public double offsetM = env("SCMS_OFFSET_M", 1500.0);
        public double speedOffsetMs = env("SCMS_SPEED_OFFSET", 15.0);
        public double randomRadiusM = env("SCMS_RANDOM_RADIUS", 2000.0);
        public double randomSpeedMax = env("SCMS_RANDOM_SPEED_MAX", 40.0);
        public double headingOffsetDeg = env("SCMS_HEADING_OFFSET", 45.0);
        public int freezeAfter = (int) env("SCMS_FREEZE_UPDATES", 5);
        public double staleDelayS = env("SCMS_STALE_DELAY", 8.0);
        public int sybilGhosts = (int) env("SCMS_SYBIL_GHOSTS", 4);
        // temporal-profile parameters
        public int rampUpdates = (int) env("SCMS_RAMP_UPDATES", 20);  // gradual: msgs to full magnitude
        public double dutyOnS = env("SCMS_DUTY_ON", 6.0);             // intermittent: attack-on window
        public double dutyOffS = env("SCMS_DUTY_OFF", 6.0);           // intermittent: attack-off window
        public double startDelayS = env("SCMS_START_DELAY", 15.0);   // delayed: benign lead-in

        private static double env(String n, double d) {
            String e = System.getenv(n);
            try {
                return (e != null && !e.isBlank()) ? Double.parseDouble(e.trim()) : d;
            } catch (NumberFormatException ex) {
                return d;
            }
        }
    }

    /** Ghost identities a Sybil variant emits (tier scales the base count). */
    public static int ghostCount(String name, Cfg cfg) {
        return Math.max(1, (int) Math.round(cfg.sybilGhosts * resolve(name).scale));
    }

    /** Per-attacker mutable state (kept reproducible via a seeded RNG). */
    public static final class State {
        public final Random rng;
        public double[] frozen = null;
        public double[] replay = null;   // {x, y, speed, heading}
        public long replayGenNs = -1;

        public State(long seed) {
            this.rng = new Random(seed);
        }
    }

    /** The claimed content of one broadcast plus behaviour flags. */
    public static final class Claim {
        public double x, y, speed, heading;
        public double posConf = 0;      // 95% position-confidence radius (m)
        public long genTimeNs;
        public boolean flood = false;   // DoS: bypass the 1 Hz throttle
        public int sybilGhosts = 0;     // Sybil: also emit this many ghost identities
    }

    /**
     * Compute the claimed CAM for one broadcast: resolve the variant, evaluate its base
     * behaviour at the tier magnitude, then apply the temporal profile relative to the truth.
     *
     * @param type       variant name ("none" for a genuine vehicle)
     * @param s          per-attacker state
     * @param sendCount  how many CAMs this vehicle has sent so far
     * @param x,y,speed,heading  the TRUE kinematics from SUMO
     * @param tNs        current sim time (ns)
     */
    public static Claim compute(String type, State s, int sendCount,
                                double x, double y, double speed, double heading, long tNs, Cfg cfg) {
        if (type == null || "none".equals(type)) {
            Claim c = new Claim();
            c.x = x; c.y = y; c.speed = speed; c.heading = heading; c.genTimeNs = tNs;
            return c;
        }
        Variant v = resolve(type);
        Claim c = computeBase(v, s, sendCount, x, y, speed, heading, tNs, cfg);
        return applyProfile(v, c, sendCount, x, y, speed, heading, tNs, cfg);
    }

    /** The falsified claim for a base behaviour at a given magnitude tier (before profiling). */
    private static Claim computeBase(Variant v, State s, int sendCount,
                                     double x, double y, double speed, double heading, long tNs, Cfg cfg) {
        Claim c = new Claim();
        c.x = x; c.y = y; c.speed = speed; c.heading = heading; c.genTimeNs = tNs;
        double sc = v.scale;
        double r1 = s.rng.nextDouble() * 2 - 1;   // in [-1, 1]
        double r2 = s.rng.nextDouble() * 2 - 1;
        double off = cfg.offsetM * sc;
        switch (v.base) {
            case "ConstPos":
                if (sendCount == cfg.freezeAfter) {
                    s.frozen = new double[] {x, y};
                }
                if (s.frozen != null) {
                    c.x = s.frozen[0]; c.y = s.frozen[1];
                }
                break;
            case "ConstPosOffset":
                c.x = x + off; c.y = y + off;
                break;
            case "ConstPosOffsetX":
                c.x = x + off;
                break;
            case "ConstPosOffsetY":
                c.y = y + off;
                break;
            case "RandomPos":
                c.x = x + r1 * cfg.randomRadiusM * sc; c.y = y + r2 * cfg.randomRadiusM * sc;
                break;
            case "RandomPosOffset":
                c.x = x + r1 * off; c.y = y + r2 * off;
                break;
            case "Teleport": {
                int slot = (sendCount / 5) % 4;
                double ang = Math.toRadians(slot * 97.0);
                c.x = x + Math.cos(ang) * off; c.y = y + Math.sin(ang) * off;
                break;
            }
            case "SineWavePos": {
                double amp = cfg.randomRadiusM * sc * 0.2;
                c.x = x + amp * Math.sin(sendCount * 0.5); c.y = y + amp * Math.cos(sendCount * 0.5);
                break;
            }
            case "ConstSpeedZero":
                c.speed = 0.0;
                break;
            case "ConstSpeedOffset":
                c.speed = speed + cfg.speedOffsetMs * sc;
                break;
            case "ConstSpeedHigh":
                c.speed = 40.0 * Math.max(0.5, sc);
                break;
            case "RandomSpeed":
                c.speed = s.rng.nextDouble() * cfg.randomSpeedMax * sc;
                break;
            case "RandomSpeedOffset":
                c.speed = Math.max(0, speed + r1 * cfg.speedOffsetMs * sc);
                break;
            case "StopAndGo":
                c.speed = (sendCount % 4 < 2) ? 0.0 : speed + cfg.speedOffsetMs;
                break;
            case "ReversedHeading":
                c.heading = (heading + 180.0) % 360.0;
                break;
            case "RandomHeading":
                c.heading = s.rng.nextDouble() * 360.0;
                break;
            case "PerpendicularHeading":
                c.heading = (heading + 90.0) % 360.0;
                break;
            case "HeadingOffset":
                c.heading = (heading + cfg.headingOffsetDeg * sc) % 360.0;
                break;
            case "Disruptive":
                c.x = x + r1 * off; c.y = y + r2 * off;
                c.speed = s.rng.nextDouble() * cfg.randomSpeedMax * sc;
                c.heading = s.rng.nextDouble() * 360.0;
                break;
            case "PosSpeedInconsistent":
                c.x = x + off; c.y = y + off; c.speed = 0.0;   // big jump but claims stopped
                break;
            case "PosHeadingInconsistent":
                c.x = x + off; c.heading = (heading + 180.0) % 360.0;
                break;
            case "EventualStop":
                if (sendCount == cfg.freezeAfter) {
                    s.frozen = new double[] {x, y};
                }
                if (s.frozen != null) {
                    c.x = s.frozen[0]; c.y = s.frozen[1]; c.speed = 0.0;
                }
                break;
            case "DataReplay":
                if (s.replay == null && sendCount >= cfg.freezeAfter) {
                    s.replay = new double[] {x, y, speed, heading};
                    s.replayGenNs = tNs;
                }
                if (s.replay != null) {
                    c.x = s.replay[0]; c.y = s.replay[1]; c.speed = s.replay[2]; c.heading = s.replay[3];
                    c.genTimeNs = s.replayGenNs;   // stale, replayed timestamp
                }
                break;
            case "DelayedMessages":
                c.genTimeNs = tNs - (long) (cfg.staleDelayS * sc * 1e9);
                break;
            case "OutOfOrder":
                c.genTimeNs = tNs - (long) (s.rng.nextDouble() * cfg.staleDelayS * 1e9);
                break;
            case "DoS":
                c.flood = true;
                break;
            case "DoSRandom":
                c.flood = true;
                c.x = x + r1 * off; c.y = y + r2 * off;
                c.speed = s.rng.nextDouble() * cfg.randomSpeedMax * sc;
                break;
            case "Sybil":
                c.sybilGhosts = ghostCount(v.name, cfg);
                break;
            default:
                break;
        }
        return c;
    }

    /** Gate/ramp the base claim by the temporal profile, relative to the true kinematics. */
    private static Claim applyProfile(Variant v, Claim c, int sendCount,
                                      double x, double y, double speed, double heading, long tNs, Cfg cfg) {
        double tSec = tNs / 1e9;
        switch (v.profile) {
            case "gradual": {
                double ramp = Math.min(1.0, cfg.rampUpdates <= 0 ? 1.0 : (double) sendCount / cfg.rampUpdates);
                c.x = x + (c.x - x) * ramp;
                c.y = y + (c.y - y) * ramp;
                c.speed = speed + (c.speed - speed) * ramp;
                c.heading = heading + angDelta(heading, c.heading) * ramp;
                if (ramp < 1.0) {
                    c.flood = false;   // no flooding until fully ramped
                }
                break;
            }
            case "intermittent": {
                double period = Math.max(0.1, cfg.dutyOnS + cfg.dutyOffS);
                boolean on = (tSec % period) < cfg.dutyOnS;
                if (!on) {
                    return truthful(x, y, speed, heading, tNs);
                }
                break;
            }
            case "delayed":
                if (tSec < cfg.startDelayS) {
                    return truthful(x, y, speed, heading, tNs);
                }
                break;
            default:   // constant
                break;
        }
        return c;
    }

    private static Claim truthful(double x, double y, double speed, double heading, long tNs) {
        Claim t = new Claim();
        t.x = x; t.y = y; t.speed = speed; t.heading = heading; t.genTimeNs = tNs;
        return t;
    }

    /** Shortest signed angular difference target-current, in degrees, in (-180, 180]. */
    private static double angDelta(double current, double target) {
        double d = (target - current) % 360.0;
        if (d > 180.0) {
            d -= 360.0;
        }
        if (d <= -180.0) {
            d += 360.0;
        }
        return d;
    }
}
