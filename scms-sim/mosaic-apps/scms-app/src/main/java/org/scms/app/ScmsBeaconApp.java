/*
 * SPDX-License-Identifier: Apache-2.0
 * SCMS-aware vehicle application (MOSAIC layer, v5 — full attack + detector suite).
 *
 * Each vehicle broadcasts a signed CAM over ITS-G5 (AdHoc CCH); MOSAIC's SNS radio decides
 * who receives it. Attack behaviour (content, timing, flooding, Sybil ghosts) comes from the
 * back-end via AttackLib. Every receiver runs a distributed detector suite over the CAMs it
 * actually receives and reports suspects to the (in-JVM) MA back-end. Detectors:
 *   staleOrReplay, beaconFrequency, acceptanceRangeThreshold, positionJump, sybilCoLocation,
 *   positionSpeedInconsistency, headingInconsistency, constantPositionFrozen.
 *
 * Realism components ported from VeReMi-NextGen (EPL-2.0, attribution in the source headers of
 * org.scms.realism.DriverProfile / org.scms.realism.SensorErrorModel), both opt-in:
 *   SCMS_DRIVER_PROFILES=1  10/80/10 aggressive/normal/passive driver parameterisation, applied
 *                           per vehicle via requestVehicleParametersUpdate (keyed on seed+vehicle id)
 *   SCMS_SENSOR_MODEL=nextgen  correlated GNSS error / relative speed error / speed-decaying heading
 *                           error on the transmitted CAM (keyed per vehicle, driven by sim time)
 *   SCMS_PSEUDONYM_POLICY=distance  NextGen's 800-1500 m + 120-360 s pseudonym change, re-based on
 *                           simulation time (upstream used the wall clock, see ScmsBackend)
 */
package org.scms.app;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.AdHocModuleConfiguration;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.CamBuilder;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.ReceivedAcknowledgement;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.ReceivedV2xMessage;
import org.eclipse.mosaic.fed.application.app.AbstractApplication;
import org.eclipse.mosaic.fed.application.app.api.CommunicationApplication;
import org.eclipse.mosaic.fed.application.app.api.VehicleApplication;
import org.eclipse.mosaic.fed.application.app.api.os.VehicleOperatingSystem;
import org.eclipse.mosaic.interactions.communication.V2xMessageTransmission;
import org.eclipse.mosaic.lib.enums.AdHocChannel;
import org.eclipse.mosaic.lib.geo.CartesianPoint;
import org.eclipse.mosaic.lib.objects.v2x.MessageRouting;
import org.eclipse.mosaic.lib.objects.vehicle.VehicleData;
import org.eclipse.mosaic.lib.util.scheduling.Event;

import org.scms.attacks.AttackLib;
import org.scms.backend.ScmsBackend;
import org.scms.realism.DriverProfile;
import org.scms.realism.SensorErrorModel;

public class ScmsBeaconApp extends AbstractApplication<VehicleOperatingSystem>
        implements VehicleApplication, CommunicationApplication {

    // Detector thresholds live in CamDetector (the suite is shared with the RSU receiver app), and
    // are re-exported here only so appParams() keeps publishing the same replay-parity key set.
    private static final double CAM_INTERVAL_S = envD("SCMS_CAM_INTERVAL", 1.0);   // ETSI T_GenCamMax
    private static final double CAM_MIN_S = envD("SCMS_CAM_MIN", 0.1);             // ETSI T_GenCamMin
    private static final int FLOOD_BURST = envI("SCMS_FLOOD_BURST", 10);           // DoS msgs per tick
    private static final int CHAN_CAPACITY = envI("SCMS_CHAN_CAPACITY", 25);   // CAMs/100ms before congestion loss (0 = off)
    private static final double CHAN_WINDOW_S = 0.1;
    private static final double WEATHER_DROP = weatherDrop();                   // 802.11p attenuation by weather
    private static final double NLOS_INTENSITY = envD("SCMS_NLOS", 0.0);        // building-obstruction loss (0 = off)

    private static double weatherDrop() {
        String w = System.getenv("SCMS_WEATHER");
        if (w == null) { return 0.0; }
        switch (w.toLowerCase()) {
            case "rain": return 0.05;
            case "fog":  return 0.03;
            case "snow": return 0.10;
            default:     return 0.0;
        }
    }

    private static int envI(String n, int d) {
        String e = System.getenv(n);
        try { return (e != null && !e.isBlank()) ? Integer.parseInt(e.trim()) : d; } catch (NumberFormatException ex) { return d; }
    }

    private static double envD(String n, double d) {
        String e = System.getenv(n);
        try { return (e != null && !e.isBlank()) ? Double.parseDouble(e.trim()) : d; } catch (NumberFormatException ex) { return d; }
    }

    private int sendCount = 0;
    private double lastSendS = Double.NEGATIVE_INFINITY;
    private double lastSentX, lastSentY, lastSentHeading, lastSentSpeed;
    private boolean haveSentBefore = false;
    private String myDigest;
    private ScmsBackend.Cred cred;
    private double selfX, selfY;
    private boolean haveSelf = false;
    // On-board sensor chain (VeReMi-NextGen port, opt-in via SCMS_SENSOR_MODEL=nextgen): the vehicle
    // MEASURES its own state before the CAM is built, so honest beacons carry realistic GNSS/odometry
    // error instead of SUMO-perfect truth. Seeded per vehicle from the scenario seed.
    private SensorErrorModel sensors;
    private static boolean scenarioNoted = false;   // resolve the scenario dir once per JVM

    private final CamDetector detector = new CamDetector();
    private double chanWinStart = Double.NEGATIVE_INFINITY;
    private int chanCount = 0, chanLoad = 0;      // local channel load (CBR proxy)
    private java.util.Random chanRng;             // deterministic contention-loss RNG

    @Override
    public void onStartup() {
        String id = getOperatingSystem().getId();
        ScmsBackend backend = ScmsBackend.instance();
        backend.register(id);
        cred = backend.getCredential(id);
        myDigest = cred.certDigest;
        chanRng = new java.util.Random(0x9E3779B97F4A7C15L ^ (long) id.hashCode());
        noteScenario(backend);
        if ("nextgen".equals(ScmsBackend.SENSOR_MODEL)) {
            // Weather scales the GNSS/compass error magnitudes exactly as it scales the built-in
            // model's sigma (SCMS_WEATHER: rain 1.5x, snow 2.0x, fog 2.5x), so the transmitted
            // position-confidence radius stays consistent with the error actually injected.
            double w = ScmsBackend.WEATHER_SENSOR_MULT;
            sensors = new SensorErrorModel(ScmsBackend.streamSeed("sensor", id),
                    ScmsBackend.SENSOR_POS_ERR_M * w, ScmsBackend.SENSOR_SPEED_ERR,
                    ScmsBackend.SENSOR_HEAD_ERR_DEG * w, ScmsBackend.SENSOR_POS_SIGMA_FRAC,
                    ScmsBackend.SENSOR_HEAD_DECAY);
        }
        if (ScmsBackend.DRIVER_PROFILES) {
            applyDriverProfile(backend, id);
        }
        getOperatingSystem().getAdHocModule().enable(new AdHocModuleConfiguration()
                .addRadio().channel(AdHocChannel.CCH).power(50).create());
    }

    /**
     * VeReMi-NextGen driver heterogeneity (VehicleCamSendingApp.java:72-88, 293-304): 10% aggressive,
     * 80% normal, 10% passive, re-parameterising SUMO's car-following/lane-change model per vehicle
     * through MOSAIC's requestVehicleParametersUpdate API.
     *
     * <p>Upstream draws the profile from an unseeded {@code new Random()} — different every run.
     * Here the draw is a pure function of (scenario seed, vehicle id), so the fleet composition is
     * reproducible and independent of vehicle-spawn ORDER (a wall-clock- and order-free key).
     */
    private void applyDriverProfile(ScmsBackend backend, String id) {
        DriverProfile profile = DriverProfile.pick(ScmsBackend.keyedUniform("driver-profile", id),
                ScmsBackend.DRIVER_AGGRESSIVE_FRAC, ScmsBackend.DRIVER_PASSIVE_FRAC);
        backend.noteDriverProfile(id, profile.name());
        try {
            getOperatingSystem().requestVehicleParametersUpdate()
                    .changeReactionTime(profile.getTau())
                    .changeMaxAcceleration(profile.getAccel())
                    .changeMaxDeceleration(profile.getDecel())
                    .changeSpeedFactor(profile.getSpeedFactor())
                    .changeImperfection(profile.getSigma())
                    .changeMinimumGap(profile.getMinGap())
                    .changeLaneChangeMode(profile.getLaneChangeMode())
                    .changeSpeedMode(profile.getSpeedMode())
                    .apply();
        } catch (RuntimeException ex) {
            // Never let a parameter-update rejection kill the run: the vehicle simply keeps its
            // prototype dynamics (and the manifest still records which profile it was assigned).
            getLog().warn("driver profile {} not applied to {}: {}", profile, id, ex.toString());
        }
    }

    /** Hand the back-end the scenario directory once, so the manifest can pick up input hashes. */
    private void noteScenario(ScmsBackend backend) {
        if (scenarioNoted) {
            return;
        }
        scenarioNoted = true;
        try {
            backend.noteScenarioDir(org.eclipse.mosaic.fed.application.ambassador.SimulationKernel
                    .SimulationKernel.getConfigurationPath());
        } catch (Throwable ignored) {
            // best-effort: without it the manifest just omits the inputs section
        }
        backend.noteAppParams(appParams());
    }

    /** App-layer knobs as resolved by THIS run, for manifest.effective_params (replay parity). */
    private static Map<String, Object> appParams() {
        Map<String, Object> p = new LinkedHashMap<>();
        p.put("SCMS_CAM_INTERVAL", CAM_INTERVAL_S);
        p.put("SCMS_CAM_MIN", CAM_MIN_S);
        p.put("SCMS_FLOOD_BURST", FLOOD_BURST);
        p.put("SCMS_FROZEN_COUNT", CamDetector.FROZEN_COUNT);
        p.put("SCMS_ART_MAX_M", CamDetector.ART_MAX_M);
        p.put("SCMS_STALE_MAX", CamDetector.STALE_MAX_S);
        p.put("SCMS_FREQ_MAX", CamDetector.FREQ_MAX);
        p.put("SCMS_SPEED_TOL", CamDetector.SPEED_TOL_M);
        p.put("SCMS_HEADING_DIFF", CamDetector.HEADING_DIFF);
        p.put("SCMS_SYBIL_MIN", CamDetector.SYBIL_MIN);
        p.put("SCMS_CHAN_CAPACITY", CHAN_CAPACITY);
        p.put("SCMS_NLOS", NLOS_INTENSITY);
        p.put("SCMS_MIN_CONSEC", CamDetector.MIN_CONSEC);
        p.put("SCMS_MAX_ACCEL", CamDetector.MAX_PLAUSIBLE_ACCEL);
        p.put("SCMS_KF_THRESH", CamDetector.KF_THRESH);
        return p;
    }

    @Override
    public void onVehicleUpdated(VehicleData previous, VehicleData updated) {
        if (updated == null) {
            return;
        }
        CartesianPoint p = updated.getProjectedPosition();
        if (p == null) {
            return;
        }
        selfX = p.getX();
        selfY = p.getY();
        haveSelf = true;
        long tNs = getOperatingSystem().getSimulationTime();
        double tS = tNs / 1e9;
        boolean flood = cred != null && ("DoS".equals(AttackLib.baseOf(cred.attackType))
                || "DoSRandom".equals(AttackLib.baseOf(cred.attackType)));
        // ETSI EN 302 637-2 CAM generation rules: trigger on dynamics (Δpos>4 m / Δhdg>4° /
        // Δspeed>0.5 m/s), floored at T_GenCamMin and heart-beating at T_GenCamMax. DoS floods.
        double dtLast = tS - lastSendS;
        boolean due;
        if (flood) {
            due = true;
        } else if (dtLast < CAM_MIN_S) {
            due = false;
        } else if (dtLast >= CAM_INTERVAL_S || !haveSentBefore) {
            due = true;
        } else {
            double dPos = Math.hypot(selfX - lastSentX, selfY - lastSentY);
            double dHdg = angleDiff(updated.getHeading(), lastSentHeading);
            double dSpd = Math.abs(updated.getSpeed() - lastSentSpeed);
            due = dPos > 4.0 || dHdg > 4.0 || dSpd > 0.5;
        }
        if (!due) {
            return;
        }
        lastSendS = tS;
        lastSentX = selfX; lastSentY = selfY;
        lastSentHeading = updated.getHeading(); lastSentSpeed = updated.getSpeed();
        haveSentBefore = true;
        sendCount++;
        ScmsBackend backend = ScmsBackend.instance();
        // Pseudonym change: period policy (default), or NextGen's distance+time privacy policy, which
        // needs the odometer. getDistanceDriven() is SUMO's true odometer, and the elapsed-time half
        // of the criterion runs on SIMULATION time in the back-end (upstream used the wall clock).
        cred = backend.beaconCred(getOperatingSystem().getId(), tNs, updated.getDistanceDriven());
        myDigest = cred.certDigest;
        // Sensor chain: the vehicle measures its own state (NextGen SensorErrorModel) BEFORE the
        // attack layer falsifies anything, and the TRUE state still goes to the back-end, so the
        // ground-truth tables keep the real trajectory and only the transmitted CAM carries noise.
        SensorErrorModel.Sample measured = null;
        if (sensors != null) {
            Double accel = updated.getLongitudinalAcceleration();
            measured = sensors.sample(selfX, selfY, updated.getSpeed(), updated.getHeading(),
                    accel != null ? accel : 0.0, tNs);
        }
        AttackLib.Claim c = backend.claim(getOperatingSystem().getId(), sendCount,
                selfX, selfY, updated.getSpeed(), updated.getHeading(), tNs, measured);
        backend.onCamSent(cred.certDigest, tS);
        send(backend, cred.certDigest, c.x, c.y, c.speed, c.heading, c.posConf, c.genTimeNs);
        if (c.flood) {   // DoS: emit a burst so receivers/channel actually see flooding
            for (int b = 1; b < FLOOD_BURST; b++) {
                backend.onCamSent(cred.certDigest, tS);
                send(backend, cred.certDigest, c.x, c.y, c.speed, c.heading, c.posConf, c.genTimeNs);
            }
        }

        if (c.sybilGhosts > 0) {   // Sybil: emit ghost identities clustered near the attacker
            List<String> ghosts = backend.ghostDigests(getOperatingSystem().getId());
            for (int k = 0; k < ghosts.size(); k++) {
                double gx = c.x + ((k % 2 == 0) ? 1.0 : -1.0);   // tight cluster (~1.5 m): physically implausible
                double gy = c.y + ((k < 2) ? 1.0 : -1.0);
                backend.onCamSent(ghosts.get(k), tS);
                send(backend, ghosts.get(k), gx, gy, c.speed, c.heading, c.posConf, tNs);
            }
        }
        backend.maybeCollude(getOperatingSystem().getId(), tNs);   // colluders file false accusations
    }

    private void send(ScmsBackend backend, String certDigest, double x, double y, double speed,
                      double heading, double posConf, long genTimeNs) {
        MessageRouting routing = getOperatingSystem().getAdHocModule().createMessageRouting()
                .channel(AdHocChannel.CCH).topological().broadcast().singlehop().build();
        getOperatingSystem().getAdHocModule().sendV2xMessage(new SignedCam(routing,
                certDigest, cred.iPeriod, cred.jIndex, cred.linkageValueHex,
                x, y, speed, heading, posConf, genTimeNs, true));
    }

    @Override
    public void onMessageReceived(ReceivedV2xMessage rx) {
        if (!(rx.getMessage() instanceof SignedCam)) {
            return;
        }
        SignedCam cam = (SignedCam) rx.getMessage();
        String dg = cam.senderCertDigest;
        if (dg.equals(myDigest)) {
            return;
        }
        double t = getOperatingSystem().getSimulationTime() / 1e9;
        ScmsBackend backend = ScmsBackend.instance();
        // Weather (rain/fog/snow) attenuates the 802.11p link — a fraction of frames are lost.
        if (WEATHER_DROP > 0 && chanRng.nextDouble() < WEATHER_DROP) {
            return;
        }
        // NLOS: buildings obstruct more-distant links in urban areas — loss grows with distance.
        // The obstruction is a property of the PHYSICAL link, so the geometry must come from the
        // TRUE sender position (back-end ORACLE), never from cam.claimedX/Y: a position-falsifying
        // attacker claiming a far-away location would otherwise drive its OWN reception probability
        // — inventing packet loss that the radio never applied, and (for a ghost claiming to be
        // nearby) making Sybil frames MORE reliable than honest ones. Ground truth is consumed here
        // for channel physics only; nothing derived from it reaches a report or an MA-visible field.
        if (NLOS_INTENSITY > 0 && haveSelf) {
            double[] txTrue = backend.truePositionOf(dg);
            if (txTrue != null) {
                double dist = Math.hypot(txTrue[0] - selfX, txTrue[1] - selfY);
                double pn = NLOS_INTENSITY * Math.min(1.0, Math.max(0.0, (dist - 150.0) / 300.0));
                if (pn > 0 && chanRng.nextDouble() < pn) {
                    return;
                }
            }
        }
        // Channel congestion (CSMA/CA contention / CBR collapse): under high local channel load a
        // fraction of frames are lost before they can be decoded. This is the main radio realism a
        // full 802.11p PHY/MAC (OMNeT++/ns-3) would add over SNS — it makes DoS floods actually
        // degrade nearby reception and gives dense traffic a realistic packet-delivery ratio.
        if (t - chanWinStart >= CHAN_WINDOW_S) {
            chanWinStart = t; chanLoad = chanCount; chanCount = 0;
        }
        chanCount++;
        if (CHAN_CAPACITY > 0 && chanLoad > CHAN_CAPACITY) {
            double pDrop = Math.min(0.95, (double) (chanLoad - CHAN_CAPACITY) / CHAN_CAPACITY);
            if (chanRng.nextDouble() < pDrop) {
                return; // frame lost to channel contention
            }
        }
        if (backend.isRevoked(dg, t)) {
            return; // ENFORCEMENT: drop revoked certificates
        }
        if (!cam.sigValid) {
            return;
        }

        // The detector suite itself is shared with the RSU receiver app (CamDetector), so vehicle
        // and infrastructure evidence come from exactly the same checks and the same thresholds.
        CamDetector.Detection d = detector.evaluate(dg, cam, t, selfX, selfY, haveSelf);
        if (d != null) {
            backend.onDetection(myDigest, dg, d.score, t, d.reason, cam.posConf, d.scoreNorm, d.detNorms);
        }
    }

    private static double norm360(double a) {
        return ((a % 360) + 360) % 360;
    }

    private static double angleDiff(double a, double b) {
        double d = Math.abs(norm360(a) - norm360(b));
        return d > 180 ? 360 - d : d;
    }

    @Override
    public void onAcknowledgementReceived(ReceivedAcknowledgement acknowledgement) {
    }

    @Override
    public void onCamBuilding(CamBuilder camBuilder) {
    }

    @Override
    public void onMessageTransmitted(V2xMessageTransmission transmission) {
    }

    @Override
    public void onShutdown() {
    }

    @Override
    public void processEvent(Event event) {
    }
}
