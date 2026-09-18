//! The mux's first-packet parser, fuzzed.
//!
//! This parser runs on bytes from anyone who can reach the published UDP port,
//! before any ICE or DTLS check has happened, so it is the most exposed code in
//! the server. It must never panic and never read out of bounds, whatever it is
//! handed.
//!
//! Phase 0 ships this one target so later phases (dependency descriptor, VP8,
//! RED, abs-capture-time, playout delay, signal frames, SDP munging, TWCC) only
//! add to existing infrastructure.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ufrag) = lk_rtcio::stun::local_ufrag_of_binding_request(data) {
        // A returned ufrag must be a plausible routing key: non-empty, within
        // the RFC 5245 bound, and free of the separator it was split on.
        assert!(!ufrag.is_empty());
        assert!(ufrag.len() <= 256);
        assert!(!ufrag.contains(':'));
        assert!(lk_rtcio::stun::is_stun(data));
    }
});
