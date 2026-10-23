//! The simulation engine: the 50 Hz propagation tick that turns source data into world snapshots.
//! The sim clock (warp, scrub, replay) and maneuver planner join this crate in later milestones;
//! today the engine runs in LIVE mode only.

pub mod engine;
