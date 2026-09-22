//! `protocol/etsi/ts102941` — the ETSI ITS PKI, as a skeleton with the right shape.
//!
//! Build decision D5 records that the ETSI PKI ASN.1 modules fail code generation on an
//! inner subtyping construct and are deferred, so there is no encoder for
//! `EtsiTs102941Data` and there will not be one in this phase. **This skeleton therefore
//! uses hand-written structures**: each message's size is the sum of its fields as
//! TS 102 941 §6.2.3 defines them, with every field's byte count taken from a cited
//! constant, and the two blocks no clause fixes — the subject-attribute container and the
//! AT payload — carried as card parameters with calibration plans. When D5's defect is
//! resolved these become real-encoder sizes exactly as the SCMS certificates already are.
//!
//! What is shaped correctly and runs: enrolment (S3) and authorization, standard variant
//! (S2/S4), as flows over queued nodes with their stage timestamps. What is declared but
//! not built: the butterfly variant of §6.2.3.5, which shares the SCMS arithmetic and so
//! belongs in the SCMS plug-in's provisioning code rather than in a second copy here; the
//! ECTL/CTL trust-list flows; and the misbehaviour-reporting path, whose payload is the
//! same TS 103 759 structure D5 blocks.

pub mod ts102941;

pub use ts102941::{ETSI_TS102941_ID, EtsiNodes, EtsiParams, EtsiRun, EtsiTs102941, Ts102941Msg};
