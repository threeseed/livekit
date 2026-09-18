//! SDP helper tests, ported from `protocol/sdp/sdp_test.go`.
//!
//! The fixtures are the Go test's fixtures verbatim, so a disagreement between
//! the two implementations shows up as a failure here rather than as a client
//! that cannot connect.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use lk_room::sdp::{
    SdpFragment, bundle_mid, codecs_from_media_description, extract_dtls_role, extract_fingerprint,
    extract_ice_credential, extract_sdp_fragment, extract_stream_id, mid_value, simulcast_rids,
};
use rtc::peer_connection::transport::RTCDtlsRole;
use rtc_sdp::description::session::SessionDescription;

const OFFER: &str = "v=0\r\no=- 4648475892259889561 3 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0 1\r\na=ice-ufrag:1hhfzwf0ijpzm\r\na=ice-pwd:jm5puo2ab1op3vs59ca53bdk7s\r\na=fingerprint:sha-256 40:42:FB:47:87:52:BF:CB:EC:3A:DF:EB:06:DA:2D:B7:2F:59:42:10:23:7B:9D:4C:C9:58:DD:FF:A2:8F:17:67\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=rtcp:9 IN IP4 0.0.0.0\r\na=setup:passive\r\na=mid:0\r\na=sendonly\r\na=rtcp-mux\r\na=rtpmap:96 H264/90000\r\na=rtcp-fb:96 nack\r\na=rtcp-fb:96 goog-remb\r\na=fmtp:96 packetization-mode=1;profile-level-id=42e01f\r\na=ssrc:1505338584 cname:10000000b5810aac\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=rtcp:9 IN IP4 0.0.0.0\r\na=setup:passive\r\na=mid:1\r\na=sendonly\r\na=rtcp-mux\r\na=rtpmap:111 opus/48000/2\r\na=ssrc:697641945 cname:10000000b5810aac\r\n";

const FRAGMENT: &str = "a=ice-lite\r\na=ice-options:trickle ice2\r\na=group:BUNDLE 0 1\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=ice-ufrag:ysXw\r\na=ice-pwd:vw5LmwG4y/e6dPP/zAP9Gp5k\r\na=candidate:1387637174 1 udp 2122260223 192.0.2.1 61764 typ host generation 0 ufrag EsAw network-id 1\r\na=candidate:3471623853 1 udp 2122194687 198.51.100.2 61765 typ host generation 0 ufrag EsAw network-id 2\r\na=candidate:473322822 1 tcp 1518280447 192.0.2.1 9 typ host tcptype active generation 0 ufrag EsAw network-id 1\r\na=candidate:2154773085 1 tcp 1518214911 198.51.100.2 9 typ host tcptype active generation 0 ufrag EsAw network-id 2\r\na=candidate:393455558 0 tcp 1518283007 [2401:4900:633c:959f:2037:680c:7c40:b3db] 9 typ host tcptype active\r\n";

/// The offer the fragment is patched into: ICE credentials at media level, so
/// the patch has somewhere to write.
const OFFER_WITH_MEDIA_ICE: &str = "v=0\r\no=- 4648475892259889561 3 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0 1\r\na=ice-options:trickle ice2\r\na=fingerprint:sha-256 40:42:FB:47:87:52:BF:CB:EC:3A:DF:EB:06:DA:2D:B7:2F:59:42:10:23:7B:9D:4C:C9:58:DD:FF:A2:8F:17:67\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=ice-ufrag:1hhfzwf0ijpzm\r\na=ice-pwd:jm5puo2ab1op3vs59ca53bdk7s\r\na=rtcp:9 IN IP4 0.0.0.0\r\na=setup:passive\r\na=mid:0\r\na=sendonly\r\na=rtcp-mux\r\na=rtpmap:96 H264/90000\r\na=rtcp-fb:96 nack\r\na=rtcp-fb:96 goog-remb\r\na=fmtp:96 packetization-mode=1;profile-level-id=42e01f\r\na=ssrc:1505338584 cname:10000000b5810aac\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=rtcp:9 IN IP4 0.0.0.0\r\na=setup:passive\r\na=mid:1\r\na=sendonly\r\na=rtcp-mux\r\na=rtpmap:111 opus/48000/2\r\na=ssrc:697641945 cname:10000000b5810aac\r\n";

const EXPECTED_CANDIDATES: [&str; 5] = [
    "1387637174 1 udp 2122260223 192.0.2.1 61764 typ host generation 0 ufrag EsAw network-id 1",
    "3471623853 1 udp 2122194687 198.51.100.2 61765 typ host generation 0 ufrag EsAw network-id 2",
    "473322822 1 tcp 1518280447 192.0.2.1 9 typ host tcptype active generation 0 ufrag EsAw network-id 1",
    "2154773085 1 tcp 1518214911 198.51.100.2 9 typ host tcptype active generation 0 ufrag EsAw network-id 2",
    "393455558 0 tcp 1518283007 [2401:4900:633c:959f:2037:680c:7c40:b3db] 9 typ host tcptype active",
];

fn parse(sdp: &str) -> SessionDescription {
    let mut reader = std::io::Cursor::new(sdp.as_bytes());
    SessionDescription::unmarshal(&mut reader).expect("fixture must parse")
}

#[test]
fn reads_the_fields_the_go_helpers_read() {
    let parsed = parse(OFFER);

    assert_eq!(mid_value(&parsed.media_descriptions[0]), "0");
    assert_eq!(mid_value(&parsed.media_descriptions[1]), "1");

    let (fingerprint, algorithm) = extract_fingerprint(&parsed).unwrap();
    assert_eq!(
        fingerprint,
        "40:42:FB:47:87:52:BF:CB:EC:3A:DF:EB:06:DA:2D:B7:2F:59:42:10:23:7B:9D:4C:C9:58:DD:FF:A2:8F:17:67"
    );
    assert_eq!(algorithm, "sha-256");

    // a=setup:passive means the remote accepts the connection, so this end is
    // the DTLS server
    assert_eq!(extract_dtls_role(&parsed), RTCDtlsRole::Server);

    let (ufrag, pwd) = extract_ice_credential(&parsed).unwrap();
    assert_eq!(ufrag, "1hhfzwf0ijpzm");
    assert_eq!(pwd, "jm5puo2ab1op3vs59ca53bdk7s");

    // no a=msid, no a=simulcast
    assert_eq!(extract_stream_id(&parsed.media_descriptions[0]), None);
    assert_eq!(simulcast_rids(&parsed.media_descriptions[1]), None);

    let codecs = codecs_from_media_description(&parsed.media_descriptions[0]).unwrap();
    assert_eq!(codecs.len(), 1);
    let codec = &codecs[0];
    assert_eq!(codec.payload_type, 96);
    assert_eq!(codec.name, "H264");
    assert_eq!(codec.clock_rate, 90000);
    assert_eq!(codec.fmtp, "packetization-mode=1;profile-level-id=42e01f");
    assert_eq!(codec.rtcp_feedback, vec!["nack", "goog-remb"]);

    assert_eq!(bundle_mid(&parsed).as_deref(), Some("0"));
}

#[test]
fn a_description_with_no_setup_attribute_answers_as_client() {
    let without_setup = OFFER.replace("a=setup:passive\r\n", "");
    assert_eq!(
        extract_dtls_role(&parse(&without_setup)),
        RTCDtlsRole::Client
    );
}

#[test]
fn conflicting_credentials_are_refused() {
    let conflicting = OFFER.replace(
        "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\n",
        "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=ice-ufrag:someoneelse\r\n",
    );
    assert_eq!(
        extract_ice_credential(&parse(&conflicting)),
        Err(lk_room::Error::ConflictingIceUfrag)
    );

    let conflicting = OFFER.replace(
        "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\n",
        "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=fingerprint:sha-256 AA:BB\r\n",
    );
    assert_eq!(
        extract_fingerprint(&parse(&conflicting)),
        Err(lk_room::Error::ConflictingFingerprints)
    );
}

#[test]
fn a_fragment_round_trips() {
    let fragment = SdpFragment::parse(FRAGMENT).unwrap();

    assert_eq!(fragment.group, "BUNDLE 0 1");
    assert_eq!(fragment.ice.options, "trickle ice2");
    assert_eq!(fragment.ice.lite, Some(true));
    assert!(fragment.ice.ufrag.is_empty());

    let media = fragment.media.as_ref().unwrap();
    assert_eq!(media.info, "audio 9 UDP/TLS/RTP/SAVPF 111");
    assert_eq!(media.mid, "0");
    assert_eq!(media.ice.ufrag, "ysXw");
    assert_eq!(media.ice.pwd, "vw5LmwG4y/e6dPP/zAP9Gp5k");
    assert_eq!(media.candidates, EXPECTED_CANDIDATES);

    // the marshalled form is the Go one, byte for byte: session attributes,
    // then the media section
    let marshalled = fragment.marshal();
    let expected = format!(
        "a=group:BUNDLE 0 1\r\na=ice-lite\r\na=ice-options:trickle ice2\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=ice-ufrag:ysXw\r\na=ice-pwd:vw5LmwG4y/e6dPP/zAP9Gp5k\r\n{}",
        EXPECTED_CANDIDATES
            .iter()
            .map(|c| format!("a=candidate:{c}\r\n"))
            .collect::<String>()
    );
    assert_eq!(marshalled, expected);

    // and it parses back to the same fragment
    let reparsed = SdpFragment::parse(&marshalled).unwrap();
    assert_eq!(reparsed, fragment);
    assert_eq!(reparsed.mid(), "0");
    assert_eq!(reparsed.candidates(), EXPECTED_CANDIDATES);
    let (ufrag, pwd) = reparsed.extract_ice_credential().unwrap();
    assert_eq!(ufrag, "ysXw");
    assert_eq!(pwd, "vw5LmwG4y/e6dPP/zAP9Gp5k");
}

#[test]
fn a_fragment_whose_mid_is_not_the_bundle_mid_is_refused() {
    let mismatched = FRAGMENT.replace("a=mid:0", "a=mid:1");
    assert!(SdpFragment::parse(&mismatched).is_err());
}

#[test]
fn a_fragment_patches_credentials_and_candidates_into_a_description() {
    let fragment = SdpFragment::parse(FRAGMENT).unwrap();
    let mut parsed = parse(OFFER_WITH_MEDIA_ICE);

    fragment.patch_into(&mut parsed).unwrap();

    let (ufrag, pwd) = extract_ice_credential(&parsed).unwrap();
    assert_eq!(ufrag, "ysXw");
    assert_eq!(pwd, "vw5LmwG4y/e6dPP/zAP9Gp5k");

    let candidates: Vec<&str> = parsed.media_descriptions[0]
        .attributes
        .iter()
        .filter(|a| a.is_ice_candidate())
        .filter_map(|a| a.value.as_deref())
        .collect();
    assert_eq!(candidates, EXPECTED_CANDIDATES);

    // extracting a fragment back out gives the bundle media section
    let extracted = extract_sdp_fragment(&parsed).unwrap();
    assert_eq!(extracted.group, "BUNDLE 0 1");
    assert_eq!(extracted.ice.options, "trickle ice2");
    let media = extracted.media.as_ref().unwrap();
    assert_eq!(media.info, "video 9 UDP/TLS/RTP/SAVPF 96");
    assert_eq!(media.mid, "0");
    assert_eq!(media.ice.ufrag, "ysXw");
    assert_eq!(media.candidates, EXPECTED_CANDIDATES);
}

#[test]
fn a_patch_disagreeing_about_ice_lite_is_refused() {
    // the description says ice-lite and the fragment does not, so applying the
    // fragment would quietly change the ICE role
    let fragment = SdpFragment::parse(&FRAGMENT.replace("a=ice-lite\r\n", "")).unwrap();
    let mut parsed = parse(&OFFER_WITH_MEDIA_ICE.replace(
        "a=ice-options:trickle ice2\r\n",
        "a=ice-options:trickle ice2\r\na=ice-lite\r\n",
    ));
    assert!(fragment.patch_into(&mut parsed).is_err());
}

#[test]
fn a_patch_naming_an_absent_mid_is_refused() {
    let fragment = SdpFragment::parse(FRAGMENT).unwrap();
    let mut parsed = parse(&OFFER_WITH_MEDIA_ICE.replace("a=mid:0", "a=mid:9"));
    assert!(fragment.patch_into(&mut parsed).is_err());
}

#[test]
fn malformed_fragments_are_errors_not_panics() {
    // the list from TestSDPFragmentUnmarshalMalformed
    for malformed in [
        "",
        "m",
        "a",
        "m\r\n",
        "a\r\n",
        "m=",
        "a=",
        "mx=audio 9 UDP/TLS/RTP/SAVPF 111",
        "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na",
        "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nax\r\n",
        "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n",
        "a=ice-lite\r\na=ice-ufrag:ysXw\r\n",
    ] {
        assert!(
            SdpFragment::parse(malformed).is_err(),
            "fragment {malformed:?} must be refused"
        );
    }
}

#[test]
fn a_description_without_a_bundle_group_yields_no_fragment() {
    let without_group = OFFER.replace("a=group:BUNDLE 0 1\r\n", "");
    assert!(extract_sdp_fragment(&parse(&without_group)).is_err());
    assert_eq!(bundle_mid(&parse(&without_group)), None);
}
