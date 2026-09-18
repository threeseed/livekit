//! The sections outside media: storage, routing, limits, telemetry and TURN.
//!
//! Ports `pkg/config`'s remaining structs plus the ones it embeds from
//! `livekit/protocol` (`redis.RedisConfig`, `webhook.WebHookConfig`,
//! `rpc.PSRPCConfig`, `logger.Config`) and from the server (`agent.Config`,
//! `metric.MetricConfig`).

use lk_config_derive::ConfigSchema;
use serde::{Deserialize, Serialize};

use crate::duration::GoDuration;

/// The operational maximum for TURN credential TTLs, 24 hours.
///
/// It bounds credential lifetime and keeps the TTL from overflowing when
/// multiplied out to a duration.
pub const TURN_MAX_TTL_SECONDS: i32 = 24 * 60 * 60;

/// Default TTL for embedded TURN credentials, and the fallback for a
/// configured TTL that is not positive.
pub const DEFAULT_TURN_TTL_SECONDS: i32 = 300;

/// Default TTL applied to external TURN (static-auth-secret) credentials when
/// the configured TTL is left at zero.
pub const DEFAULT_EXTERNAL_TURN_TTL_SECONDS: i32 = 14_400;

/// Default cap on concurrent relay allocations per participant credential.
pub const DEFAULT_TURN_PER_USER_RELAY_ALLOCATION_LIMIT: i32 = 12;

/// The `redis` section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct RedisConfig {
    /// `host:port` of a standalone server.
    pub address: String,
    /// User name, for ACL-enabled servers.
    pub username: String,
    /// Password.
    pub password: String,
    /// Database index. Ignored in cluster mode.
    pub db: i32,
    /// Deprecated: use [`Self::tls`].
    pub use_tls: bool,
    /// TLS settings.
    pub tls: Option<TlsConfig>,
    /// Sentinel master name.
    #[serde(rename = "sentinel_master_name")]
    pub master_name: String,
    /// Sentinel user name.
    pub sentinel_username: String,
    /// Sentinel password.
    pub sentinel_password: String,
    /// Sentinel addresses.
    pub sentinel_addresses: Vec<String>,
    /// Cluster addresses.
    pub cluster_addresses: Vec<String>,
    /// Dial timeout in seconds.
    pub dial_timeout: i32,
    /// Read timeout in seconds.
    pub read_timeout: i32,
    /// Write timeout in seconds.
    pub write_timeout: i32,
    /// Redirects followed in cluster mode. Unset means two.
    pub max_redirects: Option<i32>,
    /// How long a caller waits for a pooled connection.
    pub pool_timeout: GoDuration,
    /// Connections per node.
    pub pool_size: i32,
}

/// TLS settings for an outbound connection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct TlsConfig {
    /// Use TLS.
    pub enabled: bool,
    /// Skip certificate and host verification.
    pub insecure: bool,
    /// SNI name.
    pub server_name: String,
    /// Trusted roots.
    pub ca_cert_file: String,
    /// Client certificate.
    pub client_cert_file: String,
    /// Client private key.
    pub client_key_file: String,
}

/// The `webhook` section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct WebHookConfig {
    /// Endpoints every event is posted to.
    pub urls: Vec<String>,
    /// Which API key signs the webhook JWT.
    pub api_key: String,
    /// Delivery worker pool.
    pub url_notifier: UrlNotifierConfig,
    /// Per-resource delivery ordering.
    pub resource_url_notifier: ResourceUrlNotifierConfig,
    /// Event filtering.
    pub filter_params: WebhookFilterParams,
}

/// Webhook delivery worker pool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct UrlNotifierConfig {
    /// Workers posting to one URL.
    pub num_workers: i32,
    /// Queue depth per URL.
    pub queue_size: i32,
}

impl Default for UrlNotifierConfig {
    fn default() -> Self {
        Self {
            num_workers: 10,
            queue_size: 100,
        }
    }
}

/// Per-resource webhook ordering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct ResourceUrlNotifierConfig {
    /// How long a resource's queue is kept after its last event.
    pub max_age: GoDuration,
    /// Events queued per resource.
    pub max_depth: i32,
}

impl Default for ResourceUrlNotifierConfig {
    fn default() -> Self {
        Self {
            max_age: GoDuration::from_secs(30),
            max_depth: 200,
        }
    }
}

/// Which webhook events are delivered.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct WebhookFilterParams {
    /// Deliver only these events. Empty means all.
    pub include_events: Vec<String>,
    /// Never deliver these events.
    pub exclude_events: Vec<String>,
}

/// The `psrpc` section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct PsrpcConfig {
    /// Attempts per request.
    pub max_attempts: i32,
    /// Deadline per attempt.
    pub timeout: GoDuration,
    /// Wait between attempts.
    pub backoff: GoDuration,
    /// Per-subscription buffer.
    pub buffer_size: i32,
    /// Payload compression.
    pub compression: CompressionConfig,
}

impl Default for PsrpcConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            timeout: GoDuration::from_secs(3),
            backoff: GoDuration::from_secs(2),
            buffer_size: 1000,
            compression: CompressionConfig::default(),
        }
    }
}

/// Bus payload compression.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct CompressionConfig {
    /// Compression level. Zero is the codec's default.
    pub quality: i32,
    /// Payload size above which compression is applied.
    pub threshold: i32,
    /// Cap on a decompressed payload. Zero means no cap.
    pub max_decompressed_size: i32,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            quality: 0,
            threshold: 1024,
            max_decompressed_size: 0,
        }
    }
}

/// The `logging` section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct LoggingConfig {
    /// The fields shared with `protocol/logger`, inlined as in Go.
    #[serde(flatten)]
    pub base: LoggerConfig,
    /// Level for the transport's own logs.
    pub pion_level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            base: LoggerConfig::default(),
            pion_level: "error".to_owned(),
        }
    }
}

/// `protocol/logger.Config`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct LoggerConfig {
    /// Emit JSON lines rather than console output.
    pub json: bool,
    /// Global level.
    pub level: String,
    /// Throttle repeated messages.
    pub sample: bool,
    /// Per-component level overrides.
    pub component_levels: std::collections::BTreeMap<String, String>,
    /// Messages logged before sampling starts.
    pub sample_initial: i32,
    /// Every Nth message is logged once sampling starts.
    pub sample_interval: i32,
    /// Window for per-participant sampling.
    pub item_sample_seconds: i32,
    /// Messages logged per item before sampling starts.
    pub item_sample_initial: i32,
    /// Every Nth per-item message is logged once sampling starts.
    pub item_sample_interval: i32,
}

/// The `limit` section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct LimitConfig {
    /// Tracks a participant may publish. Zero means no limit.
    pub num_tracks: i32,
    /// Publisher byte rate cap.
    pub bytes_per_sec: f32,
    /// Video tracks a participant may subscribe to.
    pub subscription_limit_video: i32,
    /// Audio tracks a participant may subscribe to.
    pub subscription_limit_audio: i32,
    /// Room and participant metadata size.
    pub max_metadata_size: u32,
    /// Total size of a participant's attributes.
    pub max_attributes_size: u32,
    /// Room name length.
    pub max_room_name_length: i32,
    /// Participant identity length.
    pub max_participant_identity_length: i32,
    /// Participant name length.
    pub max_participant_name_length: i32,
    /// Data blob key length.
    pub max_data_blob_key_length: i32,
    /// Total size of a participant's data blobs.
    #[serde(rename = "max_data_blobs_size")]
    pub max_data_blob_size: u32,
    /// Length of a data track's custom encoding identifier.
    pub max_data_track_custom_encoding_length: i32,
    /// Largest signalling WebSocket frame accepted. Zero means unbounded.
    pub signal_message_size_limit: i64,
    /// Largest agent WebSocket frame accepted.
    pub agent_signal_message_size_limit: i64,
    /// Largest HTTP request body accepted on the API listener.
    pub max_api_request_body_size: i64,
}

impl Default for LimitConfig {
    fn default() -> Self {
        Self {
            num_tracks: 0,
            bytes_per_sec: 0.0,
            subscription_limit_video: 0,
            subscription_limit_audio: 0,
            max_metadata_size: 512 * 1024,
            max_attributes_size: 64 * 1024,
            max_room_name_length: 256,
            max_participant_identity_length: 256,
            max_participant_name_length: 256,
            max_data_blob_key_length: 256,
            max_data_blob_size: 64_000,
            max_data_track_custom_encoding_length: 32,
            signal_message_size_limit: 2 << 20,
            agent_signal_message_size_limit: 2 << 20,
            max_api_request_body_size: 10 << 20,
        }
    }
}

impl LimitConfig {
    /// Whether a room name is within the configured length.
    #[must_use]
    pub fn check_room_name_length(&self, name: &str) -> bool {
        self.max_room_name_length == 0 || name.len() <= self.max_room_name_length as usize
    }

    /// Whether a participant identity is within the configured length.
    #[must_use]
    pub fn check_participant_identity_length(&self, identity: &str) -> bool {
        self.max_participant_identity_length == 0
            || identity.len() <= self.max_participant_identity_length as usize
    }

    /// Whether a participant name is within the configured length.
    #[must_use]
    pub fn check_participant_name_length(&self, name: &str) -> bool {
        self.max_participant_name_length == 0
            || name.len() <= self.max_participant_name_length as usize
    }

    /// Whether a metadata string is within the configured size.
    #[must_use]
    pub fn check_metadata_size(&self, metadata: &str) -> bool {
        self.max_metadata_size == 0 || metadata.len() as u64 <= u64::from(self.max_metadata_size)
    }

    /// Whether an attribute set is within the configured total size, counting
    /// keys and values, as `LimitConfig.CheckAttributesSize` does.
    #[must_use]
    pub fn check_attributes_size<'a, I>(&self, attributes: I) -> bool
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        if self.max_attributes_size == 0 {
            return true;
        }
        let total: usize = attributes
            .into_iter()
            .map(|(key, value)| key.len() + value.len())
            .sum();
        total as u64 <= u64::from(self.max_attributes_size)
    }

    /// Whether a data blob key is within the configured length.
    #[must_use]
    pub fn check_data_blob_key_length(&self, key: &str) -> bool {
        self.max_data_blob_key_length == 0 || key.len() <= self.max_data_blob_key_length as usize
    }

    /// Whether a data track's custom encoding identifier is within the
    /// configured length.
    #[must_use]
    pub fn check_data_track_custom_encoding_length(&self, identifier: &str) -> bool {
        self.max_data_track_custom_encoding_length == 0
            || identifier.len() <= self.max_data_track_custom_encoding_length as usize
    }
}

/// The `turn` section: the embedded TURN server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct TurnConfig {
    /// Run the embedded TURN server.
    pub enabled: bool,
    /// Domain the TLS certificate is issued for.
    pub domain: String,
    /// TLS certificate.
    pub cert_file: String,
    /// TLS private key.
    pub key_file: String,
    /// TURN/TLS port. Zero disables the TLS listener.
    pub tls_port: i32,
    /// TURN/UDP port. Zero disables the UDP listener.
    pub udp_port: i32,
    /// First relay port.
    #[serde(rename = "relay_range_start")]
    pub relay_port_range_start: u16,
    /// Last relay port.
    #[serde(rename = "relay_range_end")]
    pub relay_port_range_end: u16,
    /// TLS is terminated in front of TURN.
    pub external_tls: bool,
    /// Addresses the listeners bind.
    pub bind_addresses: Vec<String>,
    /// Require a PROXY protocol header on every TCP connection and take the
    /// client address from it.
    pub proxy_protocol: bool,
    /// Proxies whose PROXY header is believed. Defaults to loopback.
    pub proxy_protocol_trusted_cidrs: Vec<String>,
    /// Concurrent relay allocations per participant credential. Zero or less
    /// disables the quota.
    pub per_user_relay_allocation_limit: i32,
    /// Credential TTL in seconds.
    pub ttl_seconds: i32,
    /// Restricted peer CIDRs that are nevertheless allowed.
    pub allow_restricted_peer_cidrs: Vec<String>,
    /// Peer CIDRs denied outright. Takes precedence over the allow list.
    pub deny_peer_cidrs: Vec<String>,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            domain: String::new(),
            cert_file: String::new(),
            key_file: String::new(),
            tls_port: 0,
            udp_port: 0,
            relay_port_range_start: 0,
            relay_port_range_end: 0,
            external_tls: false,
            bind_addresses: vec!["0.0.0.0".to_owned()],
            proxy_protocol: false,
            proxy_protocol_trusted_cidrs: vec!["127.0.0.0/8".to_owned(), "::1/128".to_owned()],
            per_user_relay_allocation_limit: DEFAULT_TURN_PER_USER_RELAY_ALLOCATION_LIMIT,
            ttl_seconds: DEFAULT_TURN_TTL_SECONDS,
            allow_restricted_peer_cidrs: Vec::new(),
            deny_peer_cidrs: Vec::new(),
        }
    }
}

/// The `node_selector` section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct NodeSelectorConfig {
    /// `any`, `cpuload`, `sysload` or `regionaware`.
    pub kind: String,
    /// How candidates are ordered before the algorithm picks.
    pub sort_by: String,
    /// `lowest` or `twochoice`.
    pub algorithm: String,
    /// CPU load above which a node is skipped.
    pub cpu_load_limit: f32,
    /// System load above which a node is skipped.
    pub sysload_limit: f32,
    /// Regions, for the region-aware selector.
    pub regions: Vec<RegionConfig>,
}

impl Default for NodeSelectorConfig {
    fn default() -> Self {
        Self {
            kind: "any".to_owned(),
            sort_by: "random".to_owned(),
            algorithm: "lowest".to_owned(),
            cpu_load_limit: 0.9,
            sysload_limit: 0.9,
            regions: Vec::new(),
        }
    }
}

/// One region, so the selector can prefer nearby nodes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct RegionConfig {
    /// Region name, matched against a node's `region`.
    pub name: String,
    /// Latitude in degrees.
    pub lat: f64,
    /// Longitude in degrees.
    pub lon: f64,
}

/// The `signal_relay` section: the inter-node signal stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct SignalRelayConfig {
    /// How long delivery is retried before the session is failed.
    pub retry_timeout: GoDuration,
    /// Shortest wait between retries.
    pub min_retry_interval: GoDuration,
    /// Longest wait between retries.
    pub max_retry_interval: GoDuration,
    /// Messages buffered per stream.
    pub stream_buffer_size: i32,
    /// Attempts made to open the stream.
    pub connect_attempts: i32,
}

impl Default for SignalRelayConfig {
    fn default() -> Self {
        Self {
            retry_timeout: GoDuration::from_millis(7500),
            min_retry_interval: GoDuration::from_millis(500),
            max_retry_interval: GoDuration::from_secs(4),
            stream_buffer_size: 1000,
            connect_attempts: 3,
        }
    }
}

/// The `agents` section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct AgentConfig {
    /// Record user data in agent jobs.
    pub enable_user_data_recording: bool,
    /// Redact user data in agent jobs.
    pub enable_user_data_redaction: bool,
    /// Worker load a dispatcher aims for before moving on to the next worker.
    pub target_load: f32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enable_user_data_recording: false,
            enable_user_data_redaction: false,
            target_load: 0.7,
        }
    }
}

/// The `metric` section: client metrics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct MetricConfig {
    /// One-way delay estimation.
    #[serde(rename = "timestamper_config")]
    pub timestamper: MetricTimestamperConfig,
    /// Collection cadence.
    pub collector: MetricsCollectorConfig,
    /// Reporting cadence.
    pub reporter: MetricsReporterConfig,
}

/// One-way delay estimation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct MetricTimestamperConfig {
    /// Shortest interval between estimates.
    pub one_way_delay_estimator_min_interval: GoDuration,
    /// Samples per estimate.
    pub one_way_delay_estimator_max_batch: i32,
}

impl Default for MetricTimestamperConfig {
    fn default() -> Self {
        Self {
            one_way_delay_estimator_min_interval: GoDuration::from_secs(5),
            one_way_delay_estimator_max_batch: 100,
        }
    }
}

/// Client metric collection cadence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct MetricsCollectorConfig {
    /// How often a sample is taken.
    pub sampling_interval_ms: u32,
    /// How often samples are batched out.
    pub batch_interval_ms: u32,
}

impl Default for MetricsCollectorConfig {
    fn default() -> Self {
        Self {
            sampling_interval_ms: 3_000,
            batch_interval_ms: 10_000,
        }
    }
}

/// Client metric reporting cadence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct MetricsReporterConfig {
    /// How often a report is emitted.
    pub reporting_interval_ms: u32,
}

impl Default for MetricsReporterConfig {
    fn default() -> Self {
        Self {
            reporting_interval_ms: 10_000,
        }
    }
}

/// The `node_stats` section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct NodeStatsConfig {
    /// How often node stats are recomputed and published.
    pub stats_update_interval: GoDuration,
    /// Windows over which rates are measured.
    pub stats_rate_measurement_intervals: Vec<GoDuration>,
    /// Age at which stats are stale enough to fail the health check.
    pub stats_max_delay: GoDuration,
}

impl Default for NodeStatsConfig {
    fn default() -> Self {
        Self {
            stats_update_interval: GoDuration::from_secs(2),
            stats_rate_measurement_intervals: vec![GoDuration::from_secs(10)],
            stats_max_delay: GoDuration::from_secs(30),
        }
    }
}

/// The `api` section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct ApiConfig {
    /// How long an API call may take.
    pub execution_timeout: GoDuration,
    /// Shortest wait between completion checks.
    pub check_interval: GoDuration,
    /// Longest wait between completion checks.
    pub max_check_interval: GoDuration,
    /// Serve `GetParticipant`/`ListParticipants` over the bus.
    pub enable_psrpc_for_get_list_participants: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            execution_timeout: GoDuration::from_secs(2),
            check_interval: GoDuration::from_millis(100),
            max_check_interval: GoDuration::from_secs(300),
            enable_psrpc_for_get_list_participants: false,
        }
    }
}

/// The `prometheus` section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct PrometheusConfig {
    /// Listener port. Zero disables the separate listener.
    pub port: u32,
    /// Basic-auth user name.
    pub username: String,
    /// Basic-auth password.
    pub password: String,
}

/// The `debug_handler` section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct DebugHandlerConfig {
    /// Listener port. Zero disables the debug listener.
    pub port: u32,
}

/// The `ingress` section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct IngressConfig {
    /// Base URL published for RTMP ingresses.
    pub rtmp_base_url: String,
    /// Base URL published for WHIP ingresses.
    pub whip_base_url: String,
    /// Allow URL-pull ingresses with a `udp://` source. Off by default: a UDP
    /// source makes the handler bind a local port and accept unauthenticated
    /// traffic from anyone who can reach it.
    pub enable_udp_url_pull: bool,
}

/// The `sip` section. Empty in the Go server, kept for wire parity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct SipConfig {}

/// The `trace` section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct TracingConfig {
    /// Jaeger endpoint: `<hostname>`, `<host>:<port>` or a URL.
    pub jaeger_url: String,
}
