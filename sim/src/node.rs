//! Node ground truth. Nothing in this module is visible to the scheduler:
//! the scheduler only ever receives `Observation`s built by the engine.

use crate::scenario::NodeSpec;
use rand::Rng;
use rand_distr::{Distribution, LogNormal};

pub type NodeId = usize;

#[derive(Debug, Clone)]
pub struct TrueNode {
    pub id: NodeId,
    pub name: String,
    pub r_true: f64,
    pub b_true: f64,
    pub c: u64,
    pub latency: f64,
    pub noise: Option<LogNormal<f64>>,
    pub tier: String,
    pub online: bool,
}

impl TrueNode {
    pub fn from_spec(id: NodeId, spec: &NodeSpec) -> TrueNode {
        let noise = if spec.noise_sigma > 0.0 {
            // Mean-one log-normal: mu = -sigma^2 / 2.
            let sigma = spec.noise_sigma;
            Some(LogNormal::new(-sigma * sigma / 2.0, sigma).expect("valid log-normal"))
        } else {
            None
        };
        TrueNode {
            id,
            name: spec.id.clone(),
            r_true: spec.r_true,
            b_true: spec.b_true,
            c: spec.c,
            latency: spec.latency,
            noise,
            tier: spec.tier.clone(),
            online: true,
        }
    }

    /// True wall time for this node to run a round of `cost` units of work
    /// moving `bytes` of input. Returns (duration, effective throughput used).
    pub fn round_duration<R: Rng>(&self, cost: f64, bytes: u64, rng: &mut R) -> (f64, f64) {
        let factor = match &self.noise {
            Some(d) => d.sample(rng),
            None => 1.0,
        };
        let r_eff = self.r_true * factor;
        let d = self.latency + bytes as f64 / self.b_true + cost / r_eff;
        (d, r_eff)
    }
}
