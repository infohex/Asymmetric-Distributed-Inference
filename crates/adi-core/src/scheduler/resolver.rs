//! Dependency resolver: releases a Unity into its ET only when every
//! prerequisite has completed. Nodes never see a waiting U.

use crate::types::UnitId;
use std::collections::HashMap;

#[derive(Default)]
pub struct Resolver {
    /// Per unit: prerequisites not yet done.
    pending: HashMap<UnitId, usize>,
    /// Per unit: the units that depend on it.
    dependents: HashMap<UnitId, Vec<UnitId>>,
}

impl Resolver {
    /// Register a unit with its prerequisites. Returns true when it is ready now.
    pub fn register(&mut self, id: UnitId, deps: &[UnitId], done: &[bool]) -> bool {
        let mut remaining = 0;
        for &d in deps {
            if done.get(d).copied().unwrap_or(false) {
                continue;
            }
            remaining += 1;
            self.dependents.entry(d).or_default().push(id);
        }
        if remaining == 0 {
            true
        } else {
            self.pending.insert(id, remaining);
            false
        }
    }

    /// A unit completed: returns the units it unlocks (all prerequisites done).
    pub fn complete(&mut self, id: UnitId) -> Vec<UnitId> {
        let mut unlocked = Vec::new();
        if let Some(deps) = self.dependents.remove(&id) {
            for d in deps {
                if let Some(n) = self.pending.get_mut(&d) {
                    *n -= 1;
                    if *n == 0 {
                        self.pending.remove(&d);
                        unlocked.push(d);
                    }
                }
            }
        }
        unlocked
    }

    pub fn waiting(&self) -> usize {
        self.pending.len()
    }
}
