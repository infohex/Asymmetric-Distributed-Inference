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
use std::cmp::Ordering;
use std::collections::BinaryHeap;

enum Event {
    Pull(NodeId),
    RoundDone {
        node: NodeId,
        issued_at: f64,
        units: Vec<UnitId>,
        cost: f64,
        bytes: u64,
        kind: RoundKind,
        r_est: f64,
        r_true_eff: f64,
    },
    Arrival(EventId),
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
}

impl Simulation {
    pub fn new(sc: &Scenario) -> Simulation {
        let nodes: Vec<TrueNode> =
            sc.nodes.iter().enumerate().map(|(i, n)| TrueNode::from_spec(i, n)).collect();
        let conc: Vec<u64> = nodes.iter().map(|n| n.c).collect();
        let scheduler = Scheduler::new(sc.scheduler.clone(), &conc);
        let workload = Workload::generate(&sc.workload);
        let n_events = workload.events.len();
        let n_nodes = nodes.len();
        let mut metrics = Metrics::default();
        metrics.total_units = workload.units.len() as u64;
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
        }
    }

    fn schedule(&mut self, at: f64, event: Event) {
        self.seq += 1;
        self.queue.push(Scheduled { time: at, seq: self.seq, event });
    }

    pub fn run(mut self) -> (Metrics, Vec<(String, f64, f64)>) {
        // Initial arrivals.
        match self.workload.arrival {
            Arrival::AllAtOnce => {
                for e in 0..self.workload.events.len() {
                    self.schedule(0.0, Event::Arrival(e));
                }
            }
            Arrival::Sequential => self.schedule(0.0, Event::Arrival(0)),
        }
        // Every node asks for work at t = 0 (after arrivals, by seq order).
        for n in 0..self.nodes.len() {
            self.schedule(0.0, Event::Pull(n));
        }

        while let Some(Scheduled { time, event, .. }) = self.queue.pop() {
            if time > self.sim_end {
                eprintln!("warning: sim_end reached with work outstanding");
                break;
            }
            self.now = time;
            match event {
                Event::Arrival(e) => self.on_arrival(e),
                Event::Pull(n) => self.on_pull(n),
                Event::RoundDone { node, issued_at, units, cost, bytes, kind, r_est, r_true_eff } => {
                    self.on_round_done(node, issued_at, units, cost, bytes, kind, r_est, r_true_eff)
                }
            }
            if self.workload.all_complete() {
                break;
            }
        }

        self.metrics.sim_time = self.now;
        self.metrics.busy =
            self.nodes.iter().zip(&self.busy).map(|(n, b)| (n.name.clone(), *b)).collect();
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
        for id in ids {
            let u = &self.workload.units[id];
            self.scheduler.enqueue(u.id, u.cost, u.bytes);
        }
    }

    fn on_pull(&mut self, node: NodeId) {
        if !self.nodes[node].online {
            return;
        }
        self.metrics.pulls += 1;
        let round = self.scheduler.pull(node, self.now);
        if round.units.is_empty() {
            self.metrics.empty_pulls += 1;
            for (i, ev) in self.workload.events.iter().enumerate() {
                if ev.submitted_at.is_some() && !ev.is_complete() && self.first_starve[i].is_none() {
                    self.first_starve[i] = Some(self.now);
                }
            }
            let retry = self.now + self.scheduler.retry_delay();
            self.schedule(retry, Event::Pull(node));
            return;
        }
        let (duration, r_true_eff) =
            self.nodes[node].round_duration(round.cost, round.bytes, &mut self.rng);
        self.busy[node] += duration;
        let done_at = self.now + duration;
        self.schedule(
            done_at,
            Event::RoundDone {
                node,
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
        self.scheduler.round_done(Observation {
            node,
            issued_at,
            returned_at: self.now,
            cost,
            bytes,
        });
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
        for id in units {
            let e = self.workload.units[id].event;
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
            });
            if self.workload.arrival == Arrival::Sequential && e + 1 < self.workload.events.len() {
                self.schedule(self.now, Event::Arrival(e + 1));
            }
        }
        // Pull again right away; the Arrival above (same time, lower seq) lands first.
        self.schedule(self.now, Event::Pull(node));
    }
}
