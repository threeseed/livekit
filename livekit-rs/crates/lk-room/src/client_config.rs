//! Device-side limitations applied globally.
//!
//! Ports `pkg/clientconfiguration`. The Go server evaluates the rules as
//! `tengo` scripts compiled at start-up; three rules exist today, and an
//! embedded scripting language for three predicates buys a parser, a VM and a
//! class of runtime errors (`NewScriptMatch` can fail, and `Match` can fail per
//! call) for no expressiveness this server uses. They are plain predicates
//! here, so a rule that does not compile is a build failure rather than a log
//! line at start-up.
//!
//! The version comparison inside the rules keeps Go's behaviour: a valid semver
//! on both sides compares as semver, anything else compares as a string.

use lk_proto::livekit::{ClientConfiguration, ClientInfo, Codec, DisabledCodecs};

/// The rule set, evaluated in order.
#[derive(Clone, Copy, Debug, Default)]
pub struct StaticClientConfigurationManager;

/// One rule: a predicate over the client info, the configuration it applies,
/// and whether later rules may still contribute.
struct Rule {
    matches: fn(&ClientInfo) -> bool,
    configuration: fn() -> ClientConfiguration,
    /// `false` ends evaluation with this rule's configuration, as Go's
    /// non-merging item does.
    merge: bool,
}

/// `StaticConfigurations` from `pkg/clientconfiguration/conf.go`.
const RULES: &[Rule] = &[
    // Safari cannot decode AV1.
    Rule {
        matches: is_safari,
        configuration: || ClientConfiguration {
            disabled_codecs: Some(DisabledCodecs {
                codecs: vec![codec("video/AV1")],
                publish: Vec::new(),
            }),
            ..empty_configuration()
        },
        merge: true,
    },
    // Safari after 18.3 cannot publish VP9.
    Rule {
        matches: is_safari_after_18_3,
        configuration: || ClientConfiguration {
            disabled_codecs: Some(DisabledCodecs {
                codecs: Vec::new(),
                publish: vec![codec("video/VP9")],
            }),
            ..empty_configuration()
        },
        merge: true,
    },
    // One Xiaomi model on Android, and Firefox on Linux or Android, cannot
    // publish H.264.
    Rule {
        matches: cannot_publish_h264,
        configuration: || ClientConfiguration {
            disabled_codecs: Some(DisabledCodecs {
                codecs: Vec::new(),
                publish: vec![codec("video/H264")],
            }),
            ..empty_configuration()
        },
        merge: false,
    },
];

impl StaticClientConfigurationManager {
    /// The configuration for a client, or `None` when no rule matches.
    ///
    /// Merging follows the Go server: a matching non-merging rule returns its
    /// configuration and stops, and merging rules are combined in order. The
    /// combination is a field-wise merge of the set fields only, which is what
    /// `proto.Merge` does and why the Go code carries a comment about zero
    /// values: a `false` or a `0` cannot be told from "not set", so it never
    /// overrides an earlier rule.
    #[must_use]
    pub fn configuration(&self, client_info: &ClientInfo) -> Option<ClientConfiguration> {
        let mut merged: Option<ClientConfiguration> = None;
        for rule in RULES {
            if !(rule.matches)(client_info) {
                continue;
            }
            let configuration = (rule.configuration)();
            if !rule.merge {
                return Some(configuration);
            }
            merged = Some(match merged {
                None => configuration,
                Some(existing) => merge(existing, configuration),
            });
        }
        merged
    }
}

/// Field-wise merge with `proto.Merge` semantics for the fields these rules
/// set: scalars only override when non-zero, and repeated fields concatenate.
fn merge(mut into: ClientConfiguration, from: ClientConfiguration) -> ClientConfiguration {
    if from.resume_connection != 0 {
        into.resume_connection = from.resume_connection;
    }
    if from.force_relay != 0 {
        into.force_relay = from.force_relay;
    }
    if let Some(video) = from.video {
        into.video = Some(video);
    }
    if let Some(screen) = from.screen {
        into.screen = Some(screen);
    }
    if let Some(disabled) = from.disabled_codecs {
        let target = into.disabled_codecs.get_or_insert_with(Default::default);
        target.codecs.extend(disabled.codecs);
        target.publish.extend(disabled.publish);
    }
    into
}

const fn empty_configuration() -> ClientConfiguration {
    ClientConfiguration {
        video: None,
        screen: None,
        resume_connection: 0,
        disabled_codecs: None,
        force_relay: 0,
    }
}

fn codec(mime: &str) -> Codec {
    Codec {
        mime: mime.to_owned(),
        fmtp_line: String::new(),
    }
}

fn is_safari(info: &ClientInfo) -> bool {
    info.browser.to_lowercase() == "safari"
}

fn is_safari_after_18_3(info: &ClientInfo) -> bool {
    is_safari(info) && compare_rule_version(&info.browser_version, "18.3") > 0
}

fn cannot_publish_h264(info: &ClientInfo) -> bool {
    let browser = info.browser.to_lowercase();
    let os = info.os.to_lowercase();
    let device_model = info.device_model.to_lowercase();

    (device_model == "xiaomi 2201117ti" && os == "android")
        || ((browser == "firefox" || browser == "firefox mobile")
            && (os == "linux" || os == "android"))
}

/// Compares two version strings the way the Go rule engine's `sdkVersion`
/// does: as semver when both sides are valid semver, and as plain strings
/// otherwise.
fn compare_rule_version(left: &str, right: &str) -> i32 {
    match (parse_semver(left), parse_semver(right)) {
        (Some(l), Some(r)) => match l.cmp(&r) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
        _ => match left.cmp(right) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
    }
}

/// `golang.org/x/mod/semver` accepts `vMAJOR[.MINOR[.PATCH]]` with optional
/// pre-release and build metadata, which is why the Go rule prefixes a `v`
/// before validating. Pre-release ordering is not reproduced: no rule uses it,
/// and a pre-release browser version would fall back to the string compare in
/// Go as well.
fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    if parts.next().is_some() {
        // more than three components is not valid semver
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    fn client(browser: &str, browser_version: &str, os: &str, device_model: &str) -> ClientInfo {
        ClientInfo {
            browser: browser.to_owned(),
            browser_version: browser_version.to_owned(),
            os: os.to_owned(),
            device_model: device_model.to_owned(),
            ..ClientInfo::default()
        }
    }

    fn mimes(codecs: &[Codec]) -> Vec<&str> {
        codecs.iter().map(|c| c.mime.as_str()).collect()
    }

    #[test]
    fn a_client_matching_nothing_gets_no_configuration() {
        let manager = StaticClientConfigurationManager;
        assert!(
            manager
                .configuration(&client("chrome", "120.0", "macos", ""))
                .is_none()
        );
    }

    #[test]
    fn safari_cannot_use_av1() {
        let manager = StaticClientConfigurationManager;
        let config = manager
            .configuration(&client("safari", "17.0", "macos", ""))
            .expect("safari matches the AV1 rule");
        let disabled = config.disabled_codecs.unwrap();
        assert_eq!(mimes(&disabled.codecs), vec!["video/AV1"]);
        assert!(disabled.publish.is_empty());
    }

    #[test]
    fn safari_after_18_3_also_cannot_publish_vp9() {
        let manager = StaticClientConfigurationManager;

        let config = manager
            .configuration(&client("Safari", "18.4", "macos", ""))
            .expect("both safari rules match");
        let disabled = config.disabled_codecs.unwrap();
        // the two merging rules combine rather than replacing each other
        assert_eq!(mimes(&disabled.codecs), vec!["video/AV1"]);
        assert_eq!(mimes(&disabled.publish), vec!["video/VP9"]);

        // exactly 18.3 does not match: the rule is strictly greater
        let config = manager
            .configuration(&client("safari", "18.3", "macos", ""))
            .unwrap();
        assert!(config.disabled_codecs.unwrap().publish.is_empty());
    }

    #[test]
    fn firefox_on_linux_or_android_cannot_publish_h264() {
        let manager = StaticClientConfigurationManager;
        for (browser, os) in [
            ("firefox", "linux"),
            ("firefox", "android"),
            ("Firefox Mobile", "Android"),
        ] {
            let config = manager
                .configuration(&client(browser, "120.0", os, ""))
                .unwrap_or_else(|| panic!("{browser} on {os} must match"));
            let disabled = config.disabled_codecs.unwrap();
            assert_eq!(mimes(&disabled.publish), vec!["video/H264"]);
        }

        // Firefox elsewhere is fine
        assert!(
            manager
                .configuration(&client("firefox", "120.0", "macos", ""))
                .is_none()
        );
    }

    #[test]
    fn the_xiaomi_model_cannot_publish_h264() {
        let manager = StaticClientConfigurationManager;
        let config = manager
            .configuration(&client("chrome", "120.0", "android", "Xiaomi 2201117TI"))
            .expect("the device rule must match");
        let disabled = config.disabled_codecs.unwrap();
        assert_eq!(mimes(&disabled.publish), vec!["video/H264"]);

        // the same model on another OS does not match
        assert!(
            manager
                .configuration(&client("chrome", "120.0", "ios", "xiaomi 2201117ti"))
                .is_none()
        );
    }

    #[test]
    fn a_non_merging_rule_ends_evaluation() {
        // Safari on Android with Firefox's user agent is contrived, but it is
        // the only way to reach a merging and a non-merging rule at once.
        let manager = StaticClientConfigurationManager;
        let config = manager
            .configuration(&client("firefox", "120.0", "android", "xiaomi 2201117ti"))
            .unwrap();
        let disabled = config.disabled_codecs.unwrap();
        assert!(disabled.codecs.is_empty());
        assert_eq!(mimes(&disabled.publish), vec!["video/H264"]);
    }

    #[test]
    fn rule_versions_compare_as_semver_then_as_strings() {
        assert_eq!(compare_rule_version("18.4", "18.3"), 1);
        assert_eq!(compare_rule_version("18.3", "18.3"), 0);
        assert_eq!(compare_rule_version("9.0", "18.3"), -1);
        // "9" > "18" as strings, which is the Go fallback for non-semver input
        assert_eq!(compare_rule_version("9.x", "18.3"), 1);
    }
}
