//! Time, state types, configuration, the published world snapshot, the bus and the arbiter.
//! No UI, network or async-runtime dependency, so it is unit-testable in isolation.

pub mod arbiter;
pub mod bus;
pub mod command;
pub mod config;
pub mod cw;
pub mod plan;
pub mod prox;
pub mod schedule;
pub mod state;
pub mod time;
pub mod world;
