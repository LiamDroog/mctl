//! Orbit events by root finding: ascending and descending nodes, apsides and
//! eclipse entry and exit. Sampled event functions are bracketed, then bisected.

use mctl_core::time::Utc;
use mctl_core::world::EventKind;
use nalgebra::Vector3;

use crate::sun::{illumination, sun_gcrf};
use crate::trajectory::Trajectory;

/// Precision of event times, seconds.
pub const TIME_TOLERANCE_S: f64 = 0.01;
/// Shadow-function level taken as the eclipse boundary: half the Sun's disc visible.
const ECLIPSE_LEVEL: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbitEvent {
    pub t: Utc,
    pub kind: EventKind,
}

/// The four event functions at `t`: z (nodes), r.v (apsides), illumination - level (eclipse).
fn functions(traj: &dyn Trajectory, t: Utc) -> Option<[f64; 3]> {
    let s = traj.state_gcrf(t).ok()?;
    let sun = sun_gcrf(t, 0.0).ok()?;
    Some([
        s.r.z,
        s.r.dot(&s.v),
        illumination(&s.r, &Vector3::from(sun)) - ECLIPSE_LEVEL,
    ])
}

/// Events in `[t0, t1]`, sorted by time. `step_s` must be well under half the shortest interval
/// between events of one kind; period / 200 is the design default.
pub fn find_events(traj: &dyn Trajectory, t0: Utc, t1: Utc, step_s: f64) -> Vec<OrbitEvent> {
    assert!(
        step_s > 0.0 && t1.0 > t0.0,
        "need a positive step and window"
    );
    let mut out = Vec::new();
    let mut prev = functions(traj, t0).map(|f| (t0, f));
    let mut t = t0;
    while t.0 < t1.0 {
        let next = Utc((t.0 + step_s).min(t1.0));
        let cur = functions(traj, next).map(|f| (next, f));
        if let (Some((ta, fa)), Some((tb, fb))) = (prev, cur) {
            for (i, (a, b)) in fa.iter().zip(fb.iter()).enumerate() {
                let rising = *a < 0.0 && *b >= 0.0;
                let falling = *a >= 0.0 && *b < 0.0;
                let falling_strict = *a > 0.0 && *b <= 0.0;
                let kind = match (i, rising, falling, falling_strict) {
                    (0, true, _, _) => EventKind::AscendingNode,
                    (0, _, _, true) => EventKind::DescendingNode,
                    (1, true, _, _) => EventKind::Periapsis,
                    (1, _, _, true) => EventKind::Apoapsis,
                    (2, _, true, _) => EventKind::EclipseEntry,
                    (2, true, _, _) => EventKind::EclipseExit,
                    _ => continue,
                };
                let (mut lo, mut hi) = (ta, tb);
                let sign = |t: Utc| functions(traj, t).map(|f| f[i] >= 0.0);
                let lo_sign = sign(lo);
                if lo_sign.is_none() {
                    continue;
                }
                while hi.minus(lo) > TIME_TOLERANCE_S {
                    let mid = Utc(0.5 * (lo.0 + hi.0));
                    match sign(mid) {
                        Some(s) if Some(s) == lo_sign => lo = mid,
                        Some(_) => hi = mid,
                        None => break,
                    }
                }
                out.push(OrbitEvent {
                    t: Utc(0.5 * (lo.0 + hi.0)),
                    kind,
                });
            }
        }
        prev = cur;
        t = next;
    }
    out.sort_by(|a, b| a.t.0.total_cmp(&b.t.0));
    out
}

#[cfg(test)]
mod tests {
    use mctl_core::state::State;
    use nalgebra::Rotation3;

    use super::*;
    use crate::kepler::MU_KM3_S2;
    use crate::trajectory::TrajectoryError;

    /// Analytic two-body orbit with known event times: periapsis at `t_peri`, ascending node where the
    /// argument of latitude is zero.
    struct TwoBody {
        a: f64,
        e: f64,
        rot: Rotation3<f64>,
        t_peri: f64,
        n: f64,
    }

    impl TwoBody {
        fn new(a: f64, e: f64, inc_deg: f64, raan_deg: f64, argp_deg: f64, t_peri: f64) -> Self {
            let rot = Rotation3::from_axis_angle(&Vector3::z_axis(), raan_deg.to_radians())
                * Rotation3::from_axis_angle(&Vector3::x_axis(), inc_deg.to_radians())
                * Rotation3::from_axis_angle(&Vector3::z_axis(), argp_deg.to_radians());
            Self {
                a,
                e,
                rot,
                t_peri,
                n: (MU_KM3_S2 / (a * a * a)).sqrt(),
            }
        }

        fn true_anomaly(&self, t: f64) -> f64 {
            let m = self.n * (t - self.t_peri);
            let mut ea = m;
            for _ in 0..30 {
                ea -= (ea - self.e * ea.sin() - m) / (1.0 - self.e * ea.cos());
            }
            2.0 * (((1.0 + self.e) / (1.0 - self.e)).sqrt() * (ea / 2.0).tan()).atan()
        }
    }

    impl Trajectory for TwoBody {
        fn state_gcrf(&self, t: Utc) -> Result<State, TrajectoryError> {
            let nu = self.true_anomaly(t.0);
            let p = self.a * (1.0 - self.e * self.e);
            let r_pf = p / (1.0 + self.e * nu.cos()) * Vector3::new(nu.cos(), nu.sin(), 0.0);
            let v_pf = (MU_KM3_S2 / p).sqrt() * Vector3::new(-nu.sin(), self.e + nu.cos(), 0.0);
            Ok(State {
                r: self.rot * r_pf,
                v: self.rot * v_pf,
            })
        }

        fn validity(&self) -> Option<(Utc, Utc)> {
            None
        }
    }

    #[test]
    fn apsides_and_nodes_are_found_at_their_analytic_times_within_a_tenth_of_a_second() {
        // Argument of perigee 90 degrees: periapsis is at the top of the orbit, so the ascending node
        // comes a quarter-revolution of true anomaly (270 degrees) after periapsis.
        let t_peri = 1.0e9;
        let o = TwoBody::new(7_000.0, 0.1, 51.6, 40.0, 90.0, t_peri);
        let period = std::f64::consts::TAU / o.n;
        let ev = find_events(
            &o,
            Utc(t_peri - 10.0),
            Utc(t_peri + 2.0 * period),
            period / 200.0,
        );
        let of = |k: EventKind| -> Vec<f64> {
            ev.iter().filter(|e| e.kind == k).map(|e| e.t.0).collect()
        };
        let peri = of(EventKind::Periapsis);
        let apo = of(EventKind::Apoapsis);
        assert_eq!(peri.len(), 2, "{ev:?}");
        assert!(
            (peri[0] - t_peri).abs() < 0.1 && (peri[1] - (t_peri + period)).abs() < 0.1,
            "{peri:?}"
        );
        assert!(
            (apo[0] - (t_peri + period / 2.0)).abs() < 0.1,
            "apoapsis half a period later: {apo:?}"
        );
        // Time from periapsis to true anomaly nu via Kepler's equation.
        let time_at = |nu: f64| {
            let ea = 2.0 * (((1.0 - o.e) / (1.0 + o.e)).sqrt() * (nu / 2.0).tan()).atan();
            let m = ea - o.e * ea.sin();
            t_peri + m.rem_euclid(std::f64::consts::TAU) / o.n
        };
        let an = of(EventKind::AscendingNode);
        let dn = of(EventKind::DescendingNode);
        assert!(
            (an[0] - time_at(270f64.to_radians())).abs() < 0.1,
            "AN {:?} vs {}",
            an,
            time_at(270f64.to_radians())
        );
        assert!(
            (dn[0] - time_at(90f64.to_radians())).abs() < 0.1,
            "DN {:?} vs {}",
            dn,
            time_at(90f64.to_radians())
        );
        assert!(
            dn[0] < an[0],
            "descending node (nu = 90) comes before the ascending node (nu = 270)"
        );
    }

    #[test]
    fn events_are_sorted_and_each_kind_alternates() {
        let o = TwoBody::new(6_800.0, 0.0007, 51.6, 10.0, 20.0, 1.0e9);
        let period = std::f64::consts::TAU / o.n;
        let ev = find_events(&o, Utc(1.0e9), Utc(1.0e9 + 3.0 * period), period / 200.0);
        assert!(ev.windows(2).all(|w| w[0].t.0 <= w[1].t.0));
        for (a, b) in [
            (EventKind::AscendingNode, EventKind::DescendingNode),
            (EventKind::Periapsis, EventKind::Apoapsis),
        ] {
            let seq: Vec<EventKind> = ev
                .iter()
                .map(|e| e.kind)
                .filter(|k| *k == a || *k == b)
                .collect();
            assert!(
                seq.len() >= 5 && seq.windows(2).all(|w| w[0] != w[1]),
                "{a:?}/{b:?} alternate: {seq:?}"
            );
        }
    }

    #[test]
    fn a_trajectory_gap_ends_the_search_there_instead_of_inventing_events() {
        struct Gappy(TwoBody);
        impl Trajectory for Gappy {
            fn state_gcrf(&self, t: Utc) -> Result<State, TrajectoryError> {
                if t.0 > 1.0e9 + 1_000.0 {
                    Err(TrajectoryError::OutOfSpan)
                } else {
                    self.0.state_gcrf(t)
                }
            }
            fn validity(&self) -> Option<(Utc, Utc)> {
                None
            }
        }
        let g = Gappy(TwoBody::new(6_800.0, 0.0007, 51.6, 10.0, 20.0, 1.0e9));
        let ev = find_events(&g, Utc(1.0e9), Utc(1.0e9 + 20_000.0), 30.0);
        assert!(ev.iter().all(|e| e.t.0 <= 1.0e9 + 1_000.0 + 1.0), "{ev:?}");
    }

    #[test]
    fn eclipse_entry_and_exit_alternate_and_are_found_for_a_leo_orbit() {
        // Orbit plane facing the Sun at the epoch would not eclipse; use a plane that does, over a few revs.
        let t0 = Utc::parse("2026-03-20T00:00:00").unwrap(); // near equinox: eclipses on every orbit
        let o = TwoBody::new(6_800.0, 0.0007, 51.6, 0.0, 0.0, t0.0);
        let period = std::f64::consts::TAU / o.n;
        let ev = find_events(&o, t0, t0.plus(4.0 * period), period / 200.0);
        let seq: Vec<EventKind> = ev
            .iter()
            .map(|e| e.kind)
            .filter(|k| matches!(k, EventKind::EclipseEntry | EventKind::EclipseExit))
            .collect();
        assert!(seq.len() >= 4, "{seq:?}");
        assert!(
            seq.windows(2).all(|w| w[0] != w[1]),
            "entry and exit alternate: {seq:?}"
        );
    }
}
