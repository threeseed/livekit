//! The `rtc.congestion_control` tree.
//!
//! Ports `pkg/config.CongestionControlConfig` and everything it nests:
//! `streamallocator.StreamAllocatorConfig`, `remotebwe.RemoteBWEConfig`,
//! `sendsidebwe.SendSideBWEConfig` and the `ccutils` trend/probe configs.
//!
//! None of it is read before phase 3, when `lk-bwe` lands. It is ported now
//! because the acceptance gate for the config layer is that an existing
//! `config.yaml` keeps its meaning against a Rust node, and a tuned deployment
//! sets these keys.

use lk_config_derive::ConfigSchema;
use serde::{Deserialize, Serialize};

use crate::duration::GoDuration;

/// The `rtc.congestion_control` section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct CongestionControlConfig {
    /// Monitor congestion and manage subscriber bandwidth.
    pub enabled: bool,
    /// Allow pausing tracks when the channel cannot carry them all.
    pub allow_pause: bool,
    /// Subscriber stream allocator.
    pub stream_allocator: StreamAllocatorConfig,
    /// REMB-driven receive-side estimator.
    pub remote_bwe: RemoteBweConfig,
    /// Use `rtc-interceptor`'s GCC rather than LiveKit's estimator.
    pub use_send_side_bwe_interceptor: bool,
    /// Use LiveKit's send-side estimator.
    pub use_send_side_bwe: bool,
    /// Pacer behaviour used with the send-side estimator.
    pub send_side_bwe_pacer: String,
    /// LiveKit's send-side estimator.
    pub send_side_bwe: SendSideBweConfig,
}

impl Default for CongestionControlConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_pause: false,
            stream_allocator: StreamAllocatorConfig::default(),
            remote_bwe: RemoteBweConfig::default(),
            use_send_side_bwe_interceptor: false,
            use_send_side_bwe: false,
            send_side_bwe_pacer: "no_queue".to_owned(),
            send_side_bwe: SendSideBweConfig::default(),
        }
    }
}

/// Subscriber stream allocator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct StreamAllocatorConfig {
    /// Floor on the channel capacity the allocator will assume.
    pub min_channel_capacity: i64,
    /// Leave unmanaged tracks out of the estimate.
    // The Go key carries a typo (`disable_etimation_unmanaged_tracks`) that is
    // part of the wire contract, so it is reproduced rather than corrected.
    #[serde(rename = "disable_etimation_unmanaged_tracks")]
    pub disable_estimation_unmanaged_tracks: bool,
    /// `padding` or `media`.
    pub probe_mode: String,
    /// How far above the current estimate a probe reaches, as a percentage.
    pub probe_overage_pct: i64,
    /// Smallest probe, in bits per second.
    pub probe_min_bps: i64,
    /// How long a track stays paused before it may be resumed.
    pub paused_min_wait: GoDuration,
}

impl Default for StreamAllocatorConfig {
    fn default() -> Self {
        Self {
            min_channel_capacity: 0,
            disable_estimation_unmanaged_tracks: false,
            probe_mode: "padding".to_owned(),
            probe_overage_pct: 120,
            probe_min_bps: 200_000,
            paused_min_wait: GoDuration::from_secs(5),
        }
    }
}

/// REMB-driven receive-side estimator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct RemoteBweConfig {
    /// Weight applied to the NACK ratio when attenuating the estimate.
    pub nack_ratio_attenuator: f64,
    /// Fraction of the estimate that must be in use before it is trusted.
    pub expected_usage_threshold: f64,
    /// Channel observer used while probing.
    pub channel_observer_probe: ChannelObserverConfig,
    /// Channel observer used outside probes.
    pub channel_observer_non_probe: ChannelObserverConfig,
    /// Probe controller.
    pub probe_controller: ProbeControllerConfig,
}

impl Default for RemoteBweConfig {
    fn default() -> Self {
        Self {
            nack_ratio_attenuator: 0.4,
            expected_usage_threshold: 0.95,
            channel_observer_probe: ChannelObserverConfig {
                estimate: TrendDetectorConfig {
                    required_samples: 3,
                    required_samples_min: 3,
                    downward_trend_threshold: 0.0,
                    downward_trend_max_wait: GoDuration::from_secs(5),
                    collapse_threshold: GoDuration::ZERO,
                    validity_window: GoDuration::from_secs(10),
                },
                nack: NackTrackerConfig {
                    window_min_duration: GoDuration::from_millis(500),
                    window_max_duration: GoDuration::from_secs(1),
                    ratio_threshold: 0.04,
                },
            },
            channel_observer_non_probe: ChannelObserverConfig {
                estimate: TrendDetectorConfig {
                    required_samples: 12,
                    required_samples_min: 8,
                    downward_trend_threshold: -0.6,
                    downward_trend_max_wait: GoDuration::from_secs(5),
                    collapse_threshold: GoDuration::from_millis(500),
                    validity_window: GoDuration::from_secs(10),
                },
                nack: NackTrackerConfig {
                    window_min_duration: GoDuration::from_secs(2),
                    window_max_duration: GoDuration::from_secs(3),
                    ratio_threshold: 0.08,
                },
            },
            probe_controller: ProbeControllerConfig::default(),
        }
    }
}

/// One channel observer: an estimate trend plus a NACK tracker.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct ChannelObserverConfig {
    /// Trend detection over the estimate.
    pub estimate: TrendDetectorConfig,
    /// NACK ratio tracking.
    pub nack: NackTrackerConfig,
}

/// NACK ratio tracking window.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct NackTrackerConfig {
    /// Shortest window considered.
    pub window_min_duration: GoDuration,
    /// Longest window considered.
    pub window_max_duration: GoDuration,
    /// NACK ratio above which the channel is judged congested.
    pub ratio_threshold: f64,
}

/// Trend detection over a series of estimates.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct TrendDetectorConfig {
    /// Samples needed for a verdict.
    pub required_samples: i32,
    /// Samples needed for a provisional verdict.
    pub required_samples_min: i32,
    /// Kendall's tau below which the trend counts as downward.
    pub downward_trend_threshold: f64,
    /// How long a downward trend is waited out before acting.
    pub downward_trend_max_wait: GoDuration,
    /// Samples closer together than this are collapsed.
    pub collapse_threshold: GoDuration,
    /// How long a sample stays in the window.
    pub validity_window: GoDuration,
}

/// Probe scheduling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct ProbeControllerConfig {
    /// Interval and duration regulation.
    pub probe_regulator: ProbeRegulatorConfig,
    /// How many RTTs to wait for the channel to settle after a probe.
    pub settle_wait_num_rtt: u32,
    /// Lower bound on the settle wait.
    pub settle_wait_min: GoDuration,
    /// Upper bound on the settle wait.
    pub settle_wait_max: GoDuration,
}

impl Default for ProbeControllerConfig {
    fn default() -> Self {
        Self {
            probe_regulator: ProbeRegulatorConfig::default(),
            settle_wait_num_rtt: 5,
            settle_wait_min: GoDuration::from_millis(250),
            settle_wait_max: GoDuration::from_secs(5),
        }
    }
}

/// Probe interval and duration regulation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct ProbeRegulatorConfig {
    /// Interval between the first probes.
    pub base_interval: GoDuration,
    /// Multiplier applied to the interval after a failed probe.
    pub backoff_factor: f64,
    /// Ceiling on the interval.
    pub max_interval: GoDuration,
    /// Shortest probe.
    pub min_duration: GoDuration,
    /// Longest probe.
    pub max_duration: GoDuration,
    /// Multiplier applied to the duration after a successful probe.
    pub duration_increase_factor: f64,
}

impl Default for ProbeRegulatorConfig {
    fn default() -> Self {
        Self {
            base_interval: GoDuration::from_secs(3),
            backoff_factor: 1.5,
            max_interval: GoDuration::from_secs(120),
            min_duration: GoDuration::from_millis(200),
            max_duration: GoDuration::from_secs(20),
            duration_increase_factor: 1.5,
        }
    }
}

/// LiveKit's send-side estimator.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct SendSideBweConfig {
    /// The congestion detector, which is the whole of the estimator's tuning.
    pub congestion_detector: CongestionDetectorConfig,
}

/// Send-side congestion detection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct CongestionDetectorConfig {
    /// Grouping of feedback into measurement windows.
    pub packet_group: PacketGroupConfig,
    /// Age at which a group is dropped.
    pub packet_group_max_age: GoDuration,
    /// Grouping used while probing.
    pub probe_packet_group: ProbePacketGroupConfig,
    /// Probe scheduling.
    pub probe_regulator: ProbeRegulatorConfig,
    /// Signal thresholds applied to a probe's result.
    pub probe_signal: ProbeSignalConfig,
    /// Queuing delay above which the channel is judged qualified (JQR).
    pub jqr_min_delay: GoDuration,
    /// Trend coefficient above which the channel is judged qualified.
    pub jqr_min_trend_coefficient: f64,
    /// Queuing delay below which the channel is judged disqualified (DQR).
    pub dqr_max_delay: GoDuration,
    /// Loss weighting.
    pub weighted_loss: WeightedLossConfig,
    /// Weighted loss above which the channel is judged qualified.
    pub jqr_min_weighted_loss: f64,
    /// Weighted loss below which the channel is judged disqualified.
    pub dqr_max_weighted_loss: f64,
    /// Early-warning signal from queuing delay, qualified.
    pub queuing_delay_early_warning_jqr: CongestionSignalConfig,
    /// Early-warning signal from queuing delay, disqualified.
    pub queuing_delay_early_warning_dqr: CongestionSignalConfig,
    /// Early-warning signal from loss, qualified.
    pub loss_early_warning_jqr: CongestionSignalConfig,
    /// Early-warning signal from loss, disqualified.
    pub loss_early_warning_dqr: CongestionSignalConfig,
    /// Congested signal from queuing delay, qualified.
    pub queuing_delay_congested_jqr: CongestionSignalConfig,
    /// Congested signal from queuing delay, disqualified.
    pub queuing_delay_congested_dqr: CongestionSignalConfig,
    /// Congested signal from loss, qualified.
    pub loss_congested_jqr: CongestionSignalConfig,
    /// Congested signal from loss, disqualified.
    pub loss_congested_dqr: CongestionSignalConfig,
    /// Trend detection on the capture-to-receive ratio while congested.
    pub congested_ctr_trend: TrendDetectorConfig,
    /// Tolerance around a capture-to-receive ratio of one.
    pub congested_ctr_epsilon: f64,
    /// Grouping used while congested.
    pub congested_packet_group: PacketGroupConfig,
    /// Window over which the estimate is computed.
    // The Go key carries a typo (`estimaton_window_duration`) that is part of
    // the wire contract.
    #[serde(rename = "estimaton_window_duration")]
    pub estimation_window_duration: GoDuration,
}

impl Default for CongestionDetectorConfig {
    fn default() -> Self {
        Self {
            packet_group: PacketGroupConfig {
                min_packets: 30,
                max_window_duration: GoDuration::from_millis(500),
            },
            packet_group_max_age: GoDuration::from_secs(10),
            probe_packet_group: ProbePacketGroupConfig::default(),
            probe_regulator: ProbeRegulatorConfig::default(),
            probe_signal: ProbeSignalConfig::default(),
            jqr_min_delay: GoDuration::from_millis(50),
            jqr_min_trend_coefficient: 0.8,
            dqr_max_delay: GoDuration::from_millis(20),
            weighted_loss: WeightedLossConfig::default(),
            jqr_min_weighted_loss: 0.25,
            dqr_max_weighted_loss: 0.1,
            queuing_delay_early_warning_jqr: CongestionSignalConfig::new(2, 200),
            queuing_delay_early_warning_dqr: CongestionSignalConfig::new(3, 300),
            loss_early_warning_jqr: CongestionSignalConfig::new(3, 300),
            loss_early_warning_dqr: CongestionSignalConfig::new(4, 400),
            queuing_delay_congested_jqr: CongestionSignalConfig::new(4, 400),
            queuing_delay_congested_dqr: CongestionSignalConfig::new(5, 500),
            loss_congested_jqr: CongestionSignalConfig::new(6, 600),
            loss_congested_dqr: CongestionSignalConfig::new(6, 600),
            congested_ctr_trend: TrendDetectorConfig {
                required_samples: 4,
                required_samples_min: 2,
                downward_trend_threshold: -0.5,
                downward_trend_max_wait: GoDuration::from_secs(2),
                collapse_threshold: GoDuration::from_millis(500),
                validity_window: GoDuration::from_secs(10),
            },
            congested_ctr_epsilon: 0.05,
            congested_packet_group: PacketGroupConfig {
                min_packets: 20,
                max_window_duration: GoDuration::from_millis(150),
            },
            estimation_window_duration: GoDuration::from_secs(1),
        }
    }
}

/// How feedback is grouped into a measurement window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct PacketGroupConfig {
    /// Packets needed to close a group.
    pub min_packets: i32,
    /// Longest a group stays open.
    pub max_window_duration: GoDuration,
}

/// Grouping used while probing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct ProbePacketGroupConfig {
    /// Grouping, sized so a probe lands in one group.
    pub packet_group: PacketGroupConfig,
    /// How many RTTs to wait for the channel to settle.
    pub settle_wait_num_rtt: u32,
    /// Lower bound on the settle wait.
    pub settle_wait_min: GoDuration,
    /// Upper bound on the settle wait.
    pub settle_wait_max: GoDuration,
}

impl Default for ProbePacketGroupConfig {
    fn default() -> Self {
        Self {
            packet_group: PacketGroupConfig {
                min_packets: 16384,
                max_window_duration: GoDuration::from_secs(60),
            },
            settle_wait_num_rtt: 5,
            settle_wait_min: GoDuration::from_millis(250),
            settle_wait_max: GoDuration::from_secs(5),
        }
    }
}

/// Thresholds applied to a probe's result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct ProbeSignalConfig {
    /// Fraction of the probe's bytes that must have been sent.
    pub min_bytes_ratio: f64,
    /// Fraction of the probe's duration that must have elapsed.
    pub min_duration_ratio: f64,
    /// Queuing delay above which the probe counts as qualified.
    pub jqr_min_delay: GoDuration,
    /// Queuing delay below which the probe counts as disqualified.
    pub dqr_max_delay: GoDuration,
    /// Loss weighting.
    pub weighted_loss: WeightedLossConfig,
    /// Weighted loss above which the probe counts as qualified.
    pub jqr_min_weighted_loss: f64,
    /// Weighted loss below which the probe counts as disqualified.
    pub dqr_max_weighted_loss: f64,
}

impl Default for ProbeSignalConfig {
    fn default() -> Self {
        Self {
            min_bytes_ratio: 0.5,
            min_duration_ratio: 0.5,
            jqr_min_delay: GoDuration::from_millis(50),
            dqr_max_delay: GoDuration::from_millis(20),
            weighted_loss: WeightedLossConfig::default(),
            jqr_min_weighted_loss: 0.25,
            dqr_max_weighted_loss: 0.1,
        }
    }
}

/// How loss is weighted against the traffic that carried it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct WeightedLossConfig {
    /// Shortest sample that counts towards a loss verdict.
    pub min_duration_for_loss_validity: GoDuration,
    /// Duration the weighting is normalised against.
    pub base_duration: GoDuration,
    /// Packet rate the weighting is normalised against.
    pub base_pps: i32,
    /// Weight applied to loss relative to delay.
    pub loss_penalty_factor: f64,
}

impl Default for WeightedLossConfig {
    fn default() -> Self {
        Self {
            min_duration_for_loss_validity: GoDuration::from_millis(100),
            base_duration: GoDuration::from_millis(500),
            base_pps: 30,
            loss_penalty_factor: 0.25,
        }
    }
}

/// How many groups, over how long, make a congestion signal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct CongestionSignalConfig {
    /// Consecutive groups that must agree.
    pub min_number_of_groups: i32,
    /// Time those groups must span.
    pub min_duration: GoDuration,
}

impl CongestionSignalConfig {
    const fn new(min_number_of_groups: i32, min_duration_ms: u64) -> Self {
        Self {
            min_number_of_groups,
            min_duration: GoDuration::from_millis(min_duration_ms),
        }
    }
}
