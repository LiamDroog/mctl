//! The simulation engine: the 50 Hz propagation tick that turns source data into world snapshots,
//! and the simulation clock (LIVE / SIM with warp, pause, step and scrub). The maneuver planner joins
//! this crate in M5.

pub mod clock;
pub mod engine;
