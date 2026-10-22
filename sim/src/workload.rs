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
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: EventId,
    pub units: Vec<UnitId>,
    pub done: usize,
    pub submitted_at: Option<f64>,
    pub completed_at: Option<f64>,
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
    pub arrival: Arrival,
}

impl Workload {
    pub fn generate(spec: &WorkloadSpec) -> Workload {
        let mut units = Vec::with_capacity(spec.events * spec.units_per_event);
        let mut events = Vec::with_capacity(spec.events);
        for e in 0..spec.events {
            let mut ids = Vec::with_capacity(spec.units_per_event);
            for _ in 0..spec.units_per_event {
                let id = units.len();
                units.push(Unit { id, event: e, cost: 1.0, bytes: spec.unit_bytes });
                ids.push(id);
            }
            events.push(Event { id: e, units: ids, done: 0, submitted_at: None, completed_at: None });
        }
        let arrival = match spec.arrival.as_str() {
            "sequential" => Arrival::Sequential,
            _ => Arrival::AllAtOnce,
        };
        Workload { units, events, arrival }
    }

    pub fn all_complete(&self) -> bool {
        self.events.iter().all(|e| e.is_complete())
    }
}
