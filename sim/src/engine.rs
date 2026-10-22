//! Discrete-event engine. Owns the clock, the nodes' ground truth, the
//! scheduler and the workload, and is the only place where truth and
//! observation meet.

use crate::metrics::{EventRecord, Metrics, RoundRecord};
use crate::node::{NodeId, TrueNode};
use crate::scenario::Scenario;
use crate::scheduler::{Observation, RoundKind, Scheduler};
use crate::workload::{Arrival, EventId, UnitId, Workload};
use rand::rngs::StdRng;
use rand::SeedableRng;
use rand_distr::{Distribution, Exp};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

enum Event {
    Pull(NodeId),
    /// The periodic fallback pull of a parked node.
    Retry(NodeId),
    RoundDone {
        node: NodeId,
        epoch: u64,
        issued_at: f64,
        units: Vec<UnitId>,
        cost: f64,
        bytes: u64,
        kind: RoundKind,
        r_est: f64,
        r_true_eff: f64,
    },
    Arrival(EventId),
    NodeOffline(NodeId),
    NodeOnline(NodeId),
    TimeoutCheck,
}

struct Scheduled {
    time: f64,
    seq: u64,
    event: Event,
}

impl PartialEq for Scheduled {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.seq == other.seq
    }
}
impl Eq for Scheduled {}
impl PartialOrd for Scheduled {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Scheduled {
    // Reversed so the BinaryHeap pops the earliest time, then lowest seq.
    fn cmp(&self, other: &Self) -> Ordering {
        other.time.total_cmp(&self.time).then_with(|| other.seq.cmp(&self.seq))
    }
}

pub struct Simulation {
    now: f64,
    seq: u64,
    queue: BinaryHeap<Scheduled>,
    nodes: Vec<TrueNode>,
    scheduler: Scheduler,
    workload: Workload,
    rng: StdRng,
    metrics: Metrics,
    first_starve: Vec<Option<f64>>,
    busy: Vec<f64>,
    sim_end: f64,
    /// ET index per Event.
    event_et: Vec<usize>,
    /// Per Event: time of the last reclaim that touched it, the Event's
    /// remaining cost at that moment (reclaimed units included), and Σ r_true
    /// of the nodes online then (V4 bound).
    last_reclaim: Vec<Option<(f64, f64, f64)>>,
    /// Per Event: cost of units whose result has been accepted.
    done_cost: Vec<f64>,
    unit_done: Vec<bool>,
    /// Rounds issued to a node whose tier is below the ET's tier (must stay 0).
    tier_violations: u64,
    /// Nodes parked on an empty pull (long-poll): woken as soon as work appears.
    parked: Vec<bool>,
    /// A retry Pull is already scheduled for the node (at most one outstanding).
    retry_pending: Vec<bool>,
    reclaims_per_event: Vec<u32>,
    churn_events: u64,
}

impl Simulation {
    pub fn new(sc: &Scenario) -> Simulation {
        let nodes: Vec<TrueNode> =
            sc.nodes.iter().enumerate().map(|(i, n)| TrueNode::from_spec(i, n)).collect();
        let conc: Vec<(u64, String)> = nodes.iter().map(|n| (n.c, n.tier.clone())).collect();
        let scheduler = Scheduler::new(sc.scheduler.clone(), &conc, &sc.ets);
        let workload = Workload::generate(&sc.workloads);
        let event_et: Vec<usize> = workload
            .events
            .iter()
            .map(|e| e.et.as_deref().and_then(|n| scheduler.et_index(n)).unwrap_or(0))
            .collect();
        let n_events = workload.events.len();
        let n_units = workload.units.len();
        let n_nodes = nodes.len();
        let mut metrics = Metrics::default();
        metrics.total_units = workload.units.len() as u64;
        metrics.total_cost = workload.units.iter().map(|u| u.cost).sum();
        metrics.sum_r_true = nodes.iter().map(|n| n.r_true).sum();
        Simulation {
            now: 0.0,
            seq: 0,
            queue: BinaryHeap::new(),
            nodes,
            scheduler,
            workload,
            rng: StdRng::seed_from_u64(sc.seed),
            metrics,
            first_starve: vec![None; n_events],
            busy: vec![0.0; n_nodes],
            sim_end: sc.sim_end,
            event_et,
            last_reclaim: vec![None; n_events],
            done_cost: vec![0.0; n_events],
            unit_done: vec![false; n_units],
            tier_violations: 0,
            parked: vec![false; n_nodes],
            retry_pending: vec![false; n_nodes],
            reclaims_per_event: vec![0; n_events],
            churn_events: 0,
        }
    }

    fn schedule(&mut self, at: f64, event: Event) {
        self.seq += 1;
        self.queue.push(Scheduled { time: at, seq: self.seq, event });
    }

    pub fn run(mut self) -> (Metrics, Vec<(String, f64, f64)>) {
        // Initial arrivals: every Event of an all-at-once chain, the first of a sequential one.
        let mut first_seen = vec![false; self.workload.arrivals.len()];
        for e in 0..self.workload.events.len() {
            let w = self.workload.events[e].workload;
            match self.workload.arrivals[w] {
                Arrival::AllAtOnce => self.schedule(0.0, Event::Arrival(e)),
                Arrival::Sequential => {
                    if !first_seen[w] {
                        first_seen[w] = true;
                        self.schedule(0.0, Event::Arrival(e));
                    }
                }
            }
        }
        // Every node asks for work at t = 0 (after arrivals, by seq order).
        for n in 0..self.nodes.len() {
            self.schedule(0.0, Event::Pull(n));
            if self.nodes[n].mean_online > 0.0 && self.nodes[n].mean_offline > 0.0 {
                let d = self.exp(self.nodes[n].mean_online);
                self.schedule(d, Event::NodeOffline(n));
            }
        }
        let tick = self.scheduler.retry_delay();
        self.schedule(tick, Event::TimeoutCheck);

        while let Some(Scheduled { time, event, .. }) = self.queue.pop() {
            if time > self.sim_end {
                eprintln!("warning: sim_end reached with work outstanding");
                break;
            }
            self.now = time;
            match event {
                Event::Arrival(e) => self.on_arrival(e),
                Event::Pull(n) => self.on_pull(n),
                Event::Retry(n) => {
                    self.retry_pending[n] = false;
                    self.on_pull(n)
                }
                Event::RoundDone { node, epoch, issued_at, units, cost, bytes, kind, r_est, r_true_eff } => {
                    if self.nodes[node].online && self.nodes[node].epoch == epoch {
                        self.on_round_done(node, issued_at, units, cost, bytes, kind, r_est, r_true_eff)
                    }
                    // else: the node dropped while this round was in flight; the result is lost.
                }
                Event::NodeOffline(n) => self.on_offline(n),
                Event::NodeOnline(n) => self.on_online(n),
                Event::TimeoutCheck => self.on_timeout_check(),
            }
            if self.workload.all_complete() {
                break;
            }
        }

        self.metrics.sim_time = self.now;
        self.metrics.busy =
            self.nodes.iter().zip(&self.busy).map(|(n, b)| (n.name.clone(), *b)).collect();
        self.metrics.transitions = self.nodes.iter().map(|n| self.scheduler.transitions(n.id)).sum();
        self.metrics.duplicate_results = self.scheduler.duplicate_results;
        self.metrics.late_results = self.scheduler.late_results;
        self.metrics.churn_events = self.churn_events;
        self.metrics.tier_violations = self.tier_violations;
        self.metrics.waiting_at_end = self.scheduler.waiting_units();
        let truth = self
            .nodes
            .iter()
            .map(|n| (n.name.clone(), n.r_true, self.scheduler.estimate(n.id).r))
            .collect();
        (self.metrics, truth)
    }

    fn on_arrival(&mut self, e: EventId) {
        let ev = &mut self.workload.events[e];
        ev.submitted_at = Some(self.now);
        let ids = ev.units.clone();
        let et = self.event_et[e];
        for id in ids {
            let u = &self.workload.units[id];
            let deps = u.deps.clone();
            self.scheduler.submit(et, u.id, u.cost, u.bytes, &deps);
        }
        self.wake_parked();
    }

    /// Work appeared: for each ET that now has work, the fastest parked node
    /// eligible for it gets its long-poll answered. Others stay parked and
    /// are woken by the next change or their retry tick.
    fn wake_parked(&mut self) {
        let n_et = self.scheduler.ets().len();
        for et in 0..n_et {
            if self.scheduler.ets()[et].ready_cost() <= 0.0 {
                continue;
            }
            let best = (0..self.nodes.len())
                .filter(|&n| self.parked[n] && self.nodes[n].online && self.scheduler.is_eligible(n, et))
                .max_by(|&a, &b| self.scheduler.estimate(a).r.total_cmp(&self.scheduler.estimate(b).r));
            if let Some(n) = best {
                self.parked[n] = false;
                self.schedule(self.now, Event::Pull(n));
            }
        }
    }

    /// True while some unit of the Event still waits on a prerequisite.
    fn event_has_waiting(&self, e: EventId) -> bool {
        let ev = &self.workload.events[e];
        ev.units.iter().any(|&u| {
            let unit = &self.workload.units[u];
            !unit.deps.is_empty() && unit.deps.iter().any(|&d| !self.unit_done[d])
        })
    }

    fn exp(&mut self, mean: f64) -> f64 {
        Exp::new(1.0 / mean).expect("positive mean").sample(&mut self.rng)
    }

    fn sum_r_true_online(&self) -> f64 {
        self.nodes.iter().filter(|n| n.online).map(|n| n.r_true).sum()
    }

    fn note_reclaim(&mut self, units: &[UnitId], cost: f64) {
        self.wake_parked();
        if std::env::var_os("SIM_TRACE").is_some() {
            eprintln!("{:10.3} reclaim {} units (cost {cost})", self.now, units.len());
        }
        self.metrics.reissued_units += units.len() as u64;
        let sum_r = self.sum_r_true_online();
        let mut touched: Vec<EventId> = units.iter().map(|&u| self.workload.units[u].event).collect();
        touched.sort_unstable();
        touched.dedup();
        let _ = cost;
        for e in touched {
            let total: f64 = self.workload.events[e].units.iter().map(|&u| self.workload.units[u].cost).sum();
            let remaining = (total - self.done_cost[e]).max(0.0);
            self.last_reclaim[e] = Some((self.now, remaining, sum_r));
            self.reclaims_per_event[e] += 1;
        }
    }

    fn on_offline(&mut self, node: NodeId) {
        if std::env::var_os("SIM_TRACE").is_some() {
            eprintln!("{:10.3} offline {} (in flight: {})", self.now, self.nodes[node].name, self.scheduler.has_in_flight(node));
        }
        self.nodes[node].online = false;
        self.nodes[node].epoch += 1;
        self.churn_events += 1;
        let d = self.exp(self.nodes[node].mean_offline);
        self.schedule(self.now + d, Event::NodeOnline(node));
    }

    fn on_online(&mut self, node: NodeId) {
        if std::env::var_os("SIM_TRACE").is_some() {
            eprintln!("{:10.3} online  {}", self.now, self.nodes[node].name);
        }
        self.nodes[node].online = true;
        if let Some(r) = self.scheduler.node_reconnected(node) {
            self.note_reclaim(&r.units, r.cost);
        }
        self.schedule(self.now, Event::Pull(node));
        let d = self.exp(self.nodes[node].mean_online);
        self.schedule(self.now + d, Event::NodeOffline(node));
    }

    fn on_timeout_check(&mut self) {
        let reclaimed = self.scheduler.check_timeouts(self.now);
        for r in reclaimed {
            self.note_reclaim(&r.units, r.cost);
        }
        let tick = self.scheduler.retry_delay();
        if !self.workload.all_complete() {
            self.schedule(self.now + tick, Event::TimeoutCheck);
        }
    }

    fn on_pull(&mut self, node: NodeId) {
        if !self.nodes[node].online || self.scheduler.has_in_flight(node) {
            return;
        }
        self.parked[node] = false;
        self.metrics.pulls += 1;
        let round = self.scheduler.pull(node, self.now);
        if round.units.is_empty() {
            self.metrics.empty_pulls += 1;
            if std::env::var_os("SIM_TRACE").is_some() && self.metrics.empty_pulls <= 12 {
                eprintln!("empty pull: {}", self.scheduler.debug_state(node, self.now));
            }
            // Starvation counts only for Events this node could have served,
            // and only once the Event has nothing left waiting on a
            // prerequisite: a chain's idle nodes are serialization, not tail.
            for (i, ev) in self.workload.events.iter().enumerate() {
                if ev.submitted_at.is_some()
                    && !ev.is_complete()
                    && self.first_starve[i].is_none()
                    && self.scheduler.is_eligible(node, self.event_et[i])
                    && !self.event_has_waiting(i)
                {
                    self.first_starve[i] = Some(self.now);
                }
            }
            self.parked[node] = true;
            if !self.retry_pending[node] {
                self.retry_pending[node] = true;
                let retry = self.now + self.scheduler.retry_delay();
                self.schedule(retry, Event::Retry(node));
            }
            return;
        }
        if self.scheduler.ets()[round.et].tier_level > crate::scheduler::tier_level(&self.nodes[node].tier) {
            self.tier_violations += 1;
        }
        let (duration, r_true_eff) =
            self.nodes[node].round_duration(round.cost, round.bytes, &mut self.rng);
        self.busy[node] += duration;
        let done_at = self.now + duration;
        let epoch = self.nodes[node].epoch;
        self.schedule(
            done_at,
            Event::RoundDone {
                node,
                epoch,
                issued_at: self.now,
                units: round.units,
                cost: round.cost,
                bytes: round.bytes,
                kind: round.kind,
                r_est: round.r_est,
                r_true_eff,
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn on_round_done(
        &mut self,
        node: NodeId,
        issued_at: f64,
        units: Vec<UnitId>,
        cost: f64,
        bytes: u64,
        kind: RoundKind,
        r_est: f64,
        r_true_eff: f64,
    ) {
        let accepted = self.scheduler.round_done(
            Observation { node, issued_at, returned_at: self.now, cost, bytes },
            &units,
        );
        self.metrics.rounds.push(RoundRecord {
            node: self.nodes[node].name.clone(),
            issued_at,
            returned_at: self.now,
            units: units.len(),
            cost,
            bytes,
            kind,
            r_est,
            r_true_eff,
        });

        let mut completed: Vec<EventId> = Vec::new();
        for id in accepted {
            self.unit_done[id] = true;
            let e = self.workload.units[id].event;
            self.done_cost[e] += self.workload.units[id].cost;
            let ev = &mut self.workload.events[e];
            ev.done += 1;
            if ev.is_complete() && ev.completed_at.is_none() {
                ev.completed_at = Some(self.now);
                completed.push(e);
            }
        }
        for e in completed {
            let ev = &self.workload.events[e];
            self.metrics.events.push(EventRecord {
                event: e,
                submitted_at: ev.submitted_at.unwrap_or(0.0),
                completed_at: self.now,
                first_starve_at: self.first_starve[e],
                reclaims: self.reclaims_per_event[e],
                last_reclaim: self.last_reclaim[e],
            });
            let chain = self.workload.events[e].workload;
            if self.workload.arrivals[chain] == Arrival::Sequential {
                if let Some(next) = self.workload.next_in_chain(e) {
                    self.schedule(self.now, Event::Arrival(next));
                }
            }
        }
        // Pull again right away; the Arrival above (same time, lower seq) lands first.
        self.schedule(self.now, Event::Pull(node));
        self.wake_parked();
    }
}
