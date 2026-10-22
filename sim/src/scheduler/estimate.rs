//! Per-node capacity estimate maintained by the scheduler from observations only.

#[derive(Debug, Clone)]
pub struct Estimate {
    /// EWMA of measured throughput (U/s).
    pub r: f64,
    /// EWMA of the variance of r_round around r; sigma = sqrt(var).
    pub var: f64,
    /// EWMA of measured bandwidth (bytes/s). Not yet used for quotas (stage 2).
    pub b: f64,
    pub samples: u64,
    /// Slow-start ramp: while true, each measurement replaces the estimate
    /// outright (alpha = 1) so the quota grows to the node's real capacity in
    /// a few rounds. The ramp ends once two consecutive measurements agree
    /// within RAMP_TOLERANCE; after that the EWMA takes over.
    pub ramp: bool,
}

/// Relative change between consecutive ramp measurements below which the
/// estimate is considered stable.
pub const RAMP_TOLERANCE: f64 = 0.05;

impl Estimate {
    pub fn new() -> Estimate {
        Estimate { r: 0.0, var: 0.0, b: 0.0, samples: 0, ramp: true }
    }

    pub fn sigma(&self) -> f64 {
        self.var.sqrt()
    }

    /// Conservative throughput used for quotas.
    pub fn r_safe(&self, k: f64) -> f64 {
        (self.r - k * self.sigma()).max(0.0)
    }

    /// Fold one round's measurement in. The first sample initialises the
    /// estimate directly (no prior); later samples use the EWMA.
    pub fn update(&mut self, r_round: f64, b_round: f64, alpha: f64) {
        if self.ramp {
            if self.samples > 0 && self.r > 0.0 && ((r_round - self.r) / self.r).abs() < RAMP_TOLERANCE {
                self.ramp = false;
            }
            self.r = r_round;
            if b_round.is_finite() {
                self.b = b_round;
            }
            self.var = 0.0;
        } else {
            let dev = r_round - self.r;
            self.r = alpha * r_round + (1.0 - alpha) * self.r;
            self.var = alpha * dev * dev + (1.0 - alpha) * self.var;
            if b_round.is_finite() {
                self.b = alpha * b_round + (1.0 - alpha) * self.b;
            }
        }
        self.samples += 1;
    }
}
