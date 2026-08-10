/*
 * SPDX-License-Identifier: Apache-2.0
 * The Security Credential Management System modelled as DISTINCT entities with real
 * trust boundaries (CAMP/USDOT SCMS, design doc §7). A single-host MOSAIC run executes
 * everything in one sequential JVM, so these are composited under one coordinator, but
 * each entity holds ONLY the state its trust domain permits -- so the boundaries are
 * structural, not merely conventional. In particular:
 *   - the Registration Authority is the ONLY entity that maps a provisioning request to
 *     an enrollment identity;
 *   - the two Linkage Authorities each hold only their own seed chain;
 *   - the Pseudonym CA holds only opaque provisioning records (no identity);
 *   - the Misbehavior Authority never holds a true identity -- it drives resolution
 *     through PCA -> LA1/LA2 -> RA and only ever receives seeds + an opaque handle.
 */
package org.scms.entities;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

import org.scms.crypto.LinkageEngine;

public final class Scms {

    // Trust-of-root domain (offline / ceremony) --------------------------------
    public final RootCa rootCa = new RootCa();
    public final IntermediateCa ica = new IntermediateCa(rootCa);
    public final Electors electors = new Electors(rootCa);
    public final PolicyGenerator pg = new PolicyGenerator(rootCa);
    // Enrollment domain --------------------------------------------------------
    public final DeviceConfigManager dcm = new DeviceConfigManager();
    public final EnrollmentCa eca = new EnrollmentCa(ica);
    // Provisioning domain ------------------------------------------------------
    public final RegistrationAuthority ra = new RegistrationAuthority(ica);
    public final PseudonymCa pca = new PseudonymCa(ica);
    public final LinkageAuthority la1 = new LinkageAuthority(1, ica);
    public final LinkageAuthority la2 = new LinkageAuthority(2, ica);
    // Enforcement domain -------------------------------------------------------
    public final MisbehaviorAuthority ma = new MisbehaviorAuthority(rootCa);
    public final CrlGenerator crlg = new CrlGenerator(rootCa);
    public final CrlStore crlStore = new CrlStore();
    public final LocationObscurerProxy lop = new LocationObscurerProxy();

    public static final List<String> ENTITY_NAMES = List.of(
            "RootCA", "IntermediateCA", "Electors", "PolicyGenerator",
            "DeviceConfigManager", "EnrollmentCA",
            "RegistrationAuthority", "PseudonymCA", "LinkageAuthority1", "LinkageAuthority2",
            "MisbehaviorAuthority", "CRLGenerator", "CRLStore", "LocationObscurerProxy");

    static String sha16(String s) {
        return sha(s).substring(0, 16);
    }

    static String sha(String s) {
        try {
            byte[] d = MessageDigest.getInstance("SHA-256").digest(s.getBytes(StandardCharsets.UTF_8));
            StringBuilder sb = new StringBuilder();
            for (byte b : d) {
                sb.append(String.format("%02x", b & 0xff));
            }
            return sb.toString();
        } catch (Exception e) {
            throw new IllegalStateException(e);
        }
    }

    // ------------------------------------------------------------------ root of trust
    /** Trust anchor; self-signed; issues component certs for ICA/PG/MA/CRLG. Offline. */
    public static final class RootCa {
        public final String id = sha16("RootCA");
        public String issue(String component) {
            return sha16("RootCA|" + component);
        }
    }

    /** Shields the Root CA; issues component certs for ECA/RA/PCA/LA. */
    public static final class IntermediateCa {
        public final String id;
        IntermediateCa(RootCa root) {
            this.id = root.issue("ICA");
        }
        public String issue(String component) {
            return sha16("ICA|" + component);
        }
    }

    /** Quorum of electors that endorse the Root CA (2n+1 model). Handle no EE data. */
    public static final class Electors {
        public final int count = 3;
        public final int quorum = 2;
        public final boolean rootEndorsed;
        Electors(RootCa root) {
            this.rootEndorsed = true; // quorum ballot endorsing the root
        }
    }

    /** Signs the Global Policy File / Global Certificate Chain File. */
    public static final class PolicyGenerator {
        public final String id;
        public final int jmax = 20;
        public final int iPeriodMinutes = 10080; // one week
        PolicyGenerator(RootCa root) {
            this.id = root.issue("PG");
        }
    }

    // ------------------------------------------------------------------ enrollment
    /** Attests device eligibility at bootstrap; knows device type, not pseudonyms. */
    public static final class DeviceConfigManager {
        public boolean attest(String unitId) {
            return true; // all simulated devices are eligible
        }
        public String deviceType(String unitId) {
            return "OBE";
        }
    }

    /** Issues a long-term enrollment certificate once the DCM has attested the device. */
    public static final class EnrollmentCa {
        public final String id;
        private final Map<String, String> enrollmentCertByUnit = new HashMap<>();
        EnrollmentCa(IntermediateCa ica) {
            this.id = ica.issue("ECA");
        }
        public String issue(String unitId) {
            return enrollmentCertByUnit.computeIfAbsent(unitId, u -> sha16("enroll|" + u));
        }
    }

    // ------------------------------------------------------------------ provisioning
    /** LA1 / LA2: hold per-device linkage seeds; return forward seeds only. Never see lv. */
    public static final class LinkageAuthority {
        public final int which;
        public final String id;
        private final Map<String, byte[]> seed0 = new HashMap<>();
        private final Map<String, Integer> laIdByHandle = new HashMap<>();
        LinkageAuthority(int which, IntermediateCa ica) {
            this.which = which;
            this.id = ica.issue("LA" + which);
        }
        public void register(String handle, byte[] ls0, int laId) {
            seed0.put(handle, ls0);
            laIdByHandle.put(handle, laId);
        }
        public byte[] seedAt(String handle, int i) {
            return LinkageEngine.linkageSeedAt(laIdByHandle.get(handle), seed0.get(handle), i);
        }
        public int laId(String handle) {
            return laIdByHandle.get(handle);
        }
    }

    /** Opaque provisioning record held by the PCA -- deliberately contains NO identity. */
    public static final class Prov {
        public final String requestHash, laHandle1, laHandle2;
        public final int i, j;
        Prov(String requestHash, int i, int j, String laHandle1, String laHandle2) {
            this.requestHash = requestHash;
            this.i = i;
            this.j = j;
            this.laHandle1 = laHandle1;
            this.laHandle2 = laHandle2;
        }
    }

    /** PCA: issues pseudonym certificates and keeps only opaque provisioning records. */
    public static final class PseudonymCa {
        public final String id;
        private final Map<String, Prov> prov = new HashMap<>();
        PseudonymCa(IntermediateCa ica) {
            this.id = ica.issue("PCA");
        }
        public void issue(String certDigest, String requestHash, int i, int j, String h1, String h2) {
            prov.put(certDigest, new Prov(requestHash, i, j, h1, h2));
        }
        public Prov resolve(String certDigest) {
            return prov.get(certDigest);
        }
    }

    /** RA: the ONLY entity that maps a provisioning request to an enrollment identity. */
    public static final class RegistrationAuthority {
        public final String id;
        private final Map<String, String> requestToEnrollment = new HashMap<>();
        public final Set<String> blacklist = new HashSet<>();
        RegistrationAuthority(IntermediateCa ica) {
            this.id = ica.issue("RA");
        }
        public void bind(String requestHash, String enrollmentId) {
            requestToEnrollment.put(requestHash, enrollmentId);
        }
        /** Blacklists the device behind a request hash and returns its enrollment id (RA-internal). */
        public String blacklistByRequest(String requestHash) {
            String enrollmentId = requestToEnrollment.get(requestHash);
            blacklist.add(enrollmentId);
            return enrollmentId;
        }
    }

    // ------------------------------------------------------------------ enforcement
    /** MA: correlates reports; drives resolution; NEVER holds a true identity. */
    public static final class MisbehaviorAuthority {
        public final String id;
        private final Map<String, Set<String>> reportersBySubject = new HashMap<>();
        MisbehaviorAuthority(RootCa root) {
            this.id = root.issue("MA");
        }
        public int addReporter(String subjectDigest, String reporterDigest) {
            reportersBySubject.computeIfAbsent(subjectDigest, k -> new HashSet<>()).add(reporterDigest);
            return reportersBySubject.get(subjectDigest).size();
        }
        public int distinctReporters(String subjectDigest) {
            Set<String> s = reportersBySubject.get(subjectDigest);
            return s == null ? 0 : s.size();
        }
    }

    /** CRL Generator: builds signed CRL entries from linkage seeds (subcomponent of the MA). */
    public static final class CrlGenerator {
        public final String id;
        public final List<LinkageEngine.CrlEntry> entries = new ArrayList<>();
        CrlGenerator(RootCa root) {
            this.id = root.issue("CRLG");
        }
        public LinkageEngine.CrlEntry issue(int i, int laId1, int laId2, byte[] ls1i, byte[] ls2i, int jmax) {
            LinkageEngine.CrlEntry e = new LinkageEngine.CrlEntry(i, laId1, laId2, ls1i, ls2i, jmax);
            entries.add(e);
            return e;
        }
        public int size() {
            return entries.size();
        }
    }

    /** CRL Store / Broadcast: pass-through distribution; the enforcement source. No private data. */
    public static final class CrlStore {
        private final Map<String, Double> revocationTime = new HashMap<>();
        public void publish(String subjectDigest, double t) {
            revocationTime.put(subjectDigest, t);
        }
        public boolean isRevoked(String subjectDigest) {
            return revocationTime.containsKey(subjectDigest);
        }
        public Double revocationTime(String subjectDigest) {
            return revocationTime.get(subjectDigest);
        }
        public boolean enforced(String subjectDigest, double t, double propagationDelay) {
            Double rt = revocationTime.get(subjectDigest);
            return rt != null && t >= rt + propagationDelay;
        }
    }

    /** LOP: strips network identifiers so EE->RA/MA traffic carries only cert digests. */
    public static final class LocationObscurerProxy {
        public boolean forward(String reporterDigest, String subjectDigest) {
            return reporterDigest != null && subjectDigest != null; // pass-through; no IP/MAC retained
        }
    }
}
