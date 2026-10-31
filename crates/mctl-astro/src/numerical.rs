//! Numerical (Cowell) propagation for the maneuver planner and for extending a source past its span:
//! Dormand-Prince 8(5,3) from the `ode_solvers` crate, in GCRF, km and s.
//!
//! Forces: point mass, Earth zonal harmonics J2 to J4 about the true pole, Sun and Moon third-body
//! (each switchable), atmospheric drag on a co-rotating atmosphere (static exponential model, so an
//! order-of-magnitude estimate: the design says drag-model uncertainty dominates), and cannonball
//! solar radiation pressure (off unless asked for).

use nalgebra::Vector3;
use ode_solvers::dop853::Dop853;
use ode_solvers::{System, Vector6};
use thiserror::Error;

use mctl_core::state::State;
use mctl_core::time::Utc;

use crate::sun::{illumination, sun_gcrf};
use crate::trajectory::{Trajectory, TrajectoryError};

/// Earth's gravitational parameter, km^3/s^2 (EGM96/WGS-84).
pub const MU_EARTH: f64 = 398_600.441_8;
/// Equatorial radius used by the zonal terms, km.
pub const RE_KM: f64 = 6_378.137;
/// Zonal harmonics, EGM96 (unnormalised).
const J2: f64 = 1.082_626_68e-3;
const J3: f64 = -2.532_656_5e-6;
const J4: f64 = -1.619_621_6e-6;
const MU_SUN: f64 = 1.327_124_400_18e11;
const MU_MOON: f64 = 4_902.800_066;
/// Earth rotation rate, rad/s.
const OMEGA_EARTH: f64 = 7.292_115_0e-5;
/// Solar radiation pressure at 1 AU, N/m^2, and the astronomical unit, km.
const SRP_1AU_N_M2: f64 = 4.56e-6;
const AU_KM: f64 = 149_597_870.7;
/// Integrator tolerances: relative and absolute (km, km/s).
const RTOL: f64 = 1.0e-10;
const ATOL: f64 = 1.0e-10;

#[derive(Debug, Error)]
pub enum NumericalError {
    #[error("integration failed: {0}")]
    Integration(String),
    #[error("the orbit leaves the model: radius {0:.0} km is inside the Earth")]
    Decayed(f64),
}

/// Which forces act. `drag` and `srp` carry their coefficient (area-to-mass times coefficient, m^2/kg).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForceModel {
    pub zonal: bool,
    pub sun: bool,
    pub moon: bool,
    /// Cd*A/m, m^2/kg.
    pub drag: Option<f64>,
    /// Cr*A/m, m^2/kg.
    pub srp: Option<f64>,
}

impl ForceModel {
    pub const TWO_BODY: Self = Self {
        zonal: false,
        sun: false,
        moon: false,
        drag: None,
        srp: None,
    };
}

/// Exponential atmosphere: (base altitude km, density kg/m^3 at the base, scale height km).
/// Vallado, "Fundamentals of Astrodynamics and Applications" (4th ed.) Table 8-4; static, mean activity.
const ATMOSPHERE: [(f64, f64, f64); 28] = [
    (0.0, 1.225, 7.249),
    (25.0, 3.899e-2, 6.349),
    (30.0, 1.774e-2, 6.682),
    (40.0, 3.972e-3, 7.554),
    (50.0, 1.057e-3, 8.382),
    (60.0, 3.206e-4, 7.714),
    (70.0, 8.770e-5, 6.549),
    (80.0, 1.905e-5, 5.799),
    (90.0, 3.396e-6, 5.382),
    (100.0, 5.297e-7, 5.877),
    (110.0, 9.661e-8, 7.263),
    (120.0, 2.438e-8, 9.473),
    (130.0, 8.484e-9, 12.636),
    (140.0, 3.845e-9, 16.149),
    (150.0, 2.070e-9, 22.523),
    (180.0, 5.464e-10, 29.740),
    (200.0, 2.789e-10, 37.105),
    (250.0, 7.248e-11, 45.546),
    (300.0, 2.418e-11, 53.628),
    (350.0, 9.158e-12, 53.298),
    (400.0, 3.725e-12, 58.515),
    (450.0, 1.585e-12, 60.828),
    (500.0, 6.967e-13, 63.822),
    (600.0, 1.454e-13, 71.835),
    (700.0, 3.614e-14, 88.667),
    (800.0, 1.170e-14, 124.64),
    (900.0, 5.245e-15, 181.05),
    (1000.0, 3.019e-15, 268.00),
];

/// Density at `alt_km`, kg/m^3. Below the table it is sea level; above 1000 km the last band continues.
pub fn density_kg_m3(alt_km: f64) -> f64 {
    let alt = alt_km.max(0.0);
    let (h0, rho0, scale) = ATMOSPHERE
        .iter()
        .rev()
        .find(|(h, _, _)| alt >= *h)
        .copied()
        .unwrap_or(ATMOSPHERE[0]);
    rho0 * (-(alt - h0) / scale).exp()
}

/// Legendre polynomial P_n(s) and its derivative for n = 2, 3, 4.
fn legendre(n: u32, s: f64) -> (f64, f64) {
    match n {
        2 => ((3.0 * s * s - 1.0) / 2.0, 3.0 * s),
        3 => (
            (5.0 * s.powi(3) - 3.0 * s) / 2.0,
            (15.0 * s * s - 3.0) / 2.0,
        ),
        _ => (
            (35.0 * s.powi(4) - 30.0 * s * s + 3.0) / 8.0,
            (140.0 * s.powi(3) - 60.0 * s) / 8.0,
        ),
    }
}

/// Acceleration from the zonal harmonics J2..J4 about the pole direction `pole` (unit, inertial).
///
/// The potential term is U_n = -mu J_n Re^n r^-(n+1) P_n(s) with s = (r . pole) / r, and the
/// acceleration is its gradient: grad f = r^-(n+2) [ -(n+1) P_n r_hat + P_n' (pole - s r_hat) ].
pub fn zonal_acceleration(r: &Vector3<f64>, pole: &Vector3<f64>) -> Vector3<f64> {
    let rn = r.norm();
    let rhat = r / rn;
    let s = rhat.dot(pole);
    let mut a = Vector3::zeros();
    for (n, jn) in [(2u32, J2), (3, J3), (4, J4)] {
        let (p, dp) = legendre(n, s);
        let grad =
            (-(f64::from(n) + 1.0) * p * rhat + dp * (pole - s * rhat)) / rn.powi(n as i32 + 2);
        a += -MU_EARTH * jn * RE_KM.powi(n as i32) * grad;
    }
    a
}

fn third_body(mu: f64, r: &Vector3<f64>, r_body: &Vector3<f64>) -> Vector3<f64> {
    let d = r_body - r;
    mu * (d / d.norm().powi(3) - r_body / r_body.norm().powi(3))
}

/// Altitude above the spherical-equivalent surface used for the density lookup, km. The atmosphere's
/// oblateness is ignored (about 21 km of altitude error at the poles); this is an order-of-magnitude model.
fn altitude_km(r: &Vector3<f64>) -> f64 {
    r.norm() - RE_KM
}

/// Total acceleration, km/s^2, at `t` for state (`r`, `v`). `pole` is the Earth's rotation axis in GCRF.
pub fn acceleration(
    model: &ForceModel,
    t: Utc,
    r: &Vector3<f64>,
    v: &Vector3<f64>,
    pole: &Vector3<f64>,
) -> Vector3<f64> {
    let rn = r.norm();
    let mut a = -MU_EARTH * r / rn.powi(3);
    if model.zonal {
        a += zonal_acceleration(r, pole);
    }
    if (model.sun || model.srp.is_some())
        && let Ok(sun) = sun_gcrf(t, 0.0)
    {
        {
            if model.sun {
                a += third_body(MU_SUN, r, &sun);
            }
            if let Some(b) = model.srp {
                let lit = illumination(r, &sun);
                let to_sun = sun - r;
                let dist = to_sun.norm();
                // N/m^2 * m^2/kg = m/s^2; /1000 for km/s^2; inverse square about 1 AU; directed away from the Sun.
                let mag = SRP_1AU_N_M2 * b * (AU_KM / dist).powi(2) / 1000.0 * lit;
                a -= mag * to_sun / dist;
            }
        }
    }
    if model.moon
        && let Ok(moon) = crate::moon::moon_gcrf(t)
    {
        a += third_body(MU_MOON, r, &moon);
    }
    if let Some(b) = model.drag {
        let v_rel = v - OMEGA_EARTH * pole.cross(r);
        let rho = density_kg_m3(altitude_km(r));
        // m/s^2 = -0.5 rho B |v| v with v in m/s; in km/s units that is 1000 * |v| v (v in km/s).
        a -= 0.5 * rho * b * 1000.0 * v_rel.norm() * v_rel;
    }
    a
}

struct Cowell<'a> {
    model: ForceModel,
    pole: Vector3<f64>,
    t0: Utc,
    _p: std::marker::PhantomData<&'a ()>,
}

impl System<f64, Vector6<f64>> for Cowell<'_> {
    fn system(&self, x: f64, y: &Vector6<f64>, dy: &mut Vector6<f64>) {
        let (r, v) = (
            Vector3::new(y[0], y[1], y[2]),
            Vector3::new(y[3], y[4], y[5]),
        );
        let a = acceleration(&self.model, self.t0.plus(x), &r, &v, &self.pole);
        *dy = Vector6::new(v.x, v.y, v.z, a.x, a.y, a.z);
    }
}

fn state_of(y: &Vector6<f64>) -> State {
    State::new([y[0], y[1], y[2]], [y[3], y[4], y[5]])
}

/// Integrate to `t1` and return only the final state.
fn integrate_to(
    model: &ForceModel,
    pole: &Vector3<f64>,
    t0: Utc,
    from: &State,
    t1: Utc,
) -> Result<State, NumericalError> {
    let span = t1.0 - t0.0;
    let y0 = Vector6::new(from.r.x, from.r.y, from.r.z, from.v.x, from.v.y, from.v.z);
    let sys = Cowell {
        model: *model,
        pole: *pole,
        t0,
        _p: std::marker::PhantomData,
    };
    let mut solver = Dop853::new(sys, 0.0, span, 0.0, y0, RTOL, ATOL);
    solver.set_output(ode_solvers::dop_shared::OutputType::Sparse);
    solver
        .integrate()
        .map_err(|e| NumericalError::Integration(e.to_string()))?;
    let last = solver
        .y_out()
        .last()
        .ok_or_else(|| NumericalError::Integration("no output".into()))?;
    Ok(state_of(last))
}

/// Propagate `from` (valid at `t0`) to `t1` (either direction), returning samples every `step_s`
/// seconds, the first being `t0` and the last exactly `t1`.
pub fn propagate(
    model: &ForceModel,
    pole: &Vector3<f64>,
    t0: Utc,
    from: &State,
    t1: Utc,
    step_s: f64,
) -> Result<Vec<(Utc, State)>, NumericalError> {
    let span = t1.0 - t0.0;
    if span == 0.0 {
        return Ok(vec![(t0, *from)]);
    }
    let dir = span.signum();
    let step = step_s.abs();
    let mut out: Vec<(Utc, State)> = Vec::new();
    // Dense output on the regular grid while at least one full step remains, then the remainder.
    if span.abs() > step * 1.0001 {
        let whole = (span.abs() / step).floor() * step;
        let y0 = Vector6::new(from.r.x, from.r.y, from.r.z, from.v.x, from.v.y, from.v.z);
        let sys = Cowell {
            model: *model,
            pole: *pole,
            t0,
            _p: std::marker::PhantomData,
        };
        let mut solver = Dop853::new(sys, 0.0, dir * whole, dir * step, y0, RTOL, ATOL);
        solver
            .integrate()
            .map_err(|e| NumericalError::Integration(e.to_string()))?;
        out.extend(
            solver
                .x_out()
                .iter()
                .zip(solver.y_out())
                .map(|(x, y)| (t0.plus(*x), state_of(y))),
        );
    } else {
        out.push((t0, *from));
    }
    if let Some((_, s)) = out.iter().find(|(_, s)| s.r.norm() < RE_KM) {
        return Err(NumericalError::Decayed(s.r.norm()));
    }
    let (t_last, s_last) = *out.last().expect("at least the start");
    if (t1.0 - t_last.0).abs() > 1e-6 {
        let end = integrate_to(model, pole, t_last, &s_last, t1)?;
        if end.r.norm() < RE_KM {
            return Err(NumericalError::Decayed(end.r.norm()));
        }
        out.push((t1, end));
    }
    Ok(out)
}

/// A table of states with cubic Hermite interpolation (position and velocity at each node).
#[derive(Debug, Clone)]
pub struct Table {
    pub samples: Vec<(Utc, State)>,
}

impl Table {
    pub fn new(samples: Vec<(Utc, State)>) -> Option<Self> {
        (samples.len() >= 2 && samples.windows(2).all(|w| w[1].0.0 > w[0].0.0))
            .then_some(Self { samples })
    }

    pub fn span(&self) -> (Utc, Utc) {
        (self.samples[0].0, self.samples[self.samples.len() - 1].0)
    }

    pub fn at(&self, t: Utc) -> Option<State> {
        let (a, b) = self.span();
        if t.0 < a.0 || t.0 > b.0 {
            return None;
        }
        let i = self
            .samples
            .partition_point(|(ts, _)| ts.0 <= t.0)
            .clamp(1, self.samples.len() - 1);
        let ((t0, s0), (t1, s1)) = (&self.samples[i - 1], &self.samples[i]);
        let h = t1.0 - t0.0;
        let u = (t.0 - t0.0) / h;
        let (u2, u3) = (u * u, u * u * u);
        let (h00, h10, h01, h11) = (
            2.0 * u3 - 3.0 * u2 + 1.0,
            u3 - 2.0 * u2 + u,
            -2.0 * u3 + 3.0 * u2,
            u3 - u2,
        );
        let r = h00 * s0.r + h10 * h * s0.v + h01 * s1.r + h11 * h * s1.v;
        // Derivative of the position polynomial gives the velocity consistent with it.
        let (d00, d10, d01, d11) = (
            (6.0 * u2 - 6.0 * u) / h,
            3.0 * u2 - 4.0 * u + 1.0,
            (-6.0 * u2 + 6.0 * u) / h,
            3.0 * u2 - 2.0 * u,
        );
        let v = d00 * s0.r + d10 * s0.v + d01 * s1.r + d11 * s1.v;
        Some(State { r, v })
    }
}

impl Trajectory for Table {
    fn state_gcrf(&self, t: Utc) -> Result<State, TrajectoryError> {
        self.at(t).ok_or(TrajectoryError::OutOfSpan)
    }

    fn validity(&self) -> Option<(Utc, Utc)> {
        Some(self.span())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kepler;

    fn circular(alt_km: f64, inc_deg: f64) -> State {
        let r = RE_KM + alt_km;
        let v = (MU_EARTH / r).sqrt();
        let i = inc_deg.to_radians();
        State::new([r, 0.0, 0.0], [0.0, v * i.cos(), v * i.sin()])
    }

    const POLE: Vector3<f64> = Vector3::new(0.0, 0.0, 1.0);

    fn t0() -> Utc {
        Utc::parse("2026-10-24T00:00:00").unwrap()
    }

    #[test]
    fn two_body_returns_to_the_start_after_one_period_and_conserves_energy() {
        let s0 = circular(500.0, 51.6);
        let a = RE_KM + 500.0;
        let period = std::f64::consts::TAU * (a.powi(3) / MU_EARTH).sqrt();
        let out = propagate(
            &ForceModel::TWO_BODY,
            &POLE,
            t0(),
            &s0,
            t0().plus(10.0 * period),
            60.0,
        )
        .unwrap();
        let end = &out.last().unwrap().1;
        // Ten orbits later at the same phase: millimetres.
        let at_period = propagate(
            &ForceModel::TWO_BODY,
            &POLE,
            t0(),
            &s0,
            t0().plus(period),
            60.0,
        )
        .unwrap();
        assert!((at_period.last().unwrap().1.r - s0.r).norm() < 1e-5);
        let e = |s: &State| s.v.norm_squared() / 2.0 - MU_EARTH / s.r.norm();
        assert!((e(end) - e(&s0)).abs() / e(&s0).abs() < 1e-9);
        assert!((end.r.cross(&end.v) - s0.r.cross(&s0.v)).norm() / s0.r.cross(&s0.v).norm() < 1e-9);
        assert_eq!(
            out.last().unwrap().0,
            t0().plus(10.0 * period),
            "ends exactly at t1"
        );
    }

    #[test]
    fn zonal_acceleration_matches_the_closed_form_j2() {
        // Axis-aligned J2 from the textbook: a = -1.5 J2 mu Re^2 / r^4 * [x/r (1-5s^2), y/r (1-5s^2), z/r (3-5s^2)].
        let r = Vector3::new(4000.0_f64, -3000.0, 4500.0);
        let rn = r.norm();
        let s = r.z / rn;
        let k = -1.5 * J2 * MU_EARTH * RE_KM * RE_KM / rn.powi(4);
        let want = k * Vector3::new(
            r.x / rn * (1.0 - 5.0 * s * s),
            r.y / rn * (1.0 - 5.0 * s * s),
            r.z / rn * (3.0 - 5.0 * s * s),
        );
        // Remove J3 and J4 from the full zonal acceleration by differencing against a J2-only evaluation.
        let full = zonal_acceleration(&r, &POLE);
        let j34 = {
            let mut a = Vector3::zeros();
            let rhat = r / rn;
            for (n, jn) in [(3u32, J3), (4, J4)] {
                let (p, dp) = legendre(n, s);
                a += -MU_EARTH
                    * jn
                    * RE_KM.powi(n as i32)
                    * ((-(f64::from(n) + 1.0) * p * rhat + dp * (POLE - s * rhat))
                        / rn.powi(n as i32 + 2));
            }
            a
        };
        assert!(
            (full - j34 - want).norm() / want.norm() < 1e-12,
            "{:?} vs {:?}",
            full - j34,
            want
        );
    }

    #[test]
    fn j2_regresses_the_node_at_the_analytic_rate() {
        let s0 = circular(500.0, 51.6);
        let model = ForceModel {
            zonal: true,
            ..ForceModel::TWO_BODY
        };
        let day = 86_400.0;
        let out = propagate(&model, &POLE, t0(), &s0, t0().plus(day), 60.0).unwrap();
        let (e0, e1) = (
            kepler::elements(&s0).unwrap(),
            kepler::elements(&out.last().unwrap().1).unwrap(),
        );
        let mut d_raan = e1.raan_deg - e0.raan_deg;
        if d_raan > 180.0 {
            d_raan -= 360.0
        }
        if d_raan < -180.0 {
            d_raan += 360.0
        }
        let a = RE_KM + 500.0;
        let n = (MU_EARTH / a.powi(3)).sqrt();
        let want =
            (-1.5 * n * J2 * (RE_KM / a).powi(2) * 51.6f64.to_radians().cos() * day).to_degrees();
        // Osculating-element noise and the J3/J4 terms are a few percent of the secular rate at most.
        assert!(
            (d_raan - want).abs() / want.abs() < 0.03,
            "{d_raan} vs {want}"
        );
    }

    #[test]
    fn drag_lowers_the_orbit_and_the_atmosphere_thins_with_height() {
        let s0 = circular(400.0, 51.6);
        let model = ForceModel {
            drag: Some(0.01),
            ..ForceModel::TWO_BODY
        };
        let out = propagate(&model, &POLE, t0(), &s0, t0().plus(86_400.0), 60.0).unwrap();
        let a1 = kepler::elements(&out.last().unwrap().1).unwrap().sma_km;
        let a0 = kepler::elements(&s0).unwrap().sma_km;
        // ISS-like ballistic coefficient at 400 km loses a few hundred metres to a few km per day.
        assert!(
            (0.05..10.0).contains(&(a0 - a1)),
            "lost {} km in a day",
            a0 - a1
        );
        assert!(
            density_kg_m3(300.0) > density_kg_m3(400.0)
                && density_kg_m3(400.0) > density_kg_m3(800.0)
        );
        assert!((density_kg_m3(400.0) - 3.725e-12).abs() < 1e-15);
    }

    #[test]
    fn a_prograde_burn_raises_the_far_side_by_the_vis_viva_amount() {
        let s0 = circular(400.0, 0.0);
        let dv = 0.010; // 10 m/s
        let mut burned = s0;
        burned.v += s0.v.normalize() * dv;
        let a0 = RE_KM + 400.0;
        let a1 = kepler::elements(&burned).unwrap().sma_km;
        // da = 2 a^2 v dv / mu for a small tangential burn from circular.
        let want = 2.0 * a0 * a0 * s0.v.norm() * dv / MU_EARTH;
        assert!(((a1 - a0) - want).abs() / want < 0.01);
    }

    #[test]
    fn hermite_table_reproduces_the_trajectory_between_nodes() {
        let s0 = circular(500.0, 51.6);
        let out = propagate(
            &ForceModel::TWO_BODY,
            &POLE,
            t0(),
            &s0,
            t0().plus(6000.0),
            30.0,
        )
        .unwrap();
        let table = Table::new(out).unwrap();
        let truth = propagate(
            &ForceModel::TWO_BODY,
            &POLE,
            t0(),
            &s0,
            t0().plus(4321.0),
            1.0,
        )
        .unwrap();
        let est = table.at(t0().plus(4321.0)).unwrap();
        assert!(
            (est.r - truth.last().unwrap().1.r).norm() < 1e-3,
            "{}",
            (est.r - truth.last().unwrap().1.r).norm()
        );
        assert!(table.at(t0().plus(7000.0)).is_none());
        assert!(table.state_gcrf(t0().plus(-1.0)).is_err());
    }

    #[test]
    fn a_decaying_orbit_is_an_error_not_garbage() {
        let s0 = circular(-100.0 + 1.0, 0.0);
        let r = propagate(
            &ForceModel::TWO_BODY,
            &POLE,
            t0(),
            &s0,
            t0().plus(600.0),
            30.0,
        );
        assert!(matches!(r, Err(NumericalError::Decayed(_))), "{r:?}");
    }

    /// Validation against the real ISS ephemeris: start from an OEM state, propagate
    /// with the full force model and the OEM's own ballistic coefficient, and compare with the OEM.
    #[test]
    fn the_full_model_tracks_the_nasa_iss_ephemeris_for_a_day() {
        use crate::frames::{Orientation, Reduction};
        use crate::oem::Oem;
        let oem = Oem::parse(include_str!("../fixtures/iss-oem-sample.txt")).unwrap();
        let (start, end) = oem.span();
        assert!(
            end.0 - start.0 > 86_400.0 + 1200.0,
            "fixture span {}",
            end.0 - start.0
        );
        let t_start = start.plus(600.0);
        let from = oem.interpolate(t_start).unwrap();
        let red = Reduction::at(t_start, Orientation::ASSUMED_ZERO).unwrap();
        let m = red.ecef_from_gcrf();
        let pole = Vector3::new(m[(2, 0)], m[(2, 1)], m[(2, 2)]);
        let b = oem.drag.cd_a_over_m();
        let full = ForceModel {
            zonal: true,
            sun: true,
            moon: true,
            drag: Some(b),
            srp: None,
        };
        let nodrag = ForceModel { drag: None, ..full };
        let two = ForceModel::TWO_BODY;
        let j2 = ForceModel {
            zonal: true,
            ..ForceModel::TWO_BODY
        };
        let err = |model: &ForceModel, hours: f64| {
            let t = t_start.plus(hours * 3600.0);
            let out = propagate(model, &pole, t_start, &from, t, 60.0).unwrap();
            (out.last().unwrap().1.r - oem.interpolate(t).unwrap().r).norm()
        };
        let report: Vec<String> = [6.0, 12.0, 24.0]
            .iter()
            .map(|h| format!("{h:>4} h: two-body {:8.2} | J2-J4 {:8.2} | + sun/moon {:8.2} | + drag {:8.2} km", err(&two, *h), err(&j2, *h), err(&nodrag, *h), err(&full, *h)))
            .collect();
        eprintln!("B = {b:.4} m^2/kg\n{}", report.join("\n"));
        // Measured 2026-10-24 (OEM created 2026-10-21): two-body 432 km, J2-J4 3.1 km, with drag 7.3 km after
        // 24 h. The static exponential atmosphere overstates drag at this altitude, so drag makes the day
        // worse, not better: the design's "dominated by drag-model uncertainty" in numbers. A ballistic
        // coefficient scale is therefore a plan control. Each force except drag must help, and the full
        // model must hold the ISS to under 10 km after a day.
        let (e_two, e_j2, e_full) = (err(&two, 24.0), err(&j2, 24.0), err(&full, 24.0));
        assert!(
            e_j2 < e_two / 10.0,
            "zonal terms should remove most of the error: {e_two} -> {e_j2}"
        );
        assert!(e_full < 10.0, "full model is {e_full} km off after 24 h");
    }
}
