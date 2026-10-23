//! Observer geometry and pass prediction (DESIGN.md FR-14, FR-15, §7.3).
//!
//! Look angles use the WGS-84 ellipsoid and the ITRF position of the vehicle. Passes are found by
//! sampling elevation at a step that cannot skip a pass, then bisecting each crossing of the mask.

use mctl_core::time::Utc;
use nalgebra::{Matrix3, Vector3};

use crate::frames::{Orientation, Reduction};
use crate::kepler::FLATTENING;
use crate::sun::EARTH_RADIUS_KM;
use crate::trajectory::Trajectory;

/// Precision of AOS/LOS/TCA times, seconds.
pub const TIME_TOLERANCE_S: f64 = 0.01;
/// Sun elevation below which the sky is dark enough to see a lit satellite: civil twilight (-6 deg).
/// A labelled convention, not a physical constant.
pub const DARK_SKY_SUN_ELEVATION_DEG: f64 = -6.0;
/// A satellite counts as lit when at least this fraction of the Sun's disc is visible from it.
pub const LIT_FRACTION: f64 = 0.99;

#[derive(Debug, Clone, PartialEq)]
pub struct Site {
    pub name: String,
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_m: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub az_deg: f64,
    pub el_deg: f64,
    pub range_km: f64,
}

impl Site {
    /// ITRF position, km.
    pub fn ecef_km(&self) -> Vector3<f64> {
        let (lat, lon) = (self.lat_deg.to_radians(), self.lon_deg.to_radians());
        let e2 = FLATTENING * (2.0 - FLATTENING);
        let n = EARTH_RADIUS_KM / (1.0 - e2 * lat.sin().powi(2)).sqrt();
        let h = self.alt_m / 1000.0;
        Vector3::new(
            (n + h) * lat.cos() * lon.cos(),
            (n + h) * lat.cos() * lon.sin(),
            (n * (1.0 - e2) + h) * lat.sin(),
        )
    }

    /// Rotation from ITRF to the local east-north-up frame.
    pub fn enu_from_ecef(&self) -> Matrix3<f64> {
        let (lat, lon) = (self.lat_deg.to_radians(), self.lon_deg.to_radians());
        Matrix3::new(
            -lon.sin(),
            lon.cos(),
            0.0,
            -lat.sin() * lon.cos(),
            -lat.sin() * lon.sin(),
            lat.cos(),
            lat.cos() * lon.cos(),
            lat.cos() * lon.sin(),
            lat.sin(),
        )
    }

    /// Azimuth (from north, clockwise), elevation and range of an ITRF position, km.
    pub fn look(&self, r_itrf_km: &Vector3<f64>) -> Look {
        let enu = self.enu_from_ecef() * (r_itrf_km - self.ecef_km());
        let range = enu.norm();
        Look {
            az_deg: enu.x.atan2(enu.y).to_degrees().rem_euclid(360.0),
            el_deg: (enu.z / range).asin().to_degrees(),
            range_km: range,
        }
    }

    /// Elevation of a GCRF direction (the Sun) at this site, degrees. Needs the same reduction used for the vehicle.
    pub fn elevation_of_direction(&self, dir_gcrf: &Vector3<f64>, red: &Reduction) -> f64 {
        let enu = self.enu_from_ecef() * (red.ecef_from_gcrf() * dir_gcrf.normalize());
        enu.z.asin().to_degrees()
    }
}

/// Look angles of `traj` from `site` at `t`; `None` where the trajectory has no state.
pub fn look_at(
    traj: &dyn Trajectory,
    site: &Site,
    t: Utc,
    orientation: Orientation,
) -> Option<Look> {
    let gcrf = traj.state_gcrf(t).ok()?;
    let red = Reduction::at(t, orientation).ok()?;
    Some(site.look(&(red.ecef_from_gcrf() * gcrf.r)))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pass {
    pub aos: Utc,
    pub tca: Utc,
    pub los: Utc,
    pub max_el_deg: f64,
    pub aos_az_deg: f64,
    pub tca_az_deg: f64,
    pub los_az_deg: f64,
    /// The vehicle was already above the mask at the start of the search window; `aos` is the window start.
    pub started_before_window: bool,
    /// The vehicle is still above the mask at the end of the window; `los` is the window end.
    pub ends_after_window: bool,
}

impl Pass {
    pub fn duration_s(&self) -> f64 {
        self.los.minus(self.aos)
    }
}

/// Passes of a vehicle over a site within `[t0, t1]` with elevation at or above `mask_deg`.
///
/// `look` returns the look angles at a time, or `None` where the vehicle has none (outside a span);
/// gaps end any pass in progress. `step_s` must be shorter than the shortest pass worth finding.
pub fn find_passes(
    look: &dyn Fn(Utc) -> Option<Look>,
    t0: Utc,
    t1: Utc,
    step_s: f64,
    mask_deg: f64,
) -> Vec<Pass> {
    assert!(
        step_s > 0.0 && t1.0 > t0.0,
        "need a positive step and window"
    );
    let f = |t: Utc| look(t).map(|l| l.el_deg - mask_deg);
    // Bisect for the time in [lo, hi] where f crosses zero, given f(lo) and f(hi) of opposite sign
    // (a missing value counts as below the mask).
    let cross = |mut lo: Utc, mut hi: Utc| -> Utc {
        let above = |t: Utc| f(t).is_some_and(|v| v >= 0.0);
        let lo_above = above(lo);
        while hi.minus(lo) > TIME_TOLERANCE_S {
            let mid = Utc(0.5 * (lo.0 + hi.0));
            if above(mid) == lo_above {
                lo = mid
            } else {
                hi = mid
            }
        }
        Utc(0.5 * (lo.0 + hi.0))
    };
    let mut out = Vec::new();
    let mut t = t0;
    let mut prev_above = f(t0).is_some_and(|v| v >= 0.0);
    let mut aos: Option<(Utc, bool)> = prev_above.then_some((t0, true));
    while t.0 < t1.0 {
        let next = Utc((t.0 + step_s).min(t1.0));
        let now_above = f(next).is_some_and(|v| v >= 0.0);
        if now_above != prev_above {
            let c = cross(t, next);
            if now_above {
                aos = Some((c, false));
            } else if let Some((a, before)) = aos.take() {
                out.push(finish(look, a, c, before, false));
            }
        }
        prev_above = now_above;
        t = next;
    }
    if let Some((a, before)) = aos {
        out.push(finish(look, a, t1, before, true));
    }
    out
}

/// Locate the culmination between `aos` and `los` and fill in the pass.
fn finish(
    look: &dyn Fn(Utc) -> Option<Look>,
    aos: Utc,
    los: Utc,
    before: bool,
    after: bool,
) -> Pass {
    let el = |t: Utc| look(t).map_or(f64::NEG_INFINITY, |l| l.el_deg);
    // Golden-section search for the maximum; elevation over one pass is unimodal.
    let phi = 0.5 * (5f64.sqrt() - 1.0);
    let (mut a, mut b) = (aos.0, los.0);
    let (mut c, mut d) = (b - phi * (b - a), a + phi * (b - a));
    while b - a > TIME_TOLERANCE_S {
        if el(Utc(c)) > el(Utc(d)) {
            b = d
        } else {
            a = c
        }
        c = b - phi * (b - a);
        d = a + phi * (b - a);
    }
    let tca = Utc(0.5 * (a + b));
    let az = |t: Utc| look(t).map_or(f64::NAN, |l| l.az_deg);
    Pass {
        aos,
        tca,
        los,
        max_el_deg: el(tca),
        aos_az_deg: az(aos),
        tca_az_deg: az(tca),
        los_az_deg: az(los),
        started_before_window: before,
        ends_after_window: after,
    }
}

/// Whether a pass is visible to the eye: the sky is dark at the site and the vehicle is in sunlight.
pub fn visible(site_sun_elevation_deg: f64, vehicle_illumination: f64) -> bool {
    site_sun_elevation_deg <= DARK_SKY_SUN_ELEVATION_DEG && vehicle_illumination >= LIT_FRACTION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sgp4model::GpModel;

    const OMM: &str = include_str!("../fixtures/iss-omm.json");

    fn site(lat: f64, lon: f64, alt_m: f64) -> Site {
        Site {
            name: "t".into(),
            lat_deg: lat,
            lon_deg: lon,
            alt_m,
        }
    }

    #[test]
    fn directly_overhead_is_ninety_degrees_at_the_altitude() {
        let s = site(0.0, 0.0, 0.0);
        let l = s.look(&Vector3::new(EARTH_RADIUS_KM + 400.0, 0.0, 0.0));
        assert!(
            (l.el_deg - 90.0).abs() < 1e-9 && (l.range_km - 400.0).abs() < 1e-6,
            "{l:?}"
        );
    }

    #[test]
    fn cardinal_directions_have_the_right_azimuth() {
        let s = site(0.0, 0.0, 0.0);
        let base = s.ecef_km();
        // ITRF at (lat 0, lon 0): +y is east, +z is north, +x is up.
        for (d, az) in [
            (Vector3::new(0.0, 0.0, 100.0), 0.0),
            (Vector3::new(0.0, 100.0, 0.0), 90.0),
            (Vector3::new(0.0, 0.0, -100.0), 180.0),
            (Vector3::new(0.0, -100.0, 0.0), 270.0),
        ] {
            let l = s.look(&(base + d));
            assert!(
                (l.az_deg - az).abs() < 1e-6 && l.el_deg.abs() < 1e-6,
                "{az}: {l:?}"
            );
        }
    }

    #[test]
    fn site_position_matches_known_geodesy() {
        // Equator and pole radii of WGS-84.
        assert!((site(0.0, 0.0, 0.0).ecef_km().norm() - 6378.137).abs() < 1e-6);
        assert!((site(90.0, 0.0, 0.0).ecef_km().norm() - 6_356.752_314).abs() < 1e-3);
        assert!(
            (site(0.0, 90.0, 1000.0).ecef_km().y - 6379.137).abs() < 1e-6,
            "altitude adds radially"
        );
        // Round trip with the geodetic conversion the engine uses.
        let g = crate::kepler::geodetic(&site(51.05, -114.07, 1045.0).ecef_km());
        assert!(
            (g.lat_deg - 51.05).abs() < 1e-9
                && (g.lon_deg + 114.07).abs() < 1e-9
                && (g.alt_km - 1.045).abs() < 1e-9
        );
    }

    #[test]
    fn synthetic_passes_have_exact_crossings_and_culminations() {
        // Elevation = 40 sin(2 pi t / 1000) + 5 degrees: above a 10 degree mask for part of each cycle.
        let look = |t: Utc| {
            Some(Look {
                az_deg: 0.0,
                el_deg: 40.0 * (std::f64::consts::TAU * t.0 / 1000.0).sin() + 5.0,
                range_km: 1.0,
            })
        };
        let p = find_passes(&look, Utc(0.0), Utc(2500.0), 10.0, 10.0);
        // sin(x) = 0.125: first crossing at asin(0.125)/2pi*1000, culmination at 250.
        let x0 = 0.125f64.asin() / std::f64::consts::TAU * 1000.0;
        assert_eq!(p.len(), 3);
        assert!(
            (p[0].aos.0 - x0).abs() < 0.02 && (p[0].los.0 - (500.0 - x0)).abs() < 0.02,
            "{:?}",
            p[0]
        );
        assert!((p[0].tca.0 - 250.0).abs() < 0.05 && (p[0].max_el_deg - 45.0).abs() < 1e-3);
        assert!(!p[0].started_before_window && !p[0].ends_after_window);
        assert!(
            p[2].ends_after_window || (p[2].los.0 < 2500.0),
            "{:?}",
            p[2]
        );
    }

    #[test]
    fn a_pass_in_progress_at_the_window_edges_is_flagged_not_dropped() {
        let look = |t: Utc| {
            Some(Look {
                az_deg: 0.0,
                el_deg: 40.0 * (std::f64::consts::TAU * t.0 / 1000.0).sin(),
                range_km: 1.0,
            })
        };
        let p = find_passes(&look, Utc(100.0), Utc(700.0), 5.0, 0.0);
        assert_eq!(p.len(), 1);
        assert!(
            p[0].started_before_window && p[0].aos.0 == 100.0 && (p[0].los.0 - 500.0).abs() < 0.02
        );
        let p = find_passes(&look, Utc(0.5), Utc(300.0), 5.0, 0.0);
        assert!(p[0].ends_after_window && p[0].los.0 == 300.0);
    }

    #[test]
    fn missing_data_ends_a_pass_instead_of_inventing_one() {
        let look = |t: Utc| {
            (t.0 < 200.0).then_some(Look {
                az_deg: 0.0,
                el_deg: 30.0,
                range_km: 1.0,
            })
        };
        let p = find_passes(&look, Utc(0.0), Utc(400.0), 5.0, 0.0);
        assert_eq!(p.len(), 1);
        assert!((p[0].los.0 - 200.0).abs() < 0.02, "{:?}", p[0]);
    }

    #[test]
    fn a_day_of_real_iss_passes_over_a_mid_latitude_site_is_physically_plausible() {
        let gp = GpModel::from_omm_json(OMM).unwrap();
        let s = site(51.05, -114.07, 1045.0);
        let t0 = gp.epoch;
        let look = |t: Utc| look_at(&gp, &s, t, Orientation::ASSUMED_ZERO);
        let mask = 10.0;
        let p = find_passes(&look, t0, t0.plus(86_400.0), 20.0, mask);
        assert!((2..=8).contains(&p.len()), "{} passes in a day", p.len());
        let mut last = t0.0;
        for x in &p {
            assert!(
                x.aos.0 >= last && x.aos.0 < x.tca.0 && x.tca.0 < x.los.0,
                "ordered: {x:?}"
            );
            last = x.los.0;
            assert!(x.max_el_deg >= mask - 0.01 && x.max_el_deg <= 90.0);
            assert!(
                look(x.aos).unwrap().el_deg >= mask - 0.05
                    && look(x.aos).unwrap().el_deg < mask + 0.5,
                "AOS is on the mask"
            );
            if !x.started_before_window && !x.ends_after_window {
                assert!(
                    x.duration_s() > 30.0 && x.duration_s() < 720.0,
                    "{} s",
                    x.duration_s()
                );
            }
            // The culmination really is a maximum.
            for dt in [-5.0, 5.0] {
                assert!(look(x.tca.plus(dt)).unwrap().el_deg <= x.max_el_deg + 1e-6);
            }
        }
        // A finer step finds the same passes, so the coarse step skipped none.
        let fine = find_passes(&look, t0, t0.plus(86_400.0), 5.0, mask);
        assert_eq!(fine.len(), p.len());
    }

    #[test]
    fn visibility_needs_a_dark_sky_and_a_lit_vehicle() {
        assert!(visible(-12.0, 1.0));
        assert!(!visible(10.0, 1.0), "daytime");
        assert!(!visible(-12.0, 0.0), "vehicle in shadow");
        assert!(!visible(-3.0, 1.0), "twilight is too bright");
    }

    /// Prints the passes for the fixture so they can be compared with an independent implementation:
    /// `cargo test -p mctl-astro print_fixture_passes -- --ignored --nocapture`.
    #[test]
    #[ignore = "diagnostic output for an external cross-check"]
    fn print_fixture_passes() {
        let gp = GpModel::from_omm_json(OMM).unwrap();
        let s = site(51.05, -114.07, 1045.0);
        let look = |t: Utc| look_at(&gp, &s, t, Orientation::ASSUMED_ZERO);
        println!("EPOCH {}", gp.epoch.iso());
        for p in find_passes(&look, gp.epoch, gp.epoch.plus(86_400.0), 5.0, 10.0) {
            println!(
                "PASS {} {} {} {:.3}",
                p.aos.0, p.tca.0, p.los.0, p.max_el_deg
            );
        }
    }
}
