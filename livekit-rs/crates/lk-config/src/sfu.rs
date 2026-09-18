//! Media-plane settings: audio level detection, PLI throttling, stream
//! trackers and video.
//!
//! Ports `pkg/sfu.AudioConfig` (which inlines `pkg/sfu/audio.AudioLevelConfig`),
//! `pkg/sfu.PLIThrottleConfig`, `pkg/sfu.StreamTrackerManagerConfig` and
//! `pkg/config.VideoConfig`. The values are consumed in phases 2 and 3; the
//! keys and defaults live here from phase 1 so an operator's existing config
//! file keeps working against a Rust node.

use std::collections::BTreeMap;

use lk_config_derive::ConfigSchema;
use serde::{Deserialize, Serialize};

use crate::duration::GoDuration;

/// The `audio` section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct AudioConfig {
    /// Audio level detection, inlined into `audio` as in Go.
    #[serde(flatten)]
    pub level: AudioLevelConfig,

    /// Encode a RED downtrack for Opus-only publishers.
    pub active_red_encoding: bool,

    /// Proxy the weakest subscriber's loss back to the publisher in RTCP RR.
    pub enable_loss_proxying: bool,
}

/// Active speaker detection thresholds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct AudioLevelConfig {
    /// Minimum level to be considered active, 0-127, where 0 is loudest.
    pub active_level: u8,
    /// Percentile of the window that must exceed [`Self::active_level`].
    pub min_percentile: u8,
    /// How often clients are updated, in milliseconds.
    pub update_interval: u32,
    /// Number of intervals averaged before reporting. Zero disables smoothing.
    pub smooth_intervals: u32,
}

impl Default for AudioLevelConfig {
    fn default() -> Self {
        // -35dBov, matching audio.DefaultAudioLevelConfig
        Self {
            active_level: 35,
            min_percentile: 40,
            update_interval: 400,
            smooth_intervals: 2,
        }
    }
}

/// Throttle periods for PLI and FIR, per published layer quality.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct PliThrottleConfig {
    /// Throttle for the low-quality layer.
    pub low_quality: GoDuration,
    /// Throttle for the mid-quality layer.
    pub mid_quality: GoDuration,
    /// Throttle for the high-quality layer.
    pub high_quality: GoDuration,
}

impl Default for PliThrottleConfig {
    fn default() -> Self {
        Self {
            low_quality: GoDuration::from_millis(500),
            mid_quality: GoDuration::from_secs(1),
            high_quality: GoDuration::from_secs(1),
        }
    }
}

/// The `video` section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct VideoConfig {
    /// How long dynacast waits before pausing a layer nobody subscribes to.
    pub dynacast_pause_delay: GoDuration,
    /// Per-source stream tracker settings.
    pub stream_tracker_manager: StreamTrackerManagerConfig,
    /// Consecutive regressions before a publisher is asked to change codec.
    pub codec_regression_threshold: i32,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            dynacast_pause_delay: GoDuration::from_secs(5),
            stream_tracker_manager: StreamTrackerManagerConfig::default(),
            codec_regression_threshold: 5,
        }
    }
}

/// Stream tracker settings per source type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct StreamTrackerManagerConfig {
    /// Camera and other continuous video.
    pub video: StreamTrackerConfig,
    /// Screen share, which is bursty and needs a longer cycle.
    pub screenshare: StreamTrackerConfig,
}

impl Default for StreamTrackerManagerConfig {
    fn default() -> Self {
        Self {
            video: StreamTrackerConfig::default_video(),
            screenshare: StreamTrackerConfig::default_screenshare(),
        }
    }
}

/// How a layer's liveness is tracked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct StreamTrackerConfig {
    /// `packet` or `frame`.
    pub stream_tracker_type: String,
    /// Bitrate reporting interval, per spatial layer.
    pub bitrate_report_interval: BTreeMap<i32, GoDuration>,
    /// Packet-tracker settings, per spatial layer.
    pub packet_tracker: BTreeMap<i32, StreamTrackerPacketConfig>,
    /// Frame-tracker settings, per spatial layer.
    pub frame_tracker: BTreeMap<i32, StreamTrackerFrameConfig>,
}

impl StreamTrackerConfig {
    /// `DefaultStreamTrackerConfigVideo`.
    #[must_use]
    pub fn default_video() -> Self {
        Self {
            stream_tracker_type: "packet".to_owned(),
            bitrate_report_interval: (0..3).map(|l| (l, GoDuration::from_secs(1))).collect(),
            packet_tracker: BTreeMap::from([
                (
                    0,
                    StreamTrackerPacketConfig {
                        samples_required: 1,
                        cycles_required: 4,
                        cycle_duration: GoDuration::from_millis(500),
                    },
                ),
                (
                    1,
                    StreamTrackerPacketConfig {
                        samples_required: 5,
                        cycles_required: 20,
                        cycle_duration: GoDuration::from_millis(500),
                    },
                ),
                (
                    2,
                    StreamTrackerPacketConfig {
                        samples_required: 5,
                        cycles_required: 20,
                        cycle_duration: GoDuration::from_millis(500),
                    },
                ),
            ]),
            frame_tracker: (0..3)
                .map(|l| (l, StreamTrackerFrameConfig { min_fps: 5.0 }))
                .collect(),
        }
    }

    /// `DefaultStreamTrackerConfigScreenshare`.
    #[must_use]
    pub fn default_screenshare() -> Self {
        Self {
            stream_tracker_type: "packet".to_owned(),
            bitrate_report_interval: (0..3).map(|l| (l, GoDuration::from_secs(4))).collect(),
            packet_tracker: (0..3)
                .map(|l| {
                    (
                        l,
                        StreamTrackerPacketConfig {
                            samples_required: 1,
                            cycles_required: 1,
                            cycle_duration: GoDuration::from_secs(2),
                        },
                    )
                })
                .collect(),
            frame_tracker: (0..3)
                .map(|l| (l, StreamTrackerFrameConfig { min_fps: 0.5 }))
                .collect(),
        }
    }
}

impl Default for StreamTrackerConfig {
    fn default() -> Self {
        Self::default_video()
    }
}

/// Packet-based liveness detection for one layer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct StreamTrackerPacketConfig {
    /// Packets needed within a cycle for it to count.
    pub samples_required: u32,
    /// Consecutive counting cycles before the layer is declared active.
    pub cycles_required: u32,
    /// Length of one cycle.
    pub cycle_duration: GoDuration,
}

/// Frame-rate-based liveness detection for one layer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ConfigSchema)]
#[serde(default)]
pub struct StreamTrackerFrameConfig {
    /// Frames per second below which the layer is considered stalled.
    pub min_fps: f64,
}
