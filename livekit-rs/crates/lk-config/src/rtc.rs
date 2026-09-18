//! The `rtc` section: ICE and transport settings.
//!
//! Ports `pkg/config.RTCConfig` together with the `rtcconfig.RTCConfig` it
//! inlines (`yaml:",inline"`), from `livekit/mediatransportutil`. The inline
//! tag is reproduced with `#[serde(flatten)]`, so `rtc.tcp_port` stays a
//! top-level key of the `rtc` mapping exactly as it is today.

use std::fmt;

use lk_config_derive::ConfigSchema;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::bwe::CongestionControlConfig;
use crate::duration::GoDuration;
use crate::sfu::PliThrottleConfig;

/// STUN servers used when none are configured, from `rtcconfig`.
pub const DEFAULT_STUN_SERVERS: [&str; 3] = [
    "global.stun.twilio.com:3478",
    "stun.l.google.com:19302",
    "stun1.l.google.com:19302",
];

/// The `rtc` section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct RtcConfig {
    /// The settings shared with `mediatransportutil`, inlined into `rtc`.
    #[serde(flatten)]
    pub base: RtcBaseConfig,

    /// External TURN servers advertised to clients.
    pub turn_servers: Vec<TurnServer>,

    /// WARP = SPED (DTLS-in-STUN) + SNAP (SCTP INIT in SDP). Experimental.
    pub enable_warp: bool,

    /// Deprecated.
    pub strict_acks: bool,

    /// Deprecated: use [`Self::packet_buffer_size_video`] and
    /// [`Self::packet_buffer_size_audio`].
    pub packet_buffer_size: i32,
    /// Packets buffered for NACK on video tracks.
    pub packet_buffer_size_video: i32,
    /// Packets buffered for NACK on audio tracks.
    pub packet_buffer_size_audio: i32,

    /// Throttle periods for PLI/FIR RTCP packets.
    pub pli_throttle: PliThrottleConfig,

    /// Send-side bandwidth management.
    pub congestion_control: CongestionControlConfig,

    /// Allow TCP and TURN/TLS fallback. Unset means the server default.
    pub allow_tcp_fallback: Option<bool>,

    /// Signalling RTT threshold in milliseconds governing ICE/TCP fallback.
    /// Zero disables the check and always attempts ICE/TCP.
    pub tcp_fallback_rtt_threshold: i32,

    /// Migrate an established UDP connection with sustained loss to ICE/TCP or
    /// TURN/TLS. Requires [`Self::tcp_fallback_rtt_threshold`] to be positive.
    pub allow_udp_unstable_fallback: bool,

    /// Force a reconnect on a publication error.
    pub reconnect_on_publication_error: Option<bool>,
    /// Force a reconnect on a subscription error.
    pub reconnect_on_subscription_error: Option<bool>,
    /// Force a reconnect on a data channel error.
    pub reconnect_on_data_channel_error: Option<bool>,

    /// Deprecated.
    pub data_channel_max_buffered_amount: u64,

    /// Buffered amount above which a data channel is considered too slow and
    /// packets may be dropped rather than blocking the room.
    pub datachannel_slow_threshold: i32,

    /// Target latency for lossy data channels.
    pub datachannel_lossy_target_latency: GoDuration,

    /// Target latency for data-track channels. Zero disables the bound.
    pub datachannel_data_track_target_latency: GoDuration,

    /// Forwarding latency statistics.
    pub forward_stats: ForwardStatsConfig,

    /// Enable RTP stream restart detection for published tracks.
    pub enable_rtp_stream_restart_detection: bool,
}

impl Default for RtcConfig {
    fn default() -> Self {
        Self {
            base: RtcBaseConfig::default(),
            turn_servers: Vec::new(),
            enable_warp: false,
            strict_acks: false,
            packet_buffer_size: 500,
            packet_buffer_size_video: 500,
            packet_buffer_size_audio: 200,
            pli_throttle: PliThrottleConfig::default(),
            congestion_control: CongestionControlConfig::default(),
            allow_tcp_fallback: None,
            tcp_fallback_rtt_threshold: 0,
            allow_udp_unstable_fallback: false,
            reconnect_on_publication_error: None,
            reconnect_on_subscription_error: None,
            reconnect_on_data_channel_error: None,
            data_channel_max_buffered_amount: 0,
            datachannel_slow_threshold: 0,
            datachannel_lossy_target_latency: GoDuration::ZERO,
            datachannel_data_track_target_latency: GoDuration::from_millis(100),
            forward_stats: ForwardStatsConfig::default(),
            enable_rtp_stream_restart_detection: false,
        }
    }
}

impl RtcConfig {
    /// Fills in the port defaults, as `rtcconfig.RTCConfig.Validate` does.
    ///
    /// Development mode collapses to a single UDP port so a container needs one
    /// published port; production takes the 50000-60000 ICE range.
    ///
    /// Node IP resolution is deliberately not done here: it dials STUN, and a
    /// config load must stay pure. `lk-server` resolves it after loading.
    pub fn apply_port_defaults(&mut self, development: bool) {
        if !self.base.udp_port.is_valid() && self.base.port_range_start == 0 {
            if development {
                self.base.udp_port = PortRange {
                    start: 7882,
                    end: 0,
                };
            } else {
                self.base.port_range_start = 50000;
                self.base.port_range_end = 60000;
            }
        }
    }
}

/// The part of the `rtc` section shared with `mediatransportutil`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct RtcBaseConfig {
    /// UDP port or port range used for client traffic when muxing.
    ///
    /// A scalar on the wire, so it is opaque to the flag generator; the
    /// hand-written `--udp-port` flag sets it, as it does in `cmd/server`.
    #[config(opaque)]
    pub udp_port: PortRange,
    /// ICE/TCP port. Zero disables ICE-TCP.
    pub tcp_port: u32,
    /// First port of the ICE UDP range.
    pub port_range_start: u32,
    /// Last port of the ICE UDP range.
    pub port_range_end: u32,
    /// Reuse the STUN port for ICE.
    pub use_stun_port_as_ice: bool,
    /// Explicit node address, overriding discovery. A scalar on the wire; set
    /// from the command line with the hand-written `--node-ip` flag.
    #[config(opaque)]
    pub node_ip: NodeIp,
    /// STUN servers advertised to clients.
    pub stun_servers: Vec<String>,
    /// Discover the host's public IP over STUN.
    pub use_external_ip: bool,
    /// Require a routable IPv4 during discovery.
    pub require_ipv4: bool,
    /// Skip the self-ping validation of the discovered external IP.
    pub skip_external_ip_validation: bool,
    /// Advertise mapped external and internal IPs together.
    pub advertise_internal_ip: bool,
    /// Run ICE in lite mode.
    pub use_ice_lite: bool,
    /// Interface include/exclude filters for candidate gathering.
    pub interfaces: NameFilters,
    /// IP include/exclude filters for candidate gathering.
    pub ips: NameFilters,
    /// Gather loopback candidates.
    pub enable_loopback_candidate: bool,
    /// Accept mDNS candidates.
    pub use_mdns: bool,
    /// Advertise only the external IP when discovery is on.
    pub external_ip_only: bool,
    /// Batched socket I/O.
    pub batch_io: BatchIoConfig,
    /// SCTP minimum congestion window.
    pub sctp_min_cwnd: i32,
    /// SCTP fast retransmit window.
    pub sctp_fast_rtx_wnd: i32,
    /// SCTP congestion-avoidance step.
    pub sctp_cwnd_ca_step: i32,
    /// Minimum wait before accepting a host candidate pair.
    pub host_acceptance_min_wait: GoDuration,
    /// Minimum wait before accepting a server-reflexive candidate pair.
    pub srflx_acceptance_min_wait: GoDuration,
    /// Minimum wait before accepting a peer-reflexive candidate pair.
    pub prflx_acceptance_min_wait: GoDuration,
    /// Minimum wait before accepting a relay candidate pair.
    pub relay_acceptance_min_wait: GoDuration,
    /// Disable UDP entirely. For tests.
    pub force_tcp: bool,
}

impl Default for RtcBaseConfig {
    fn default() -> Self {
        Self {
            udp_port: PortRange::default(),
            tcp_port: 7881,
            port_range_start: 0,
            port_range_end: 0,
            use_stun_port_as_ice: false,
            node_ip: NodeIp::default(),
            stun_servers: Vec::new(),
            use_external_ip: false,
            require_ipv4: false,
            skip_external_ip_validation: false,
            advertise_internal_ip: false,
            use_ice_lite: false,
            interfaces: NameFilters::default(),
            ips: NameFilters::default(),
            enable_loopback_candidate: false,
            use_mdns: false,
            external_ip_only: false,
            batch_io: BatchIoConfig::default(),
            sctp_min_cwnd: 0,
            sctp_fast_rtx_wnd: 0,
            sctp_cwnd_ca_step: 0,
            host_acceptance_min_wait: GoDuration::ZERO,
            srflx_acceptance_min_wait: GoDuration::ZERO,
            prflx_acceptance_min_wait: GoDuration::ZERO,
            relay_acceptance_min_wait: GoDuration::ZERO,
            force_tcp: false,
        }
    }
}

/// Include/exclude name filters, used for interfaces and IPs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct NameFilters {
    /// Names to include; an empty list includes everything.
    pub includes: Vec<String>,
    /// Names to exclude; applied after includes.
    pub excludes: Vec<String>,
}

/// Batched socket I/O settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct BatchIoConfig {
    /// Datagrams per batch.
    pub batch_size: i32,
    /// Longest a partial batch waits before being flushed.
    pub max_flush_interval: GoDuration,
}

/// A UDP port or an inclusive port range.
///
/// On the wire this is a scalar, not a mapping: `udp_port: 7882` or
/// `udp_port: 7882-7892`, matching `PortRange.UnmarshalYAML`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortRange {
    /// First port, and the only port when [`Self::end`] is zero.
    pub start: i32,
    /// Last port, or zero for a single port.
    pub end: i32,
}

impl PortRange {
    /// Whether a port was configured at all.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.start > 0
    }

    /// The ports covered, as `rtcconfig.PortRange.ToSlice` computes them.
    #[must_use]
    pub fn ports(self) -> Vec<i32> {
        if self.end == 0 || self.end <= self.start {
            return vec![self.start];
        }
        (self.start..=self.end).collect()
    }

    /// Parses the scalar form used in YAML and on the command line.
    ///
    /// # Errors
    ///
    /// Returns a message when the string is not a port or a `start-end` range,
    /// or when the range runs backwards.
    pub fn parse(input: &str) -> Result<Self, String> {
        let s = input.trim();
        if s.is_empty() {
            return Ok(Self::default());
        }
        if let Some((start, end)) = s.split_once('-') {
            let start: i32 = start
                .trim()
                .parse()
                .map_err(|_| format!("invalid start port {start:?}"))?;
            let end: i32 = end
                .trim()
                .parse()
                .map_err(|_| format!("invalid end port {end:?}"))?;
            if end <= start {
                return Err(format!(
                    "end port {end} must be greater than start port {start}"
                ));
            }
            return Ok(Self { start, end });
        }
        let port: i32 = s.parse().map_err(|_| format!("invalid port {s:?}"))?;
        Ok(Self {
            start: port,
            end: 0,
        })
    }
}

impl fmt::Display for PortRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.end == 0 {
            write!(f, "{}", self.start)
        } else {
            write!(f, "{}-{}", self.start, self.end)
        }
    }
}

impl Serialize for PortRange {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.end == 0 {
            serializer.serialize_i32(self.start)
        } else {
            serializer.serialize_str(&self.to_string())
        }
    }
}

impl<'de> Deserialize<'de> for PortRange {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ScalarVisitor {
            what: "a port such as 7882, or a range such as 7882-7892",
            parse: PortRange::parse,
        })
    }
}

/// The node's advertised address.
///
/// Like [`PortRange`] this is a scalar on the wire: `node_ip: 1.2.3.4`, or
/// `node_ip: 1.2.3.4,2001:db8::1` for a dual-stack node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeIp {
    /// The IPv4 address, if any.
    pub v4: String,
    /// The IPv6 address, if any.
    pub v6: String,
}

impl NodeIp {
    /// Whether neither address is set, in which case the server resolves one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.v4.is_empty() && self.v6.is_empty()
    }

    /// Parses the scalar form: one address, or `<ipv4>,<ipv6>`.
    ///
    /// # Errors
    ///
    /// Returns a message when a part is not an IP address, when more than two
    /// parts are given, or when a family is given twice.
    pub fn parse(input: &str) -> Result<Self, String> {
        let s = input.trim();
        let mut out = Self::default();
        if s.is_empty() {
            return Ok(out);
        }
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() > 2 {
            return Err(format!("invalid node ip {s:?}, should be <ipv4>,<ipv6>"));
        }
        for part in parts {
            let part = part.trim();
            let ip: std::net::IpAddr = part.parse().map_err(|_| format!("invalid ip {part:?}"))?;
            match ip {
                std::net::IpAddr::V4(_) => {
                    if !out.v4.is_empty() {
                        return Err(format!(
                            "multiple ipv4 addresses provided: {} and {ip}",
                            out.v4
                        ));
                    }
                    out.v4 = ip.to_string();
                }
                std::net::IpAddr::V6(_) => {
                    if !out.v6.is_empty() {
                        return Err(format!(
                            "multiple ipv6 addresses provided: {} and {ip}",
                            out.v6
                        ));
                    }
                    out.v6 = ip.to_string();
                }
            }
        }
        Ok(out)
    }
}

impl fmt::Display for NodeIp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.v4.is_empty(), self.v6.is_empty()) {
            (false, false) => write!(f, "{},{}", self.v4, self.v6),
            (false, true) => f.write_str(&self.v4),
            _ => f.write_str(&self.v6),
        }
    }
}

impl Serialize for NodeIp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for NodeIp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ScalarVisitor {
            what: "an ip address, or <ipv4>,<ipv6>",
            parse: NodeIp::parse,
        })
    }
}

/// Deserialises a type whose YAML form is a scalar parsed from its string
/// rendering, which is how Go's `UnmarshalYAML` reaches `UnmarshalString`.
struct ScalarVisitor<T> {
    what: &'static str,
    parse: fn(&str) -> Result<T, String>,
}

impl<T> Visitor<'_> for ScalarVisitor<T> {
    type Value = T;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.what)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        (self.parse)(v).map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        (self.parse)(&v.to_string()).map_err(E::custom)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        (self.parse)(&v.to_string()).map_err(E::custom)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        (self.parse)("").map_err(E::custom)
    }
}

/// An external TURN server advertised to clients.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct TurnServer {
    /// Host name.
    pub host: String,
    /// Port.
    pub port: i32,
    /// `tls`, `tcp` or `udp`.
    pub protocol: String,
    /// Static-auth username.
    pub username: String,
    /// Static-auth credential.
    pub credential: String,
    /// Shared secret for the TURN static-auth-secret mechanism. When set,
    /// credentials are generated per participant with HMAC-SHA1.
    pub secret: String,
    /// File holding the shared secret.
    pub secret_file: String,
    /// Generated-credential TTL in seconds. Zero means the 4 h default.
    pub ttl: i32,
}

/// Forwarding latency statistics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct ForwardStatsConfig {
    /// How often a summary line is logged.
    pub summary_interval: GoDuration,
    /// How often samples are reported.
    pub report_interval: GoDuration,
    /// The window each report covers.
    pub report_window: GoDuration,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn port_range_reads_both_wire_forms() {
        let single: PortRange = serde_yaml::from_str("7882").unwrap();
        assert_eq!(
            single,
            PortRange {
                start: 7882,
                end: 0
            }
        );
        let range: PortRange = serde_yaml::from_str("7882-7892").unwrap();
        assert_eq!(
            range,
            PortRange {
                start: 7882,
                end: 7892
            }
        );
        assert_eq!(range.ports().len(), 11);
        assert!(serde_yaml::from_str::<PortRange>("7892-7882").is_err());
    }

    #[test]
    fn node_ip_reads_dual_stack() {
        let ip: NodeIp = serde_yaml::from_str("1.2.3.4,2001:db8::1").unwrap();
        assert_eq!(ip.v4, "1.2.3.4");
        assert_eq!(ip.v6, "2001:db8::1");
        assert_eq!(ip.to_string(), "1.2.3.4,2001:db8::1");
        assert!(serde_yaml::from_str::<NodeIp>("not-an-ip").is_err());
    }

    #[test]
    fn port_defaults_follow_development_mode() {
        let mut dev = RtcConfig::default();
        dev.apply_port_defaults(true);
        assert_eq!(dev.base.udp_port.start, 7882);
        assert_eq!(dev.base.port_range_start, 0);

        let mut prod = RtcConfig::default();
        prod.apply_port_defaults(false);
        assert_eq!(prod.base.udp_port.start, 0);
        assert_eq!(prod.base.port_range_start, 50000);
        assert_eq!(prod.base.port_range_end, 60000);
    }
}
