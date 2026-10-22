//! Scheduling logic shared by the simulator (`sim/`) and the scheduler
//! service. Nothing here knows about time sources, networks or node ground
//! truth: the caller feeds in `Observation`s and asks for `Round`s.

pub mod metrics;
pub mod scheduler;
pub mod types;

pub use types::{EtSpec, NodeId, SchedulerParams, UnitId};
