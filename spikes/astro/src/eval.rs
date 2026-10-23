//! The six checks, run identically against every candidate. Results are data (serialisable), so the
//! ADR quotes numbers produced by this code rather than hand-copied ones.

use std::time::Instant;

use nalgebra::Vector3;
use serde::Serialize;

use crate::candidate::{Candidate, CandidateError, NumericalSetup, TimeOffsets};
use crate::geom::{ResidualStats, State, residual_stats, rtn_residual};
use crate::oem::Oem;
use crate::timeutil::Utc;
use crate::vallado::Case;

/// Sample spacing for the 24 h comparisons, seconds.
pub const SAMPLE_STEP_S: f64 = 60.0;
/// Window length, seconds.
pub const WINDOW_S: f64 = 86_400.0;
/// Hourly output spacing for numerical-propagation residuals, seconds.
const NUMERICAL_STEP_S: f64 = 3_600.0;
/// Vallado comparison tolerances. The reference is printed to 8 decimals in km (1e-8 km = 0.01 mm), so
/// 1e-5 km (1 cm) allows for platform and algorithm-ordering differences without hiding real errors.
pub const VALLADO_POS_TOL_KM: f64 = 1.0e-5;
pub const VALLADO_VEL_TOL_KM_S: f64 = 1.0e-8;
/// Density-scale search bracket for "tuned drag".
const SCALE_LO: f64 = 0.05;
const SCALE_HI: f64 = 20.0;
const SCALE_ITERS: usize = 40;
const GOLDEN: f64 = 0.618_033_988_749_895;

fn grid(start: Utc, step_s: f64, span_s: f64) -> Vec<Utc> {
    (0..=((span_s / step_s).round() as usize))
        .map(|k| start.plus(k as f64 * step_s))
        .collect()
}

fn err_text(e: &CandidateError) -> String {
    e.to_string()
}

/// One row of results per candidate; `Err` text when the candidate could not do the check at all.
#[derive(Serialize)]
pub struct PerCandidate<T> {
    pub candidate: String,
    pub result: Result<T, String>,
}

fn per<T>(c: &dyn Candidate, f: impl FnOnce() -> Result<T, CandidateError>) -> PerCandidate<T> {
    PerCandidate {
        candidate: c.name(),
        result: f().map_err(|e| err_text(&e)),
    }
}

// ---- 1. SGP4 from OMM ---------------------------------------------------------------------------------

#[derive(Serialize)]
pub struct OmmAgreement {
    pub samples: usize,
    pub max_position_diff_vs_reference_m: f64,
    pub max_velocity_diff_vs_reference_mm_s: f64,
    pub radius_range_km: (f64, f64),
}

pub struct OmmRun {
    pub times: Vec<Utc>,
    pub teme: Vec<Vec<State>>, // per candidate, aligned with `candidates`; empty when failed
}

pub fn t1_sgp4_omm(
    cands: &mut [&mut dyn Candidate],
    omm_json: &str,
    epoch: Utc,
) -> (Vec<PerCandidate<OmmAgreement>>, OmmRun) {
    let times = grid(epoch, SAMPLE_STEP_S, WINDOW_S);
    let mut runs: Vec<Result<Vec<State>, String>> = Vec::new();
    for c in cands.iter_mut() {
        let t0 = Instant::now();
        let r = c.sgp4_omm(omm_json, &times).map_err(|e| err_text(&e));
        tracing::info!(
            candidate = c.name(),
            ms = t0.elapsed().as_millis() as u64,
            ok = r.is_ok(),
            "T1 SGP4 from OMM"
        );
        runs.push(r);
    }
    let reference = runs.first().and_then(|r| r.as_ref().ok()).cloned();
    let rows = cands
        .iter()
        .zip(&runs)
        .map(|(c, r)| PerCandidate {
            candidate: c.name(),
            result: r.clone().and_then(|states| {
                let reference = reference
                    .as_ref()
                    .ok_or_else(|| "no reference run".to_owned())?;
                let max_r = states
                    .iter()
                    .zip(reference)
                    .map(|(a, b)| (a.r - b.r).norm() * 1000.0)
                    .fold(0.0, f64::max);
                let max_v = states
                    .iter()
                    .zip(reference)
                    .map(|(a, b)| (a.v - b.v).norm() * 1.0e6)
                    .fold(0.0, f64::max);
                let (lo, hi) = states
                    .iter()
                    .map(|s| s.r.norm())
                    .fold((f64::INFINITY, 0.0f64), |(l, h), r| (l.min(r), h.max(r)));
                Ok(OmmAgreement {
                    samples: states.len(),
                    max_position_diff_vs_reference_m: max_r,
                    max_velocity_diff_vs_reference_mm_s: max_v,
                    radius_range_km: (lo, hi),
                })
            }),
        })
        .collect();
    let teme = runs.into_iter().map(|r| r.unwrap_or_default()).collect();
    (rows, OmmRun { times, teme })
}

// ---- 2. SGP4 vs OEM, with each candidate's TEME -> GCRF ---------------------------------------------------

#[derive(Serialize)]
pub struct OemComparison {
    pub stats: ResidualStats,
    /// RTN residual (R, T, N) in metres at 0, 6, 12, 18, 24 h.
    pub rtn_m_at_6h: Vec<[f64; 3]>,
}

pub fn t2_sgp4_vs_oem(
    cands: &[&mut dyn Candidate],
    run: &OmmRun,
    oem: &Oem,
) -> Vec<PerCandidate<OemComparison>> {
    cands
        .iter()
        .enumerate()
        .map(|(i, c)| {
            per(&**c, || {
                let teme = &run.teme[i];
                if teme.is_empty() {
                    return Err(CandidateError::Failed("no SGP4 states from T1".into()));
                }
                let mut rtn = Vec::with_capacity(teme.len());
                for (t, s) in run.times.iter().zip(teme) {
                    let gcrf = c.teme_to_gcrf(*t, s)?;
                    let truth = oem.interpolate(*t).map_err(CandidateError::failed)?;
                    rtn.push(rtn_residual(&truth, &gcrf));
                }
                let every_6h = (6.0 * 3600.0 / SAMPLE_STEP_S) as usize;
                Ok(OemComparison {
                    stats: residual_stats(&rtn),
                    rtn_m_at_6h: rtn
                        .iter()
                        .step_by(every_6h)
                        .map(|v| [v[0] * 1000.0, v[1] * 1000.0, v[2] * 1000.0])
                        .collect(),
                })
            })
        })
        .collect()
}

// ---- 3. Vallado verification vectors ------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ValladoCaseResult {
    pub satnum: u32,
    pub rows_compared: usize,
    pub max_pos_err_km: Option<f64>,
    pub max_vel_err_km_s: Option<f64>,
    /// Library returned an error at a time where the reference has a state.
    pub unexpected_errors: usize,
    /// Reference stops (SGP4 error) before the schedule ends; did the library also report an error there?
    pub expected_error_detected: Option<bool>,
    /// Radius of the state the library returned where the reference reports an error, km (decay
    /// shows as a radius below the Earth's).
    pub radius_where_reference_errors_km: Option<f64>,
    /// The library refused the TLE as given (parse or initialisation).
    pub init_error: Option<String>,
    /// The TLE had to be repaired (checksum / blank ephemeris type) before this library accepted it.
    pub repaired: bool,
    /// Largest position difference from the first candidate on the same inputs, km. Shows whether two
    /// implementations agree with each other when both disagree with the reference.
    pub max_pos_diff_vs_first_km: Option<f64>,
}

#[derive(Serialize)]
pub struct ValladoSummary {
    pub cases: usize,
    pub cases_within_tolerance: usize,
    pub worst_pos_err_km: f64,
    pub worst_vel_err_km_s: f64,
    pub unexpected_errors: usize,
    pub missed_errors: usize,
    pub init_failures: usize,
    pub repaired_cases: usize,
    pub detail: Vec<ValladoCaseResult>,
}

type CaseOutputs = Vec<Option<Vec<crate::candidate::Sgp4Outcome>>>;

/// Run one case, retrying once with a repaired TLE if the library refuses the original.
fn run_case(
    c: &mut dyn Candidate,
    case: &Case,
    times: &[f64],
) -> Result<(Vec<crate::candidate::Sgp4Outcome>, bool), CandidateError> {
    match c.sgp4_tle_afspc(&case.line1, &case.line2, times) {
        Ok(out) => Ok((out, false)),
        Err(CandidateError::Unsupported(m)) => Err(CandidateError::Unsupported(m)),
        Err(first) => {
            let (l1, l2) = crate::vallado::repair_tle(&case.line1, &case.line2);
            if (l1.as_str(), l2.as_str()) == (case.line1.as_str(), case.line2.as_str()) {
                return Err(first);
            }
            c.sgp4_tle_afspc(&l1, &l2, times)
                .map(|out| (out, true))
                .map_err(|_| first)
        }
    }
}

pub fn t3_vallado(
    cands: &mut [&mut dyn Candidate],
    cases: &[Case],
) -> Vec<PerCandidate<ValladoSummary>> {
    let mut first_outputs: CaseOutputs = vec![None; cases.len()];
    let mut results = Vec::new();
    for (ci, c) in cands.iter_mut().enumerate() {
        let name = c.name();
        let result = (|| -> Result<ValladoSummary, CandidateError> {
            let mut detail = Vec::new();
            for (k, case) in cases.iter().enumerate() {
                let mut times: Vec<f64> = case.reference.iter().map(|r| r.tsince_min).collect();
                // The first scheduled time past the last reference row, where the reference had an SGP4 error.
                let last_ref = case
                    .reference
                    .iter()
                    .map(|r| r.tsince_min)
                    .fold(f64::NEG_INFINITY, f64::max);
                let first_missing = case
                    .scheduled_times()
                    .into_iter()
                    .find(|&t| t > last_ref + 1e-9);
                if let Some(t) = first_missing {
                    times.push(t);
                }
                let (out, repaired) = match run_case(&mut **c, case, &times) {
                    Ok(x) => x,
                    Err(CandidateError::Unsupported(m)) => {
                        return Err(CandidateError::Unsupported(m));
                    }
                    Err(_) if case.reference.is_empty() => {
                        // The reference itself reports an error for this case, so refusing it is correct.
                        detail.push(ValladoCaseResult {
                            satnum: case.satnum,
                            rows_compared: 0,
                            max_pos_err_km: Some(0.0),
                            max_vel_err_km_s: Some(0.0),
                            unexpected_errors: 0,
                            expected_error_detected: Some(true),
                            radius_where_reference_errors_km: None,
                            init_error: None,
                            repaired: false,
                            max_pos_diff_vs_first_km: None,
                        });
                        continue;
                    }
                    Err(e) => {
                        // One TLE the library refuses must not hide results on the other cases.
                        detail.push(ValladoCaseResult {
                            satnum: case.satnum,
                            rows_compared: 0,
                            max_pos_err_km: None,
                            max_vel_err_km_s: None,
                            unexpected_errors: case.reference.len(),
                            expected_error_detected: None,
                            radius_where_reference_errors_km: None,
                            init_error: Some(e.to_string()),
                            repaired: false,
                            max_pos_diff_vs_first_km: None,
                        });
                        continue;
                    }
                };
                let (mut max_p, mut max_v, mut unexpected) = (0.0f64, 0.0f64, 0usize);
                for (row, got) in case.reference.iter().zip(&out) {
                    match got {
                        Ok(s) => {
                            max_p = max_p.max((s.r - row.state.r).norm());
                            max_v = max_v.max((s.v - row.state.v).norm());
                        }
                        Err(_) => unexpected += 1,
                    }
                }
                let expected_error_detected =
                    first_missing.map(|_| out.last().is_some_and(|o| o.is_err()));
                let radius_where_reference_errors_km = match (first_missing, out.last()) {
                    (Some(_), Some(Ok(s))) => Some(s.r.norm()),
                    _ => None,
                };
                let vs_first = if ci == 0 {
                    None
                } else {
                    first_outputs[k].as_ref().map(|f| {
                        f.iter()
                            .zip(&out)
                            .filter_map(|(a, b)| {
                                Some((a.as_ref().ok()?.r - b.as_ref().ok()?.r).norm())
                            })
                            .fold(0.0, f64::max)
                    })
                };
                if ci == 0 {
                    first_outputs[k] = Some(out);
                }
                detail.push(ValladoCaseResult {
                    satnum: case.satnum,
                    rows_compared: case.reference.len(),
                    max_pos_err_km: Some(max_p),
                    max_vel_err_km_s: Some(max_v),
                    unexpected_errors: unexpected,
                    expected_error_detected,
                    radius_where_reference_errors_km,
                    init_error: None,
                    repaired,
                    max_pos_diff_vs_first_km: vs_first,
                });
            }
            let ok_cases = || detail.iter().filter(|d| d.init_error.is_none());
            Ok(ValladoSummary {
                cases: detail.len(),
                cases_within_tolerance: ok_cases()
                    .filter(|d| {
                        d.unexpected_errors == 0
                            && d.max_pos_err_km.is_some_and(|e| e <= VALLADO_POS_TOL_KM)
                            && d.max_vel_err_km_s
                                .is_some_and(|e| e <= VALLADO_VEL_TOL_KM_S)
                    })
                    .count(),
                init_failures: detail.iter().filter(|d| d.init_error.is_some()).count(),
                repaired_cases: detail.iter().filter(|d| d.repaired).count(),
                worst_pos_err_km: ok_cases()
                    .filter_map(|d| d.max_pos_err_km)
                    .fold(0.0, f64::max),
                worst_vel_err_km_s: ok_cases()
                    .filter_map(|d| d.max_vel_err_km_s)
                    .fold(0.0, f64::max),
                unexpected_errors: detail.iter().map(|d| d.unexpected_errors).sum(),
                missed_errors: detail
                    .iter()
                    .filter(|d| d.expected_error_detected == Some(false))
                    .count(),
                detail,
            })
        })();
        results.push(PerCandidate {
            candidate: name,
            result: result.map_err(|e| err_text(&e)),
        });
    }
    results
}

// ---- 4. Frames: candidates against each other --------------------------------------------------------------

#[derive(Serialize)]
pub struct FrameAgreement {
    pub against: String,
    pub teme_to_gcrf_max_m: f64,
    pub gcrf_to_itrf_max_m: f64,
}

/// Compare every candidate's TEME->GCRF and GCRF->ITRF with the reference candidate's, on the same TEME states.
pub fn t4_frames(
    cands: &[&mut dyn Candidate],
    run: &OmmRun,
    reference_idx: usize,
) -> Vec<PerCandidate<FrameAgreement>> {
    let reference = &cands[reference_idx];
    cands
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != reference_idx)
        .map(|(_, c)| {
            per(&**c, || {
                let teme = run
                    .teme
                    .iter()
                    .find(|t| !t.is_empty())
                    .ok_or_else(|| CandidateError::Failed("no TEME states".into()))?;
                let (mut gcrf_max, mut itrf_max) = (0.0f64, 0.0f64);
                for (t, s) in run.times.iter().zip(teme).step_by(10) {
                    let (ga, gb) = (reference.teme_to_gcrf(*t, s)?, c.teme_to_gcrf(*t, s)?);
                    gcrf_max = gcrf_max.max((ga.r - gb.r).norm() * 1000.0);
                    let (ia, ib) = (reference.gcrf_to_itrf(*t, &ga)?, c.gcrf_to_itrf(*t, &ga)?);
                    itrf_max = itrf_max.max((ia.r - ib.r).norm() * 1000.0);
                }
                Ok(FrameAgreement {
                    against: reference.name(),
                    teme_to_gcrf_max_m: gcrf_max,
                    gcrf_to_itrf_max_m: itrf_max,
                })
            })
        })
        .collect()
}

// ---- 5. Sun against JPL Horizons -----------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SunError {
    pub epochs: usize,
    pub max_arcsec: f64,
    pub mean_arcsec: f64,
    pub max_distance_error_km: f64,
}

pub fn t5_sun(
    cands: &[&mut dyn Candidate],
    horizons: &[(f64, [f64; 3])],
) -> Vec<PerCandidate<SunError>> {
    cands
        .iter()
        .map(|c| {
            per(&**c, || {
                let (mut max_a, mut sum_a, mut max_d) = (0.0f64, 0.0, 0.0f64);
                for (jd, truth) in horizons {
                    // Horizons gives Julian dates in UT; at this precision UT1~UTC for choosing the instant.
                    let t = Utc((jd - 2_440_587.5) * 86_400.0);
                    let (mine, truth) = (Vector3::from(c.sun_gcrf_km(t)?), Vector3::from(*truth));
                    let ang = mine.angle(&truth).to_degrees() * 3600.0;
                    max_a = max_a.max(ang);
                    sum_a += ang;
                    max_d = max_d.max((mine.norm() - truth.norm()).abs());
                }
                Ok(SunError {
                    epochs: horizons.len(),
                    max_arcsec: max_a,
                    mean_arcsec: sum_a / horizons.len() as f64,
                    max_distance_error_km: max_d,
                })
            })
        })
        .collect()
}

// ---- 6. Numerical propagation against the OEM --------------------------------------------------------------

#[derive(Serialize)]
pub struct NumericalCase {
    pub label: String,
    pub density_scale: f64,
    pub stats: ResidualStats,
    pub runtime_ms: u128,
}

#[derive(Serialize)]
pub struct NumericalResult {
    pub cases: Vec<NumericalCase>,
}

fn run_numerical(
    c: &mut dyn Candidate,
    oem: &Oem,
    t0: Utc,
    setup: &NumericalSetup,
) -> Result<(ResidualStats, u128), CandidateError> {
    let truth0 = oem.interpolate(t0).map_err(CandidateError::failed)?;
    let outputs: Vec<Utc> = grid(t0, NUMERICAL_STEP_S, WINDOW_S)
        .into_iter()
        .skip(1)
        .collect();
    let started = Instant::now();
    let states = c.propagate(t0, &truth0, &outputs, setup)?;
    let ms = started.elapsed().as_millis();
    let rtn: Vec<Vector3<f64>> = outputs
        .iter()
        .zip(&states)
        .map(|(t, s)| {
            Ok(rtn_residual(
                &oem.interpolate(*t).map_err(CandidateError::failed)?,
                s,
            ))
        })
        .collect::<Result<_, CandidateError>>()?;
    Ok((residual_stats(&rtn), ms))
}

/// Fit one density multiplier minimising the worst 3-D error against the OEM (golden-section on log scale).
fn fit_density_scale(
    c: &mut dyn Candidate,
    oem: &Oem,
    t0: Utc,
    setup: &NumericalSetup,
) -> Result<f64, CandidateError> {
    let objective = |c: &mut dyn Candidate, k: f64| {
        run_numerical(
            c,
            oem,
            t0,
            &NumericalSetup {
                density_scale: k,
                ..*setup
            },
        )
        .map(|(s, _)| s.max_3d_m)
    };
    let (mut lo, mut hi) = (SCALE_LO.ln(), SCALE_HI.ln());
    let (mut x1, mut x2) = (hi - GOLDEN * (hi - lo), lo + GOLDEN * (hi - lo));
    let (mut f1, mut f2) = (objective(c, x1.exp())?, objective(c, x2.exp())?);
    for _ in 0..SCALE_ITERS {
        if f1 < f2 {
            hi = x2;
            (x2, f2) = (x1, f1);
            x1 = hi - GOLDEN * (hi - lo);
            f1 = objective(c, x1.exp())?;
        } else {
            lo = x1;
            (x1, f1) = (x2, f2);
            x2 = lo + GOLDEN * (hi - lo);
            f2 = objective(c, x2.exp())?;
        }
    }
    Ok(((lo + hi) / 2.0).exp())
}

fn push_case(
    c: &mut dyn Candidate,
    oem: &Oem,
    t0: Utc,
    setup: &NumericalSetup,
    label: String,
    out: &mut Vec<NumericalCase>,
) -> Result<(), CandidateError> {
    let (stats, runtime_ms) = run_numerical(c, oem, t0, setup)?;
    out.push(NumericalCase {
        label,
        density_scale: if setup.drag_cd_a_over_m.is_some() {
            setup.density_scale
        } else {
            0.0
        },
        stats,
        runtime_ms,
    });
    Ok(())
}

pub fn t6_numerical(
    cands: &mut [&mut dyn Candidate],
    oem: &Oem,
    t0: Utc,
    zonal_degree: u16,
) -> Vec<PerCandidate<NumericalResult>> {
    let b = oem.drag.cd_a_over_m();
    cands
        .iter_mut()
        .map(|c| {
            let name = c.name();
            let result = (|| -> Result<NumericalResult, CandidateError> {
                let mut cases = Vec::new();
                let base = NumericalSetup {
                    zonal_degree,
                    drag_cd_a_over_m: None,
                    density_scale: 1.0,
                    high_fidelity: false,
                };
                push_case(
                    &mut **c,
                    oem,
                    t0,
                    &base,
                    format!("J2..J{zonal_degree} only, no drag"),
                    &mut cases,
                )?;
                let with_drag = NumericalSetup {
                    drag_cd_a_over_m: Some(b),
                    ..base
                };
                push_case(
                    &mut **c,
                    oem,
                    t0,
                    &with_drag,
                    "plus drag with OEM header Cd*A/m, library density as shipped".into(),
                    &mut cases,
                )?;
                let best = fit_density_scale(&mut **c, oem, t0, &with_drag)?;
                push_case(
                    &mut **c,
                    oem,
                    t0,
                    &NumericalSetup {
                        density_scale: best,
                        ..with_drag
                    },
                    "plus drag, density scale fitted to the OEM at 24 h".into(),
                    &mut cases,
                )?;
                // A richer force model, where the library has one: what it takes to get under the 1 km target.
                let rich = NumericalSetup {
                    high_fidelity: true,
                    ..with_drag
                };
                match push_case(
                    &mut **c,
                    oem,
                    t0,
                    &rich,
                    "high fidelity (20x20 gravity + Sun + Moon) plus drag, density as shipped"
                        .into(),
                    &mut cases,
                ) {
                    Ok(()) | Err(CandidateError::Unsupported(_)) => {}
                    Err(e) => return Err(e),
                }
                match fit_density_scale(&mut **c, oem, t0, &rich) {
                    Ok(k) => push_case(
                        &mut **c,
                        oem,
                        t0,
                        &NumericalSetup {
                            density_scale: k,
                            ..rich
                        },
                        "high fidelity (20x20 gravity + Sun + Moon) plus drag, density fitted"
                            .into(),
                        &mut cases,
                    )?,
                    Err(CandidateError::Unsupported(why)) => {
                        tracing::info!(candidate = %c.name(), %why, "high-fidelity variant skipped")
                    }
                    Err(e) => return Err(e),
                }
                Ok(NumericalResult { cases })
            })();
            PerCandidate {
                candidate: name,
                result: result.map_err(|e| err_text(&e)),
            }
        })
        .collect()
}

// ---- 7. Time scales and EOP ----------------------------------------------------------------------------------

#[derive(Serialize)]
pub struct TimeProbe {
    pub label: String,
    pub result: Result<TimeOffsets, String>,
}

pub fn t7_time(c: &dyn Candidate, probes: &[(&str, Utc)]) -> Vec<TimeProbe> {
    probes
        .iter()
        .map(|(label, t)| TimeProbe {
            label: (*label).to_owned(),
            result: c.time_offsets(*t).map_err(|e| err_text(&e)),
        })
        .collect()
}
