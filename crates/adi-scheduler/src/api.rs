//! Wire types of the control plane (JSON).

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct RegisterReq {
    pub node_id: String,
    #[serde(default = "default_tier")]
    pub tier: String,
    pub c: u64,
    #[serde(default)]
    pub hardware: serde_json::Value,
}

fn default_tier() -> String {
    "general".to_string()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterResp {
    /// Numeric id used in later requests.
    pub node: usize,
    pub token: String,
    pub w: f64,
    pub retry_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct PullReq {
    pub node: usize,
    #[serde(default)]
    pub wait_ms: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UnitInput {
    pub a: String,
    pub b: String,
    pub bi: usize,
    pub bj: usize,
    pub block: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UnitWire {
    pub u_id: usize,
    pub e_id: usize,
    pub logic_hash: String,
    pub input: UnitInput,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullResp {
    pub round_id: u64,
    pub et: String,
    pub issued_at: f64,
    pub units: Vec<UnitWire>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ResultWire {
    pub u_id: usize,
    pub hash: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ReportReq {
    pub node: usize,
    pub round_id: u64,
    pub started_at: f64,
    pub finished_at: f64,
    #[serde(default)]
    pub bytes_in: u64,
    pub results: Vec<ResultWire>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportResp {
    pub accepted: Vec<usize>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitReq {
    pub a_hash: String,
    pub b_hash: String,
    #[serde(default = "default_block")]
    pub block: usize,
    pub et: Option<String>,
}

fn default_block() -> usize {
    128
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubmitResp {
    pub e_id: usize,
    pub units: usize,
    pub m: usize,
    pub n: usize,
    pub k: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EventStatus {
    pub e_id: usize,
    pub status: String,
    pub done: usize,
    pub total: usize,
    pub submitted_at: f64,
    pub completed_at: Option<f64>,
    /// Result block hashes by u_id, once complete.
    pub results: Option<Vec<(usize, String)>>,
}
