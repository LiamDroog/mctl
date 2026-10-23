//! The capability surface each library must offer for the evaluation, and the shared test inputs.
//!
//! A candidate that lacks a capability returns [`CandidateError::Unsupported`] with the reason;
//! that is a result, not a failure of the harness.

use thiserror::Error;

use crate::geom::State;
use crate::timeutil::Utc;

#[derive(Debug, Error)]
pub enum CandidateError {
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Failed(String),
}

impl CandidateError {
    pub fn failed(e: impl std::fmt::Display) -> Self {
        Self::Failed(e.to_string())
    }
}

pub type CResult<T> = Result<T, CandidateError>;

/// Force-model request for a numerical propagation, in library-neutral terms.
#[derive(Debug, Clone, Copy)]
pub struct NumericalSetup {
    /// Highest zonal harmonic (2 = J2 only, 4 = J2..J4).
    pub zonal_degree: u16,
    /// Atmospheric drag on, with this ballistic parameter Cd*A/m, m^2/kg.
    pub drag_cd_a_over_m: Option<f64>,
    /// Multiplier on the library's own atmospheric density (1.0 = as shipped). Used to tune drag.
    pub density_scale: f64,
    /// Richer force model: 20x20 geopotential plus Sun and Moon third-body gravity (when supported).
    /// `zonal_degree` is then ignored.
    pub high_fidelity: bool,
}

/// Time-scale offsets at one UTC instant, seconds.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TimeOffsets {
    pub tai_minus_utc: f64,
    pub tt_minus_utc: f64,
    /// `None` when UT1-UTC is unavailable at this instant and the library said so (rather than guessing).
    pub ut1_minus_utc: Option<f64>,
    /// Why UT1-UTC is missing, when it is.
    pub ut1_note: Option<String>,
}

/// What an SGP4 verification case returns at one time: a state, or the library's error text.
pub type Sgp4Outcome = Result<State, String>;

pub trait Candidate {
    fn name(&self) -> String;

    /// SGP4 from an OMM JSON record (CelesTrak GP, `FORMAT=JSON`), TEME state at each time.
    fn sgp4_omm(&mut self, omm_json: &str, times: &[Utc]) -> CResult<Vec<State>>;

    /// SGP4 from TLE lines in AFSPC compatibility mode with WGS-72, at minutes since epoch.
    /// This is the configuration the Vallado verification vectors were generated with.
    fn sgp4_tle_afspc(
        &mut self,
        line1: &str,
        line2: &str,
        tsince_min: &[f64],
    ) -> CResult<Vec<Sgp4Outcome>>;

    fn teme_to_gcrf(&self, t: Utc, s: &State) -> CResult<State>;
    fn gcrf_to_itrf(&self, t: Utc, s: &State) -> CResult<State>;

    /// Geocentric Sun position in GCRF/ICRF, kilometres.
    fn sun_gcrf_km(&self, t: Utc) -> CResult<[f64; 3]>;

    /// Numerical propagation of a GCRF state from `t0` to each of `outputs` (ascending, after `t0`).
    fn propagate(
        &mut self,
        t0: Utc,
        s0: &State,
        outputs: &[Utc],
        setup: &NumericalSetup,
    ) -> CResult<Vec<State>>;

    fn time_offsets(&self, t: Utc) -> CResult<TimeOffsets>;
}
