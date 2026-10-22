//! Node ground truth. Nothing in this module is visible to the scheduler:
//! the scheduler only ever receives `Observation`s built by the engine.

use crate::scenario::NodeSpec;
use rand::Rng;
use rand_distr::{Distribution, StandardNormal};

pub use adi_core::NodeId;

#[derive(Debug, Clone)]
pub struct TrueNode {
    pub id: NodeId,
    pub name: String,
    pub r_true: f64,
    pub b_true: f64,
    pub c: u64,
    pub latency: f64,
    /// Sigma of the log-normal multiplicative noise on r_true (0 = none).
    pub noise_sigma: f64,
    /// AR(1) coefficient in log space: 0 = i.i.d. per round.
    pub noise_rho: f64,
    /// Current log-noise state (mean-zero).
    noise_x: f64,
    pub tier: String,
    pub online: bool,
    pub mean_online: f64,
    pub mean_offline: f64,
    /// Bumped on every offline transition; a round issued in an earlier
    /// epoch never returns.
    pub epoch: u64,
}

impl TrueNode {
    pub fn from_spec(id: NodeId, spec: &NodeSpec) -> TrueNode {
        let noise_rho = if spec.noise_corr_rounds > 0.0 {
            (-1.0 / spec.noise_corr_rounds).exp()
        } else {
            0.0
        };
        TrueNode {
            id,
            name: spec.id.clone(),
            r_true: spec.r_true,
            b_true: spec.b_true,
            c: spec.c,
            latency: spec.latency,
            noise_sigma: spec.noise_sigma,
            noise_rho,
            noise_x: 0.0,
            tier: spec.tier.clone(),
            online: true,
            mean_online: spec.mean_online,
            mean_offline: spec.mean_offline,
            epoch: 0,
        }
    }

    /// True wall time for this node to run a round of `cost` units of work
    /// moving `bytes` of input. Returns (duration, effective throughput used).
    pub fn round_duration<R: Rng>(&mut self, cost: f64, bytes: u64, rng: &mut R) -> (f64, f64) {
        let factor = if self.noise_sigma > 0.0 {
            let eps: f64 = StandardNormal.sample(rng);
            // Stationary AR(1) with marginal sigma = noise_sigma; mean-one log-normal.
            self.noise_x = self.noise_rho * self.noise_x
                + (1.0 - self.noise_rho * self.noise_rho).sqrt() * self.noise_sigma * eps;
            (self.noise_x - self.noise_sigma * self.noise_sigma / 2.0).exp()
        } else {
            1.0
        };
        let r_eff = self.r_true * factor;
        let d = self.latency + bytes as f64 / self.b_true + cost / r_eff;
        (d, r_eff)
    }
}
