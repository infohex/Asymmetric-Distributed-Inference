//! The scheduler: node table, Event Trenches, pull rules, eligibility with
//! hysteresis.
//!
//! It never sees node ground truth. Everything it knows comes in through
//! `Observation`s (issue time, return time, counts, bytes), the node's
//! declared concurrency `c` and its declared hardware tier.

pub mod estimate;
pub mod resolver;

use crate::node::NodeId;
use crate::scenario::{EtSpec, SchedulerParams};
use crate::workload::UnitId;
use estimate::Estimate;
use resolver::Resolver;
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
    /// ET the round was taken from.
    pub et: usize,
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
    et: usize,
    units: Vec<ReadyUnit>,
}

/// Units handed back to an ET after a timeout or a reconnect.
#[derive(Debug, Clone)]
pub struct Reclaim {
    pub node: NodeId,
    pub et: usize,
    pub units: Vec<UnitId>,
    pub cost: f64,
}

#[derive(Debug, Clone)]
struct NodeEntry {
    c: u64,
    tier_level: u8,
    est: Estimate,
    in_flight: Option<InFlight>,
    /// Per ET: currently eligible.
    eligible: Vec<bool>,
    /// Per ET: consecutive qualifying rounds (promotion hysteresis).
    pass_count: Vec<u32>,
    /// Count of eligibility changes (promotions + demotions) over all ETs.
    transitions: u64,
    /// Last pull or result from this node. An idle node pulls every
    /// retry_delay, so silence longer than a couple of those means it is gone.
    last_seen: f64,
}

#[derive(Debug, Clone, Copy)]
struct ReadyUnit {
    id: UnitId,
    cost: f64,
    bytes: u64,
}

#[derive(Debug, Clone)]
pub struct Trench {
    pub name: String,
    pub tier_level: u8,
    pub t_max: f64,
    pub w: f64,
    ready: VecDeque<ReadyUnit>,
    ready_cost: f64,
}

impl Trench {
    pub fn ready_cost(&self) -> f64 {
        self.ready_cost
    }
}

pub fn tier_level(tier: &str) -> u8 {
    match tier {
        "general" => 0,
        "gpu" => 1,
        "special" => 2,
        _ => 0,
    }
}

pub struct Scheduler {
    params: SchedulerParams,
    nodes: Vec<NodeEntry>,
    ets: Vec<Trench>,
    /// Per unit: a result has been accepted (first result wins).
    done: Vec<bool>,
    pub duplicate_results: u64,
    pub late_results: u64,
    resolver: Resolver,
    /// Per unit: (et, cost, bytes), so an unlocked unit can be enqueued later.
    unit_info: Vec<(usize, f64, u64)>,
}

impl Scheduler {
    pub fn new(params: SchedulerParams, nodes: &[(u64, String)], ets: &[EtSpec]) -> Scheduler {
        let w = params.w;
        let mut trenches: Vec<Trench> = ets
            .iter()
            .map(|e| Trench {
                name: e.name.clone(),
                tier_level: tier_level(&e.tier),
                t_max: e.t_max.unwrap_or(f64::INFINITY),
                w: e.w.unwrap_or(w),
                ready: VecDeque::new(),
                ready_cost: 0.0,
            })
            .collect();
        // Fastest ET first: that is the order a node checks them in. At equal
        // ceilings the higher tier comes first, so a GPU node serves the work
        // only it can do before dropping into the general ET.
        trenches.sort_by(|a, b| a.t_max.total_cmp(&b.t_max).then(b.tier_level.cmp(&a.tier_level)));
        let n_et = trenches.len();
        let nodes = nodes
            .iter()
            .map(|(c, tier)| {
                let lvl = tier_level(tier);
                NodeEntry {
                    c: *c,
                    tier_level: lvl,
                    est: Estimate::new(),
                    in_flight: None,
                    // Before any measurement a node may only take from the
                    // slowest ET its hardware allows: the safe place to slow-start.
                    eligible: Self::initial_eligibility(&trenches, lvl),
                    pass_count: vec![0; n_et],
                    transitions: 0,
                    last_seen: 0.0,
                }
            })
            .collect();
        Scheduler {
            params,
            nodes,
            ets: trenches,
            done: Vec::new(),
            duplicate_results: 0,
            late_results: 0,
            resolver: Resolver::default(),
            unit_info: Vec::new(),
        }
    }

    /// Submit a unit of an Event: it enters its ET now if every prerequisite
    /// is done, otherwise it waits in the resolver.
    pub fn submit(&mut self, et: usize, id: UnitId, cost: f64, bytes: u64, deps: &[UnitId]) {
        if self.unit_info.len() <= id {
            self.unit_info.resize(id + 1, (0, 0.0, 0));
            self.done.resize(id + 1, false);
        }
        self.unit_info[id] = (et, cost, bytes);
        if self.resolver.register(id, deps, &self.done) {
            self.enqueue(et, id, cost, bytes);
        }
    }

    pub fn waiting_units(&self) -> usize {
        self.resolver.waiting()
    }

    pub fn has_in_flight(&self, node: NodeId) -> bool {
        self.nodes[node].in_flight.is_some()
    }

    pub fn timeout(&self, et: usize) -> f64 {
        self.params.timeout_mult * self.ets[et].w
    }

    fn put_back(&mut self, et: usize, units: &[ReadyUnit]) -> (Vec<UnitId>, f64) {
        let t = &mut self.ets[et];
        let mut ids = Vec::with_capacity(units.len());
        let mut cost = 0.0;
        // Front of the queue: they were due before anything behind them.
        for u in units.iter().rev() {
            if !self.done[u.id] {
                t.ready.push_front(*u);
                t.ready_cost += u.cost;
                ids.push(u.id);
                cost += u.cost;
            }
        }
        ids.reverse();
        (ids, cost)
    }

    /// Reclaim every round older than its ET's timeout.
    pub fn check_timeouts(&mut self, now: f64) -> Vec<Reclaim> {
        let mut out = Vec::new();
        for node in 0..self.nodes.len() {
            let expired = match &self.nodes[node].in_flight {
                Some(f) => now - f.issued_at > self.timeout(f.et),
                None => false,
            };
            if expired {
                let f = self.nodes[node].in_flight.take().unwrap();
                let (units, cost) = self.put_back(f.et, &f.units);
                // No observation came back; treat it as a failed qualification.
                for c in self.nodes[node].pass_count.iter_mut() {
                    *c = 0;
                }
                out.push(Reclaim { node, et: f.et, units, cost });
            }
        }
        out
    }

    /// A node announced itself again. Whatever it still held is reissued now
    /// rather than at the timeout.
    pub fn node_reconnected(&mut self, node: NodeId) -> Option<Reclaim> {
        let f = self.nodes[node].in_flight.take()?;
        let (units, cost) = self.put_back(f.et, &f.units);
        Some(Reclaim { node, et: f.et, units, cost })
    }

    fn initial_eligibility(ets: &[Trench], lvl: u8) -> Vec<bool> {
        let mut v = vec![false; ets.len()];
        if let Some(i) = ets.iter().rposition(|e| e.tier_level <= lvl) {
            v[i] = true;
        }
        v
    }

    pub fn w(&self) -> f64 {
        self.params.w
    }

    pub fn retry_delay(&self) -> f64 {
        self.params.w * self.params.retry_fraction
    }

    pub fn estimate(&self, node: NodeId) -> &Estimate {
        &self.nodes[node].est
    }

    pub fn transitions(&self, node: NodeId) -> u64 {
        self.nodes[node].transitions
    }

    pub fn et_index(&self, name: &str) -> Option<usize> {
        self.ets.iter().position(|e| e.name == name)
    }

    pub fn ets(&self) -> &[Trench] {
        &self.ets
    }

    pub fn debug_state(&self, node: NodeId, now: f64) -> String {
        let ets: Vec<String> = self.ets.iter().enumerate().map(|(i, e)| format!("{}[{}]: ready {} cost {:.0} elig {}", e.name, i, e.ready.len(), e.ready_cost, self.nodes[node].eligible[i])).collect();
        let n = &self.nodes[node];
        format!("t={now:.3} node {node} r {:.1} samples {} ramp {} inflight {} :: {}", n.est.r, n.est.samples, n.est.ramp, n.in_flight.is_some(), ets.join(" | "))
    }

    pub fn is_eligible(&self, node: NodeId, et: usize) -> bool {
        self.nodes[node].eligible[et]
    }

    pub fn enqueue(&mut self, et: usize, id: UnitId, cost: f64, bytes: u64) {
        if self.done.len() <= id {
            self.done.resize(id + 1, false);
        }
        let t = &mut self.ets[et];
        t.ready.push_back(ReadyUnit { id, cost, bytes });
        t.ready_cost += cost;
    }

    /// Node `node` asks for work at time `now`. ETs are tried fastest first;
    /// the first one that yields a non-empty round wins. A zero share in one
    /// ET (remainder reserved for a faster node) falls through to the next.
    /// The round may be empty.
    pub fn pull(&mut self, node: NodeId, now: f64) -> Round {
        assert!(self.nodes[node].in_flight.is_none(), "node {node} pulled while a round is in flight");
        self.nodes[node].last_seen = now;
        let eligible_somewhere = self.nodes[node].eligible.iter().any(|&e| e);
        let lvl = self.nodes[node].tier_level;
        let candidates: Vec<(usize, bool)> = if eligible_somewhere {
            (0..self.ets.len())
                .filter(|&i| self.nodes[node].eligible[i] && self.ets[i].ready_cost > 0.0)
                .map(|i| (i, false))
                .collect()
        } else {
            // Qualifies nowhere: keep measuring it with a minimal probe from
            // the slowest ET its hardware allows, so it can be re-enabled
            // once its t recovers.
            (0..self.ets.len())
                .rev()
                .find(|&i| self.ets[i].tier_level <= lvl && self.ets[i].ready_cost > 0.0)
                .map(|i| vec![(i, true)])
                .unwrap_or_default()
        };
        for (et, probe) in candidates {
            let (quota_cost, kind, r_est) = self.quota(node, et, now, probe);
            let t = &mut self.ets[et];
            let mut taken: Vec<ReadyUnit> = Vec::new();
            let mut cost = 0.0;
            let mut bytes = 0;
            while let Some(u) = t.ready.front() {
                if cost + u.cost > quota_cost + 1e-9 {
                    break;
                }
                let u = t.ready.pop_front().unwrap();
                cost += u.cost;
                bytes += u.bytes;
                taken.push(u);
            }
            if taken.is_empty() {
                continue;
            }
            t.ready_cost -= cost;
            if t.ready_cost < 1e-9 {
                t.ready_cost = 0.0;
            }
            let units: Vec<UnitId> = taken.iter().map(|u| u.id).collect();
            self.nodes[node].in_flight = Some(InFlight { issued_at: now, cost, et, units: taken });
            return Round { units, cost, bytes, kind, r_est, et };
        }
        Round { units: Vec::new(), cost: 0.0, bytes: 0, kind: RoundKind::Normal, r_est: 0.0, et: 0 }
    }

    /// How much of ET `et` node `node` should take now: (cost, kind, r_est).
    fn quota(&self, node: NodeId, et: usize, now: f64, probe: bool) -> (f64, RoundKind, f64) {
        let entry = &self.nodes[node];
        let k = self.params.k;
        let w = self.ets[et].w;
        let ready_cost = self.ets[et].ready_cost;
        if entry.est.samples == 0 || probe {
            return (entry.c as f64, RoundKind::SlowStart, entry.est.r_safe(k));
        }
        let r_i = entry.est.r_safe(k);
        if entry.est.ramp {
            let q = (r_i * w).floor().max(entry.c as f64).min(ready_cost);
            return (q, RoundKind::SlowStart, r_i);
        }
        // Σ r over the nodes actually serving this ET right now: eligible,
        // with an estimate, alive (heard from recently or holding a round),
        // and either idle or running a round from this ET. A node busy in
        // another ET is not a server of this one (it only drops down when
        // its own ET is empty), and a silent node is probably gone; counting
        // either would shrink everyone else's share and leave the remainder
        // to a node that never comes. Same set for the "fastest node" rule.
        let silence = 2.0 * self.retry_delay();
        let mut sum_r = 0.0;
        let mut remaining_other = 0.0;
        // Fastest node among those idle right now: the one that should take
        // a remainder nobody's share covers. A busy node would only get to
        // it after its current round.
        let mut max_r_idle = 0.0_f64;
        for (j, n) in self.nodes.iter().enumerate() {
            if n.est.samples == 0 || !n.eligible[et] {
                continue;
            }
            let serving_here = match &n.in_flight {
                None => j == node || now - n.last_seen <= silence,
                Some(f) => f.et == et,
            };
            if !serving_here {
                continue;
            }
            let r_j = n.est.r_safe(k);
            sum_r += r_j;
            if n.in_flight.is_none() {
                max_r_idle = max_r_idle.max(r_j);
            }
            if j != node {
                if let Some(f) = &n.in_flight {
                    let done = r_j * (now - f.issued_at);
                    remaining_other += (f.cost - done).max(0.0);
                }
            }
        }
        if sum_r <= 0.0 || r_i <= 0.0 {
            return (entry.c as f64, RoundKind::Normal, r_i);
        }
        let d = (ready_cost + remaining_other) / sum_r;
        if d >= w {
            ((r_i * w).floor().max(entry.c as f64), RoundKind::Normal, r_i)
        } else {
            let mut q = (r_i * d).floor();
            // The share may be smaller than the next U itself (a large reduce
            // U, or the last crumb). The fastest idle node takes it whole
            // rather than leaving it for a share that never grows.
            let front = self.ets[et].ready.front().map(|u| u.cost).unwrap_or(0.0);
            if q < front && ready_cost > 0.0 && r_i >= max_r_idle {
                q = front.max(1.0);
            }
            (q, RoundKind::Taper, r_i)
        }
    }

    /// A round came back. Updates the node's estimate and its ET eligibility.
    /// Returns the units whose result is accepted (first result wins; a late
    /// round after a reclaim only counts for units not yet done).
    pub fn round_done(&mut self, obs: Observation, units: &[UnitId]) -> Vec<UnitId> {
        let w = self.params.w;
        let alpha = self.params.alpha;
        let k = self.params.k;
        let n_hyst = self.params.hysteresis_n.max(1);
        let mut accepted = Vec::with_capacity(units.len());
        for &u in units {
            if self.done[u] {
                self.duplicate_results += 1;
            } else {
                self.done[u] = true;
                accepted.push(u);
            }
        }
        // Release whatever these completions unlock.
        for &u in &accepted {
            for d in self.resolver.complete(u) {
                let (et, cost, bytes) = self.unit_info[d];
                self.enqueue(et, d, cost, bytes);
            }
        }
        let entry = &mut self.nodes[obs.node];
        entry.last_seen = obs.returned_at;
        let late = match &entry.in_flight {
            Some(f) => (f.issued_at - obs.issued_at).abs() > 1e-9,
            None => true,
        };
        if late {
            // Already reclaimed (or superseded): still a measurement, no round to close.
            self.late_results += 1;
        } else {
            entry.in_flight = None;
        }
        let duration = (obs.returned_at - obs.issued_at).max(1e-9);
        let r_round = obs.cost / duration;
        let b_round = obs.bytes as f64 / duration;
        // A round smaller than the node's concurrency does not even fill its
        // hardware: it measures the round-trip overhead, not capacity, so it
        // never touches the estimate. Larger rounds that are still much
        // shorter than W carry little information and are weighted by length.
        if obs.cost >= entry.c as f64 || entry.est.samples == 0 {
            let alpha_eff = alpha * (duration / w).min(1.0);
            entry.est.update(r_round, b_round, alpha_eff);
        }

        // Eligibility: t = c / r_safe against each ET's ceiling, hardware tier
        // as the hard condition. Promotion after n_hyst consecutive qualifying
        // rounds, demotion after one failing round.
        let r_safe = entry.est.r_safe(k);
        let t_est = if r_safe > 0.0 { entry.c as f64 / r_safe } else { f64::INFINITY };
        let band = self.params.hysteresis_band.clamp(0.0, 0.9);
        for (i, et) in self.ets.iter().enumerate() {
            let hw_ok = et.tier_level <= entry.tier_level;
            // Value hysteresis: a wider margin to get in than to stay in.
            let promote_ok = hw_ok && t_est <= et.t_max * (1.0 - band);
            let stay_ok = hw_ok && t_est <= et.t_max;
            if entry.eligible[i] {
                if !stay_ok {
                    entry.eligible[i] = false;
                    entry.pass_count[i] = 0;
                    entry.transitions += 1;
                }
            } else if promote_ok {
                entry.pass_count[i] += 1;
                if entry.pass_count[i] >= n_hyst {
                    entry.eligible[i] = true;
                    entry.transitions += 1;
                }
            } else {
                entry.pass_count[i] = 0;
            }
        }
        accepted
    }
}
