//! Keep the vendored protos pinned to the revision the Go server uses.
//!
//! `livekit/protocol` moves weekly, and a Rust media node that joins a live Go
//! cluster has to speak whatever the Go nodes speak. The pin is therefore not
//! a convenience: a drift between `crates/lk-proto/protobufs/PIN` and the Go
//! server's `go.mod` is a wire-compatibility bug that has not bitten yet.
//!
//! `--check` fails CI on that drift. Updating the pin is deliberate: bump
//! `go.mod`, re-vendor, and let the differential and interop suites say whether
//! anything moved underneath.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

/// `cargo xtask proto-sync`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Fail instead of reporting, when the pin and `go.mod` disagree.
    #[arg(long)]
    pub check: bool,

    /// Path to the Go server's `go.mod`. Defaults to the one above this
    /// workspace.
    #[arg(long)]
    pub go_mod: Option<PathBuf>,
}

/// What the pin file records.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pin {
    /// The vendored git revision.
    pub revision: String,
    /// The Go module pseudo-version the revision belongs to.
    pub go_module_version: String,
}

/// Parse `PIN`'s `key = value` lines, ignoring comments and blanks.
pub fn parse_pin(contents: &str) -> Pin {
    let mut pin = Pin::default();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "revision" => pin.revision = value.trim().to_owned(),
            "go_module_version" => pin.go_module_version = value.trim().to_owned(),
            _ => {}
        }
    }
    pin
}

/// Extract the `github.com/livekit/protocol` version from a `go.mod`.
pub fn protocol_version_in_go_mod(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let line = line.trim();
        // `replace` directives name the module too, but point elsewhere; only
        // the require line states the version in use.
        if line.starts_with("replace ") {
            return None;
        }
        let rest = line.strip_prefix("github.com/livekit/protocol ")?;
        Some(rest.split_whitespace().next()?.to_owned())
    })
}

/// A pseudo-version's trailing 12-character revision, if it has one.
///
/// `v1.51.1-0.20260914053631-a879e945e713` yields `a879e945e713`. A plain
/// tagged version such as `v1.51.0` yields `None`, and is compared by version
/// string instead.
pub fn revision_in_go_version(version: &str) -> Option<&str> {
    let candidate = version.rsplit('-').next()?;
    let is_revision = candidate.len() == 12 && candidate.chars().all(|c| c.is_ascii_hexdigit());
    is_revision.then_some(candidate)
}

/// Run the command.
pub fn run(args: &Args) -> Result<()> {
    let workspace = workspace_root()?;
    let pin_path = workspace.join("crates/lk-proto/protobufs/PIN");
    let pin_contents = std::fs::read_to_string(&pin_path)
        .with_context(|| format!("reading {}", pin_path.display()))?;
    let pin = parse_pin(&pin_contents);

    let go_mod_path = args
        .go_mod
        .clone()
        .unwrap_or_else(|| workspace.join("../go.mod"));
    let go_mod = std::fs::read_to_string(&go_mod_path)
        .with_context(|| format!("reading {}", go_mod_path.display()))?;

    let Some(go_version) = protocol_version_in_go_mod(&go_mod) else {
        bail!(
            "{} does not require github.com/livekit/protocol",
            go_mod_path.display()
        );
    };

    let matches = if pin.go_module_version == go_version {
        true
    } else {
        match revision_in_go_version(&go_version) {
            Some(revision) => pin.revision.starts_with(revision),
            None => false,
        }
    };

    if matches {
        println!("proto pin ok: {} ({})", pin.revision, go_version);
        return Ok(());
    }

    let message = format!(
        "vendored protos are pinned to {} ({}) but {} requires {}.\n\
         Re-vendor with:\n  \
         git clone https://github.com/livekit/protocol /tmp/livekit-protocol\n  \
         git -C /tmp/livekit-protocol checkout {}\n  \
         rm -rf crates/lk-proto/protobufs/* && cp -r /tmp/livekit-protocol/protobufs/. crates/lk-proto/protobufs/\n  \
         (then restore options.proto from livekit/psrpc and update PIN)",
        pin.revision,
        pin.go_module_version,
        go_mod_path.display(),
        go_version,
        revision_in_go_version(&go_version).unwrap_or(&go_version),
    );

    if args.check {
        bail!(message);
    }
    println!("{message}");
    Ok(())
}

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("xtask has no parent directory"))?;
    Ok(root.to_path_buf())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    const GO_MOD: &str = "\
module github.com/livekit/livekit-server

go 1.25

require (
\tgithub.com/livekit/protocol v1.51.1-0.20260914053631-a879e945e713
\tgithub.com/pion/webrtc/v4 v4.2.18
)

replace github.com/pion/webrtc/v4 => github.com/livekit/webrtc-pion/v4 v4.2.18-warp.1
";

    #[test]
    fn reads_the_protocol_version_from_go_mod() {
        assert_eq!(
            protocol_version_in_go_mod(GO_MOD).as_deref(),
            Some("v1.51.1-0.20260914053631-a879e945e713")
        );
    }

    #[test]
    fn ignores_replace_directives() {
        let go_mod = "replace github.com/livekit/protocol => ../protocol\n";
        assert_eq!(protocol_version_in_go_mod(go_mod), None);
    }

    #[test]
    fn extracts_the_revision_from_a_pseudo_version() {
        assert_eq!(
            revision_in_go_version("v1.51.1-0.20260914053631-a879e945e713"),
            Some("a879e945e713")
        );
    }

    #[test]
    fn a_tagged_version_has_no_revision() {
        assert_eq!(revision_in_go_version("v1.51.0"), None);
        // A pre-release suffix that is not a revision must not be mistaken for
        // one, or a release bump would silently pass the pin check.
        assert_eq!(revision_in_go_version("v1.51.0-rc.1"), None);
    }

    #[test]
    fn parses_the_pin_file_ignoring_comments() {
        let pin = parse_pin(
            "# a comment\n\
             repository = https://github.com/livekit/protocol\n\
             revision = a879e945e713e966f17c204ff06873f2aeaa9048\n\
             \n\
             go_module_version = v1.51.1-0.20260914053631-a879e945e713\n",
        );
        assert_eq!(pin.revision, "a879e945e713e966f17c204ff06873f2aeaa9048");
        assert_eq!(
            pin.go_module_version,
            "v1.51.1-0.20260914053631-a879e945e713"
        );
    }

    #[test]
    fn a_short_revision_in_go_mod_matches_the_full_one_in_the_pin() {
        let pin = parse_pin("revision = a879e945e713e966f17c204ff06873f2aeaa9048\n");
        let version = protocol_version_in_go_mod(GO_MOD).unwrap();
        let revision = revision_in_go_version(&version).unwrap();
        assert!(pin.revision.starts_with(revision));
    }

    #[test]
    fn a_different_revision_does_not_match() {
        let pin = parse_pin("revision = 0000000000000000000000000000000000000000\n");
        let version = protocol_version_in_go_mod(GO_MOD).unwrap();
        let revision = revision_in_go_version(&version).unwrap();
        assert!(!pin.revision.starts_with(revision));
    }
}
