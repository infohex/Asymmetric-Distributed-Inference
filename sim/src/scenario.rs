//! Scenario configuration (TOML).

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub name: String,
    #[serde(default = "default_seed")]
    pub seed: u64,
    pub scheduler: SchedulerParams,
    pub nodes: Vec<NodeSpec>,
    pub workload: WorkloadSpec,
    /// Hard stop for the simulation clock (seconds). The run normally ends
    /// when every Event has completed; this only guards against a stall.
    #[serde(default = "default_sim_end")]
    pub sim_end: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SchedulerParams {
    /// Time window W (seconds).
    pub w: f64,
    /// EWMA coefficient alpha.
    #[serde(default = "default_alpha")]
    pub alpha: f64,
    /// Safety factor k in r_safe = r - k * sigma.
    #[serde(default = "default_k")]
    pub k: f64,
    /// When a pull yields nothing, the node retries after w * retry_fraction.
    #[serde(default = "default_retry_fraction")]
    pub retry_fraction: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeSpec {
    pub id: String,
    /// True compute throughput, U per second.
    pub r_true: f64,
    /// True bandwidth, bytes per second.
    pub b_true: f64,
    /// Concurrency: minimum units per round, also the slow-start first round.
    pub c: u64,
    /// Fixed per-round-trip overhead, seconds.
    pub latency: f64,
    /// Sigma of the log-normal multiplicative noise on r_true per round (0 = none).
    #[serde(default)]
    pub noise_sigma: f64,
    #[serde(default = "default_tier")]
    pub tier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkloadSpec {
    pub events: usize,
    pub units_per_event: usize,
    #[serde(default = "default_unit_bytes")]
    pub unit_bytes: u64,
    /// "all_at_once": every Event is submitted at t = 0.
    /// "sequential": the next Event is submitted when the previous one completes.
    #[serde(default = "default_arrival")]
    pub arrival: String,
}

fn default_seed() -> u64 {
    1
}
fn default_sim_end() -> f64 {
    3600.0
}
fn default_alpha() -> f64 {
    0.3
}
fn default_k() -> f64 {
    1.0
}
fn default_retry_fraction() -> f64 {
    0.25
}
fn default_tier() -> String {
    "general".to_string()
}
fn default_unit_bytes() -> u64 {
    1024
}
fn default_arrival() -> String {
    "all_at_once".to_string()
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Scenario, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let s: Scenario =
            toml::from_str(&text).map_err(|e| format!("bad scenario {}: {e}", path.display()))?;
        s.validate()?;
        Ok(s)
    }

    fn validate(&self) -> Result<(), String> {
        if self.scheduler.w <= 0.0 {
            return Err("scheduler.w must be > 0".into());
        }
        if !(0.0 < self.scheduler.alpha && self.scheduler.alpha <= 1.0) {
            return Err("scheduler.alpha must be in (0, 1]".into());
        }
        if self.nodes.is_empty() {
            return Err("at least one node is required".into());
        }
        for n in &self.nodes {
            if n.r_true <= 0.0 || n.b_true <= 0.0 || n.c == 0 || n.latency < 0.0 {
                return Err(format!("node {}: r_true, b_true > 0, c >= 1, latency >= 0", n.id));
            }
        }
        if self.workload.events == 0 || self.workload.units_per_event == 0 {
            return Err("workload.events and workload.units_per_event must be >= 1".into());
        }
        match self.workload.arrival.as_str() {
            "all_at_once" | "sequential" => Ok(()),
            other => Err(format!("workload.arrival: unknown mode {other:?}")),
        }
    }
}
