//! Records of what happened, the V1–V4 criteria, and CSV / summary output.

use crate::scheduler::RoundKind;
use std::fmt::Write as _;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RoundRecord {
    pub node: String,
    pub issued_at: f64,
    pub returned_at: f64,
    pub units: usize,
    pub cost: f64,
    pub bytes: u64,
    pub kind: RoundKind,
    pub r_est: f64,
    pub r_true_eff: f64,
}

#[derive(Debug, Clone)]
pub struct EventRecord {
    pub event: usize,
    pub submitted_at: f64,
    pub completed_at: f64,
    /// First time a node asked for work and got none while this Event was
    /// incomplete; None if that never happened.
    pub first_starve_at: Option<f64>,
}

impl EventRecord {
    pub fn tail_wait(&self) -> f64 {
        match self.first_starve_at {
            Some(t) => (self.completed_at - t).max(0.0),
            None => 0.0,
        }
    }
}

#[derive(Default)]
pub struct Metrics {
    pub rounds: Vec<RoundRecord>,
    pub events: Vec<EventRecord>,
    pub pulls: u64,
    pub empty_pulls: u64,
    pub reissued_units: u64,
    pub total_units: u64,
    pub sim_time: f64,
    /// Per node: total time with a round in flight.
    pub busy: Vec<(String, f64)>,
    pub total_cost: f64,
    /// Σ r_true over all nodes (ground truth, used only for the efficiency figure).
    pub sum_r_true: f64,
}

pub struct Verdicts {
    pub w: f64,
    /// Total work ÷ Σ r_true: the makespan a perfect scheduler would reach.
    pub ideal_makespan: f64,
    pub efficiency: f64,
    pub v1_spread: Option<f64>,
    /// P95 − P5 of steady-state round durations: the spread without the extreme rounds.
    pub v1_spread_p90: Option<f64>,
    pub v1_mean: Option<f64>,
    pub v1_pass: Option<bool>,
    pub v2_max_tail: f64,
    pub v2_pass: bool,
}

impl Metrics {
    pub fn verdicts(&self, w: f64) -> Verdicts {
        let ideal_makespan = if self.sum_r_true > 0.0 { self.total_cost / self.sum_r_true } else { f64::NAN };
        let efficiency = ideal_makespan / self.sim_time.max(1e-9);
        let mut steady: Vec<f64> = self
            .rounds
            .iter()
            .filter(|r| r.kind == RoundKind::Normal)
            .map(|r| r.returned_at - r.issued_at)
            .collect();
        steady.sort_by(|a, b| a.total_cmp(b));
        let (v1_spread, v1_spread_p90, v1_mean, v1_pass) = if steady.is_empty() {
            (None, None, None, None)
        } else {
            let min = steady[0];
            let max = steady[steady.len() - 1];
            let q = |p: f64| steady[((steady.len() - 1) as f64 * p).round() as usize];
            let mean = steady.iter().sum::<f64>() / steady.len() as f64;
            (Some(max - min), Some(q(0.95) - q(0.05)), Some(mean), Some(max - min <= 0.2 * w))
        };
        let v2_max_tail = self.events.iter().map(|e| e.tail_wait()).fold(0.0, f64::max);
        Verdicts { w, ideal_makespan, efficiency, v1_spread, v1_spread_p90, v1_mean, v1_pass, v2_max_tail, v2_pass: v2_max_tail <= w }
    }

    pub fn write_all(&self, dir: &Path, w: f64, node_truth: &[(String, f64, f64)]) -> std::io::Result<String> {
        std::fs::create_dir_all(dir)?;

        let mut wtr = csv::Writer::from_path(dir.join("rounds.csv"))?;
        wtr.write_record([
            "node", "issued_at", "returned_at", "duration", "units", "cost", "bytes", "kind", "r_est", "r_true_eff",
        ])?;
        for r in &self.rounds {
            wtr.write_record([
                r.node.clone(),
                fmt(r.issued_at),
                fmt(r.returned_at),
                fmt(r.returned_at - r.issued_at),
                r.units.to_string(),
                fmt(r.cost),
                r.bytes.to_string(),
                format!("{:?}", r.kind),
                fmt(r.r_est),
                fmt(r.r_true_eff),
            ])?;
        }
        wtr.flush()?;

        let mut wtr = csv::Writer::from_path(dir.join("batches.csv"))?;
        wtr.write_record(["event", "submitted_at", "completed_at", "makespan", "first_starve_at", "tail_wait"])?;
        for e in &self.events {
            wtr.write_record([
                e.event.to_string(),
                fmt(e.submitted_at),
                fmt(e.completed_at),
                fmt(e.completed_at - e.submitted_at),
                e.first_starve_at.map(fmt).unwrap_or_default(),
                fmt(e.tail_wait()),
            ])?;
        }
        wtr.flush()?;

        let summary = self.summary(w, node_truth);
        std::fs::write(dir.join("summary.txt"), &summary)?;
        Ok(summary)
    }

    pub fn summary(&self, w: f64, node_truth: &[(String, f64, f64)]) -> String {
        let v = self.verdicts(w);
        let mut s = String::new();
        let _ = writeln!(s, "W = {w}");
        let _ = writeln!(s, "sim time = {:.3} s, rounds = {}, pulls = {} (empty {}), units = {}, reissued = {}",
            self.sim_time, self.rounds.len(), self.pulls, self.empty_pulls, self.total_units, self.reissued_units);
        let _ = writeln!(s, "scheduler messages/s = {:.1}",
            (self.pulls + self.rounds.len() as u64) as f64 / self.sim_time.max(1e-9));
        let _ = writeln!(s, "ideal makespan (Σcost ÷ Σ r_true) = {:.3} s, efficiency = {:.3}", v.ideal_makespan, v.efficiency);
        let _ = writeln!(s);
        let _ = writeln!(s, "per node: name, r_true, r_est(final), steady rounds, mean round duration, idle ratio");
        for (name, r_true, r_est) in node_truth {
            let steady: Vec<f64> = self.rounds.iter()
                .filter(|r| &r.node == name && r.kind == RoundKind::Normal)
                .map(|r| r.returned_at - r.issued_at).collect();
            let mean = if steady.is_empty() { f64::NAN } else { steady.iter().sum::<f64>() / steady.len() as f64 };
            let busy = self.busy.iter().find(|(n, _)| n == name).map(|(_, b)| *b).unwrap_or(0.0);
            let idle = 1.0 - busy / self.sim_time.max(1e-9);
            let _ = writeln!(s, "  {name:>8}  r_true {r_true:>9.2}  r_est {r_est:>9.2}  rounds {:>4}  mean {mean:.4} s  idle {idle:.3}", steady.len());
        }
        let _ = writeln!(s);
        match (v.v1_spread, v.v1_spread_p90, v.v1_mean, v.v1_pass) {
            (Some(spread), Some(p90), Some(mean), Some(pass)) => {
                let _ = writeln!(s, "V1 steady-state round duration: mean {mean:.4} s, spread max-min {spread:.4} s (P95-P5 {p90:.4} s), limit {:.4} s -> {}",
                    0.2 * w, pf(pass));
            }
            _ => { let _ = writeln!(s, "V1: no steady-state rounds recorded -> N/A"); }
        }
        let _ = writeln!(s, "V2 max tail wait over {} events: {:.4} s, limit {:.4} s -> {}",
            self.events.len(), v.v2_max_tail, w, pf(v.v2_pass));
        let _ = writeln!(s, "V3: not in stage 1");
        let _ = writeln!(s, "V4: not in stage 1");
        s
    }
}

fn fmt(x: f64) -> String {
    format!("{x:.6}")
}

fn pf(b: bool) -> &'static str {
    if b { "PASS" } else { "FAIL" }
}
