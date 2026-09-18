//! Monotonic version stamps.
//!
//! Ports `protocol/utils.TimedVersion`. Participant and room updates carry one
//! so a client can drop an update it has already applied, and nodes compare
//! them against each other, so the encoding has to be bit-identical to the Go
//! one: a microsecond timestamp since 1999-11-30 shifted left by thirteen bits,
//! with a thirteen-bit tick counter in the low bits for versions minted within
//! the same microsecond.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Bits reserved for the tick counter.
const TICK_BITS: u64 = 13;

/// Mask covering the tick counter.
const TICK_MASK: u64 = (1 << TICK_BITS) - 1;

/// The epoch, in microseconds since the Unix epoch.
///
/// Go writes this as `time.Date(2000, 0, 0, ...)`, which normalises to
/// 1999-11-30T00:00:00Z. The odd value is part of the encoding, so it is
/// reproduced rather than rounded to the year 2000.
pub const EPOCH_MICROS: i64 = 943_920_000_000_000;

/// A version stamp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimedVersion(pub u64);

impl TimedVersion {
    /// Builds a version from a microsecond timestamp and a tick counter.
    ///
    /// A timestamp before the epoch is clamped to it, as in Go: a clock that
    /// far behind would otherwise wrap into a version from the future.
    #[must_use]
    pub const fn from_components(unix_micros: i64, ticks: i32) -> Self {
        let micros = if unix_micros < EPOCH_MICROS {
            EPOCH_MICROS
        } else {
            unix_micros
        };
        Self((((micros - EPOCH_MICROS) as u64) << TICK_BITS) | (ticks as u64 & TICK_MASK))
    }

    /// The timestamp and tick counter this version encodes.
    #[must_use]
    pub const fn components(self) -> (i64, i32) {
        (
            ((self.0 >> TICK_BITS) as i64) + EPOCH_MICROS,
            (self.0 & TICK_MASK) as i32,
        )
    }

    /// The version as the protobuf message clients receive.
    #[must_use]
    pub fn to_proto(self) -> crate::livekit::TimedVersion {
        let (unix_micro, ticks) = self.components();
        crate::livekit::TimedVersion { unix_micro, ticks }
    }

    /// A version from the protobuf message.
    #[must_use]
    pub const fn from_proto(proto: &crate::livekit::TimedVersion) -> Self {
        Self::from_components(proto.unix_micro, proto.ticks)
    }

    /// Takes `other` when it is newer. Returns whether anything changed.
    pub const fn upgrade(&mut self, other: Self) -> bool {
        if self.0 < other.0 {
            self.0 = other.0;
            return true;
        }
        false
    }
}

/// Mints versions that never repeat and never go backwards.
#[derive(Debug, Default)]
pub struct TimedVersionGenerator {
    state: Mutex<GeneratorState>,
}

#[derive(Debug, Default)]
struct GeneratorState {
    micros: i64,
    ticks: u64,
}

impl TimedVersionGenerator {
    /// A generator starting from the current clock.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The next version.
    ///
    /// Within one microsecond the tick counter advances; a clock that goes
    /// backwards is ignored in favour of the last version minted, because a
    /// version that repeats would make a client drop a real update.
    pub fn next(&self) -> TimedVersion {
        let mut now = unix_micros();
        let Ok(mut state) = self.state.lock() else {
            // A poisoned lock means another thread panicked mid-mint. The
            // clock still moves, so a version from it is safe to hand out.
            return TimedVersion::from_components(now, 0);
        };

        loop {
            if now < state.micros {
                now = state.micros;
            }
            if state.micros == now {
                if state.ticks == TICK_MASK {
                    // 8,192 versions in one microsecond: wait for the clock
                    // rather than wrapping into the next microsecond's range
                    std::thread::sleep(std::time::Duration::from_micros(1));
                    now = unix_micros();
                    continue;
                }
                state.ticks += 1;
            } else {
                state.micros = now;
                state.ticks = 0;
            }
            return TimedVersion::from_components(state.micros, state.ticks as i32);
        }
    }
}

fn unix_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn components_round_trip() {
        let version = TimedVersion::from_components(1_700_000_000_000_000, 5);
        assert_eq!(version.components(), (1_700_000_000_000_000, 5));
    }

    #[test]
    fn a_timestamp_before_the_epoch_is_clamped() {
        let version = TimedVersion::from_components(0, 0);
        assert_eq!(version.components(), (EPOCH_MICROS, 0));
    }

    #[test]
    fn the_proto_form_round_trips() {
        let version = TimedVersion::from_components(1_700_000_000_000_123, 7);
        assert_eq!(TimedVersion::from_proto(&version.to_proto()), version);
    }

    #[test]
    fn versions_are_strictly_increasing() {
        let generator = TimedVersionGenerator::new();
        let mut previous = generator.next();
        for _ in 0..10_000 {
            let next = generator.next();
            assert!(next > previous, "{next:?} must be newer than {previous:?}");
            previous = next;
        }
    }

    #[test]
    fn upgrade_takes_the_newer_version_only() {
        let mut version = TimedVersion::from_components(1_000_000_000_000_000, 0);
        let newer = TimedVersion::from_components(1_000_000_000_000_001, 0);
        assert!(version.upgrade(newer));
        assert_eq!(version, newer);
        assert!(!version.upgrade(TimedVersion::from_components(999_999_999_999_999, 0)));
        assert_eq!(version, newer);
    }
}
