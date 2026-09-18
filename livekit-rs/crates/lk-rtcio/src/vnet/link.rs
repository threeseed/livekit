//! Link impairments.
//!
//! A link is directional: A to B and B to A are configured separately, because
//! the asymmetry is the interesting case (an uplink that saturates while the
//! downlink is clean is exactly the shape the stream allocator has to handle).

use std::time::Duration;

/// A two-state burst-loss model (Gilbert-Elliott).
///
/// Independent per-packet loss is the wrong model for a real network: losses
/// arrive in runs, and a repair scheme tuned against uniform loss falls apart
/// against runs. This is the cheapest model that produces runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BurstLoss {
    /// Probability per packet of entering the lossy state.
    pub enter: f64,
    /// Probability per packet of leaving it.
    pub exit: f64,
    /// Loss probability while in the lossy state.
    pub loss_in_burst: f64,
}

impl BurstLoss {
    /// A model losing runs of roughly `mean_run` packets, entering a run about
    /// every `mean_gap` packets.
    #[must_use]
    pub fn runs(mean_run: f64, mean_gap: f64) -> Self {
        Self {
            enter: if mean_gap > 0.0 { 1.0 / mean_gap } else { 0.0 },
            exit: if mean_run > 0.0 { 1.0 / mean_run } else { 1.0 },
            loss_in_burst: 1.0,
        }
    }
}

/// What one direction of a link does to the packets crossing it.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkConfig {
    /// One-way propagation delay.
    pub delay: Duration,
    /// Uniform jitter added on top of `delay`, in `[0, jitter)`.
    pub jitter: Duration,
    /// Independent per-packet loss probability, in `[0, 1]`.
    pub loss: f64,
    /// Optional burst-loss model, applied in addition to `loss`.
    pub burst_loss: Option<BurstLoss>,
    /// Probability that a packet is delayed by `reorder_delay`, which pushes it
    /// behind later packets.
    pub reorder: f64,
    /// Extra delay applied to a reordered packet.
    pub reorder_delay: Duration,
    /// Bandwidth cap in bits per second. `None` means unmetered.
    ///
    /// The cap is enforced by a serialisation queue, not a token bucket, so a
    /// sender that overshoots sees the queueing delay grow. A bandwidth
    /// estimator that only ever sees loss and never sees delay is not being
    /// tested.
    pub bandwidth_bps: Option<u64>,
    /// How deep the queue may get before packets are tail-dropped.
    pub max_queue_delay: Duration,
    /// Bytes of per-packet overhead charged against the bandwidth cap
    /// (Ethernet plus IP plus UDP is about 42).
    pub overhead_bytes: u64,
}

impl LinkConfig {
    /// A perfect link: no delay, no loss, no cap. The default.
    #[must_use]
    pub fn perfect() -> Self {
        Self {
            delay: Duration::ZERO,
            jitter: Duration::ZERO,
            loss: 0.0,
            burst_loss: None,
            reorder: 0.0,
            reorder_delay: Duration::from_millis(30),
            bandwidth_bps: None,
            max_queue_delay: Duration::from_millis(500),
            overhead_bytes: 42,
        }
    }

    /// A typical domestic broadband path.
    #[must_use]
    pub fn broadband() -> Self {
        Self {
            delay: Duration::from_millis(20),
            jitter: Duration::from_millis(4),
            loss: 0.001,
            bandwidth_bps: Some(10_000_000),
            ..Self::perfect()
        }
    }

    /// A congested mobile path: high latency, jitter, loss in runs, and a cap
    /// low enough that a 2 Mbps video stream queues.
    #[must_use]
    pub fn lossy_mobile() -> Self {
        Self {
            delay: Duration::from_millis(80),
            jitter: Duration::from_millis(25),
            loss: 0.01,
            burst_loss: Some(BurstLoss::runs(4.0, 200.0)),
            reorder: 0.005,
            bandwidth_bps: Some(1_500_000),
            max_queue_delay: Duration::from_millis(300),
            ..Self::perfect()
        }
    }

    /// Set the one-way delay.
    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Set the jitter bound.
    #[must_use]
    pub fn with_jitter(mut self, jitter: Duration) -> Self {
        self.jitter = jitter;
        self
    }

    /// Set the independent loss probability.
    #[must_use]
    pub fn with_loss(mut self, loss: f64) -> Self {
        self.loss = loss.clamp(0.0, 1.0);
        self
    }

    /// Set the burst-loss model.
    #[must_use]
    pub fn with_burst_loss(mut self, burst: BurstLoss) -> Self {
        self.burst_loss = Some(burst);
        self
    }

    /// Set the reorder probability.
    #[must_use]
    pub fn with_reorder(mut self, probability: f64, delay: Duration) -> Self {
        self.reorder = probability.clamp(0.0, 1.0);
        self.reorder_delay = delay;
        self
    }

    /// Set the bandwidth cap.
    #[must_use]
    pub fn with_bandwidth_bps(mut self, bps: u64) -> Self {
        self.bandwidth_bps = Some(bps);
        self
    }

    /// Set how deep the queue may get before tail drop.
    #[must_use]
    pub fn with_max_queue_delay(mut self, delay: Duration) -> Self {
        self.max_queue_delay = delay;
        self
    }

    /// How long `bytes` takes to serialise at this link's rate.
    #[must_use]
    pub fn serialisation_delay(&self, bytes: usize) -> Duration {
        let Some(bps) = self.bandwidth_bps else {
            return Duration::ZERO;
        };
        if bps == 0 {
            return Duration::ZERO;
        }
        let bits = (bytes as u64)
            .saturating_add(self.overhead_bytes)
            .saturating_mul(8);
        // Nanoseconds, computed in u128 so a slow link and a big packet cannot
        // overflow before the divide.
        let nanos = (u128::from(bits) * 1_000_000_000u128) / u128::from(bps);
        Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
    }
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self::perfect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn serialisation_delay_tracks_the_rate() {
        // 1000 bytes plus 42 overhead is 8336 bits; at 1 Mbps that is 8.336 ms.
        let link = LinkConfig::perfect().with_bandwidth_bps(1_000_000);
        assert_eq!(
            link.serialisation_delay(1000),
            Duration::from_nanos(8_336_000)
        );
    }

    #[test]
    fn an_unmetered_link_serialises_instantly() {
        assert_eq!(
            LinkConfig::perfect().serialisation_delay(1200),
            Duration::ZERO
        );
    }

    #[test]
    fn a_zero_rate_link_does_not_divide_by_zero() {
        let link = LinkConfig::perfect().with_bandwidth_bps(0);
        assert_eq!(link.serialisation_delay(1200), Duration::ZERO);
    }

    #[test]
    fn burst_runs_translate_to_transition_probabilities() {
        let burst = BurstLoss::runs(4.0, 200.0);
        assert!((burst.exit - 0.25).abs() < f64::EPSILON);
        assert!((burst.enter - 0.005).abs() < f64::EPSILON);
    }

    #[test]
    fn loss_is_clamped_to_a_probability() {
        assert!((LinkConfig::perfect().with_loss(2.0).loss - 1.0).abs() < f64::EPSILON);
        assert!((LinkConfig::perfect().with_loss(-1.0).loss).abs() < f64::EPSILON);
    }
}
