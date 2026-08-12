/*
 * SPDX-License-Identifier: Apache-2.0
 * A signed Cooperative Awareness Message broadcast over MOSAIC AdHoc (ITS-G5 CCH).
 *
 * Carries the claimed kinematics plus the sender's pseudonym-certificate digest and
 * its CAMP-SCP2 linkage value (i, j, lv). Because MOSAIC delivers the actual object
 * within one JVM, fields are read directly on receipt — the EncodedPayload length is
 * a nominal size so MOSAIC's radio model accounts for realistic on-air bytes.
 */
package org.scms.app;

import org.eclipse.mosaic.lib.objects.v2x.EncodedPayload;
import org.eclipse.mosaic.lib.objects.v2x.MessageRouting;
import org.eclipse.mosaic.lib.objects.v2x.V2xMessage;

public final class SignedCam extends V2xMessage {

    private static final long NOMINAL_BYTES = 300L; // signed CAM ~ headers + BSM/CAM + 1609.2 sig

    private final EncodedPayload payload;
    public final String senderCertDigest;
    public final int iPeriod;
    public final int jIndex;
    public final String linkageValueHex;
    public final double claimedX;
    public final double claimedY;
    public final double claimedSpeed;
    public final double claimedHeading;
    public final double posConf;    // 95% position-confidence radius (m), per ETSI CAM
    public final long genTimeNs;
    public final boolean sigValid;

    public SignedCam(MessageRouting routing, String senderCertDigest, int iPeriod, int jIndex,
                     String linkageValueHex, double claimedX, double claimedY, double claimedSpeed,
                     double claimedHeading, double posConf, long genTimeNs, boolean sigValid) {
        super(routing);
        this.senderCertDigest = senderCertDigest;
        this.iPeriod = iPeriod;
        this.jIndex = jIndex;
        this.linkageValueHex = linkageValueHex;
        this.claimedX = claimedX;
        this.claimedY = claimedY;
        this.claimedSpeed = claimedSpeed;
        this.claimedHeading = claimedHeading;
        this.posConf = posConf;
        this.genTimeNs = genTimeNs;
        this.sigValid = sigValid;
        this.payload = new EncodedPayload(NOMINAL_BYTES);
    }

    @Override
    public EncodedPayload getPayload() {
        return payload;
    }
}
