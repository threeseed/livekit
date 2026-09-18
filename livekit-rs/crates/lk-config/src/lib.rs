//! YAML plus CLI configuration, defaults, validation and config-sample parity.
//!
//! Replaces the Go packages: `pkg/config`, `pkg/rtc/config`
//!
//! See `docs/RUST_PORT_PLAN.md` for the component tables behind this mapping.
//!
//! # The contract
//!
//! `config-sample.yaml` in the repository root is the contract. Every YAML key
//! the Go server accepts is accepted here under the same name, with the same
//! default, and an unknown key is an error unless strict mode is switched off,
//! which is what `NewConfig(confString, strictMode, ...)` does.
//!
//! Three mechanisms carry that:
//!
//! - Each struct is `#[serde(default)]`, so a missing key takes the value from
//!   its `Default` impl, which holds the value from `config.DefaultConfig`.
//! - `#[derive(ConfigSchema)]` emits the field list that [`strict`] walks to
//!   reject unknown keys and that [`cli`] walks to generate one flag per field.
//! - [`duration::GoDuration`] keeps Go's `500ms` duration spelling on the wire.
//!
//! # Not done here
//!
//! Node IP discovery (`rtcconfig.resolveNodeIP`) dials STUN and enumerates
//! interfaces. Loading a config stays pure, so `lk-server` runs discovery after
//! [`Config::load`] returns and writes the result into
//! [`rtc::RtcBaseConfig::node_ip`].

extern crate self as lk_config;

pub mod bwe;
pub mod cli;
pub mod duration;
pub mod error;
pub mod room;
pub mod rtc;
pub mod schema;
pub mod service;
pub mod sfu;
pub mod strict;

use std::collections::BTreeMap;
use std::path::Path;

use lk_config_derive::ConfigSchema;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

pub use crate::error::{Error, Result};
use crate::room::RoomConfig;
use crate::rtc::RtcConfig;
use crate::schema::HasSchema;
use crate::service::{
    AgentConfig, ApiConfig, DebugHandlerConfig, IngressConfig, LimitConfig, LoggingConfig,
    MetricConfig, NodeSelectorConfig, NodeStatsConfig, PrometheusConfig, PsrpcConfig, RedisConfig,
    SignalRelayConfig, SipConfig, TURN_MAX_TTL_SECONDS, TracingConfig, TurnConfig, WebHookConfig,
};
use crate::sfu::{AudioConfig, VideoConfig};

/// The server configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct Config {
    /// The main TCP port, serving both the API and the signal WebSocket.
    pub port: u32,
    /// Addresses the main listener binds. Empty binds every interface.
    pub bind_addresses: Vec<String>,
    /// Deprecated: use `prometheus.port`.
    pub prometheus_port: u32,
    /// The separate Prometheus listener.
    pub prometheus: PrometheusConfig,
    /// The `pprof`-style debug listener.
    pub debug_handler: DebugHandlerConfig,
    /// ICE and transport settings.
    pub rtc: RtcConfig,
    /// Redis, which also switches the server into distributed mode.
    pub redis: RedisConfig,
    /// Audio level detection.
    pub audio: AudioConfig,
    /// Video layer tracking.
    pub video: VideoConfig,
    /// Room lifecycle and codecs.
    pub room: RoomConfig,
    /// The embedded TURN server.
    pub turn: TurnConfig,
    /// Ingress base URLs.
    pub ingress: IngressConfig,
    /// SIP, empty today.
    pub sip: SipConfig,
    /// Webhook delivery.
    pub webhook: WebHookConfig,
    /// How a room is placed on a node.
    pub node_selector: NodeSelectorConfig,
    /// File holding API key/secret pairs.
    pub key_file: String,
    /// API key/secret pairs given inline.
    pub keys: BTreeMap<String, String>,
    /// This node's region, for the region-aware selector.
    pub region: String,
    /// The inter-node signal stream.
    pub signal_relay: SignalRelayConfig,
    /// The RPC bus.
    pub psrpc: PsrpcConfig,
    /// Deprecated: use `logging.level`.
    pub log_level: String,
    /// Logging.
    pub logging: LoggingConfig,
    /// Size and rate limits.
    pub limit: LimitConfig,
    /// Agent workers.
    pub agents: AgentConfig,
    /// Development mode: shorter port ranges and debug logging.
    pub development: bool,
    /// Client metrics.
    pub metric: MetricConfig,
    /// Distributed tracing.
    pub trace: TracingConfig,
    /// Node statistics.
    pub node_stats: NodeStatsConfig,
    /// Serve data tracks.
    pub enable_data_tracks: bool,
    /// Serve participant data blobs.
    pub enable_participant_data_blob: bool,
    /// API behaviour.
    pub api: ApiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 7880,
            bind_addresses: Vec::new(),
            prometheus_port: 0,
            prometheus: PrometheusConfig::default(),
            debug_handler: DebugHandlerConfig::default(),
            rtc: RtcConfig::default(),
            redis: RedisConfig::default(),
            audio: AudioConfig::default(),
            video: VideoConfig::default(),
            room: RoomConfig::default(),
            turn: TurnConfig::default(),
            ingress: IngressConfig::default(),
            sip: SipConfig::default(),
            webhook: WebHookConfig::default(),
            node_selector: NodeSelectorConfig::default(),
            key_file: String::new(),
            keys: BTreeMap::new(),
            region: String::new(),
            signal_relay: SignalRelayConfig::default(),
            psrpc: PsrpcConfig::default(),
            log_level: String::new(),
            logging: LoggingConfig::default(),
            limit: LimitConfig::default(),
            agents: AgentConfig::default(),
            development: false,
            metric: MetricConfig::default(),
            trace: TracingConfig::default(),
            node_stats: NodeStatsConfig::default(),
            enable_data_tracks: true,
            enable_participant_data_blob: false,
            api: ApiConfig::default(),
        }
    }
}

impl Config {
    /// Loads a config the way `config.NewConfig` does: defaults, then the YAML
    /// document, then the generated CLI overrides, then the fix-ups that depend
    /// on the merged result.
    ///
    /// `overrides` are `(dotted path, raw value)` pairs, as
    /// [`cli::overrides_from_matches`] produces them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Yaml`] when the document does not parse,
    /// [`Error::UnknownFields`] when strict mode finds a key the schema does
    /// not declare, and [`Error::Cli`] when an override does not apply.
    pub fn load(yaml: &str, strict: bool, overrides: &[(String, String)]) -> Result<Self> {
        let mut document: Value = if yaml.trim().is_empty() {
            Value::Mapping(serde_yaml::Mapping::new())
        } else {
            serde_yaml::from_str(yaml)?
        };

        cli::apply_overrides(&mut document, Self::schema(), overrides)?;

        if strict {
            let unknown = strict::unknown_fields(&document, Self::schema());
            if !unknown.is_empty() {
                return Err(Error::UnknownFields(unknown));
            }
        }

        let mut config: Self = serde_yaml::from_value(document)?;
        config.finalize();
        Ok(config)
    }

    /// The fix-ups `NewConfig` applies after decoding: port defaults, TURN TTL
    /// clamping, the TURN relay range, the deprecated log level and metadata
    /// limits, and the pion component levels.
    ///
    /// Idempotent, so a caller that mutates the config afterwards can run it
    /// again.
    pub fn finalize(&mut self) {
        self.rtc.apply_port_defaults(self.development);
        self.normalize_turn_ttls();

        if self.turn.relay_port_range_start == 0 || self.turn.relay_port_range_end == 0 {
            // Development defaults to two ports so a container publishes two,
            // not ten thousand.
            self.turn.relay_port_range_start = 30000;
            self.turn.relay_port_range_end = if self.development { 30002 } else { 40000 };
        }

        if !self.log_level.is_empty() {
            self.logging.base.level = self.log_level.clone();
        }
        if self.logging.base.level.is_empty() && self.development {
            self.logging.base.level = "debug".to_owned();
        }
        if !self.logging.pion_level.is_empty() {
            let level = self.logging.pion_level.clone();
            self.logging
                .base
                .component_levels
                .insert("transport.pion".to_owned(), level.clone());
            self.logging
                .base
                .component_levels
                .insert("pion".to_owned(), level);
        }

        // The room-level limits were moved to `limit`; a config that still
        // sets them wins, as it does in Go.
        if self.room.max_metadata_size != 0 {
            self.limit.max_metadata_size = self.room.max_metadata_size;
        }
        if self.room.max_participant_identity_length != 0 {
            self.limit.max_participant_identity_length = self.room.max_participant_identity_length;
        }
        if self.room.max_room_name_length != 0 {
            self.limit.max_room_name_length = self.room.max_room_name_length;
        }
    }

    /// Whether any TURN/TLS endpoint is available, embedded or external.
    #[must_use]
    pub fn is_turns_enabled(&self) -> bool {
        if self.turn.enabled && self.turn.tls_port != 0 {
            return true;
        }
        self.rtc
            .turn_servers
            .iter()
            .any(|server| server.protocol == "tls")
    }

    /// Clamps the configured TURN TTLs into the safe range. Safe to call more
    /// than once.
    ///
    /// Returns the adjustments made, so the caller can log them the way
    /// `NormalizeTURNTTLs` does.
    pub fn normalize_turn_ttls(&mut self) -> Vec<TtlAdjustment> {
        let mut adjustments = Vec::new();
        let (clamped, changed) = clamp_turn_ttl_seconds(self.turn.ttl_seconds);
        if changed {
            adjustments.push(TtlAdjustment {
                host: None,
                configured: self.turn.ttl_seconds,
                clamped,
            });
            self.turn.ttl_seconds = clamped;
        }
        for server in &mut self.rtc.turn_servers {
            // An external TTL of 0 means "use the default", so only the upper
            // bound is enforced here.
            if server.ttl > TURN_MAX_TTL_SECONDS {
                adjustments.push(TtlAdjustment {
                    host: Some(server.host.clone()),
                    configured: server.ttl,
                    clamped: TURN_MAX_TTL_SECONDS,
                });
                server.ttl = TURN_MAX_TTL_SECONDS;
            }
        }
        adjustments
    }

    /// Loads the API keys, preferring `key_file` over inline `keys`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyFilePermissions`] when the key file is readable by
    /// others, [`Error::Io`] when it cannot be read, [`Error::Yaml`] when it
    /// does not parse, and [`Error::KeysNotSet`] when no key is configured.
    pub fn validate_keys(&mut self) -> Result<()> {
        if !self.key_file.is_empty() {
            let path = Path::new(&self.key_file);
            let metadata = std::fs::metadata(path).map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
            if others_can_access(&metadata) {
                return Err(Error::KeyFilePermissions);
            }
            let contents = std::fs::read_to_string(path).map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
            self.keys = serde_yaml::from_str(&contents)?;
        }

        if self.keys.is_empty() {
            return Err(Error::KeysNotSet);
        }
        Ok(())
    }

    /// API keys whose secret is shorter than the 32 characters the server wants
    /// outside development mode. The caller logs them; the Go server does not
    /// fail start-up over this.
    #[must_use]
    pub fn weak_keys(&self) -> Vec<&str> {
        if self.development {
            return Vec::new();
        }
        self.keys
            .iter()
            .filter(|(_, secret)| secret.len() < 32)
            .map(|(key, _)| key.as_str())
            .collect()
    }

    /// Resolves each external TURN server's shared secret from its
    /// `secret_file`, and checks that every server has usable credentials.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TurnSecretFilePermissions`] when a secret file is
    /// readable by others, [`Error::TurnSecretEmpty`] when it holds only
    /// whitespace, and [`Error::TurnServerNoCredentials`] when a server has
    /// neither a secret nor a username and credential.
    pub fn load_turn_secrets(&mut self) -> Result<Vec<String>> {
        let mut warnings = Vec::new();
        for server in &mut self.rtc.turn_servers {
            // Trim first, so a blank inline secret falls back to secret_file
            // rather than counting as set.
            let inline = server.secret.trim().to_owned();
            if !server.secret_file.is_empty() && !inline.is_empty() {
                warnings.push(format!(
                    "both secret and secret_file are set for TURN server {}:{}, the hardcoded \
                     secret will be used",
                    server.host, server.port
                ));
                server.secret = inline;
            } else if !server.secret_file.is_empty() {
                let path = Path::new(&server.secret_file);
                let metadata = std::fs::metadata(path).map_err(|source| Error::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                if others_can_access(&metadata) {
                    return Err(Error::TurnSecretFilePermissions);
                }
                let contents = std::fs::read_to_string(path).map_err(|source| Error::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                let secret = contents.trim().to_owned();
                if secret.is_empty() {
                    return Err(Error::TurnSecretEmpty {
                        host: server.host.clone(),
                        path: path.to_path_buf(),
                    });
                }
                server.secret = secret;
            } else {
                server.secret = inline;
            }

            let has_dynamic = !server.secret.is_empty();
            let has_static = !server.username.is_empty() && !server.credential.is_empty();
            if !has_dynamic && !has_static {
                return Err(Error::TurnServerNoCredentials(server.host.clone()));
            }
        }
        Ok(warnings)
    }
}

/// One TTL the server clamped, for logging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TtlAdjustment {
    /// The external TURN server this applies to, or `None` for the embedded
    /// server's `turn.ttl_seconds`.
    pub host: Option<String>,
    /// What the config asked for.
    pub configured: i32,
    /// What the server will use.
    pub clamped: i32,
}

/// Bounds a TURN credential TTL to `[DEFAULT_TURN_TTL_SECONDS,
/// TURN_MAX_TTL_SECONDS]`.
///
/// A non-positive TTL would produce already-expired credentials, so it falls
/// back to the default rather than being used. The second value reports
/// whether the input was out of range.
#[must_use]
pub fn clamp_turn_ttl_seconds(ttl_seconds: i32) -> (i32, bool) {
    if ttl_seconds <= 0 {
        (service::DEFAULT_TURN_TTL_SECONDS, true)
    } else if ttl_seconds > TURN_MAX_TTL_SECONDS {
        (TURN_MAX_TTL_SECONDS, true)
    } else {
        (ttl_seconds, false)
    }
}

#[cfg(unix)]
fn others_can_access(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o007 != 0
}

#[cfg(not(unix))]
fn others_can_access(_metadata: &std::fs::Metadata) -> bool {
    // Windows has no others bit; the Go server's check is a no-op there too.
    false
}
