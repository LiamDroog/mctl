//! The in-house candidate: the `sgp4` crate plus `nalgebra`, with everything else written here.
//!
//! This is deliberately the *cheapest honest version* of each capability, so the evaluation can
//! say what DESIGN.md's in-house path would cost and what accuracy it buys:
//! - frames: IAU-76 precession with a 10-term IAU-80 nutation series (full series has 106 terms);
//! - Sun: Vallado's low-precision series;
//! - time: an embedded leap-second table and `finals2000A` for UT1-UTC and polar motion;
//! - numerical: Dormand-Prince 5(4) Cowell integration with zonal gravity and exponential drag.

use std::f64::consts::{PI, TAU};

use nalgebra::{Matrix3, Vector3};
use thiserror::Error;

use crate::candidate::{
    CResult, Candidate, CandidateError, NumericalSetup, Sgp4Outcome, TimeOffsets,
};
use crate::geom::State;
use crate::timeutil::{JD_MJD_OFFSET, SECONDS_PER_DAY, Utc};

// ---- constants ---------------------------------------------------------------------------------------

/// Earth GM, km^3/s^2, EGM2008 header value (3.986004415e14 m^3/s^2).
const MU_KM3_S2: f64 = 398_600.441_5;
/// Equatorial radius, km, EGM2008 header value (6378136.3 m).
const RE_KM: f64 = 6378.1363;
/// Zonal harmonics J2, J3, J4 = -sqrt(2n+1) * C(n,0), from the EGM2008 tide-free coefficients.
const ZONAL_J: [f64; 3] = [
    1.082_626_173_852_2e-3,
    -2.532_410_518_567_7e-6,
    -1.619_897_599_916_9e-6,
];
/// Earth rotation rate, rad/s (IERS 1996 nominal).
const OMEGA_EARTH_RAD_S: f64 = 7.292_115_146_706_979e-5;
/// WGS-84 flattening, used only to turn geocentric radius into altitude for drag.
const FLATTENING: f64 = 1.0 / 298.257_223_563;
/// TT - TAI, seconds (exact by definition).
const TT_MINUS_TAI_S: f64 = 32.184;
/// Julian date of J2000.0 (TT).
const JD_J2000: f64 = 2_451_545.0;
/// Days per Julian century.
const DAYS_PER_CENTURY: f64 = 36_525.0;
const ARCSEC_TO_RAD: f64 = PI / (180.0 * 3600.0);
/// Mean Earth-Sun distance, km (IAU 2012 astronomical unit).
const AU_KM: f64 = 149_597_870.7;

/// In-house exponential atmosphere: density at the reference altitude and scale height.
///
/// These two numbers are *assumptions*, not data: ~3e-12 kg/m^3 at 420 km is a typical
/// mean-solar-activity value and 58 km a typical scale height there. A real model (Harris-Priester,
/// NRLMSISE-00) or an fitted scale is needed for accuracy; the evaluation tunes `density_scale`.
const RHO_REF_KG_M3: f64 = 3.0e-12;
const RHO_REF_ALT_KM: f64 = 420.0;
const RHO_SCALE_HEIGHT_KM: f64 = 58.0;

/// Integrator tolerances (Dormand-Prince 5(4)): relative and absolute, in km and km/s.
const RTOL: f64 = 1.0e-12;
const ATOL: f64 = 1.0e-10;
const MAX_STEPS: usize = 2_000_000;

/// Leap-second table: first UTC date (y, m, d) from which TAI-UTC takes the given value.
/// Source: IERS Bulletin C history. The latest entry is 2017-01-01; nothing newer is known to this table.
const LEAP_TABLE: &[((i32, u32, u32), f64)] = &[
    ((1972, 1, 1), 10.0),
    ((1972, 7, 1), 11.0),
    ((1973, 1, 1), 12.0),
    ((1974, 1, 1), 13.0),
    ((1975, 1, 1), 14.0),
    ((1976, 1, 1), 15.0),
    ((1977, 1, 1), 16.0),
    ((1978, 1, 1), 17.0),
    ((1979, 1, 1), 18.0),
    ((1980, 1, 1), 19.0),
    ((1981, 7, 1), 20.0),
    ((1982, 7, 1), 21.0),
    ((1983, 7, 1), 22.0),
    ((1985, 7, 1), 23.0),
    ((1988, 1, 1), 24.0),
    ((1990, 1, 1), 25.0),
    ((1991, 1, 1), 26.0),
    ((1992, 7, 1), 27.0),
    ((1993, 7, 1), 28.0),
    ((1994, 7, 1), 29.0),
    ((1996, 1, 1), 30.0),
    ((1997, 7, 1), 31.0),
    ((1999, 1, 1), 32.0),
    ((2006, 1, 1), 33.0),
    ((2009, 1, 1), 34.0),
    ((2012, 7, 1), 35.0),
    ((2015, 7, 1), 36.0),
    ((2017, 1, 1), 37.0),
];

#[derive(Debug, Error)]
pub enum InHouseError {
    #[error("no leap-second data before 1972-01-01 (UTC was not yet defined by leap seconds)")]
    BeforeUtc,
    #[error("EOP file: {0}")]
    Eop(String),
    #[error("no EOP data for MJD {mjd:.1}: table covers {first:.1} .. {last:.1}")]
    EopCoverage { mjd: f64, first: f64, last: f64 },
}

/// TAI - UTC, seconds, from the embedded table.
pub fn tai_minus_utc(t: Utc) -> Result<f64, InHouseError> {
    let mut found = None;
    for &((y, m, d), dt) in LEAP_TABLE {
        let start = Utc::from_ymd_hms(y, m, d, 0, 0, 0.0).expect("table dates are valid");
        if t.0 >= start.0 {
            found = Some(dt);
        }
    }
    found.ok_or(InHouseError::BeforeUtc)
}

// ---- EOP ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct EopRow {
    pub mjd: f64,
    /// Polar motion, arcseconds.
    pub xp: f64,
    pub yp: f64,
    /// UT1 - UTC, seconds.
    pub dut1: f64,
}

/// IERS `finals2000A` (Bulletin A) daily values.
pub struct EopTable {
    rows: Vec<EopRow>,
}

impl EopTable {
    pub fn parse(text: &str) -> Result<Self, InHouseError> {
        let mut rows = Vec::new();
        for (n, l) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            let field = |a: usize, b: usize| l.get(a..b).map(str::trim).filter(|s| !s.is_empty());
            let num = |a: usize, b: usize, what: &str| -> Result<Option<f64>, InHouseError> {
                field(a, b)
                    .map(|s| {
                        s.parse::<f64>().map_err(|_| {
                            InHouseError::Eop(format!("line {}: bad {what} {s:?}", n + 1))
                        })
                    })
                    .transpose()
            };
            let (Some(mjd), Some(xp), Some(yp), Some(dut1)) = (
                num(7, 15, "MJD")?,
                num(18, 27, "x")?,
                num(37, 46, "y")?,
                num(58, 68, "UT1-UTC")?,
            ) else {
                // Rows past the end of predictions can be blank in these columns; they carry no usable value.
                continue;
            };
            if let Some(prev) = rows.last().map(|r: &EopRow| r.mjd)
                && mjd <= prev
            {
                return Err(InHouseError::Eop(format!(
                    "line {}: MJD {mjd} not after {prev}",
                    n + 1
                )));
            }
            rows.push(EopRow { mjd, xp, yp, dut1 });
        }
        if rows.len() < 2 {
            return Err(InHouseError::Eop("fewer than two usable rows".into()));
        }
        Ok(Self { rows })
    }

    /// Linearly interpolated values. Outside the table is an error: no silent zeros.
    ///
    /// UT1-UTC steps by one second at a leap second; interpolating across one would be wrong, so a
    /// jump of more than 0.5 s between neighbours is rejected.
    pub fn at(&self, mjd_utc: f64) -> Result<EopRow, InHouseError> {
        let (first, last) = (self.rows[0].mjd, self.rows[self.rows.len() - 1].mjd);
        if mjd_utc < first || mjd_utc > last {
            return Err(InHouseError::EopCoverage {
                mjd: mjd_utc,
                first,
                last,
            });
        }
        let i = self
            .rows
            .partition_point(|r| r.mjd <= mjd_utc)
            .saturating_sub(1)
            .min(self.rows.len() - 2);
        let (a, b) = (self.rows[i], self.rows[i + 1]);
        if (b.dut1 - a.dut1).abs() > 0.5 {
            return Err(InHouseError::Eop(format!(
                "UT1-UTC jumps by {:.3} s near MJD {:.0} (leap second); not interpolated",
                b.dut1 - a.dut1,
                a.mjd
            )));
        }
        let w = (mjd_utc - a.mjd) / (b.mjd - a.mjd);
        Ok(EopRow {
            mjd: mjd_utc,
            xp: a.xp + w * (b.xp - a.xp),
            yp: a.yp + w * (b.yp - a.yp),
            dut1: a.dut1 + w * (b.dut1 - a.dut1),
        })
    }
}

// ---- rotations and frames ------------------------------------------------------------------------------

/// Passive rotation about axis 1 by `a` (Vallado's ROT1).
fn rot1(a: f64) -> Matrix3<f64> {
    let (s, c) = a.sin_cos();
    Matrix3::new(1.0, 0.0, 0.0, 0.0, c, s, 0.0, -s, c)
}
fn rot2(a: f64) -> Matrix3<f64> {
    let (s, c) = a.sin_cos();
    Matrix3::new(c, 0.0, -s, 0.0, 1.0, 0.0, s, 0.0, c)
}
fn rot3(a: f64) -> Matrix3<f64> {
    let (s, c) = a.sin_cos();
    Matrix3::new(c, s, 0.0, -s, c, 0.0, 0.0, 0.0, 1.0)
}

/// Julian centuries of TT since J2000 and the Julian date of UT1 for a UTC instant.
fn time_args(t: Utc, ut1_minus_utc: f64) -> Result<(f64, f64), InHouseError> {
    let jd_utc = t.mjd_utc() + JD_MJD_OFFSET;
    let tt_minus_utc = tai_minus_utc(t)? + TT_MINUS_TAI_S;
    let t_tt = (jd_utc + tt_minus_utc / SECONDS_PER_DAY - JD_J2000) / DAYS_PER_CENTURY;
    Ok((t_tt, jd_utc + ut1_minus_utc / SECONDS_PER_DAY))
}

/// Greenwich mean sidereal time, rad (IAU-82, Vallado).
fn gmst(jd_ut1: f64) -> f64 {
    let t = (jd_ut1 - JD_J2000) / DAYS_PER_CENTURY;
    let sec = 67_310.548_41 + (876_600.0 * 3600.0 + 8_640_184.812_866) * t + 0.093_104 * t * t
        - 6.2e-6 * t * t * t;
    (sec * TAU / SECONDS_PER_DAY).rem_euclid(TAU)
}

/// IAU-76 precession angles (zeta, theta, z), rad.
fn precession_angles(t: f64) -> (f64, f64, f64) {
    let zeta = (2306.2181 * t + 0.30188 * t * t + 0.017998 * t * t * t) * ARCSEC_TO_RAD;
    let theta = (2004.3109 * t - 0.42665 * t * t - 0.041833 * t * t * t) * ARCSEC_TO_RAD;
    let z = (2306.2181 * t + 1.09468 * t * t + 0.018203 * t * t * t) * ARCSEC_TO_RAD;
    (zeta, theta, z)
}

/// Mean-of-date to J2000 rotation (transpose of the usual J2000 -> MOD precession matrix).
fn mod_to_j2000(t: f64) -> Matrix3<f64> {
    let (zeta, theta, z) = precession_angles(t);
    rot3(zeta) * rot2(-theta) * rot3(z)
}

/// Fundamental arguments l, l', F, D, Omega (IAU-80), rad.
fn fundamental_args(t: f64) -> [f64; 5] {
    let d = |a: f64, b: f64, c: f64| (a + b * t + c * t * t).to_radians().rem_euclid(TAU);
    [
        d(134.962_981_39, 477_198.867_398_1, 0.008_697_2),
        d(357.527_723_33, 35_999.050_340_0, -0.000_160_3),
        d(93.271_910_28, 483_202.017_538_1, -0.003_682_5),
        d(297.850_363_06, 445_267.111_480_0, -0.001_914_2),
        d(125.044_522_22, -1_934.136_260_8, 0.002_070_8),
    ]
}

/// Leading 10 terms of the IAU-80 nutation series: (l, l', F, D, Om, dpsi0, dpsi1, deps0, deps1), coefficients in 1e-4 arcsec.
///
/// Truncated on purpose: this is the in-house baseline. The omitted 96 terms bound the error at a few mas.
#[rustfmt::skip]
const NUTATION_TERMS: [([i8; 5], f64, f64, f64, f64); 10] = [
    ([0, 0, 0, 0, 1], -171_996.0, -174.2, 92_025.0, 8.9),
    ([0, 0, 2, -2, 2], -13_187.0, -1.6, 5_736.0, -3.1),
    ([0, 0, 2, 0, 2], -2_274.0, -0.2, 977.0, -0.5),
    ([0, 0, 0, 0, 2], 2_062.0, 0.2, -895.0, 0.5),
    ([0, 1, 0, 0, 0], 1_426.0, -3.4, 54.0, -0.1),
    ([1, 0, 0, 0, 0], 712.0, 0.1, -7.0, 0.0),
    ([0, 1, 2, -2, 2], -517.0, 1.2, 224.0, -0.6),
    ([0, 0, 2, 0, 1], -386.0, -0.4, 200.0, 0.0),
    ([1, 0, 2, 0, 2], -301.0, 0.0, 129.0, -0.1),
    ([0, -1, 2, -2, 2], 217.0, -0.5, -95.0, 0.3),
];

/// (delta psi, delta epsilon) in rad.
fn nutation(t: f64) -> (f64, f64) {
    let args = fundamental_args(t);
    let (mut dpsi, mut deps) = (0.0, 0.0);
    for (m, p0, p1, e0, e1) in NUTATION_TERMS {
        let arg: f64 = m.iter().zip(args).map(|(&k, a)| f64::from(k) * a).sum();
        dpsi += (p0 + p1 * t) * arg.sin();
        deps += (e0 + e1 * t) * arg.cos();
    }
    (dpsi * 1.0e-4 * ARCSEC_TO_RAD, deps * 1.0e-4 * ARCSEC_TO_RAD)
}

/// Mean obliquity of the ecliptic, rad (IAU-76).
fn mean_obliquity(t: f64) -> f64 {
    (84_381.448 - 46.8150 * t - 0.00059 * t * t + 0.001_813 * t * t * t) * ARCSEC_TO_RAD
}

/// Rotation TEME -> TOD, MOD, J2000 pieces for one instant.
struct Reduction {
    gmst: f64,
    teme_to_j2000: Matrix3<f64>,
    /// Unit vector of the true pole of date in J2000, for zonal gravity.
    pole_j2000: Vector3<f64>,
}

fn reduction(t: f64, jd_ut1: f64) -> Reduction {
    let (dpsi, deps) = nutation(t);
    let eps_mean = mean_obliquity(t);
    let eps_true = eps_mean + deps;
    let omega = fundamental_args(t)[4];
    // Equation of the equinoxes, with the two small post-1997 terms (Vallado).
    let eq_eq = dpsi * eps_mean.cos()
        + (0.002_64 * omega.sin() + 0.000_063 * (2.0 * omega).sin()) * ARCSEC_TO_RAD;
    let teme_to_tod = rot3(-eq_eq);
    let n = rot1(-eps_true) * rot3(-dpsi) * rot1(eps_mean); // MOD -> TOD
    let p = mod_to_j2000(t);
    Reduction {
        gmst: gmst(jd_ut1),
        teme_to_j2000: p * n.transpose() * teme_to_tod,
        pole_j2000: p * n.transpose() * Vector3::z(),
    }
}

/// How the in-house frame chain is configured.
#[derive(Clone, Copy)]
pub struct InHouse {
    /// Apply polar motion in GCRF -> ITRF. DESIGN.md §6.4 says to ignore it (~10 m).
    pub polar_motion: bool,
}

pub struct InHouseCandidate<'e> {
    pub cfg: InHouse,
    pub eop: &'e EopTable,
}

fn ut1_for(eop: &EopTable, t: Utc) -> Result<EopRow, CandidateError> {
    eop.at(t.mjd_utc()).map_err(CandidateError::failed)
}

impl InHouseCandidate<'_> {
    fn reduce(&self, t: Utc) -> CResult<(Reduction, EopRow)> {
        let row = ut1_for(self.eop, t)?;
        let (t_tt, jd_ut1) = time_args(t, row.dut1).map_err(CandidateError::failed)?;
        Ok((reduction(t_tt, jd_ut1), row))
    }
}

// ---- Sun ----------------------------------------------------------------------------------------------

/// Geocentric Sun position in J2000, km. Vallado Algorithm 29 (low precision), precessed to J2000.
fn sun_j2000(t_tt: f64, t_ut1: f64) -> Vector3<f64> {
    let lambda_mean = (280.460 + 36_000.771 * t_ut1).rem_euclid(360.0);
    let m = (357.529_109_2 + 35_999.050_34 * t_tt).to_radians();
    let lambda =
        (lambda_mean + 1.914_666_471 * m.sin() + 0.019_994_643 * (2.0 * m).sin()).to_radians();
    let r_au = 1.000_140_612 - 0.016_708_617 * m.cos() - 0.000_139_589 * (2.0 * m).cos();
    let eps = (23.439_291 - 0.013_004_2 * t_tt).to_radians();
    let r_mod = r_au
        * AU_KM
        * Vector3::new(
            lambda.cos(),
            eps.cos() * lambda.sin(),
            eps.sin() * lambda.sin(),
        );
    mod_to_j2000(t_tt) * r_mod
}

// ---- numerical propagation ------------------------------------------------------------------------------

fn legendre(n: usize, s: f64) -> (f64, f64) {
    match n {
        2 => ((3.0 * s * s - 1.0) / 2.0, 3.0 * s),
        3 => (
            (5.0 * s.powi(3) - 3.0 * s) / 2.0,
            (15.0 * s * s - 3.0) / 2.0,
        ),
        4 => (
            (35.0 * s.powi(4) - 30.0 * s * s + 3.0) / 8.0,
            (35.0 * s.powi(3) - 15.0 * s) / 2.0,
        ),
        _ => unreachable!("zonal degree is validated to 2..=4"),
    }
}

/// Acceleration from zonal harmonics J2..J_degree about the pole direction `k`, km/s^2.
///
/// From the potential V = (mu/r) [1 - sum J_n (Re/r)^n P_n(s)], s = (r.k)/r, taking the gradient with
/// the chain rule: a_n = (n+1) mu J_n Re^n r^-(n+2) P_n r_hat - mu J_n Re^n r^-(n+2) P_n' (k - s r_hat).
fn zonal_accel(r: &Vector3<f64>, k: &Vector3<f64>, degree: u16) -> Vector3<f64> {
    let rn = r.norm();
    let rhat = r / rn;
    let s = rhat.dot(k);
    let mut a = Vector3::zeros();
    for n in 2..=usize::from(degree) {
        let (p, dp) = legendre(n, s);
        let c = MU_KM3_S2 * ZONAL_J[n - 2] * RE_KM.powi(n as i32) / rn.powi(n as i32 + 2);
        a += (n as f64 + 1.0) * c * p * rhat - c * dp * (k - s * rhat);
    }
    a
}

fn density_kg_m3(r: &Vector3<f64>, k: &Vector3<f64>, scale: f64) -> f64 {
    let rn = r.norm();
    let s = r.dot(k) / rn;
    let altitude = rn - RE_KM * (1.0 - FLATTENING * s * s);
    scale * RHO_REF_KG_M3 * (-(altitude - RHO_REF_ALT_KM) / RHO_SCALE_HEIGHT_KM).exp()
}

struct Forces {
    pole: Vector3<f64>,
    zonal_degree: u16,
    drag: Option<(f64, f64)>,
}

impl Forces {
    fn accel(&self, r: &Vector3<f64>, v: &Vector3<f64>) -> Vector3<f64> {
        let rn = r.norm();
        let mut a = -MU_KM3_S2 * r / (rn * rn * rn);
        if self.zonal_degree >= 2 {
            a += zonal_accel(r, &self.pole, self.zonal_degree);
        }
        if let Some((cd_a_over_m, scale)) = self.drag {
            // The atmosphere co-rotates with the Earth about its true pole, not about GCRF +Z.
            let v_rel = v - OMEGA_EARTH_RAD_S * self.pole.cross(r);
            let speed_ms = v_rel.norm() * 1.0e3;
            let rho = density_kg_m3(r, &self.pole, scale);
            // a [m/s^2] = -0.5 rho B |v| v; v_rel is in km/s so the result in km/s^2 is x1e3 /1e3 for units.
            a += -0.5 * rho * cd_a_over_m * speed_ms * v_rel; // (m/s)(km/s) -> m * km / s^2, then /1e3 for km/s^2 * 1e3 for m->km cancels
        }
        a
    }
}

/// Dormand-Prince 5(4) coefficients (Butcher tableau).
const A: [[f64; 6]; 7] = [
    [0.0; 6],
    [1.0 / 5.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    [3.0 / 40.0, 9.0 / 40.0, 0.0, 0.0, 0.0, 0.0],
    [44.0 / 45.0, -56.0 / 15.0, 32.0 / 9.0, 0.0, 0.0, 0.0],
    [
        19_372.0 / 6_561.0,
        -25_360.0 / 2_187.0,
        64_448.0 / 6_561.0,
        -212.0 / 729.0,
        0.0,
        0.0,
    ],
    [
        9_017.0 / 3_168.0,
        -355.0 / 33.0,
        46_732.0 / 5_247.0,
        49.0 / 176.0,
        -5_103.0 / 18_656.0,
        0.0,
    ],
    [
        35.0 / 384.0,
        0.0,
        500.0 / 1_113.0,
        125.0 / 192.0,
        -2_187.0 / 6_784.0,
        11.0 / 84.0,
    ],
];
const B5: [f64; 7] = [
    35.0 / 384.0,
    0.0,
    500.0 / 1_113.0,
    125.0 / 192.0,
    -2_187.0 / 6_784.0,
    11.0 / 84.0,
    0.0,
];
const B4: [f64; 7] = [
    5_179.0 / 57_600.0,
    0.0,
    7_571.0 / 16_695.0,
    393.0 / 640.0,
    -92_097.0 / 339_200.0,
    187.0 / 2_100.0,
    1.0 / 40.0,
];

type Y = [f64; 6];

fn deriv(f: &Forces, y: &Y) -> Y {
    let (r, v) = (
        Vector3::new(y[0], y[1], y[2]),
        Vector3::new(y[3], y[4], y[5]),
    );
    let a = f.accel(&r, &v);
    [v.x, v.y, v.z, a.x, a.y, a.z]
}

/// Integrate from `t0` to `t1` seconds with adaptive Dormand-Prince 5(4).
fn integrate(
    f: &Forces,
    mut y: Y,
    t0: f64,
    t1: f64,
    h_guess: &mut f64,
    steps: &mut usize,
) -> Result<Y, String> {
    let mut t = t0;
    while t < t1 {
        let h = h_guess.min(t1 - t);
        let mut k = [[0.0; 6]; 7];
        k[0] = deriv(f, &y);
        for s in 1..7 {
            let mut ys = y;
            for (j, kj) in k.iter().enumerate().take(s) {
                for i in 0..6 {
                    ys[i] += h * A[s][j] * kj[i];
                }
            }
            k[s] = deriv(f, &ys);
        }
        let (mut y5, mut err) = (y, 0.0);
        for i in 0..6 {
            let (mut inc5, mut inc4) = (0.0, 0.0);
            for s in 0..7 {
                inc5 += B5[s] * k[s][i];
                inc4 += B4[s] * k[s][i];
            }
            y5[i] = y[i] + h * inc5;
            let sc = ATOL + RTOL * y[i].abs().max(y5[i].abs());
            err += (h * (inc5 - inc4) / sc).powi(2);
        }
        err = (err / 6.0).sqrt();
        *steps += 1;
        if *steps > MAX_STEPS {
            return Err(format!("integrator exceeded {MAX_STEPS} steps"));
        }
        if !err.is_finite() {
            return Err("integrator produced a non-finite error estimate".into());
        }
        if err <= 1.0 {
            t += h;
            y = y5;
        }
        // Standard step controller with safety factor 0.9 and bounded growth.
        *h_guess = h * (0.9 * err.max(1e-10).powf(-0.2)).clamp(0.2, 5.0);
    }
    Ok(y)
}

// ---- the Candidate impl -------------------------------------------------------------------------------

fn sgp4_state(p: &sgp4::Prediction) -> State {
    State::new(p.position, p.velocity)
}

impl Candidate for InHouseCandidate<'_> {
    fn name(&self) -> String {
        format!(
            "in-house ({})",
            if self.cfg.polar_motion {
                "with polar motion"
            } else {
                "design: no polar motion"
            }
        )
    }

    fn sgp4_omm(&mut self, omm_json: &str, times: &[Utc]) -> CResult<Vec<State>> {
        let records: Vec<sgp4::Elements> =
            serde_json::from_str(omm_json).map_err(CandidateError::failed)?;
        let el = records
            .first()
            .ok_or_else(|| CandidateError::Failed("OMM JSON contains no records".into()))?;
        let epoch_text = serde_json::from_str::<serde_json::Value>(omm_json)
            .ok()
            .and_then(|v| v[0]["EPOCH"].as_str().map(str::to_owned))
            .ok_or_else(|| CandidateError::Failed("OMM record has no EPOCH".into()))?;
        let epoch = Utc::parse(&epoch_text).map_err(CandidateError::failed)?;
        // `Constants::from_elements` would use WGS-84; CelesTrak/Space-Track elements are fit with the
        // AFSPC convention (WGS-72), which the crate calls "compatibility mode". Using WGS-84 costs ~70 m
        // over 24 h for the ISS (measured in docs/spikes/astro-results.json via brahe, which does so).
        let constants = sgp4::Constants::from_elements_afspc_compatibility_mode(el)
            .map_err(CandidateError::failed)?;
        times
            .iter()
            .map(|t| {
                constants
                    .propagate_afspc_compatibility_mode(sgp4::MinutesSinceEpoch(
                        t.minus(epoch) / 60.0,
                    ))
                    .map(|p| sgp4_state(&p))
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
        let el = sgp4::Elements::from_tle(None, line1.as_bytes(), line2.as_bytes())
            .map_err(CandidateError::failed)?;
        let constants = sgp4::Constants::from_elements_afspc_compatibility_mode(&el)
            .map_err(CandidateError::failed)?;
        Ok(tsince_min
            .iter()
            .map(|&t| {
                constants
                    .propagate_afspc_compatibility_mode(sgp4::MinutesSinceEpoch(t))
                    .map(|p| sgp4_state(&p))
                    .map_err(|e| e.to_string())
            })
            .collect())
    }

    fn teme_to_gcrf(&self, t: Utc, s: &State) -> CResult<State> {
        let (red, _) = self.reduce(t)?;
        Ok(State {
            r: red.teme_to_j2000 * s.r,
            v: red.teme_to_j2000 * s.v,
        })
    }

    fn gcrf_to_itrf(&self, t: Utc, s: &State) -> CResult<State> {
        let (red, row) = self.reduce(t)?;
        // J2000 -> TEME -> PEF (GMST rotation) -> ITRF (polar motion).
        let teme = State {
            r: red.teme_to_j2000.transpose() * s.r,
            v: red.teme_to_j2000.transpose() * s.v,
        };
        let r_pef = rot3(red.gmst) * teme.r;
        let v_pef =
            rot3(red.gmst) * teme.v - Vector3::new(0.0, 0.0, OMEGA_EARTH_RAD_S).cross(&r_pef);
        let w = if self.cfg.polar_motion {
            rot2(-row.xp * ARCSEC_TO_RAD) * rot1(-row.yp * ARCSEC_TO_RAD)
        } else {
            Matrix3::identity()
        };
        Ok(State {
            r: w * r_pef,
            v: w * v_pef,
        })
    }

    fn sun_gcrf_km(&self, t: Utc) -> CResult<[f64; 3]> {
        let row = ut1_for(self.eop, t)?;
        let (t_tt, jd_ut1) = time_args(t, row.dut1).map_err(CandidateError::failed)?;
        let s = sun_j2000(t_tt, (jd_ut1 - JD_J2000) / DAYS_PER_CENTURY);
        Ok([s.x, s.y, s.z])
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
                "in-house force model is zonal J2..J4 plus drag only".into(),
            ));
        }
        if !(2..=4).contains(&setup.zonal_degree) && setup.zonal_degree != 0 {
            return Err(CandidateError::Unsupported(format!(
                "in-house zonal model has J2..J4, not degree {}",
                setup.zonal_degree
            )));
        }
        let last = *outputs
            .last()
            .ok_or_else(|| CandidateError::Failed("no output times".into()))?;
        if outputs.windows(2).any(|w| w[1].0 <= w[0].0) || outputs[0].0 <= t0.0 {
            return Err(CandidateError::Failed(
                "output times must be strictly ascending and after t0".into(),
            ));
        }
        // The pole moves ~0.14 arcsec in 24 h; holding it at mid-window changes the result by well under a metre.
        let mid = t0.plus(last.minus(t0) / 2.0);
        let (red, _) = self.reduce(mid)?;
        let forces = Forces {
            pole: red.pole_j2000,
            zonal_degree: setup.zonal_degree,
            drag: setup.drag_cd_a_over_m.map(|b| (b, setup.density_scale)),
        };
        let mut y: Y = [s0.r.x, s0.r.y, s0.r.z, s0.v.x, s0.v.y, s0.v.z];
        let (mut t_prev, mut h, mut steps) = (t0, 10.0, 0usize);
        let mut out = Vec::with_capacity(outputs.len());
        for &t in outputs {
            y = integrate(&forces, y, 0.0, t.minus(t_prev), &mut h, &mut steps)
                .map_err(CandidateError::Failed)?;
            t_prev = t;
            out.push(State::new([y[0], y[1], y[2]], [y[3], y[4], y[5]]));
        }
        Ok(out)
    }

    fn time_offsets(&self, t: Utc) -> CResult<TimeOffsets> {
        let tai = tai_minus_utc(t).map_err(CandidateError::failed)?;
        // A missing EOP row is reported, never replaced by zero or a held value.
        let (ut1, note) = match self.eop.at(t.mjd_utc()) {
            Ok(row) => (Some(row.dut1), None),
            Err(e) => (None, Some(e.to_string())),
        };
        Ok(TimeOffsets {
            tai_minus_utc: tai,
            tt_minus_utc: tai + TT_MINUS_TAI_S,
            ut1_minus_utc: ut1,
            ut1_note: note,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EOP_TEXT: &str = include_str!("../fixtures/finals2000A.all");

    fn eop() -> EopTable {
        EopTable::parse(EOP_TEXT).unwrap()
    }

    #[test]
    fn leap_seconds_around_the_2016_insertion() {
        let before = Utc::from_ymd_hms(2016, 12, 31, 23, 59, 59.0).unwrap();
        let after = Utc::from_ymd_hms(2017, 1, 1, 0, 0, 0.0).unwrap();
        assert_eq!(tai_minus_utc(before).unwrap(), 36.0);
        assert_eq!(tai_minus_utc(after).unwrap(), 37.0);
        assert_eq!(
            tai_minus_utc(Utc::from_ymd_hms(1972, 1, 1, 0, 0, 0.0).unwrap()).unwrap(),
            10.0
        );
        assert!(tai_minus_utc(Utc::from_ymd_hms(1971, 12, 31, 0, 0, 0.0).unwrap()).is_err());
    }

    #[test]
    fn eop_matches_the_published_row_and_rejects_outside() {
        let e = eop();
        let row = e.at(61_335.0).unwrap();
        assert!(
            (row.dut1 - (-0.0428601)).abs() < 1e-9
                && (row.xp - 0.149159).abs() < 1e-9
                && (row.yp - 0.317685).abs() < 1e-9
        );
        let mid = e.at(61_335.5).unwrap();
        assert!(
            mid.dut1 < row.dut1 && mid.dut1 > e.at(61_336.0).unwrap().dut1,
            "linear in between"
        );
        assert!(matches!(
            e.at(10_000.0),
            Err(InHouseError::EopCoverage { .. })
        ));
        assert!(matches!(
            e.at(99_999.0),
            Err(InHouseError::EopCoverage { .. })
        ));
        assert!(EopTable::parse("").is_err());
    }

    #[test]
    fn eop_refuses_to_interpolate_across_a_leap_second() {
        // Two rows a day apart whose UT1-UTC differs by ~1 s, as happens when a leap second is inserted.
        let t = "170101 57754.00 I  0.100000 0.000001  0.300000 0.000001  I 0.4000000 0.0000100\n170102 57755.00 I  0.100000 0.000001  0.300000 0.000001  I-0.5990000 0.0000100\n";
        let table = EopTable::parse(t).unwrap();
        assert!(table.at(57_754.5).is_err());
    }

    #[test]
    fn rotations_are_orthonormal() {
        let red = reduction(0.26, 2_461_336.5);
        let m = red.teme_to_j2000;
        assert!((m * m.transpose() - Matrix3::identity()).norm() < 1e-14);
        assert!((m.determinant() - 1.0).abs() < 1e-14);
        assert!((red.pole_j2000.norm() - 1.0).abs() < 1e-14);
    }

    #[test]
    fn gmst_at_j2000_matches_the_known_value() {
        // GMST at 2000-01-01 12:00 UT1 is 18h 41m 50.548s = 280.4606 deg.
        assert!((gmst(JD_J2000).to_degrees() - 280.4606).abs() < 1e-3);
    }

    fn circular_state(alt_km: f64, inc_deg: f64) -> State {
        let r = RE_KM + alt_km;
        let v = (MU_KM3_S2 / r).sqrt();
        let i = inc_deg.to_radians();
        State::new([r, 0.0, 0.0], [0.0, v * i.cos(), v * i.sin()])
    }

    #[test]
    fn two_body_circular_orbit_closes_after_one_period() {
        let s0 = circular_state(500.0, 51.6);
        let period = TAU * ((RE_KM + 500.0).powi(3) / MU_KM3_S2).sqrt();
        let f = Forces {
            pole: Vector3::z(),
            zonal_degree: 0,
            drag: None,
        };
        let (mut h, mut steps) = (10.0, 0);
        let y = integrate(
            &f,
            [s0.r.x, s0.r.y, s0.r.z, s0.v.x, s0.v.y, s0.v.z],
            0.0,
            period,
            &mut h,
            &mut steps,
        )
        .unwrap();
        assert!(
            (Vector3::new(y[0], y[1], y[2]) - s0.r).norm() < 1e-6,
            "closure error {} km",
            (Vector3::new(y[0], y[1], y[2]) - s0.r).norm()
        );
    }

    #[test]
    fn j2_nodal_regression_matches_the_analytic_rate() {
        let s0 = circular_state(500.0, 51.6);
        let (a, i) = (RE_KM + 500.0, 51.6f64.to_radians());
        let n = (MU_KM3_S2 / a.powi(3)).sqrt();
        let analytic = -1.5 * n * ZONAL_J[0] * (RE_KM / a).powi(2) * i.cos(); // rad/s, circular orbit
        let f = Forces {
            pole: Vector3::z(),
            zonal_degree: 2,
            drag: None,
        };
        let days = 2.0;
        let (mut h, mut steps) = (10.0, 0);
        let y = integrate(
            &f,
            [s0.r.x, s0.r.y, s0.r.z, s0.v.x, s0.v.y, s0.v.z],
            0.0,
            days * 86_400.0,
            &mut h,
            &mut steps,
        )
        .unwrap();
        let (r, v) = (
            Vector3::new(y[0], y[1], y[2]),
            Vector3::new(y[3], y[4], y[5]),
        );
        let node = |r: &Vector3<f64>, v: &Vector3<f64>| {
            let hvec = r.cross(v);
            hvec.x.atan2(-hvec.y)
        };
        let drift = (node(&r, &v) - node(&s0.r, &s0.v)) / (days * 86_400.0);
        // The osculating (not mean) elements and the J2 short-period terms make exact agreement impossible; 5% is the check.
        assert!(
            (drift / analytic - 1.0).abs() < 0.05,
            "drift {drift:e} vs analytic {analytic:e}"
        );
    }

    #[test]
    fn zonal_acceleration_is_the_gradient_of_the_potential() {
        // Finite-difference the potential V = mu/r * (1 - sum J_n (Re/r)^n P_n(s)) and compare.
        let pole = Vector3::new(0.1, -0.05, 1.0).normalize();
        let r = Vector3::new(4000.0, -3000.0, 5000.0);
        let potential = |p: &Vector3<f64>| {
            let (rn, s) = (p.norm(), p.dot(&pole) / p.norm());
            let sum: f64 = (2..=4)
                .map(|n| ZONAL_J[n - 2] * (RE_KM / rn).powi(n as i32) * legendre(n, s).0)
                .sum();
            MU_KM3_S2 / rn * (1.0 - sum)
        };
        let central = |p: &Vector3<f64>| MU_KM3_S2 / p.norm();
        let h = 1e-3;
        let grad = |f: &dyn Fn(&Vector3<f64>) -> f64| {
            Vector3::from_fn(|i, _| {
                let mut e = Vector3::zeros();
                e[i] = h;
                (f(&(r + e)) - f(&(r - e))) / (2.0 * h)
            })
        };
        let numeric = grad(&potential) - grad(&central);
        let analytic = zonal_accel(&r, &pole, 4);
        assert!(
            (numeric - analytic).norm() / analytic.norm() < 1e-6,
            "{numeric:?} vs {analytic:?}"
        );
    }

    #[test]
    fn sun_is_about_one_au_and_near_the_expected_declination() {
        // Near the December solstice the geocentric Sun has declination close to -23.4 deg.
        let t = Utc::from_ymd_hms(2026, 12, 21, 12, 0, 0.0).unwrap();
        let e = eop();
        let c = InHouseCandidate {
            cfg: InHouse {
                polar_motion: false,
            },
            eop: &e,
        };
        let s = c.sun_gcrf_km(t).unwrap();
        let v = Vector3::from(s);
        assert!(
            (v.norm() / AU_KM - 0.984).abs() < 0.005,
            "{}",
            v.norm() / AU_KM
        );
        assert!(((v.z / v.norm()).asin().to_degrees() + 23.4).abs() < 0.2);
    }

    #[test]
    fn unsupported_zonal_degree_and_bad_output_times_are_errors() {
        let e = eop();
        let mut c = InHouseCandidate {
            cfg: InHouse {
                polar_motion: false,
            },
            eop: &e,
        };
        let t0 = Utc::from_ymd_hms(2026, 10, 22, 0, 0, 0.0).unwrap();
        let s0 = circular_state(420.0, 51.6);
        let bad = NumericalSetup {
            zonal_degree: 9,
            drag_cd_a_over_m: None,
            density_scale: 1.0,
            high_fidelity: false,
        };
        assert!(matches!(
            c.propagate(t0, &s0, &[t0.plus(60.0)], &bad),
            Err(CandidateError::Unsupported(_))
        ));
        let ok = NumericalSetup {
            zonal_degree: 2,
            drag_cd_a_over_m: None,
            density_scale: 1.0,
            high_fidelity: false,
        };
        assert!(c.propagate(t0, &s0, &[], &ok).is_err());
        assert!(
            c.propagate(t0, &s0, &[t0.plus(60.0), t0.plus(30.0)], &ok)
                .is_err()
        );
    }
}
