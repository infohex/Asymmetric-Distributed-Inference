//! Events (E) and Unities (U). Stage 1: fully parallel E, uniform U cost.

use crate::scenario::WorkloadSpec;

pub type UnitId = usize;
pub type EventId = usize;

#[derive(Debug, Clone)]
pub struct Unit {
    pub id: UnitId,
    pub event: EventId,
    /// Compute cost in relative units; 1.0 = one "standard" U.
    pub cost: f64,
    pub bytes: u64,
    /// Prerequisite units (ids within the same Event).
    pub deps: Vec<UnitId>,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: EventId,
    pub units: Vec<UnitId>,
    pub done: usize,
    pub submitted_at: Option<f64>,
    pub completed_at: Option<f64>,
    /// Which workload (arrival chain) this Event belongs to.
    pub workload: usize,
    /// Target ET name (None = first ET).
    pub et: Option<String>,
}

impl Event {
    pub fn is_complete(&self) -> bool {
        self.done == self.units.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arrival {
    AllAtOnce,
    Sequential,
}

pub struct Workload {
    pub units: Vec<Unit>,
    pub events: Vec<Event>,
    /// Arrival mode per workload chain.
    pub arrivals: Vec<Arrival>,
}

impl Workload {
    pub fn generate(specs: &[WorkloadSpec]) -> Workload {
        let mut units = Vec::new();
        let mut events = Vec::new();
        let mut arrivals = Vec::new();
        for (wi, spec) in specs.iter().enumerate() {
            for _ in 0..spec.events {
                let e = events.len();
                let mut ids = Vec::with_capacity(spec.units_per_event + 1);
                match spec.shape.as_str() {
                    "serial" => {
                        // A chain: each U depends on the previous one.
                        for i in 0..spec.units_per_event {
                            let id = units.len();
                            let deps = if i == 0 { Vec::new() } else { vec![id - 1] };
                            units.push(Unit { id, event: e, cost: 1.0, bytes: spec.unit_bytes, deps });
                            ids.push(id);
                        }
                    }
                    "fanout_reduce" => {
                        // N parallel U, then one reduce U that depends on all of them.
                        for _ in 0..spec.units_per_event {
                            let id = units.len();
                            units.push(Unit { id, event: e, cost: 1.0, bytes: spec.unit_bytes, deps: Vec::new() });
                            ids.push(id);
                        }
                        let id = units.len();
                        units.push(Unit { id, event: e, cost: spec.reduce_cost, bytes: spec.unit_bytes, deps: ids.clone() });
                        ids.push(id);
                    }
                    _ => {
                        for _ in 0..spec.units_per_event {
                            let id = units.len();
                            units.push(Unit { id, event: e, cost: 1.0, bytes: spec.unit_bytes, deps: Vec::new() });
                            ids.push(id);
                        }
                    }
                }
                events.push(Event {
                    id: e,
                    units: ids,
                    done: 0,
                    submitted_at: None,
                    completed_at: None,
                    workload: wi,
                    et: spec.et.clone(),
                });
            }
            arrivals.push(match spec.arrival.as_str() {
                "sequential" => Arrival::Sequential,
                _ => Arrival::AllAtOnce,
            });
        }
        Workload { units, events, arrivals }
    }

    /// The next Event in the same chain after `e`, if any.
    pub fn next_in_chain(&self, e: EventId) -> Option<EventId> {
        let w = self.events[e].workload;
        self.events[e + 1..].iter().find(|x| x.workload == w).map(|x| x.id)
    }

    pub fn all_complete(&self) -> bool {
        self.events.iter().all(|e| e.is_complete())
    }
}
