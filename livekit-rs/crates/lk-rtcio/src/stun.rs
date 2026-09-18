//! Just enough STUN to route a first packet.
//!
//! The mux has no PeerConnection to hand an unknown datagram to, so for the
//! very first packet of a flow it reads the ICE `USERNAME` out of the STUN
//! Binding Request and routes on the local ufrag. Everything after that is
//! routed by 5-tuple, so this runs once per flow, not once per packet.
//!
//! This is a deliberate partial parse: no attribute is validated, no integrity
//! is checked, nothing is allocated. `rtc` does the real STUN work once the
//! datagram has been placed on a connection.

/// The STUN magic cookie (RFC 5389 section 6).
const MAGIC_COOKIE: u32 = 0x2112_A442;
/// `USERNAME` attribute type (RFC 5389 section 15.3).
const ATTR_USERNAME: u16 = 0x0006;
/// Binding Request message type (RFC 5389 section 18.1).
const MSG_BINDING_REQUEST: u16 = 0x0001;
/// Fixed STUN header length.
const HEADER_LEN: usize = 20;
/// A ufrag is 4..256 characters (RFC 5245 section 15.4); anything outside that
/// is not worth a hash lookup.
const MAX_UFRAG_LEN: usize = 256;

/// Whether `buf` looks like a STUN message at all.
///
/// Used to tell STUN from DTLS and SRTP on the demultiplexing path: the first
/// two bits of a STUN message are zero and the magic cookie is fixed.
#[must_use]
pub fn is_stun(buf: &[u8]) -> bool {
    let Some(header) = buf.first_chunk::<HEADER_LEN>() else {
        return false;
    };
    // Fixed-size array, constant offsets: the bounds are a compile-time fact
    // rather than a runtime check, which is what this parser needs on a path
    // that runs against unauthenticated bytes.
    header[0] & 0xC0 == 0
        && u32::from_be_bytes([header[4], header[5], header[6], header[7]]) == MAGIC_COOKIE
}

/// Extract the local ufrag from a STUN Binding Request.
///
/// ICE `USERNAME` is `<peer-ufrag>:<sender-ufrag>` (RFC 5245 section 7.1.1.3).
/// In a request *we* receive, the peer is us, so the part before the colon is
/// our local ufrag, which is what the mux registered the connection under.
///
/// Returns `None` for anything that is not a well-formed Binding Request with a
/// plausible `USERNAME`. Nothing here indexes: every read is a `get`, because
/// this runs on bytes from anyone who can reach the published port.
#[must_use]
pub fn local_ufrag_of_binding_request(buf: &[u8]) -> Option<&str> {
    let header = buf.first_chunk::<HEADER_LEN>()?;
    if header[0] & 0xC0 != 0
        || u32::from_be_bytes([header[4], header[5], header[6], header[7]]) != MAGIC_COOKIE
        || u16::from_be_bytes([header[0], header[1]]) != MSG_BINDING_REQUEST
    {
        return None;
    }

    // The header length field counts attribute bytes only, and the sender may
    // have written a shorter buffer than it claims; trust the shorter of the
    // two so a truncated packet cannot walk off the end.
    let claimed = usize::from(u16::from_be_bytes([header[2], header[3]]));
    let end = HEADER_LEN.saturating_add(claimed).min(buf.len());

    let mut at = HEADER_LEN;
    while at.saturating_add(4) <= end {
        let attr_header = buf.get(at..at + 4)?.first_chunk::<4>()?;
        let attr_type = u16::from_be_bytes([attr_header[0], attr_header[1]]);
        let attr_len = usize::from(u16::from_be_bytes([attr_header[2], attr_header[3]]));
        let value_at = at + 4;
        let value_end = value_at.checked_add(attr_len)?;
        if value_end > end {
            return None;
        }

        if attr_type == ATTR_USERNAME {
            let username = buf.get(value_at..value_end)?;
            if username.len() > MAX_UFRAG_LEN * 2 + 1 {
                return None;
            }
            let colon = username.iter().position(|b| *b == b':')?;
            let local = username.get(..colon)?;
            if local.is_empty() || local.len() > MAX_UFRAG_LEN {
                return None;
            }
            return std::str::from_utf8(local).ok();
        }

        // Attributes are padded to a 4-byte boundary; the padding is not
        // counted in the length field.
        at = value_at.checked_add(attr_len.next_multiple_of(4))?;
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// Build a Binding Request carrying `username`, optionally preceded by a
    /// filler attribute so the walk has to skip something.
    fn binding_request(username: &[u8], with_filler: bool) -> Vec<u8> {
        let mut attrs = Vec::new();
        if with_filler {
            // PRIORITY (0x0024), 4 bytes, already aligned.
            attrs.extend_from_slice(&0x0024u16.to_be_bytes());
            attrs.extend_from_slice(&4u16.to_be_bytes());
            attrs.extend_from_slice(&0x7E00_00FFu32.to_be_bytes());
        }
        attrs.extend_from_slice(&ATTR_USERNAME.to_be_bytes());
        attrs.extend_from_slice(&(username.len() as u16).to_be_bytes());
        attrs.extend_from_slice(username);
        attrs.resize(attrs.len().next_multiple_of(4), 0);

        let mut msg = Vec::with_capacity(HEADER_LEN + attrs.len());
        msg.extend_from_slice(&MSG_BINDING_REQUEST.to_be_bytes());
        msg.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
        msg.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        msg.extend_from_slice(&[0xAB; 12]);
        msg.extend_from_slice(&attrs);
        msg
    }

    #[test]
    fn reads_the_local_ufrag_before_the_colon() {
        let msg = binding_request(b"ourUfrag:theirUfrag", false);
        assert_eq!(local_ufrag_of_binding_request(&msg), Some("ourUfrag"));
    }

    #[test]
    fn skips_attributes_ahead_of_username() {
        let msg = binding_request(b"ourUfrag:theirUfrag", true);
        assert_eq!(local_ufrag_of_binding_request(&msg), Some("ourUfrag"));
    }

    #[test]
    fn handles_unaligned_username_padding() {
        // 7 bytes of USERNAME pads to 8; a parser that forgot the padding
        // would resume mid-attribute.
        let msg = binding_request(b"ab:cdef", true);
        assert_eq!(local_ufrag_of_binding_request(&msg), Some("ab"));
    }

    #[test]
    fn rejects_non_stun() {
        // A DTLS ClientHello: content type 22, and no magic cookie.
        let dtls = [
            22u8, 0xFE, 0xFD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert!(!is_stun(&dtls));
        assert_eq!(local_ufrag_of_binding_request(&dtls), None);

        // An SRTP packet: version 2 sets the top two bits, which STUN forbids.
        let srtp = [0x80u8; 32];
        assert!(!is_stun(&srtp));
    }

    #[test]
    fn truncation_never_panics_and_never_invents_a_ufrag() {
        assert_eq!(local_ufrag_of_binding_request(&[]), None);
        let msg = binding_request(b"ourUfrag:theirUfrag", false);
        // The USERNAME attribute ends at 20 (header) + 4 (attr header) + 19.
        const USERNAME_END: usize = 43;
        for cut in 0..msg.len() {
            let got = local_ufrag_of_binding_request(&msg[..cut]);
            if cut < USERNAME_END {
                assert_eq!(got, None, "truncation at {cut} produced {got:?}");
            } else {
                // Trailing padding may be missing; the attribute itself is
                // whole, so reading it is correct.
                assert_eq!(got, Some("ourUfrag"), "truncation at {cut}");
            }
        }
    }

    #[test]
    fn rejects_a_length_field_that_overruns_the_buffer() {
        let mut msg = binding_request(b"ourUfrag:theirUfrag", false);
        msg[2..4].copy_from_slice(&4096u16.to_be_bytes());
        // The claimed length is clamped to the real buffer, so the USERNAME is
        // still found rather than read out of bounds.
        assert_eq!(local_ufrag_of_binding_request(&msg), Some("ourUfrag"));
    }

    #[test]
    fn rejects_a_username_with_no_colon() {
        let msg = binding_request(b"noColonHere", false);
        assert_eq!(local_ufrag_of_binding_request(&msg), None);
    }

    #[test]
    fn rejects_an_empty_local_ufrag() {
        let msg = binding_request(b":theirUfrag", false);
        assert_eq!(local_ufrag_of_binding_request(&msg), None);
    }

    #[test]
    fn rejects_a_binding_response() {
        let mut msg = binding_request(b"ourUfrag:theirUfrag", false);
        msg[0..2].copy_from_slice(&0x0101u16.to_be_bytes());
        assert_eq!(local_ufrag_of_binding_request(&msg), None);
    }
}
