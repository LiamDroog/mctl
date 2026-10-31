//! Reference-frame reductions: TEME -> GCRF -> ITRF.
//!
//! IAU-76 precession with a 10-term IAU-80 nutation series and no frame bias: about 1.4 m from the
//! full IAU 2006/2000A chain at LEO (measured against satkit and brahe in M0, ADR 0001), which is
//! irrelevant next to SGP4's own ~1 km. Polar motion is applied from EOP when available (11.5 m effect).

use std::f64::consts::{PI, TAU};

use mctl_core::state::State;
use mctl_core::time::{
    JD_MJD_OFFSET, SECONDS_PER_DAY, TT_MINUS_TAI_S, TimeError, Utc, tai_minus_utc,
};
use nalgebra::{Matrix3, Vector3};

/// Earth rotation rate, rad/s (IERS 1996 nominal).
pub const OMEGA_EARTH_RAD_S: f64 = 7.292_115_146_706_979e-5;
const JD_J2000: f64 = 2_451_545.0;
const DAYS_PER_CENTURY: f64 = 36_525.0;
const ARCSEC_TO_RAD: f64 = PI / (180.0 * 3600.0);

/// Passive rotation about axis 1 by `a` (Vallado's ROT1).
fn rot1(a: f64) -> Matrix3<f64> {
    let (s, c) = a.sin_cos();
    Matrix3::new(1.0, 0.0, 0.0, 0.0, c, s, 0.0, -s, c)
}
fn rot2(a: f64) -> Matrix3<f64> {
    let (s, c) = a.sin_cos();
    Matrix3::new(c, 0.0, -s, 0.0, 1.0, 0.0, s, 0.0, c)
}
pub fn rot3(a: f64) -> Matrix3<f64> {
    let (s, c) = a.sin_cos();
    Matrix3::new(c, s, 0.0, -s, c, 0.0, 0.0, 0.0, 1.0)
}

/// Julian centuries of TT since J2000 for a UTC instant.
pub fn centuries_tt(t: Utc) -> Result<f64, TimeError> {
    let tt_minus_utc = tai_minus_utc(t)? + TT_MINUS_TAI_S;
    Ok((t.jd_utc() + tt_minus_utc / SECONDS_PER_DAY - JD_J2000) / DAYS_PER_CENTURY)
}

/// Greenwich mean sidereal time, rad (IAU-82, Vallado).
pub fn gmst(jd_ut1: f64) -> f64 {
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

/// Mean-of-date to J2000 rotation.
pub fn mod_to_j2000(t: f64) -> Matrix3<f64> {
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

/// Leading 10 terms of the IAU-80 nutation series: multipliers of (l, l', F, D, Omega), then
/// dpsi0, dpsi1, deps0, deps1 in 1e-4 arcsec (and per century). The omitted 96 terms bound the error at a few mas.
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

fn mean_obliquity(t: f64) -> f64 {
    (84_381.448 - 46.8150 * t - 0.00059 * t * t + 0.001_813 * t * t * t) * ARCSEC_TO_RAD
}

/// Earth orientation as used for one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Orientation {
    /// UT1 - UTC, seconds.
    pub dut1_s: f64,
    /// Polar motion, arcseconds.
    pub xp_as: f64,
    pub yp_as: f64,
    /// True when EOP was unavailable and zeros were used (assumed zero, and flagged).
    pub assumed: bool,
}

impl Orientation {
    /// Zero UT1-UTC and polar motion, flagged as assumed.
    pub const ASSUMED_ZERO: Self = Self {
        dut1_s: 0.0,
        xp_as: 0.0,
        yp_as: 0.0,
        assumed: true,
    };
}

/// All frame rotations for one instant.
#[derive(Debug, Clone, Copy)]
pub struct Reduction {
    pub gmst_rad: f64,
    pub teme_to_gcrf: Matrix3<f64>,
    /// Unit vector of the true pole of date, in GCRF.
    pub pole_gcrf: Vector3<f64>,
    orientation: Orientation,
}

impl Reduction {
    pub fn at(t: Utc, orientation: Orientation) -> Result<Self, TimeError> {
        let tc = centuries_tt(t)?;
        let jd_ut1 = t.jd_utc() + orientation.dut1_s / SECONDS_PER_DAY;
        let (dpsi, deps) = nutation(tc);
        let eps_mean = mean_obliquity(tc);
        let eps_true = eps_mean + deps;
        let omega = fundamental_args(tc)[4];
        // Equation of the equinoxes, with the two small post-1997 terms (Vallado).
        let eq_eq = dpsi * eps_mean.cos()
            + (0.002_64 * omega.sin() + 0.000_063 * (2.0 * omega).sin()) * ARCSEC_TO_RAD;
        let n = rot1(-eps_true) * rot3(-dpsi) * rot1(eps_mean); // MOD -> TOD
        let p = mod_to_j2000(tc);
        Ok(Self {
            gmst_rad: gmst(jd_ut1),
            teme_to_gcrf: p * n.transpose() * rot3(-eq_eq),
            pole_gcrf: p * n.transpose() * Vector3::z(),
            orientation,
        })
    }

    pub fn teme_to_gcrf(&self, s: &State) -> State {
        State {
            r: self.teme_to_gcrf * s.r,
            v: self.teme_to_gcrf * s.v,
        }
    }

    /// Polar-motion matrix (TIRS -> ITRF).
    fn polar(&self) -> Matrix3<f64> {
        rot2(-self.orientation.xp_as * ARCSEC_TO_RAD)
            * rot1(-self.orientation.yp_as * ARCSEC_TO_RAD)
    }

    /// Rotation of positions from GCRF to ITRF.
    pub fn ecef_from_gcrf(&self) -> Matrix3<f64> {
        self.polar() * rot3(self.gmst_rad) * self.teme_to_gcrf.transpose()
    }

    /// GCRF state to ITRF, including the Earth-rotation term in the velocity.
    pub fn gcrf_to_itrf(&self, s: &State) -> State {
        let to_pef = rot3(self.gmst_rad) * self.teme_to_gcrf.transpose();
        let r_pef = to_pef * s.r;
        let v_pef = to_pef * s.v - Vector3::new(0.0, 0.0, OMEGA_EARTH_RAD_S).cross(&r_pef);
        let w = self.polar();
        State {
            r: w * r_pef,
            v: w * v_pef,
        }
    }
}

pub fn mjd_utc_to_jd(mjd: f64) -> f64 {
    mjd + JD_MJD_OFFSET
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red() -> Reduction {
        Reduction::at(
            Utc::from_ymd_hms(2026, 10, 23, 12, 0, 0.0).unwrap(),
            Orientation {
                dut1_s: -0.044,
                xp_as: 0.148,
                yp_as: 0.317,
                assumed: false,
            },
        )
        .unwrap()
    }

    #[test]
    fn rotations_are_orthonormal() {
        let r = red();
        let m = r.ecef_from_gcrf();
        assert!((m * m.transpose() - Matrix3::identity()).norm() < 1e-14);
        assert!((m.determinant() - 1.0).abs() < 1e-14);
        assert!((r.pole_gcrf.norm() - 1.0).abs() < 1e-14);
    }

    #[test]
    fn gmst_at_j2000_matches_the_known_value() {
        // GMST at 2000-01-01 12:00 UT1 is 18h 41m 50.548s = 280.4606 deg.
        assert!((gmst(JD_J2000).to_degrees() - 280.4606).abs() < 1e-3);
    }

    #[test]
    fn itrf_velocity_removes_earth_rotation() {
        // A point fixed to the Earth moves in GCRF at omega x r; in ITRF its velocity must be ~0.
        let r = red();
        let fixed_itrf = Vector3::new(4000.0, 3000.0, 4500.0);
        let r_gcrf = r.ecef_from_gcrf().transpose() * fixed_itrf;
        let w_pole = r.pole_gcrf;
        let v_gcrf = OMEGA_EARTH_RAD_S * w_pole.cross(&r_gcrf);
        let s = r.gcrf_to_itrf(&State {
            r: r_gcrf,
            v: v_gcrf,
        });
        assert!((s.r - fixed_itrf).norm() < 1e-9);
        assert!(s.v.norm() < 1e-6, "residual ITRF speed {} km/s", s.v.norm());
    }

    #[test]
    fn assumed_orientation_is_flagged_and_zero() {
        let o = Orientation::ASSUMED_ZERO;
        assert!(std::hint::black_box(o).assumed);
        assert_eq!((o.dut1_s, o.xp_as, o.yp_as), (0.0, 0.0, 0.0));
    }
}
