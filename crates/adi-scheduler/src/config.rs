//! Service configuration (TOML).

use adi_core::{EtSpec, SchedulerParams};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Listen address, e.g. "0.0.0.0:7700".
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Directory for blobs and metrics. Memory only when omitted.
    pub data_dir: Option<PathBuf>,
    /// Token required on /events and /metrics. No check when empty.
    #[serde(default)]
    pub admin_token: String,
    pub scheduler: SchedulerParams,
    /// Omitted = one ET "default", tier general, no ceiling.
    #[serde(default)]
    pub ets: Vec<EtSpec>,
}

fn default_listen() -> String {
    "0.0.0.0:7700".to_string()
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut c: Config =
            toml::from_str(&text).map_err(|e| format!("bad config {}: {e}", path.display()))?;
        c.finish();
        Ok(c)
    }

    pub fn finish(&mut self) {
        if self.ets.is_empty() {
            self.ets.push(EtSpec { name: "default".into(), tier: "general".into(), t_max: None, w: None });
        }
    }
}
