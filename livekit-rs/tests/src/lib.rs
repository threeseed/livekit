//! Cross-crate integration tests.
//!
//! The crate itself holds only shared fixtures; the tests live under `tests/`
//! so each is its own binary and `cargo nextest` can run them in parallel.
