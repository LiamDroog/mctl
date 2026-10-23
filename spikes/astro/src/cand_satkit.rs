//! `satkit` 0.24 behind the common [`Candidate`] interface. SI units inside the library; km at the boundary.

use satkit::frametransform::transform_state;
use satkit::omm::OMM;
use satkit::orbitprop::{PropSettings, SatProperties, SatPropertiesSimple, TideModel, propagate};
use satkit::sgp4::{GravConst, OpsMode, SGP4Error, sgp4, sgp4_full};
use satkit::{Duration, Frame, Instant, TLE, TimeScale, Vector3};

use crate::candidate::{
    CResult, Candidate, CandidateError, NumericalSetup, Sgp4Outcome, TimeOffsets,
};
use crate::geom::State;
use crate::timeutil::Utc;

/// Metres per kilometre.
const M_PER_KM: f64 = 1000.0;
const SECONDS_PER_DAY: f64 = 86_400.0;
/// Degree and order of the geopotential in the high-fidelity variant.
const HIGH_FIDELITY_DEGREE: u16 = 20;

pub struct Satkit;

fn instant(t: Utc) -> CResult<Instant> {
    let (y, mo, d, h, mi, s) = t.to_ymd_hms();
    Instant::from_datetime(y, mo as i32, d as i32, h as i32, mi as i32, s)
        .map_err(CandidateError::failed)
}

fn to_state(p: &Vector3, v: &Vector3) -> State {
    State::new(
        [p[0] / M_PER_KM, p[1] / M_PER_KM, p[2] / M_PER_KM],
        [v[0] / M_PER_KM, v[1] / M_PER_KM, v[2] / M_PER_KM],
    )
}

fn vec3(v: &nalgebra::Vector3<f64>) -> Vector3 {
    Vector3::from_array([v.x * M_PER_KM, v.y * M_PER_KM, v.z * M_PER_KM])
}

impl Satkit {
    /// Load the EOP table from a file so frame results are reproducible and no download is attempted.
    pub fn init_offline(eop_path: &std::path::Path) -> Result<(), CandidateError> {
        satkit::utils::set_offline(true);
        satkit::earth_orientation_params::init_from_path(eop_path).map_err(CandidateError::failed)
    }
}

impl Candidate for Satkit {
    fn name(&self) -> String {
        "satkit 0.24.1".into()
    }

    fn sgp4_omm(&mut self, omm_json: &str, times: &[Utc]) -> CResult<Vec<State>> {
        let mut omm = OMM::from_json_string(omm_json)
            .map_err(CandidateError::failed)?
            .into_iter()
            .next()
            .ok_or_else(|| CandidateError::Failed("OMM JSON contains no records".into()))?;
        let ts: Vec<Instant> = times.iter().map(|&t| instant(t)).collect::<CResult<_>>()?;
        let out = sgp4(&mut omm, &ts).map_err(CandidateError::failed)?;
        (0..ts.len())
            .map(|i| {
                if out.errcode[i] != SGP4Error::SGP4Success {
                    return Err(CandidateError::Failed(format!(
                        "SGP4 error at sample {i}: {}",
                        out.errcode[i]
                    )));
                }
                Ok(to_state(
                    &Vector3::from_slice(out.pos.col_slice(i)),
                    &Vector3::from_slice(out.vel.col_slice(i)),
                ))
            })
            .collect()
    }

    fn sgp4_tle_afspc(
        &mut self,
        line1: &str,
        line2: &str,
        tsince_min: &[f64],
    ) -> CResult<Vec<Sgp4Outcome>> {
        let mut tle = TLE::from_lines(&[line1.to_owned(), line2.to_owned()])
            .map_err(CandidateError::failed)?
            .into_iter()
            .next()
            .ok_or_else(|| CandidateError::Failed("no TLE parsed".into()))?;
        let ts: Vec<Instant> = tsince_min
            .iter()
            .map(|&m| tle.epoch + Duration::from_minutes(m))
            .collect();
        // One call per time so that an error at one time does not hide the others.
        ts.iter()
            .map(|t| {
                let out = sgp4_full(
                    &mut tle,
                    std::slice::from_ref(t),
                    GravConst::WGS72,
                    OpsMode::AFSPC,
                )
                .map_err(CandidateError::failed)?;
                Ok(if out.errcode[0] == SGP4Error::SGP4Success {
                    Ok(to_state(
                        &Vector3::from_slice(out.pos.col_slice(0)),
                        &Vector3::from_slice(out.vel.col_slice(0)),
                    ))
                } else {
                    Err(out.errcode[0].to_string())
                })
            })
            .collect()
    }

    fn teme_to_gcrf(&self, t: Utc, s: &State) -> CResult<State> {
        let tm = instant(t)?;
        let (p, v) = transform_state(Frame::TEME, Frame::GCRF, &tm, &vec3(&s.r), &vec3(&s.v))
            .map_err(CandidateError::failed)?;
        Ok(to_state(&p, &v))
    }

    fn gcrf_to_itrf(&self, t: Utc, s: &State) -> CResult<State> {
        let tm = instant(t)?;
        let (p, v) = transform_state(Frame::GCRF, Frame::ITRF, &tm, &vec3(&s.r), &vec3(&s.v))
            .map_err(CandidateError::failed)?;
        Ok(to_state(&p, &v))
    }

    fn sun_gcrf_km(&self, t: Utc) -> CResult<[f64; 3]> {
        let p = satkit::lpephem::sun::pos_gcrf(&instant(t)?);
        Ok([p[0] / M_PER_KM, p[1] / M_PER_KM, p[2] / M_PER_KM])
    }

    fn propagate(
        &mut self,
        t0: Utc,
        s0: &State,
        outputs: &[Utc],
        setup: &NumericalSetup,
    ) -> CResult<Vec<State>> {
        let last = *outputs
            .last()
            .ok_or_else(|| CandidateError::Failed("no output times".into()))?;
        let mut settings = PropSettings::default();
        if setup.high_fidelity {
            settings
                .set_gravity(HIGH_FIDELITY_DEGREE, HIGH_FIDELITY_DEGREE)
                .map_err(CandidateError::failed)?;
            settings.use_sun_gravity = true;
            settings.use_moon_gravity = true;
        } else {
            settings
                .set_gravity(setup.zonal_degree, 0)
                .map_err(CandidateError::failed)?;
            // Only zonal gravity and drag, to match the other candidates' force models.
            settings.use_sun_gravity = false;
            settings.use_moon_gravity = false;
        }
        settings.use_relativistic_correction = false;
        // The default solid-tide model needs Sun/Moon from the 100+ MB JPL ephemeris file, which satkit
        // downloads on first use; with downloads off that file is unavailable and propagation fails.
        settings.tide_model = TideModel::None;
        // Without this the density model would fetch space-weather data from the network.
        settings.use_spaceweather = false;
        let pv0 = satkit::mathtypes::Vector6::from_array([
            s0.r.x * M_PER_KM,
            s0.r.y * M_PER_KM,
            s0.r.z * M_PER_KM,
            s0.v.x * M_PER_KM,
            s0.v.y * M_PER_KM,
            s0.v.z * M_PER_KM,
        ]);
        // Density scaling is equivalent to scaling Cd*A/m, since drag acceleration is linear in both.
        let props = setup
            .drag_cd_a_over_m
            .map(|b| SatPropertiesSimple::new(b * setup.density_scale, 0.0));
        let (t_start, t_end) = (instant(t0)?, instant(last)?);
        let res = propagate(
            &pv0,
            &t_start,
            &t_end,
            &settings,
            props.as_ref().map(|p| p as &dyn SatProperties),
        )
        .map_err(CandidateError::failed)?;
        outputs
            .iter()
            .map(|&t| {
                let y = res.interp(&instant(t)?).map_err(CandidateError::failed)?;
                Ok(State::new(
                    [y[0] / M_PER_KM, y[1] / M_PER_KM, y[2] / M_PER_KM],
                    [y[3] / M_PER_KM, y[4] / M_PER_KM, y[5] / M_PER_KM],
                ))
            })
            .collect()
    }

    fn time_offsets(&self, t: Utc) -> CResult<TimeOffsets> {
        let tm = instant(t)?;
        let d = |scale| {
            (tm.as_mjd_with_scale(scale) - tm.as_mjd_with_scale(TimeScale::UTC)) * SECONDS_PER_DAY
        };
        Ok(TimeOffsets {
            tai_minus_utc: d(TimeScale::TAI),
            tt_minus_utc: d(TimeScale::TT),
            ut1_minus_utc: Some(d(TimeScale::UT1)),
            ut1_note: None,
        })
    }
}
