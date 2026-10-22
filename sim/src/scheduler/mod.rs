//! The scheduler: node table, ready queue (a single ET in stage 1), pull rules.
//!
//! It never sees node ground truth. Everything it knows comes in through
//! `Observation`s (issue time, return time, counts, bytes) and the node's
//! declared concurrency `c`.

pub mod estimate;

use crate::node::NodeId;
use crate::scenario::SchedulerParams;
use crate::workload::UnitId;
use estimate::Estimate;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundKind {
    /// First round of a node: c units, no estimate yet.
    SlowStart,
    /// Enough work for a full window: quota = r_safe * W.
    Normal,
    /// Less than a window of work left: proportional share so everyone finishes together.
    Taper,
}

#[derive(Debug, Clone)]
pub struct Round {
    pub units: Vec<UnitId>,
    pub cost: f64,
    pub bytes: u64,
    pub kind: RoundKind,
    /// Scheduler's r_safe for the node at issue time (0 during slow start).
    pub r_est: f64,
}

/// What the scheduler is told when a round comes back.
#[derive(Debug, Clone, Copy)]
pub struct Observation {
    pub node: NodeId,
    pub issued_at: f64,
    pub returned_at: f64,
    pub cost: f64,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
struct InFlight {
    issued_at: f64,
    cost: f64,
}

#[derive(Debug, Clone)]
struct NodeEntry {
    c: u64,
    est: Estimate,
    in_flight: Option<InFlight>,
}

#[derive(Debug, Clone, Copy)]
struct ReadyUnit {
    id: UnitId,
    cost: f64,
    bytes: u64,
}

pub struct Scheduler {
    params: SchedulerParams,
    nodes: Vec<NodeEntry>,
    ready: VecDeque<ReadyUnit>,
    ready_cost: f64,
}

impl Scheduler {
    pub fn new(params: SchedulerParams, node_concurrency: &[u64]) -> Scheduler {
        let nodes = node_concurrency
            .iter()
            .map(|&c| NodeEntry { c, est: Estimate::new(), in_flight: None })
            .collect();
        Scheduler { params, nodes, ready: VecDeque::new(), ready_cost: 0.0 }
    }

    pub fn w(&self) -> f64 {
        self.params.w
    }

    pub fn retry_delay(&self) -> f64 {
        self.params.w * self.params.retry_fraction
    }

    pub fn ready_len(&self) -> usize {
        self.ready.len()
    }

    pub fn estimate(&self, node: NodeId) -> &Estimate {
        &self.nodes[node].est
    }

    pub fn enqueue(&mut self, id: UnitId, cost: f64, bytes: u64) {
        self.ready.push_back(ReadyUnit { id, cost, bytes });
        self.ready_cost += cost;
    }

    /// Node `node` asks for work at time `now`. Returns the round it gets,
    /// which may be empty.
    pub fn pull(&mut self, node: NodeId, now: f64) -> Round {
        let entry = &self.nodes[node];
        assert!(entry.in_flight.is_none(), "node {node} pulled while a round is in flight");
        let k = self.params.k;
        let w = self.params.w;

        let (quota_cost, kind, r_est) = if entry.est.samples == 0 {
            (entry.c as f64, RoundKind::SlowStart, 0.0)
        } else if entry.est.ramp {
            // Ramp: take a full window at the latest measurement, grow until stable.
            let r_i = entry.est.r_safe(k);
            let q = (r_i * w).floor().max(entry.c as f64).min(self.ready_cost.max(0.0));
            (q, RoundKind::SlowStart, r_i)
        } else {
            let r_i = entry.est.r_safe(k);
            // Σ r over nodes that have an estimate, and their expected remaining in-flight work.
            let mut sum_r = 0.0;
            let mut remaining_other = 0.0;
            let mut max_r = 0.0_f64;
            for (j, n) in self.nodes.iter().enumerate() {
                if n.est.samples == 0 {
                    continue;
                }
                let r_j = n.est.r_safe(k);
                sum_r += r_j;
                max_r = max_r.max(r_j);
                if j != node {
                    if let Some(f) = &n.in_flight {
                        let done = r_j * (now - f.issued_at);
                        remaining_other += (f.cost - done).max(0.0);
                    }
                }
            }
            if sum_r <= 0.0 || r_i <= 0.0 {
                // Degenerate estimate: fall back to the minimum.
                (entry.c as f64, RoundKind::Normal, r_i)
            } else {
                let d = (self.ready_cost + remaining_other) / sum_r;
                if d >= w {
                    ((r_i * w).floor().max(entry.c as f64), RoundKind::Normal, r_i)
                } else {
                    let mut q = (r_i * d).floor();
                    if q < 1.0 && self.ready_cost > 0.0 && r_i >= max_r {
                        // Rounding remainder goes to the fastest node.
                        q = (r_i * d).ceil().max(1.0);
                    }
                    (q, RoundKind::Taper, r_i)
                }
            }
        };

        let mut units = Vec::new();
        let mut cost = 0.0;
        let mut bytes = 0;
        while let Some(u) = self.ready.front() {
            if cost + u.cost > quota_cost + 1e-9 {
                break;
            }
            let u = self.ready.pop_front().unwrap();
            cost += u.cost;
            bytes += u.bytes;
            units.push(u.id);
        }
        self.ready_cost -= cost;
        if self.ready_cost < 1e-9 {
            self.ready_cost = 0.0;
        }
        if !units.is_empty() {
            self.nodes[node].in_flight = Some(InFlight { issued_at: now, cost });
        }
        Round { units, cost, bytes, kind, r_est }
    }

    /// A round came back. Updates the node's estimate from the observation.
    pub fn round_done(&mut self, obs: Observation) {
        let entry = &mut self.nodes[obs.node];
        let f = entry.in_flight.take().expect("round_done without a round in flight");
        debug_assert!((f.issued_at - obs.issued_at).abs() < 1e-9);
        let duration = (obs.returned_at - obs.issued_at).max(1e-9);
        // Stage 1: no separation of transfer and compute time; bandwidth is
        // recorded as bytes over the whole round and not yet used.
        let r_round = obs.cost / duration;
        let b_round = obs.bytes as f64 / duration;
        // A round much shorter than W (taper, crumbs) is dominated by the fixed
        // round-trip overhead and carries little information about capacity:
        // weight it by its length relative to the window.
        let alpha_eff = self.params.alpha * (duration / self.params.w).min(1.0);
        entry.est.update(r_round, b_round, alpha_eff);
    }
}
