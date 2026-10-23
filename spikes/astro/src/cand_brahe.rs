//! `brahe` 1.8 behind the common [`Candidate`] interface. SI units inside the library; km at the boundary.

use brahe::eop::{EOPExtrapolation, FileEOPProvider, set_global_eop_provider};
use brahe::frames::{CelestialFrame, state_gcrf_to_itrf, state_teme_to_gcrf};
use brahe::orbit_dynamics::sun_position;
use brahe::propagators::traits::{DStatePropagator, SStatePropagator};
use brahe::propagators::{
    AtmosphericModel, DNumericalOrbitPropagator, DragConfiguration, ForceModelConfig,
    GravityConfiguration, NumericalPropagationConfig, ParameterSource, SGPPropagator,
    ZonalHarmonicsDegree,
};
use brahe::time::{Epoch, TimeSystem};
use brahe::types::GPRecord;
use brahe::utils::{DStateProvider, SOrbitStateProvider};
use nalgebra::{DVector, SVector};

use crate::candidate::{
    CResult, Candidate, CandidateError, NumericalSetup, Sgp4Outcome, TimeOffsets,
};
use crate::geom::State;
use crate::timeutil::Utc;

const M_PER_KM: f64 = 1000.0;
const NS_PER_S: f64 = 1.0e9;
/// Step size the SGP propagator uses for its stored trajectory; irrelevant to point queries.
const SGP_STEP_S: f64 = 60.0;

pub struct Brahe;

fn epoch(t: Utc) -> Epoch {
    let (y, mo, d, h, mi, s) = t.to_ymd_hms();
    let whole = s.floor();
    Epoch::from_datetime(
        y as u32,
        mo as u8,
        d as u8,
        h as u8,
        mi as u8,
        whole,
        (s - whole) * NS_PER_S,
        TimeSystem::UTC,
    )
}

fn sv(s: &State) -> SVector<f64, 6> {
    SVector::<f64, 6>::new(
        s.r.x * M_PER_KM,
        s.r.y * M_PER_KM,
        s.r.z * M_PER_KM,
        s.v.x * M_PER_KM,
        s.v.y * M_PER_KM,
        s.v.z * M_PER_KM,
    )
}

fn from_sv(x: &[f64]) -> State {
    State::new(
        [x[0] / M_PER_KM, x[1] / M_PER_KM, x[2] / M_PER_KM],
        [x[3] / M_PER_KM, x[4] / M_PER_KM, x[5] / M_PER_KM],
    )
}

impl Brahe {
    /// Install the EOP table from a file. `Hold` rather than `Error` for out-of-range queries:
    /// the `Error` mode panics inside the library instead of returning a `Result`.
    pub fn init(eop_path: &std::path::Path) -> Result<(), CandidateError> {
        let provider = FileEOPProvider::from_file(eop_path, true, EOPExtrapolation::Hold)
            .map_err(CandidateError::failed)?;
        set_global_eop_provider(provider);
        Ok(())
    }
}

impl Candidate for Brahe {
    fn name(&self) -> String {
        "brahe 1.8.0".into()
    }

    fn sgp4_omm(&mut self, omm_json: &str, times: &[Utc]) -> CResult<Vec<State>> {
        let records: Vec<GPRecord> =
            serde_json::from_str(omm_json).map_err(CandidateError::failed)?;
        let rec = records
            .first()
            .ok_or_else(|| CandidateError::Failed("OMM JSON contains no records".into()))?;
        let prop =
            SGPPropagator::from_gp_record(rec, SGP_STEP_S).map_err(CandidateError::failed)?;
        times
            .iter()
            .map(|&t| {
                prop.state_in_frame(CelestialFrame::TEME, epoch(t))
                    .map(|x| from_sv(x.as_slice()))
                    .map_err(CandidateError::failed)
            })
            .collect()
    }

    fn sgp4_tle_afspc(
        &mut self,
        line1: &str,
        line2: &str,
        tsince_min: &[f64],
    ) -> CResult<Vec<Sgp4Outcome>> {
        // brahe wraps the `sgp4` crate with `Constants::from_elements`, i.e. NOT AFSPC compatibility
        // mode, and offers no switch. Results can differ only for Lyddane-choice deep-space cases.
        let prop =
            SGPPropagator::from_tle(line1, line2, SGP_STEP_S).map_err(CandidateError::failed)?;
        let t0 = prop.initial_epoch();
        Ok(tsince_min
            .iter()
            .map(|&m| {
                prop.state_in_frame(CelestialFrame::TEME, t0 + m * 60.0)
                    .map(|x| from_sv(x.as_slice()))
                    .map_err(|e| e.to_string())
            })
            .collect())
    }

    fn teme_to_gcrf(&self, t: Utc, s: &State) -> CResult<State> {
        Ok(from_sv(state_teme_to_gcrf(epoch(t), sv(s)).as_slice()))
    }

    fn gcrf_to_itrf(&self, t: Utc, s: &State) -> CResult<State> {
        Ok(from_sv(state_gcrf_to_itrf(epoch(t), sv(s)).as_slice()))
    }

    fn sun_gcrf_km(&self, t: Utc) -> CResult<[f64; 3]> {
        let p = sun_position(epoch(t));
        Ok([p.x / M_PER_KM, p.y / M_PER_KM, p.z / M_PER_KM])
    }

    fn propagate(
        &mut self,
        t0: Utc,
        s0: &State,
        outputs: &[Utc],
        setup: &NumericalSetup,
    ) -> CResult<Vec<State>> {
        if setup.high_fidelity {
            return Err(CandidateError::Unsupported(
                "high-fidelity variant is not wired up for brahe in this spike".into(),
            ));
        }
        let degree = match setup.zonal_degree {
            2 => ZonalHarmonicsDegree::J2,
            3 => ZonalHarmonicsDegree::J3,
            4 => ZonalHarmonicsDegree::J4,
            n => return Err(CandidateError::Unsupported(format!("zonal degree {n}"))),
        };
        // Drag needs mass, area and Cd separately; only their ratio is known here. Use unit mass and
        // Cd = 1 so that area = Cd*A/m numerically, with the density scale folded in.
        let drag = setup.drag_cd_a_over_m.map(|b| DragConfiguration {
            model: AtmosphericModel::HarrisPriester,
            area: ParameterSource::Value(b * setup.density_scale),
            cd: ParameterSource::Value(1.0),
            body: None,
        });
        let force = ForceModelConfig {
            gravity: GravityConfiguration::EarthZonal { degree },
            drag,
            srp: None,
            third_body: None,
            relativity: false,
            mass: setup.drag_cd_a_over_m.map(|_| ParameterSource::Value(1.0)),
            ..Default::default()
        };
        let state = DVector::from_vec(vec![
            s0.r.x * M_PER_KM,
            s0.r.y * M_PER_KM,
            s0.r.z * M_PER_KM,
            s0.v.x * M_PER_KM,
            s0.v.y * M_PER_KM,
            s0.v.z * M_PER_KM,
        ]);
        let mut prop = DNumericalOrbitPropagator::new(
            epoch(t0),
            state,
            NumericalPropagationConfig::default(),
            force,
            None,
            None,
            None,
            None,
        )
        .map_err(CandidateError::failed)?;
        outputs
            .iter()
            .map(|&t| {
                let e = epoch(t);
                prop.propagate_to(e).map_err(CandidateError::failed)?;
                let x = prop.state(e).map_err(CandidateError::failed)?;
                Ok(from_sv(x.as_slice()))
            })
            .collect()
    }

    fn time_offsets(&self, t: Utc) -> CResult<TimeOffsets> {
        let e = epoch(t);
        let off = |sys| e.mjd_as_time_system(sys) - e.mjd_as_time_system(TimeSystem::UTC);
        Ok(TimeOffsets {
            tai_minus_utc: off(TimeSystem::TAI) * 86_400.0,
            tt_minus_utc: off(TimeSystem::TT) * 86_400.0,
            ut1_minus_utc: Some(off(TimeSystem::UT1) * 86_400.0),
            ut1_note: None,
        })
    }
}
