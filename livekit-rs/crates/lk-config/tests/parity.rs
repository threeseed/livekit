//! Acceptance tests for the config layer.
//!
//! `config-sample.yaml` in the repository root is the contract between an
//! operator's config file and the server, so it is parsed here from its real
//! location rather than from a copy: a copy would drift, and a drifted copy
//! that still passes is worse than no test.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::path::PathBuf;

use lk_config::Config;
use lk_config::duration::GoDuration;
use lk_config::schema::HasSchema;
use lk_config::service::{DEFAULT_TURN_TTL_SECONDS, TURN_MAX_TTL_SECONDS};

/// `livekit-rs/crates/lk-config` -> the repository root that holds the sample.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
}

#[test]
fn config_sample_parses_in_strict_mode() {
    let path = repo_root().join("config-sample.yaml");
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));

    let config = Config::load(&yaml, true, &[]).expect("config-sample.yaml must parse strictly");

    // the keys the sample actually sets, as a guard against a parse that
    // silently produced defaults
    assert_eq!(config.port, 7880);
    assert_eq!(config.redis.address, "redis.host:6379");
    assert_eq!(config.rtc.base.port_range_start, 50000);
    assert_eq!(config.rtc.base.port_range_end, 60000);
    assert_eq!(config.rtc.base.tcp_port, 7881);
    assert!(config.rtc.base.use_external_ip);
    assert_eq!(config.keys.get("key1").map(String::as_str), Some("secret1"));
    assert_eq!(config.keys.get("key2").map(String::as_str), Some("secret2"));
}

#[test]
fn defaults_match_the_go_server() {
    // Field by field against `config.DefaultConfig` in pkg/config/config.go.
    let config = Config::default();

    assert_eq!(config.port, 7880);
    assert!(config.enable_data_tracks);

    assert_eq!(config.rtc.base.tcp_port, 7881);
    assert_eq!(config.rtc.base.port_range_start, 0);
    assert_eq!(config.rtc.base.port_range_end, 0);
    assert!(!config.rtc.base.use_external_ip);
    assert!(config.rtc.base.stun_servers.is_empty());
    assert_eq!(config.rtc.packet_buffer_size, 500);
    assert_eq!(config.rtc.packet_buffer_size_video, 500);
    assert_eq!(config.rtc.packet_buffer_size_audio, 200);
    assert_eq!(
        config.rtc.datachannel_data_track_target_latency,
        GoDuration::from_millis(100)
    );

    assert_eq!(
        config.rtc.pli_throttle.low_quality,
        GoDuration::from_millis(500)
    );
    assert_eq!(
        config.rtc.pli_throttle.mid_quality,
        GoDuration::from_secs(1)
    );
    assert_eq!(
        config.rtc.pli_throttle.high_quality,
        GoDuration::from_secs(1)
    );

    assert!(config.rtc.congestion_control.enabled);
    assert!(!config.rtc.congestion_control.allow_pause);
    assert!(!config.rtc.congestion_control.use_send_side_bwe);
    assert!(!config.rtc.congestion_control.use_send_side_bwe_interceptor);
    assert_eq!(
        config.rtc.congestion_control.send_side_bwe_pacer,
        "no_queue"
    );
    assert_eq!(
        config
            .rtc
            .congestion_control
            .stream_allocator
            .probe_overage_pct,
        120
    );
    assert_eq!(
        config.rtc.congestion_control.stream_allocator.probe_min_bps,
        200_000
    );
    assert_eq!(
        config
            .rtc
            .congestion_control
            .stream_allocator
            .paused_min_wait,
        GoDuration::from_secs(5)
    );
    assert!(
        (config
            .rtc
            .congestion_control
            .remote_bwe
            .nack_ratio_attenuator
            - 0.4)
            .abs()
            < f64::EPSILON
    );
    assert!(
        (config
            .rtc
            .congestion_control
            .remote_bwe
            .expected_usage_threshold
            - 0.95)
            .abs()
            < f64::EPSILON
    );
    assert_eq!(
        config
            .rtc
            .congestion_control
            .remote_bwe
            .channel_observer_non_probe
            .estimate
            .required_samples,
        12
    );
    assert_eq!(
        config
            .rtc
            .congestion_control
            .send_side_bwe
            .congestion_detector
            .packet_group
            .min_packets,
        30
    );
    assert_eq!(
        config
            .rtc
            .congestion_control
            .send_side_bwe
            .congestion_detector
            .estimation_window_duration,
        GoDuration::from_secs(1)
    );

    assert_eq!(config.audio.level.active_level, 35);
    assert_eq!(config.audio.level.min_percentile, 40);
    assert_eq!(config.audio.level.update_interval, 400);
    assert_eq!(config.audio.level.smooth_intervals, 2);

    assert_eq!(config.video.dynacast_pause_delay, GoDuration::from_secs(5));
    assert_eq!(config.video.codec_regression_threshold, 5);
    assert_eq!(
        config
            .video
            .stream_tracker_manager
            .video
            .stream_tracker_type,
        "packet"
    );

    assert!(config.room.auto_create);
    assert_eq!(config.room.empty_timeout, 300);
    assert_eq!(config.room.departure_timeout, 20);
    assert_eq!(config.room.create_room_timeout, GoDuration::from_secs(10));
    assert_eq!(config.room.create_room_attempts, 3);
    assert_eq!(config.room.update_batch_target_size, 128 * 1024);
    let codecs: Vec<&str> = config
        .room
        .enabled_codecs
        .iter()
        .map(|c| c.mime.as_str())
        .collect();
    assert_eq!(
        codecs,
        vec![
            "audio/PCMU",
            "audio/PCMA",
            "audio/opus",
            "audio/red",
            "video/VP8",
            "video/H264",
            "video/VP9",
            "video/AV1",
            "video/H265",
            "video/rtx",
        ]
    );

    assert_eq!(config.limit.max_metadata_size, 512 * 1024);
    assert_eq!(config.limit.max_attributes_size, 64 * 1024);
    assert_eq!(config.limit.max_room_name_length, 256);
    assert_eq!(config.limit.max_participant_identity_length, 256);
    assert_eq!(config.limit.max_participant_name_length, 256);
    assert_eq!(config.limit.max_data_blob_key_length, 256);
    assert_eq!(config.limit.max_data_blob_size, 64_000);
    assert_eq!(config.limit.max_data_track_custom_encoding_length, 32);
    assert_eq!(config.limit.signal_message_size_limit, 2 << 20);
    assert_eq!(config.limit.agent_signal_message_size_limit, 2 << 20);
    assert_eq!(config.limit.max_api_request_body_size, 10 << 20);

    assert_eq!(config.logging.pion_level, "error");

    assert!(!config.turn.enabled);
    assert_eq!(config.turn.bind_addresses, vec!["0.0.0.0".to_owned()]);
    assert_eq!(
        config.turn.proxy_protocol_trusted_cidrs,
        vec!["127.0.0.0/8".to_owned(), "::1/128".to_owned()]
    );
    assert_eq!(config.turn.ttl_seconds, DEFAULT_TURN_TTL_SECONDS);
    assert_eq!(config.turn.per_user_relay_allocation_limit, 12);

    assert_eq!(config.node_selector.kind, "any");
    assert_eq!(config.node_selector.sort_by, "random");
    assert_eq!(config.node_selector.algorithm, "lowest");
    assert!((config.node_selector.sysload_limit - 0.9).abs() < f32::EPSILON);
    assert!((config.node_selector.cpu_load_limit - 0.9).abs() < f32::EPSILON);

    assert_eq!(
        config.signal_relay.retry_timeout,
        GoDuration::from_millis(7500)
    );
    assert_eq!(
        config.signal_relay.min_retry_interval,
        GoDuration::from_millis(500)
    );
    assert_eq!(
        config.signal_relay.max_retry_interval,
        GoDuration::from_secs(4)
    );
    assert_eq!(config.signal_relay.stream_buffer_size, 1000);
    assert_eq!(config.signal_relay.connect_attempts, 3);

    assert!((config.agents.target_load - 0.7).abs() < f32::EPSILON);

    assert_eq!(config.psrpc.max_attempts, 3);
    assert_eq!(config.psrpc.timeout, GoDuration::from_secs(3));
    assert_eq!(config.psrpc.backoff, GoDuration::from_secs(2));
    assert_eq!(config.psrpc.buffer_size, 1000);
    assert_eq!(config.psrpc.compression.threshold, 1024);

    assert_eq!(config.webhook.url_notifier.num_workers, 10);
    assert_eq!(config.webhook.url_notifier.queue_size, 100);
    assert_eq!(
        config.webhook.resource_url_notifier.max_age,
        GoDuration::from_secs(30)
    );
    assert_eq!(config.webhook.resource_url_notifier.max_depth, 200);

    assert_eq!(
        config
            .metric
            .timestamper
            .one_way_delay_estimator_min_interval,
        GoDuration::from_secs(5)
    );
    assert_eq!(
        config.metric.timestamper.one_way_delay_estimator_max_batch,
        100
    );
    assert_eq!(config.metric.collector.sampling_interval_ms, 3_000);
    assert_eq!(config.metric.collector.batch_interval_ms, 10_000);
    assert_eq!(config.metric.reporter.reporting_interval_ms, 10_000);

    assert_eq!(
        config.node_stats.stats_update_interval,
        GoDuration::from_secs(2)
    );
    assert_eq!(
        config.node_stats.stats_rate_measurement_intervals,
        vec![GoDuration::from_secs(10)]
    );
    assert_eq!(config.node_stats.stats_max_delay, GoDuration::from_secs(30));

    assert_eq!(config.api.execution_timeout, GoDuration::from_secs(2));
    assert_eq!(config.api.check_interval, GoDuration::from_millis(100));
    assert_eq!(config.api.max_check_interval, GoDuration::from_secs(300));
}

#[test]
fn every_default_survives_a_yaml_round_trip() {
    // The Go server marshals DefaultConfig and unmarshals it before applying
    // the user's document, so every default must be expressible in YAML and
    // must be a key the schema knows. A field with a wrong serde rename shows
    // up here as an unknown key rather than as a silent default.
    let defaults = Config::default();
    let yaml = serde_yaml::to_string(&defaults).unwrap();

    let document: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let unknown = lk_config::strict::unknown_fields(&document, Config::schema());
    assert!(
        unknown.is_empty(),
        "defaults produced unknown keys: {unknown:?}"
    );

    let mut reparsed = Config::load(&yaml, true, &[]).unwrap();
    // finalize() fills in the port and TURN ranges, so compare against a
    // finalized copy of the defaults rather than the raw ones.
    let mut expected = defaults;
    expected.finalize();
    reparsed.finalize();
    assert_eq!(reparsed, expected);
}

#[test]
fn strict_mode_rejects_unknown_keys_and_lax_mode_does_not() {
    let yaml = "port: 7880\nnot_a_key: 3\nrtc:\n  also_not: true\n";

    let err = Config::load(yaml, true, &[]).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("not_a_key"), "{message}");
    assert!(message.contains("rtc.also_not"), "{message}");

    // --disable-strict-config
    let config = Config::load(yaml, false, &[]).unwrap();
    assert_eq!(config.port, 7880);
}

#[test]
fn a_user_document_overrides_only_the_keys_it_sets() {
    let yaml = "
room:
  empty_timeout: 42
rtc:
  tcp_port: 9000
";
    let config = Config::load(yaml, true, &[]).unwrap();
    assert_eq!(config.room.empty_timeout, 42);
    assert_eq!(config.rtc.base.tcp_port, 9000);
    // untouched keys keep the Go defaults
    assert_eq!(config.room.departure_timeout, 20);
    assert_eq!(config.room.enabled_codecs.len(), 10);
    assert_eq!(config.rtc.packet_buffer_size_audio, 200);
}

#[test]
fn deprecated_room_limits_still_win() {
    let yaml = "
room:
  max_metadata_size: 1024
  max_room_name_length: 64
  max_participant_identity_length: 32
";
    let config = Config::load(yaml, true, &[]).unwrap();
    assert_eq!(config.limit.max_metadata_size, 1024);
    assert_eq!(config.limit.max_room_name_length, 64);
    assert_eq!(config.limit.max_participant_identity_length, 32);
}

#[test]
fn development_mode_shrinks_the_port_ranges() {
    let dev = Config::load("development: true", true, &[]).unwrap();
    assert_eq!(dev.rtc.base.udp_port.start, 7882);
    assert_eq!(dev.turn.relay_port_range_start, 30000);
    assert_eq!(dev.turn.relay_port_range_end, 30002);
    assert_eq!(dev.logging.base.level, "debug");

    let prod = Config::load("", true, &[]).unwrap();
    assert_eq!(prod.rtc.base.port_range_start, 50000);
    assert_eq!(prod.rtc.base.port_range_end, 60000);
    assert_eq!(prod.turn.relay_port_range_end, 40000);
}

#[test]
fn turn_ttls_are_clamped_into_the_safe_range() {
    let yaml = format!(
        "
turn:
  ttl_seconds: -1
rtc:
  turn_servers:
    - host: turn.example.com
      protocol: tls
      secret: shhh
      ttl: {}
",
        TURN_MAX_TTL_SECONDS + 1
    );
    let config = Config::load(&yaml, true, &[]).unwrap();
    assert_eq!(config.turn.ttl_seconds, DEFAULT_TURN_TTL_SECONDS);
    assert_eq!(config.rtc.turn_servers[0].ttl, TURN_MAX_TTL_SECONDS);
    assert!(config.is_turns_enabled());
}

#[test]
fn pion_level_reaches_the_component_levels() {
    let config = Config::load("logging:\n  pion_level: debug\n", true, &[]).unwrap();
    assert_eq!(
        config.logging.base.component_levels.get("pion"),
        Some(&"debug".to_owned())
    );
    assert_eq!(
        config.logging.base.component_levels.get("transport.pion"),
        Some(&"debug".to_owned())
    );
}

#[test]
fn keys_come_from_the_key_file_when_set() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.yaml");
    let mut file = std::fs::File::create(&path).unwrap();
    writeln!(file, "APIkey: {}", "s".repeat(40)).unwrap();
    drop(file);

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut config = Config::load(&format!("key_file: {}", path.display()), true, &[]).unwrap();
    config.validate_keys().unwrap();
    assert_eq!(config.keys.len(), 1);
    assert!(config.weak_keys().is_empty());

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o604)).unwrap();
    let mut config = Config::load(&format!("key_file: {}", path.display()), true, &[]).unwrap();
    let err = config.validate_keys().unwrap_err();
    assert!(err.to_string().contains("others permissions"), "{err}");
}

#[test]
fn a_config_without_keys_is_rejected() {
    let mut config = Config::load("", true, &[]).unwrap();
    assert!(matches!(
        config.validate_keys(),
        Err(lk_config::Error::KeysNotSet)
    ));
}

#[test]
fn short_secrets_are_reported_outside_development() {
    let mut config = Config::load("keys:\n  APIkey: short\n", true, &[]).unwrap();
    config.validate_keys().unwrap();
    assert_eq!(config.weak_keys(), vec!["APIkey"]);

    let mut dev = Config::load("development: true\nkeys:\n  APIkey: short\n", true, &[]).unwrap();
    dev.validate_keys().unwrap();
    assert!(dev.weak_keys().is_empty());
}

#[test]
fn turn_secrets_load_from_their_file() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("turn-secret");
    let mut file = std::fs::File::create(&path).unwrap();
    writeln!(file, "  super-secret  ").unwrap();
    drop(file);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let yaml = format!(
        "rtc:\n  turn_servers:\n    - host: turn.example.com\n      secret_file: {}\n",
        path.display()
    );
    let mut config = Config::load(&yaml, true, &[]).unwrap();
    let warnings = config.load_turn_secrets().unwrap();
    assert!(warnings.is_empty());
    assert_eq!(config.rtc.turn_servers[0].secret, "super-secret");

    // an empty secret file fails start-up rather than falling back
    let empty = dir.path().join("empty-secret");
    std::fs::write(&empty, "   \n").unwrap();
    std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o600)).unwrap();
    let yaml = format!(
        "rtc:\n  turn_servers:\n    - host: turn.example.com\n      secret_file: {}\n",
        empty.display()
    );
    let mut config = Config::load(&yaml, true, &[]).unwrap();
    assert!(matches!(
        config.load_turn_secrets(),
        Err(lk_config::Error::TurnSecretEmpty { .. })
    ));
}

#[test]
fn a_turn_server_without_credentials_is_rejected() {
    let yaml = "rtc:\n  turn_servers:\n    - host: turn.example.com\n      protocol: tls\n";
    let mut config = Config::load(yaml, true, &[]).unwrap();
    assert!(matches!(
        config.load_turn_secrets(),
        Err(lk_config::Error::TurnServerNoCredentials(_))
    ));
}

#[test]
fn cli_overrides_apply_after_the_document() {
    let yaml = "port: 7880\nrtc:\n  tcp_port: 7881\n";
    let config = Config::load(
        yaml,
        true,
        &[
            ("rtc.tcp_port".to_owned(), "7000".to_owned()),
            ("development".to_owned(), "true".to_owned()),
            ("room.empty_timeout".to_owned(), "60".to_owned()),
        ],
    )
    .unwrap();
    assert_eq!(config.rtc.base.tcp_port, 7000);
    assert!(config.development);
    assert_eq!(config.room.empty_timeout, 60);
    assert_eq!(config.port, 7880);
}

#[test]
fn a_bad_cli_value_is_an_error_not_a_default() {
    let err = Config::load("", true, &[("port".to_owned(), "seven".to_owned())]).unwrap_err();
    assert!(err.to_string().contains("port"), "{err}");
}
