/*
 * SPDX-License-Identifier: Apache-2.0
 * SCMS road-side unit: a static, always-trusted misbehaviour observer.
 *
 * Why this class exists at all: MOSAIC assigns applications to UNITS, and an application typed
 * AbstractApplication<VehicleOperatingSystem> (ScmsBeaconApp) cannot be attached to an RSU unit --
 * MOSAIC fails at unit start-up. That is why every generated scenario used to strip `rsus` from the
 * mapping wholesale, which in turn meant the MOSAIC path had NO infrastructure evidence at all,
 * while the pure-Python engine and the feature builder both already modelled it.
 *
 * An RSU here is deliberately a RECEIVER ONLY: it never beacons, never carries a driver or an
 * attack role, and never appears in gt_vehicle / gt_identity_map (see ScmsBackend.rsuCredential).
 * It runs the SAME detector suite as a vehicle (CamDetector) over the CAMs its antenna actually
 * receives, from a fixed, surveyed position -- which is the whole point of infrastructure evidence:
 * a static receiver with a known-good position is not fooled by the mobility a mobile witness has
 * to guess at, and it cannot itself be an attacker or a colluder.
 *
 * Channel realism mirrors the vehicle receiver (weather attenuation, distance-growing NLOS loss
 * computed from the sender's TRUE position via the back-end oracle, CSMA/CA congestion loss), so
 * RSU links are not unrealistically perfect relative to vehicle links.
 *
 * Placement, count and the WGS-84 positions come from the scenario generator
 * (mapgen.rsu_units / SCMS_RSUS / SCMS_RSU_PLACEMENT); this class only has to be present in the
 * jar for gen_scenario to start emitting RSU units at all.
 */
package org.scms.app;

import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.AdHocModuleConfiguration;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.CamBuilder;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.ReceivedAcknowledgement;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.ReceivedV2xMessage;
import org.eclipse.mosaic.fed.application.app.AbstractApplication;
import org.eclipse.mosaic.fed.application.app.api.CommunicationApplication;
import org.eclipse.mosaic.fed.application.app.api.os.RoadSideUnitOperatingSystem;
import org.eclipse.mosaic.interactions.communication.V2xMessageTransmission;
import org.eclipse.mosaic.lib.enums.AdHocChannel;
import org.eclipse.mosaic.lib.geo.CartesianPoint;
import org.eclipse.mosaic.lib.geo.GeoPoint;
import org.eclipse.mosaic.lib.util.scheduling.Event;

import org.scms.backend.ScmsBackend;

public class ScmsRsuApp extends AbstractApplication<RoadSideUnitOperatingSystem>
        implements CommunicationApplication {

    private static final int CHAN_CAPACITY = CamDetector.envI("SCMS_CHAN_CAPACITY", 25);
    private static final double CHAN_WINDOW_S = 0.1;
    private static final double NLOS_INTENSITY = CamDetector.envD("SCMS_NLOS", 0.0);
    private static final double WEATHER_DROP = weatherDrop();

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

    private final CamDetector detector = new CamDetector();
    private String myDigest;
    private double selfX, selfY;
    private boolean haveSelf = false;
    private double chanWinStart = Double.NEGATIVE_INFINITY;
    private int chanCount = 0, chanLoad = 0;
    private java.util.Random chanRng;

    @Override
    public void onStartup() {
        String id = getOperatingSystem().getId();
        ScmsBackend backend = ScmsBackend.instance();
        myDigest = backend.rsuCredential(id).certDigest;
        chanRng = new java.util.Random(0x9E3779B97F4A7C15L ^ (long) id.hashCode());
        // A road-side unit is surveyed: its position is known exactly and never changes, so it is
        // resolved once at start-up rather than tracked from mobility updates.
        GeoPoint pos = getOperatingSystem().getPosition();
        if (pos != null) {
            CartesianPoint c = pos.toCartesian();
            if (c != null) {
                selfX = c.getX();
                selfY = c.getY();
                haveSelf = true;
            }
        }
        if (!haveSelf) {
            // Without a position the range check cannot run; every other detector still can, so
            // degrade instead of failing (and say so, because it points at a mapping error).
            getLog().warn("RSU {} has no resolvable position; acceptanceRangeThreshold disabled", id);
        }
        getOperatingSystem().getAdHocModule().enable(new AdHocModuleConfiguration()
                .addRadio().channel(AdHocChannel.CCH).power(50).create());
        getLog().info("SCMS RSU app up: {} at ({}, {})", id, selfX, selfY);
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
        if (WEATHER_DROP > 0 && chanRng.nextDouble() < WEATHER_DROP) {
            return;
        }
        // NLOS geometry from the TRUE sender position (channel physics only — see ScmsBeaconApp for
        // why a claimed position must never steer reception probability).
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
        if (t - chanWinStart >= CHAN_WINDOW_S) {
            chanWinStart = t; chanLoad = chanCount; chanCount = 0;
        }
        chanCount++;
        if (CHAN_CAPACITY > 0 && chanLoad > CHAN_CAPACITY) {
            double pDrop = Math.min(0.95, (double) (chanLoad - CHAN_CAPACITY) / CHAN_CAPACITY);
            if (chanRng.nextDouble() < pDrop) {
                return;
            }
        }
        if (backend.isRevoked(dg, t)) {
            return;
        }
        if (!cam.sigValid) {
            return;
        }
        CamDetector.Detection d = detector.evaluate(dg, cam, t, selfX, selfY, haveSelf);
        if (d != null) {
            backend.onDetection(myDigest, dg, d.score, t, d.reason, cam.posConf, d.scoreNorm, d.detNorms);
        }
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
