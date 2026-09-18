//! Prefixed GUIDs.
//!
//! Ports `protocol/utils/guid`. Every id the server hands a client carries a
//! type prefix (`RM_`, `PA_`, `TR_`) followed by twelve characters from
//! shortuuid's base-57 alphabet. Clients and logs parse the prefix, so the
//! shape is part of the wire contract even though the characters are random.
//!
//! The alphabet leaves out the characters that read alike in a log line:
//! `0`, `O`, `1`, `I` and `l`.

use rand::Rng as _;

/// Characters a GUID body is drawn from: shortuuid's default alphabet.
pub const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// How many characters follow the prefix.
pub const SIZE: usize = 12;

/// The `RM_` prefix: a room.
pub const ROOM_PREFIX: &str = "RM_";
/// The `ND_` prefix: a node.
pub const NODE_PREFIX: &str = "ND_";
/// The `PA_` prefix: a participant.
pub const PARTICIPANT_PREFIX: &str = "PA_";
/// The `TR_` prefix: a track.
pub const TRACK_PREFIX: &str = "TR_";
/// The `DTR_` prefix: a data track.
pub const DATA_TRACK_PREFIX: &str = "DTR_";
/// The `CO_` prefix: a signal connection.
pub const CONNECTION_PREFIX: &str = "CO_";

/// A generated identifier.
pub type Guid = String;

/// A new GUID with the given prefix.
#[must_use]
pub fn new_guid(prefix: &str) -> Guid {
    let mut rng = rand::rng();
    let mut out = String::with_capacity(prefix.len() + SIZE);
    out.push_str(prefix);
    for _ in 0..SIZE {
        let index = rng.random_range(0..ALPHABET.len());
        out.push(char::from(ALPHABET[index]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guid_is_its_prefix_plus_twelve_alphabet_characters() {
        let id = new_guid(PARTICIPANT_PREFIX);
        assert!(id.starts_with("PA_"));
        assert_eq!(id.len(), PARTICIPANT_PREFIX.len() + SIZE);
        assert!(
            id[PARTICIPANT_PREFIX.len()..]
                .bytes()
                .all(|b| ALPHABET.contains(&b))
        );
    }

    #[test]
    fn guids_do_not_repeat() {
        let ids: std::collections::BTreeSet<_> = (0..1_000).map(|_| new_guid("RM_")).collect();
        assert_eq!(ids.len(), 1_000);
    }

    #[test]
    fn the_alphabet_leaves_out_lookalike_characters() {
        for confusable in [b'0', b'O', b'1', b'I', b'l'] {
            assert!(!ALPHABET.contains(&confusable));
        }
    }
}
