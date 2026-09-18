//! The SDP fragment parser, fuzzed.
//!
//! A fragment arrives on a WHIP `PATCH` from whoever holds the resource URL and
//! is parsed line by line rather than through the SDP parser, so it is its own
//! attack surface. It must never panic, and anything that parses must marshal
//! and parse back to the same value: a fragment that changes shape on a round
//! trip is one an ICE restart would apply differently than it read.
//!
//! Mirrors `FuzzSDPFragmentUnmarshal` in `protocol/sdp/sdp_test.go`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lk_room::sdp::SdpFragment;

fuzz_target!(|data: &str| {
    let Ok(fragment) = SdpFragment::parse(data) else {
        return;
    };

    // accessors on anything that parsed must be safe
    let _ = fragment.mid();
    let _ = fragment.candidates();
    let _ = fragment.extract_ice_credential();

    let marshalled = fragment.marshal();
    let reparsed = SdpFragment::parse(&marshalled)
        .expect("a marshalled fragment must parse back");
    assert_eq!(fragment, reparsed);
});
