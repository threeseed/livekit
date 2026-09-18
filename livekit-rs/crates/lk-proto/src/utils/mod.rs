//! Shared identifiers and version stamps.
//!
//! Ports the parts of `protocol/utils` and `pkg/utils` that every crate needs:
//! the prefixed GUIDs that name rooms, participants and tracks on the wire, and
//! the `TimedVersion` stamps that order updates across nodes.

pub mod guid;
pub mod timed_version;

pub use guid::{Guid, new_guid};
pub use timed_version::{TimedVersion, TimedVersionGenerator};
