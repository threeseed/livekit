//! The signal protocol version gates.
//!
//! Ports `pkg/rtc/types/protocol_version.go`. Every gate is a comparison
//! against the version the client announced in its join request, and each one
//! guards a message or a behaviour that an older client cannot parse. Getting a
//! gate wrong does not fail a build or a test elsewhere: it sends a frame an
//! old client drops, so the gates are ported as a table and tested as a table.

/// The protocol version this server speaks.
pub const CURRENT_PROTOCOL: i32 = 17;

/// A client's announced signal protocol version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProtocolVersion(pub i32);

impl ProtocolVersion {
    /// The version this server speaks.
    #[must_use]
    pub const fn current() -> Self {
        Self(CURRENT_PROTOCOL)
    }

    /// The raw version number.
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }

    /// Stream ids are packed into one field rather than sent separately.
    #[must_use]
    pub const fn supports_packed_stream_id(self) -> bool {
        self.0 > 0
    }

    /// Signal frames may be protobuf rather than JSON.
    #[must_use]
    pub const fn supports_protobuf(self) -> bool {
        self.0 > 0
    }

    /// The client handles `DataPacket`s.
    #[must_use]
    pub const fn handles_data_packets(self) -> bool {
        self.0 > 1
    }

    /// The client initiates the subscriber connection as primary.
    #[must_use]
    pub const fn subscriber_as_primary(self) -> bool {
        self.0 > 2
    }

    /// The client takes speaker deltas rather than the full list every time.
    #[must_use]
    pub const fn supports_speaker_changed(self) -> bool {
        self.0 > 2
    }

    /// Transceivers may be reused, which keeps the SDP small.
    #[must_use]
    pub const fn supports_transceiver_reuse(self) -> bool {
        self.0 > 3
    }

    /// Connection quality updates are understood, so they can be sent often.
    #[must_use]
    pub const fn supports_connection_quality(self) -> bool {
        self.0 > 4
    }

    /// The client can migrate its session to another node.
    #[must_use]
    pub const fn supports_session_migrate(self) -> bool {
        self.0 > 5
    }

    /// The client copes with an ICE-lite server.
    #[must_use]
    pub const fn supports_ice_lite(self) -> bool {
        self.0 > 5
    }

    /// The client understands `Unpublish`.
    #[must_use]
    pub const fn supports_unpublish(self) -> bool {
        self.0 > 6
    }

    /// Media streams may be sent in the first offer.
    #[must_use]
    pub const fn support_fast_start(self) -> bool {
        self.0 > 7
    }

    /// The client understands a disconnected-participant update.
    #[must_use]
    pub const fn supports_disconnected_update(self) -> bool {
        self.0 > 8
    }

    /// The client understands the synchronised stream id.
    #[must_use]
    pub const fn supports_sync_stream_id(self) -> bool {
        self.0 > 9
    }

    /// The client understands the "connection quality lost" state.
    #[must_use]
    pub const fn supports_connection_quality_lost(self) -> bool {
        self.0 > 10
    }

    /// The room id may arrive after the join response.
    #[must_use]
    pub const fn supports_async_room_id(self) -> bool {
        self.0 > 11
    }

    /// Reconnection is keyed by identity rather than by participant sid.
    #[must_use]
    pub const fn supports_identity_based_reconnection(self) -> bool {
        self.0 > 11
    }

    /// The leave request may carry region information.
    #[must_use]
    pub const fn supports_regions_in_leave_request(self) -> bool {
        self.0 > 12
    }

    /// A non-error signal response is understood.
    #[must_use]
    pub const fn supports_non_error_signal_response(self) -> bool {
        self.0 > 14
    }

    /// The client can be moved to another room.
    #[must_use]
    pub const fn supports_moving(self) -> bool {
        self.0 > 15
    }

    /// The client understands the packet trailer.
    #[must_use]
    pub const fn supports_packet_trailer(self) -> bool {
        self.0 > 16
    }
}

impl From<i32> for ProtocolVersion {
    fn from(value: i32) -> Self {
        Self(value)
    }
}

impl From<ProtocolVersion> for i32 {
    fn from(value: ProtocolVersion) -> Self {
        value.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// Each gate with the lowest version that passes it, from
    /// `protocol_version.go`. A gate that moves without this table moving is a
    /// silent wire-compatibility break.
    #[allow(clippy::type_complexity)]
    const GATES: &[(&str, fn(ProtocolVersion) -> bool, i32)] = &[
        (
            "supports_packed_stream_id",
            ProtocolVersion::supports_packed_stream_id,
            1,
        ),
        ("supports_protobuf", ProtocolVersion::supports_protobuf, 1),
        (
            "handles_data_packets",
            ProtocolVersion::handles_data_packets,
            2,
        ),
        (
            "subscriber_as_primary",
            ProtocolVersion::subscriber_as_primary,
            3,
        ),
        (
            "supports_speaker_changed",
            ProtocolVersion::supports_speaker_changed,
            3,
        ),
        (
            "supports_transceiver_reuse",
            ProtocolVersion::supports_transceiver_reuse,
            4,
        ),
        (
            "supports_connection_quality",
            ProtocolVersion::supports_connection_quality,
            5,
        ),
        (
            "supports_session_migrate",
            ProtocolVersion::supports_session_migrate,
            6,
        ),
        ("supports_ice_lite", ProtocolVersion::supports_ice_lite, 6),
        ("supports_unpublish", ProtocolVersion::supports_unpublish, 7),
        ("support_fast_start", ProtocolVersion::support_fast_start, 8),
        (
            "supports_disconnected_update",
            ProtocolVersion::supports_disconnected_update,
            9,
        ),
        (
            "supports_sync_stream_id",
            ProtocolVersion::supports_sync_stream_id,
            10,
        ),
        (
            "supports_connection_quality_lost",
            ProtocolVersion::supports_connection_quality_lost,
            11,
        ),
        (
            "supports_async_room_id",
            ProtocolVersion::supports_async_room_id,
            12,
        ),
        (
            "supports_identity_based_reconnection",
            ProtocolVersion::supports_identity_based_reconnection,
            12,
        ),
        (
            "supports_regions_in_leave_request",
            ProtocolVersion::supports_regions_in_leave_request,
            13,
        ),
        (
            "supports_non_error_signal_response",
            ProtocolVersion::supports_non_error_signal_response,
            15,
        ),
        ("supports_moving", ProtocolVersion::supports_moving, 16),
        (
            "supports_packet_trailer",
            ProtocolVersion::supports_packet_trailer,
            17,
        ),
    ];

    #[test]
    fn every_gate_opens_at_the_version_the_go_server_opens_it() {
        for (name, gate, first_supported) in GATES {
            for version in 0..=CURRENT_PROTOCOL + 1 {
                let expected = version >= *first_supported;
                assert_eq!(
                    gate(ProtocolVersion(version)),
                    expected,
                    "{name} at protocol {version}"
                );
            }
        }
    }

    #[test]
    fn the_current_protocol_passes_every_gate() {
        let current = ProtocolVersion::current();
        for (name, gate, _) in GATES {
            assert!(gate(current), "{name} must pass at the current protocol");
        }
    }

    #[test]
    fn protocol_zero_passes_nothing() {
        let zero = ProtocolVersion(0);
        for (name, gate, _) in GATES {
            assert!(!gate(zero), "{name} must not pass at protocol 0");
        }
    }
}
