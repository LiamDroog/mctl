//! A trajectory source the engine can evaluate at any time inside its validity span.

use mctl_core::state::State;
use mctl_core::time::Utc;
use thiserror::Error;

use crate::oem::Oem;
use crate::sgp4model::GpModel;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum TrajectoryError {
    #[error("{0}")]
    Failed(String),
    #[error("outside validity span")]
    OutOfSpan,
    #[error("decayed (radius {0:.0} km is inside the Earth)")]
    Decayed(f64),
}

pub trait Trajectory: Send + Sync {
    /// Inertial (GCRF/EME2000) state at `t`.
    fn state_gcrf(&self, t: Utc) -> Result<State, TrajectoryError>;
    /// Span over which the source claims validity, if bounded.
    fn validity(&self) -> Option<(Utc, Utc)>;
}

impl Trajectory for Oem {
    fn state_gcrf(&self, t: Utc) -> Result<State, TrajectoryError> {
        self.interpolate(t).map_err(|e| match e {
            crate::oem::OemError::OutOfSpan { .. } => TrajectoryError::OutOfSpan,
            other => TrajectoryError::Failed(other.to_string()),
        })
    }

    fn validity(&self) -> Option<(Utc, Utc)> {
        Some(self.span())
    }
}

impl Trajectory for GpModel {
    fn state_gcrf(&self, t: Utc) -> Result<State, TrajectoryError> {
        let teme = self.state_teme(t)?;
        // TEME -> GCRF depends on TT only; UT1 enters only the Earth-rotation angle, which is not used here.
        let red = crate::frames::Reduction::at(t, crate::frames::Orientation::ASSUMED_ZERO)
            .map_err(|e| TrajectoryError::Failed(e.to_string()))?;
        Ok(red.teme_to_gcrf(&teme))
    }

    fn validity(&self) -> Option<(Utc, Utc)> {
        None
    }
}
