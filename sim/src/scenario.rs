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
    /// Event Trenches. Omitted = one ET named "default", tier general, no ceiling.
    #[serde(default)]
    pub ets: Vec<EtSpec>,
    /// Single workload (kept for simple scenarios) ...
    #[serde(default)]
    pub workload: Option<WorkloadSpec>,
    /// ... or several, each aimed at an ET.
    #[serde(default)]
    pub workloads: Vec<WorkloadSpec>,
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
    /// Consecutive qualifying rounds needed to promote a node into an ET
    /// (demotion always takes one failing round). 1 = no hysteresis.
    #[serde(default = "default_hysteresis_n")]
    pub hysteresis_n: u32,
    /// Value hysteresis: promotion requires t <= t_max * (1 - band); demotion
    /// happens at t > t_max. 0 = none.
    #[serde(default = "default_hysteresis_band")]
    pub hysteresis_band: f64,
    /// A round not returned within timeout_mult * W is reclaimed and reissued.
    #[serde(default = "default_timeout_mult")]
    pub timeout_mult: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EtSpec {
    pub name: String,
    #[serde(default = "default_tier")]
    pub tier: String,
    /// Per-U time ceiling in seconds; omitted = no ceiling.
    pub t_max: Option<f64>,
    /// Window for this ET; omitted = scheduler.w.
    pub w: Option<f64>,
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
    /// Correlation time of the noise in rounds. 0 = independent per round
    /// (i.i.d.); larger values make the noise a slow drift (AR(1) in log
    /// space) that an EWMA can track.
    #[serde(default)]
    pub noise_corr_rounds: f64,
    /// Mean online duration (s) before the node drops; 0 = never drops.
    #[serde(default)]
    pub mean_online: f64,
    /// Mean offline duration (s) before it returns.
    #[serde(default)]
    pub mean_offline: f64,
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
    /// Target ET by name; omitted = the first ET.
    pub et: Option<String>,
    /// "parallel" (default), "serial" (a chain) or "fanout_reduce"
    /// (units_per_event parallel U + one reduce U depending on all of them).
    #[serde(default = "default_shape")]
    pub shape: String,
    /// Cost of the reduce U in fanout_reduce (in standard-U units).
    #[serde(default = "default_reduce_cost")]
    pub reduce_cost: f64,
}

fn default_seed() -> u64 {
    1
}
fn default_sim_end() -> f64 {
    3600.0
}
fn default_alpha() -> f64 {
    0.5
}
fn default_k() -> f64 {
    1.0
}
fn default_retry_fraction() -> f64 {
    0.25
}
fn default_hysteresis_n() -> u32 {
    3
}
fn default_hysteresis_band() -> f64 {
    0.2
}
fn default_timeout_mult() -> f64 {
    2.0
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
fn default_shape() -> String {
    "parallel".to_string()
}
fn default_reduce_cost() -> f64 {
    1.0
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Scenario, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut s: Scenario =
            toml::from_str(&text).map_err(|e| format!("bad scenario {}: {e}", path.display()))?;
        if let Some(w) = s.workload.take() {
            s.workloads.insert(0, w);
        }
        if s.ets.is_empty() {
            s.ets.push(EtSpec { name: "default".into(), tier: "general".into(), t_max: None, w: None });
        }
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
        if self.workloads.is_empty() {
            return Err("at least one workload is required".into());
        }
        for wl in &self.workloads {
            if wl.events == 0 || wl.units_per_event == 0 {
                return Err("workload.events and workload.units_per_event must be >= 1".into());
            }
            match wl.arrival.as_str() {
                "all_at_once" | "sequential" => {}
                other => return Err(format!("workload.arrival: unknown mode {other:?}")),
            }
            match wl.shape.as_str() {
                "parallel" | "serial" | "fanout_reduce" => {}
                other => return Err(format!("workload.shape: unknown shape {other:?}")),
            }
            if let Some(et) = &wl.et {
                if !self.ets.iter().any(|e| &e.name == et) {
                    return Err(format!("workload.et {et:?} names no ET"));
                }
            }
        }
        for e in &self.ets {
            match e.tier.as_str() {
                "general" | "gpu" | "special" => {}
                other => return Err(format!("et {}: unknown tier {other:?}", e.name)),
            }
        }
        for n in &self.nodes {
            match n.tier.as_str() {
                "general" | "gpu" | "special" => {}
                other => return Err(format!("node {}: unknown tier {other:?}", n.id)),
            }
        }
        Ok(())
    }
}

/// Parameter sweep: a base scenario plus a grid of scheduler overrides.
#[derive(Debug, Clone, Deserialize)]
pub struct SweepSpec {
    pub name: String,
    /// Path of the base scenario, relative to the sweep file's directory.
    pub scenario: String,
    pub grid: Grid,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Grid {
    #[serde(default)]
    pub w: Vec<f64>,
    #[serde(default)]
    pub alpha: Vec<f64>,
    #[serde(default)]
    pub k: Vec<f64>,
    #[serde(default)]
    pub timeout_mult: Vec<f64>,
    /// Seeds to average over for each combination (default: the scenario's seed).
    #[serde(default)]
    pub seeds: Vec<u64>,
}

impl SweepSpec {
    pub fn load(path: &Path) -> Result<SweepSpec, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("bad sweep {}: {e}", path.display()))
    }

    /// Every combination of the grid, as (w, alpha, k, timeout_mult). An
    /// empty axis keeps the base scenario's value (None).
    pub fn combinations(&self) -> Vec<(Option<f64>, Option<f64>, Option<f64>, Option<f64>)> {
        let axis = |v: &Vec<f64>| -> Vec<Option<f64>> {
            if v.is_empty() { vec![None] } else { v.iter().map(|x| Some(*x)).collect() }
        };
        let mut out = Vec::new();
        for w in axis(&self.grid.w) {
            for a in axis(&self.grid.alpha) {
                for k in axis(&self.grid.k) {
                    for t in axis(&self.grid.timeout_mult) {
                        out.push((w, a, k, t));
                    }
                }
            }
        }
        out
    }
}
