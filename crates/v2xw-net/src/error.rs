//! Crate error types and the drop causes this crate produces.
//!
//! Two kinds, split by who is at fault:
//!
//! * [`NetError`] — a caller asked the network layer for something the standard cannot
//!   express: an SDU above the MTU of a layer that has no fragmentation field, a PSID
//!   outside the p-encodable range, a payload whose length field would not fit.
//! * [`DropCause`] — a packet, a fragment or an SDU was discarded *inside* the model. This
//!   is state, not a Rust error: it travels in [`crate::netlayer::DecapOutcome`], in
//!   [`crate::frag::ReassemblyOutcome`] and into the `net.frag` record, and it is what the
//!   `drops by cause` column of `node.telemetry` (03-interfaces.md §14) counts.
//!
//! Both are `#[non_exhaustive]`, and both carry only variants something in this crate
//! actually produces. A variant nothing returns is worse than no variant: a caller writes a
//! match arm for it, the arm is never taken, and the dead branch looks like tested
//! behaviour. `#[non_exhaustive]` is what makes adding one later a non-breaking change.
//!
//! # Why `DropCause` lives here and not in `v2xw-radio`
//!
//! 03-interfaces.md names one `DropCause` for both `Mac::enqueue` (§4) and `SendOutcome`
//! (§5). `v2xw-radio` does not exist yet and does not depend on this crate, so the type is
//! declared here, where it is first produced, exactly as `v2xw-msg` declares `DccState`
//! ahead of the DCC crate. When the radio crate lands it either re-exports this type or
//! converts into it; the variant set is the union either way, and the
//! `#[non_exhaustive]` attribute means adding the MAC's own causes is not a breaking
//! change.

use serde::{Deserialize, Serialize};

/// Why a packet, a fragment or a reassembly was discarded.
///
/// Serialised in kebab-case, which is the spelling the `net.frag` record and the
/// `node.telemetry` drop counters use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "kebab-case", tag = "cause")]
#[non_exhaustive]
pub enum DropCause {
    /// The SDU is larger than the MTU and the configured strategy does not fragment.
    ///
    /// The cause 04-models.md §7.3 names for `fragmenter/none`, and the one
    /// [`crate::netlayer::NetLayer::encapsulate`] reports for either network layer, since
    /// neither WSMP nor GeoNetworking has a fragmentation field.
    #[error("SDU of {sdu_bytes} B exceeds the {mtu} B MTU and this strategy does not fragment")]
    Mtu {
        /// The SDU's size, bytes.
        sdu_bytes: u32,
        /// The MTU it was measured against, bytes.
        mtu: u32,
    },

    /// A fragment arrived at a strategy that never produces fragments.
    ///
    /// Produced by `fragmenter/none`'s reassembly path, which accepts only the whole-SDU
    /// descriptor it emits itself: a multi-fragment descriptor at that model means the
    /// sender and the receiver were configured with different strategies.
    #[error("fragment {index} of {count} arrived at a strategy that does not fragment")]
    UnexpectedFragment {
        /// The fragment's index within its SDU.
        index: u16,
        /// The fragment count it declared.
        count: u16,
    },

    /// Two fragments of one SDU disagreed about how many fragments there are, or an index
    /// was outside the declared count.
    ///
    /// A reassembly buffer keyed by `(sender, sdu)` cannot tell a re-sent SDU from a
    /// corrupted count, so the whole set is refused rather than silently resized.
    #[error("fragment set for this SDU declared {had} fragments, then {saw}")]
    FragmentCountMismatch {
        /// The count the first fragment declared.
        had: u16,
        /// The count this fragment declared.
        saw: u16,
    },

    /// The SDU would need more fragments than the fragment header can address.
    #[error("{needed} fragments needed, but the fragment header addresses at most {max}")]
    TooManyFragments {
        /// How many fragments the SDU and the effective MTU imply.
        needed: u32,
        /// The largest count the header can carry.
        max: u16,
    },

    /// The certificate would need more fragments than the certificate cycle has messages.
    ///
    /// The Partially-Hybrid design of 04-models.md §7.3 spreads the hybrid certificate over
    /// the first α messages of a τ-message cycle; α > τ means the next cycle would start
    /// before the certificate finished, so the receiver could never assemble one.
    #[error("certificate needs {needed} fragments but the cycle is only {tau} messages long")]
    CycleTooShort {
        /// Fragments the certificate would need at this MTU.
        needed: u16,
        /// The cycle length τ, messages.
        tau: u16,
    },

    /// The reassembly buffer already holds as many partly received SDUs as it may.
    ///
    /// Reported for the *arriving* fragment rather than evicting an older set, so the loss
    /// is attributed to the SDU that could not be admitted and the decision does not depend
    /// on an eviction order.
    #[error("reassembly buffer holds {open} of at most {max} partly received SDUs")]
    ReassemblyBufferFull {
        /// How many SDUs are open.
        open: usize,
        /// The configured maximum.
        max: usize,
    },

    /// The PDU does not belong to this network layer, or it is missing a field this layer
    /// needs in order to interpret it.
    #[error("malformed or foreign network PDU: {detail}")]
    MalformedPdu {
        /// What was wrong.
        detail: MalformedPdu,
    },
}

/// What was wrong with a network PDU ([`DropCause::MalformedPdu`]).
///
/// An enum rather than a free-text string so the cause survives serialisation into a record
/// and back, and so a metric provider can count the two cases apart without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum MalformedPdu {
    /// The PDU was produced by the other network layer: a BTP/GeoNetworking PDU at the WSMP
    /// layer, or a WSMP PDU at the GeoNetworking layer. A configuration error, not a
    /// per-packet event.
    ForeignLayer,
    /// A BTP PDU arrived without a GeoNetworking transport type, so the header size it
    /// claims cannot be checked against a shape.
    MissingGnTransport,
}

impl MalformedPdu {
    /// The stable name this cause is reported under.
    pub const fn as_str(self) -> &'static str {
        match self {
            MalformedPdu::ForeignLayer => "the PDU belongs to the other network layer",
            MalformedPdu::MissingGnTransport => {
                "a BTP PDU arrived without a GeoNetworking transport type"
            }
        }
    }
}

impl core::fmt::Display for MalformedPdu {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Something this crate refuses outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NetError {
    /// The SDU is above the layer's MTU, and neither network layer fragments
    /// (04-models.md §7.1, §7.2). A [`crate::frag::Fragmenter`] must act first.
    #[error(
        "{layer}: an SDU of {sdu_bytes} B exceeds the {mtu} B MTU; neither WSMP nor \
         GeoNetworking fragments, so a fragmenter must act above this layer"
    )]
    Oversize {
        /// The model id of the layer that refused it.
        layer: &'static str,
        /// The SDU's size, bytes.
        sdu_bytes: u32,
        /// The layer's MTU, bytes.
        mtu: u32,
    },

    /// A PSID outside the range the IEEE 1609.3 p-encoding can represent.
    #[error(
        "PSID {psid:#x} is outside the p-encodable range 0..={max:#x} \
         (IEEE 1609.3 VarLengthNumber)"
    )]
    PsidOutOfRange {
        /// The offending value.
        psid: u32,
        /// The largest representable PSID.
        max: u32,
    },

    /// A payload whose length the WSMP-T length field cannot express.
    #[error(
        "a payload of {payload_bytes} B needs a {needed}-octet WSMP-T length field, but the \
         field is 1-2 octets (at most {max} B of payload)"
    )]
    PayloadTooLong {
        /// The payload offered.
        payload_bytes: u32,
        /// Octets the p-encoding would need for that length.
        needed: u32,
        /// The largest payload a two-octet length field can express.
        max: u32,
    },
}

/// `Result<T>` is `Result<T, NetError>`.
pub type Result<T, E = NetError> = core::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_causes_serialise_in_the_spelling_the_record_uses() {
        let json = serde_json::to_string(&DropCause::Mtu {
            sdu_bytes: 1_500,
            mtu: 1_398,
        })
        .unwrap();
        assert_eq!(
            json, r#"{"cause":"mtu","sdu_bytes":1500,"mtu":1398}"#,
            "the tag is the kebab-case cause name"
        );
        let back: DropCause = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back,
            DropCause::Mtu {
                sdu_bytes: 1_500,
                mtu: 1_398
            }
        );
    }

    #[test]
    fn every_error_message_names_the_numbers_a_reader_needs() {
        let e = NetError::Oversize {
            layer: "net/gn-btp/en302636",
            sdu_bytes: 1_600,
            mtu: 1_398,
        };
        let text = e.to_string();
        assert!(text.contains("net/gn-btp/en302636"));
        assert!(text.contains("1600") && text.contains("1398"));
    }
}
