//! Mints a token with the fixtures' key and prints it, so the Go verifier can
//! read a Rust-minted token:
//!
//! ```sh
//! token=$(cargo run -p lk-auth --example mint)
//! go run ./livekit-rs/crates/lk-auth/testdata/gotools verify "$token"
//! ```
//!
//! That is the other half of the round trip in `tests/go_interop.rs`, which
//! checks Go-minted tokens against this crate.

use lk_auth::{AccessToken, VideoGrant};

const API_KEY: &str = "devkey";
const API_SECRET: &str = "secret-that-is-at-least-32-characters";

fn main() -> Result<(), lk_auth::Error> {
    let token = AccessToken::new(API_KEY, API_SECRET)
        .with_identity("alice")
        .with_name("Alice")
        .with_video_grant(VideoGrant {
            room_join: true,
            room: "my-room".to_owned(),
            ..VideoGrant::default()
        })
        .to_jwt()?;
    println!("{token}");
    Ok(())
}
