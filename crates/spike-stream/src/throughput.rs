//! EWMA throughput estimation.
//!
//! Used by the scheduler to pick an appropriate HLS variant. Bytes-per-
//! second samples are converted to bits-per-second and smoothed with an
//! exponentially weighted moving average so a single slow segment does not
//! tank the estimate (and one fast CDN response does not spike it).

/// Exponentially weighted moving average of network throughput (bps).
#[derive(Debug, Clone)]
pub struct ThroughputMonitor {
    ewma_bps: Option<f64>,
    alpha: f64,
    samples: u64,
}

impl ThroughputMonitor {
    /// `alpha` in `0..=1`; smaller = smoother, larger = more reactive.
    pub fn new(alpha: f64) -> Self {
        Self {
            ewma_bps: None,
            alpha: alpha.clamp(0.01, 1.0),
            samples: 0,
        }
    }

    /// Feed one transfer sample.
    pub fn sample(&mut self, bytes: u64, elapsed_secs: f64) {
        if bytes == 0 || elapsed_secs <= 1e-6 {
            return;
        }
        let bps = (bytes as f64) * 8.0 / elapsed_secs;
        self.ewma_bps = Some(match self.ewma_bps {
            None => bps,
            Some(prev) => prev + self.alpha * (bps - prev),
        });
        self.samples += 1;
    }

    /// Current smoothed estimate, if any samples exist.
    pub fn estimate_bps(&self) -> Option<f64> {
        self.ewma_bps
    }

    /// Number of accepted samples.
    pub fn samples(&self) -> u64 {
        self.samples
    }
}

impl Default for ThroughputMonitor {
    fn default() -> Self {
        Self::new(0.2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_seeds_estimate() {
        let mut t = ThroughputMonitor::default();
        assert_eq!(t.estimate_bps(), None);
        // 1_000 bytes in 0.001s = 8_000_000 bps.
        t.sample(1_000, 0.001);
        assert!((t.estimate_bps().unwrap() - 8_000_000.0).abs() < 1.0);
        assert_eq!(t.samples(), 1);
    }

    #[test]
    fn ewma_smooths_towards_new_samples() {
        let mut t = ThroughputMonitor::new(0.5);
        t.sample(1_000, 0.001); // 8 mbps
        t.sample(1_000, 0.003); // ~2.67 mbps
        let est = t.estimate_bps().unwrap();
        let expected = 8_000_000.0 + 0.5 * (8_000_000.0 / 3.0 - 8_000_000.0);
        assert!((est - expected).abs() < 1.0);
    }

    #[test]
    fn invalid_samples_are_ignored() {
        let mut t = ThroughputMonitor::default();
        t.sample(0, 1.0);
        t.sample(100, 0.0);
        assert_eq!(t.estimate_bps(), None);
        assert_eq!(t.samples(), 0);
    }
}
