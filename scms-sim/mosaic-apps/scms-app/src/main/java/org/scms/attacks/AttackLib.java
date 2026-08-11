/*
 * SPDX-License-Identifier: Apache-2.0
 * Attack library — the misbehaviour taxonomy and per-CAM content falsification.
 *
 * Covers the VeReMi-Extension / F2MD family plus heading attacks:
 *   position: ConstPos, ConstPosOffset, RandomPos, RandomPosOffset
 *   speed:    ConstSpeed, ConstSpeedOffset, RandomSpeed, RandomSpeedOffset
 *   other:    EventualStop, ReversedHeading, Disruptive
 *   timing:   DataReplay, DelayedMessages, DoS, DoSRandom
 *   identity: Sybil (ghost identities, emitted by the app)
 *
 * The set is config-driven: env SCMS_ATTACKS = "all" (default) or a comma list. Each
 * attacker is deterministically assigned one enabled type. compute() returns the claimed
 * CAM content (plus timing/flood/sybil flags) for one broadcast; per-attacker mutable
 * state (frozen position, replay snapshot, RNG) lives in State so runs stay reproducible.
 */
package org.scms.attacks;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Random;

public final class AttackLib {

    public static final List<String> ALL = List.of(
            "ConstPos", "ConstPosOffset", "RandomPos", "RandomPosOffset",
            "ConstSpeed", "ConstSpeedOffset", "RandomSpeed", "RandomSpeedOffset",
            "EventualStop", "ReversedHeading", "Disruptive",
            "DataReplay", "DelayedMessages", "DoS", "DoSRandom", "Sybil");

    private AttackLib() {
    }

    /** Enabled attack types from env SCMS_ATTACKS ("all" or comma list). */
    public static List<String> enabled() {
        String e = System.getenv("SCMS_ATTACKS");
        if (e == null || e.isBlank() || e.equalsIgnoreCase("all")) {
            return ALL;
        }
        List<String> out = new ArrayList<>();
        for (String s : e.split(",")) {
            String t = s.trim();
            for (String a : ALL) {
                if (a.equalsIgnoreCase(t)) {
                    out.add(a);
                }
            }
        }
        return out.isEmpty() ? ALL : out;
    }

    /** Deterministically assign one enabled attack type to an attacker. */
    public static String assign(String unitId, long seed, List<String> en) {
        int idx = Math.floorMod(hash("atktype|" + seed + "|" + unitId), en.size());
        return en.get(idx);
    }

    private static int hash(String s) {
        int h = 0x811c9dc5;
        for (byte b : s.getBytes(StandardCharsets.UTF_8)) {
            h ^= (b & 0xff);
            h *= 0x01000193;
        }
        return h;
    }

    /** Tunable attack magnitudes (from env, with defaults). */
    public static final class Cfg {
        public double offsetM = env("SCMS_OFFSET_M", 1500.0);
        public double speedOffsetMs = env("SCMS_SPEED_OFFSET", 15.0);
        public double randomRadiusM = env("SCMS_RANDOM_RADIUS", 2000.0);
        public double randomSpeedMax = env("SCMS_RANDOM_SPEED_MAX", 40.0);
        public int freezeAfter = (int) env("SCMS_FREEZE_UPDATES", 5);
        public double staleDelayS = env("SCMS_STALE_DELAY", 8.0);
        public int sybilGhosts = (int) env("SCMS_SYBIL_GHOSTS", 4);

        private static double env(String n, double d) {
            String e = System.getenv(n);
            try {
                return (e != null && !e.isBlank()) ? Double.parseDouble(e.trim()) : d;
            } catch (NumberFormatException ex) {
                return d;
            }
        }
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
        public long genTimeNs;
        public boolean flood = false;   // DoS: bypass the 1 Hz throttle
        public int sybilGhosts = 0;     // Sybil: also emit this many ghost identities
    }

    /**
     * Compute the claimed CAM for one broadcast.
     *
     * @param type       attack type ("none" for a genuine vehicle)
     * @param s          per-attacker state
     * @param sendCount  how many CAMs this vehicle has sent so far
     * @param x,y,speed,heading  the TRUE kinematics from SUMO
     * @param tNs        current sim time (ns)
     */
    public static Claim compute(String type, State s, int sendCount,
                                double x, double y, double speed, double heading, long tNs, Cfg cfg) {
        Claim c = new Claim();
        c.x = x; c.y = y; c.speed = speed; c.heading = heading; c.genTimeNs = tNs;
        if (type == null || "none".equals(type)) {
            return c;
        }
        double r1 = s.rng.nextDouble() * 2 - 1;   // in [-1, 1]
        double r2 = s.rng.nextDouble() * 2 - 1;
        switch (type) {
            case "ConstPos":
                if (sendCount == cfg.freezeAfter) {
                    s.frozen = new double[] {x, y};
                }
                if (s.frozen != null) {
                    c.x = s.frozen[0]; c.y = s.frozen[1];
                }
                break;
            case "ConstPosOffset":
                c.x = x + cfg.offsetM; c.y = y + cfg.offsetM;
                break;
            case "RandomPos":
                c.x = x + r1 * cfg.randomRadiusM; c.y = y + r2 * cfg.randomRadiusM;
                break;
            case "RandomPosOffset":
                c.x = x + r1 * cfg.offsetM; c.y = y + r2 * cfg.offsetM;
                break;
            case "ConstSpeed":
                c.speed = 0.0;   // claim stopped while actually moving
                break;
            case "ConstSpeedOffset":
                c.speed = speed + cfg.speedOffsetMs;
                break;
            case "RandomSpeed":
                c.speed = s.rng.nextDouble() * cfg.randomSpeedMax;
                break;
            case "RandomSpeedOffset":
                c.speed = Math.max(0, speed + r1 * cfg.speedOffsetMs);
                break;
            case "EventualStop":
                if (sendCount == cfg.freezeAfter) {
                    s.frozen = new double[] {x, y};
                }
                if (s.frozen != null) {
                    c.x = s.frozen[0]; c.y = s.frozen[1]; c.speed = 0.0;
                }
                break;
            case "ReversedHeading":
                c.heading = (heading + 180.0) % 360.0;
                break;
            case "Disruptive":
                c.x = x + r1 * cfg.offsetM; c.y = y + r2 * cfg.offsetM;
                c.speed = s.rng.nextDouble() * cfg.randomSpeedMax;
                c.heading = s.rng.nextDouble() * 360.0;
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
                c.genTimeNs = tNs - (long) (cfg.staleDelayS * 1e9);
                break;
            case "DoS":
                c.flood = true;
                break;
            case "DoSRandom":
                c.flood = true;
                c.x = x + r1 * cfg.offsetM; c.y = y + r2 * cfg.offsetM;
                c.speed = s.rng.nextDouble() * cfg.randomSpeedMax;
                break;
            case "Sybil":
                c.sybilGhosts = cfg.sybilGhosts;
                break;
            default:
                break;
        }
        return c;
    }
}
