//! Per-client capability gates.
//!
//! Ports `pkg/rtc/clientinfo.go`. The server has to answer questions like "can
//! this client take a RED-encoded audio track" or "will it handle an ICE/TCP
//! candidate" before it has anything but the `ClientInfo` from the join
//! request, and the answers are browser and SDK quirks rather than protocol
//! versions.
//!
//! The version comparison is deliberately the Go one, not semver: Go compares
//! the first three dot-separated components as integers, with a missing or
//! non-numeric component read as zero. A real semver parser would reject
//! versions that the Go server accepts, so the two would disagree on exactly
//! the malformed versions that show up in the field.

use lk_proto::livekit::{ClientInfo, client_info};

use crate::protocol_version::ProtocolVersion;

/// A wrapper over the protobuf `ClientInfo` carrying the capability gates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientInfoExt {
    /// The client info from the join request, absent for a client that sent
    /// none.
    pub info: Option<ClientInfo>,
}

impl ClientInfoExt {
    /// Wraps a client info.
    #[must_use]
    pub fn new(info: Option<ClientInfo>) -> Self {
        Self { info }
    }

    fn sdk(&self) -> client_info::Sdk {
        self.info
            .as_ref()
            .and_then(|i| client_info::Sdk::try_from(i.sdk).ok())
            .unwrap_or(client_info::Sdk::Unknown)
    }

    fn browser(&self) -> &str {
        self.info.as_ref().map_or("", |i| i.browser.as_str())
    }

    fn os(&self) -> &str {
        self.info.as_ref().map_or("", |i| i.os.as_str())
    }

    /// The protocol version the client announced.
    #[must_use]
    pub fn protocol(&self) -> ProtocolVersion {
        ProtocolVersion(self.info.as_ref().map_or(0, |i| i.protocol))
    }

    /// Firefox, desktop or mobile.
    #[must_use]
    pub fn is_firefox(&self) -> bool {
        self.browser().eq_ignore_ascii_case("firefox")
            || self.browser().eq_ignore_ascii_case("firefox mobile")
    }

    /// Safari.
    #[must_use]
    pub fn is_safari(&self) -> bool {
        self.browser().eq_ignore_ascii_case("safari")
    }

    /// The Go SDK, which is pion rather than libwebrtc.
    #[must_use]
    pub fn is_go(&self) -> bool {
        self.sdk() == client_info::Sdk::Go
    }

    /// Linux.
    #[must_use]
    pub fn is_linux(&self) -> bool {
        self.os().eq_ignore_ascii_case("linux")
    }

    /// Android.
    #[must_use]
    pub fn is_android(&self) -> bool {
        self.os().eq_ignore_ascii_case("android")
    }

    /// OBS, which announces itself inside the browser string.
    #[must_use]
    pub fn is_obs(&self) -> bool {
        self.browser().contains("OBS")
    }

    /// RED-encoded audio may be sent to this client.
    #[must_use]
    pub fn supports_audio_red(&self) -> bool {
        !self.is_firefox() && !self.is_safari()
    }

    /// Peer-reflexive candidates over a relay are understood.
    #[must_use]
    pub fn supports_prflx_over_relay(&self) -> bool {
        !self.is_firefox()
    }

    /// The Go SDK fires `on_track` from the first RTP packet; browsers and
    /// libwebrtc fire it from the SDP, so the server must not wait for media.
    #[must_use]
    pub fn fire_track_by_rtp_packet(&self) -> bool {
        self.is_go()
    }

    /// The publisher's codec may be changed mid-session.
    #[must_use]
    pub fn supports_codec_change(&self) -> bool {
        let sdk = self.sdk();
        self.info.is_some() && sdk != client_info::Sdk::Go && sdk != client_info::Sdk::Unknown
    }

    /// A `Reconnect` response is understood. The JS SDK gained it in 1.6.3;
    /// before that an unknown response tore the connection down.
    #[must_use]
    pub fn can_handle_reconnect_response(&self) -> bool {
        if self.sdk() == client_info::Sdk::Js {
            return self.compare_version("1.6.3") >= 0;
        }
        true
    }

    /// ICE/TCP candidates may be offered.
    #[must_use]
    pub fn supports_ice_tcp(&self) -> bool {
        let Some(_) = &self.info else {
            return false;
        };
        match self.sdk() {
            // pion has no active TCP
            client_info::Sdk::Go => false,
            // added in Swift 1.0.5
            client_info::Sdk::Swift => self.compare_version("1.0.5") >= 0,
            _ => true,
        }
    }

    /// `RTCRtpSender.setParameters` may toggle an encoding's `active` flag.
    #[must_use]
    pub fn supports_change_rtp_sender_encoding_active(&self) -> bool {
        !self.is_firefox()
    }

    /// The client honours the codec order in the SDP answer.
    #[must_use]
    pub fn comply_with_codec_order_in_sdp_answer(&self) -> bool {
        (!self.is_linux() && !self.is_android()) || !self.is_firefox()
    }

    /// `TrackSubscribed` may be sent. The Rust SDK could not decode unknown
    /// signal messages before protocol 10.
    #[must_use]
    pub fn supports_track_subscribed_event(&self) -> bool {
        self.sdk() != client_info::Sdk::Rust || self.protocol().get() >= 10
    }

    /// A request/response exchange may be used, which rides the same rule.
    #[must_use]
    pub fn supports_request_response(&self) -> bool {
        self.supports_track_subscribed_event()
    }

    /// SCTP zero checksum may be negotiated.
    #[must_use]
    pub fn supports_sctp_zero_checksum(&self) -> bool {
        self.sdk() != client_info::Sdk::Unknown
            && (!self.is_go() || self.compare_version("2.4.0") >= 0)
    }

    /// Transceivers may be reused. Safari does not cope.
    #[must_use]
    pub fn supports_transceiver_reuse(&self) -> bool {
        !self.is_safari()
    }

    /// Whether the client announced a capability.
    #[must_use]
    pub fn has_capability(&self, capability: client_info::Capability) -> bool {
        self.info
            .as_ref()
            .is_some_and(|i| i.capabilities.contains(&(capability as i32)))
    }

    /// The client announced packet-trailer support.
    #[must_use]
    pub fn supports_packet_trailer(&self) -> bool {
        self.has_capability(client_info::Capability::CapPacketTrailer)
    }

    /// Compares the client's SDK version against `version`, returning 1, 0 or
    /// -1, exactly as `ClientInfo.compareVersion` does: the first three
    /// dot-separated components as integers, anything missing or non-numeric
    /// read as zero. A client with no `ClientInfo` compares as older.
    #[must_use]
    pub fn compare_version(&self, version: &str) -> i32 {
        let Some(info) = &self.info else {
            return -1;
        };
        compare_versions(&info.version, version)
    }
}

/// Go's three-component numeric version comparison.
fn compare_versions(left: &str, right: &str) -> i32 {
    let mut left_parts = left.split('.');
    let mut right_parts = right.split('.');
    for _ in 0..3 {
        let l = component(left_parts.next());
        let r = component(right_parts.next());
        if l > r {
            return 1;
        }
        if l < r {
            return -1;
        }
    }
    0
}

/// One version component: absent or non-numeric reads as zero, which is what
/// Go's `strconv.Atoi` plus ignored error does.
fn component(part: Option<&str>) -> i64 {
    part.and_then(|p| p.parse::<i64>().ok()).unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn info(browser: &str, os: &str, sdk: client_info::Sdk, version: &str) -> ClientInfoExt {
        ClientInfoExt::new(Some(ClientInfo {
            sdk: sdk as i32,
            version: version.to_owned(),
            browser: browser.to_owned(),
            os: os.to_owned(),
            ..ClientInfo::default()
        }))
    }

    #[test]
    fn browser_matching_ignores_case() {
        assert!(info("Firefox", "", client_info::Sdk::Js, "").is_firefox());
        assert!(info("FIREFOX MOBILE", "", client_info::Sdk::Js, "").is_firefox());
        assert!(info("Safari", "", client_info::Sdk::Js, "").is_safari());
        assert!(!info("Chrome", "", client_info::Sdk::Js, "").is_safari());
        assert!(info("chrome (OBS)", "", client_info::Sdk::Js, "").is_obs());
    }

    #[test]
    fn red_is_withheld_from_firefox_and_safari() {
        assert!(info("chrome", "", client_info::Sdk::Js, "").supports_audio_red());
        assert!(!info("firefox", "", client_info::Sdk::Js, "").supports_audio_red());
        assert!(!info("safari", "", client_info::Sdk::Js, "").supports_audio_red());
    }

    #[test]
    fn ice_tcp_follows_the_sdk() {
        assert!(!info("", "", client_info::Sdk::Go, "2.0.0").supports_ice_tcp());
        assert!(!info("", "", client_info::Sdk::Swift, "1.0.4").supports_ice_tcp());
        assert!(info("", "", client_info::Sdk::Swift, "1.0.5").supports_ice_tcp());
        assert!(info("", "", client_info::Sdk::Swift, "1.1.0").supports_ice_tcp());
        assert!(info("", "", client_info::Sdk::Js, "").supports_ice_tcp());
        // no client info at all
        assert!(!ClientInfoExt::default().supports_ice_tcp());
    }

    #[test]
    fn the_js_sdk_gets_reconnect_responses_from_1_6_3() {
        assert!(!info("", "", client_info::Sdk::Js, "1.6.2").can_handle_reconnect_response());
        assert!(info("", "", client_info::Sdk::Js, "1.6.3").can_handle_reconnect_response());
        assert!(info("", "", client_info::Sdk::Js, "1.7.0").can_handle_reconnect_response());
        // other SDKs are unaffected
        assert!(info("", "", client_info::Sdk::Swift, "0.0.1").can_handle_reconnect_response());
    }

    #[test]
    fn version_comparison_matches_gos_three_component_rule() {
        assert_eq!(compare_versions("1.6.3", "1.6.3"), 0);
        assert_eq!(compare_versions("1.6.4", "1.6.3"), 1);
        assert_eq!(compare_versions("1.6.2", "1.6.3"), -1);
        // a missing component reads as zero
        assert_eq!(compare_versions("2", "2.0.0"), 0);
        assert_eq!(compare_versions("2.1", "2.0.9"), 1);
        // so does a non-numeric one, which a semver parser would reject
        assert_eq!(compare_versions("1.6.3-beta", "1.6.3"), -1);
        // "v1" is not a number either, so the leading component is zero and
        // the comparison falls through to the minor component
        assert_eq!(compare_versions("v1.6.3", "0.0.0"), 1);
        assert_eq!(compare_versions("v1.6.3", "0.7.0"), -1);
        // the fourth component is ignored, as in Go
        assert_eq!(compare_versions("1.2.3.4", "1.2.3"), 0);
    }

    #[test]
    fn the_rust_sdk_gets_track_subscribed_from_protocol_10() {
        let mut client = ClientInfoExt::new(Some(ClientInfo {
            sdk: client_info::Sdk::Rust as i32,
            protocol: 9,
            ..ClientInfo::default()
        }));
        assert!(!client.supports_track_subscribed_event());
        assert!(!client.supports_request_response());

        if let Some(info) = client.info.as_mut() {
            info.protocol = 10;
        }
        assert!(client.supports_track_subscribed_event());
        assert!(client.supports_request_response());
    }

    #[test]
    fn sctp_zero_checksum_needs_a_known_sdk_and_a_recent_go() {
        assert!(!ClientInfoExt::default().supports_sctp_zero_checksum());
        assert!(!info("", "", client_info::Sdk::Go, "2.3.9").supports_sctp_zero_checksum());
        assert!(info("", "", client_info::Sdk::Go, "2.4.0").supports_sctp_zero_checksum());
        assert!(info("", "", client_info::Sdk::Js, "0.1.0").supports_sctp_zero_checksum());
    }

    #[test]
    fn codec_order_compliance_follows_platform_and_browser() {
        // Firefox on Linux or Android does not comply
        assert!(
            !info("firefox", "linux", client_info::Sdk::Js, "")
                .comply_with_codec_order_in_sdp_answer()
        );
        assert!(
            !info("firefox", "android", client_info::Sdk::Js, "")
                .comply_with_codec_order_in_sdp_answer()
        );
        // Firefox elsewhere does
        assert!(
            info("firefox", "macos", client_info::Sdk::Js, "")
                .comply_with_codec_order_in_sdp_answer()
        );
        // and so does anything else on Linux
        assert!(
            info("chrome", "linux", client_info::Sdk::Js, "")
                .comply_with_codec_order_in_sdp_answer()
        );
    }

    #[test]
    fn capabilities_come_from_the_announced_list() {
        let client = ClientInfoExt::new(Some(ClientInfo {
            capabilities: vec![client_info::Capability::CapPacketTrailer as i32],
            ..ClientInfo::default()
        }));
        assert!(client.supports_packet_trailer());
        assert!(!ClientInfoExt::default().supports_packet_trailer());
    }

    #[test]
    fn codec_change_needs_a_known_non_go_sdk() {
        assert!(!ClientInfoExt::default().supports_codec_change());
        assert!(!info("", "", client_info::Sdk::Go, "").supports_codec_change());
        assert!(!info("", "", client_info::Sdk::Unknown, "").supports_codec_change());
        assert!(info("", "", client_info::Sdk::Js, "").supports_codec_change());
    }
}
