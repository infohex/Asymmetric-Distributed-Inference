//! Identifiers and configuration shared across crates.

use serde::Deserialize;

pub type NodeId = usize;
pub type UnitId = usize;

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

pub fn default_alpha() -> f64 {
    0.5
}
pub fn default_k() -> f64 {
    1.0
}
pub fn default_retry_fraction() -> f64 {
    0.25
}
pub fn default_hysteresis_n() -> u32 {
    3
}
pub fn default_hysteresis_band() -> f64 {
    0.2
}
pub fn default_timeout_mult() -> f64 {
    2.0
}
pub fn default_tier() -> String {
    "general".to_string()
}
