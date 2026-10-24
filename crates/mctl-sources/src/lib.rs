//! Data-source adapters: HTTP cache, supervised fetch tasks (CelesTrak GP, NASA OEM, IERS EOP) and
//! the ISSLIVE Lightstreamer client. Observer geolocation and Horizons arrive with their milestones.

pub mod cache;
pub mod geoclue;
pub mod isslive;
pub mod observer;
pub mod schedules;
pub mod sky;
pub mod tasks;
