//! Geocentric Sun position (analytic; arcminute level is sufficient) and the
//! conical shadow model.

use mctl_core::time::Utc;
use nalgebra::Vector3;

use crate::frames::{centuries_tt, mod_to_j2000};

/// Mean Earth-Sun distance, km (IAU 2012 astronomical unit).
pub const AU_KM: f64 = 149_597_870.7;
/// Solar radius, km (IAU 2015 nominal).
pub const SOLAR_RADIUS_KM: f64 = 695_700.0;
/// Earth equatorial radius, km (WGS-84).
pub const EARTH_RADIUS_KM: f64 = 6378.137;

/// Geocentric Sun position in GCRF, km: Vallado Algorithm 29 (apparent direction), precessed to J2000.
///
/// Measured against JPL Horizons in M0: 15.8 arcsec max (apparent direction).
pub fn sun_gcrf(t: Utc, dut1_s: f64) -> Result<Vector3<f64>, mctl_core::time::TimeError> {
    let t_tt = centuries_tt(t)?;
    let t_ut1 = (t.jd_utc() + dut1_s / mctl_core::time::SECONDS_PER_DAY - 2_451_545.0) / 36_525.0;
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
    Ok(mod_to_j2000(t_tt) * r_mod)
}

/// Fraction of the Sun's disc visible from `r_sat` (km, geocentric): 1 sunlit, 0 umbra, between = penumbra.
///
/// Conical model: Earth and Sun as discs of angular radius asin(R/d) seen from the satellite; the
/// visible fraction is one minus the overlap of the two discs.
pub fn illumination(r_sat: &Vector3<f64>, r_sun: &Vector3<f64>) -> f64 {
    let to_sun = r_sun - r_sat;
    let (d_e, d_s) = (r_sat.norm(), to_sun.norm());
    if d_e <= EARTH_RADIUS_KM {
        return 0.0;
    }
    let rho_e = (EARTH_RADIUS_KM / d_e).asin();
    let rho_s = (SOLAR_RADIUS_KM / d_s).asin();
    // Angular separation of the two disc centres as seen from the satellite.
    let theta = ((-r_sat).dot(&to_sun) / (d_e * d_s))
        .clamp(-1.0, 1.0)
        .acos();
    if theta >= rho_e + rho_s {
        return 1.0;
    }
    if theta <= rho_e - rho_s {
        return 0.0;
    }
    if theta <= rho_s - rho_e {
        // Annular: Earth entirely inside the Sun's disc.
        return 1.0 - (rho_e / rho_s).powi(2);
    }
    // Lens-shaped overlap of two circular discs.
    let (a, b) = (rho_e, rho_s);
    let x = (theta * theta + a * a - b * b) / (2.0 * theta * a);
    let y = (theta * theta + b * b - a * a) / (2.0 * theta * b);
    let overlap = a * a * x.clamp(-1.0, 1.0).acos() + b * b * y.clamp(-1.0, 1.0).acos()
        - 0.5
            * ((-theta + a + b) * (theta + a - b) * (theta - a + b) * (theta + a + b))
                .max(0.0)
                .sqrt();
    (1.0 - overlap / (std::f64::consts::PI * b * b)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_is_about_one_au_and_near_the_solstice_declination() {
        let t = Utc::from_ymd_hms(2026, 12, 21, 12, 0, 0.0).unwrap();
        let s = sun_gcrf(t, 0.0).unwrap();
        assert!((s.norm() / AU_KM - 0.984).abs() < 0.005);
        assert!(((s.z / s.norm()).asin().to_degrees() + 23.4).abs() < 0.2);
    }

    #[test]
    fn shadow_geometry_limits() {
        let sun = Vector3::new(AU_KM, 0.0, 0.0);
        let r = EARTH_RADIUS_KM + 420.0;
        assert_eq!(
            illumination(&Vector3::new(r, 0.0, 0.0), &sun),
            1.0,
            "sub-solar point is sunlit"
        );
        assert_eq!(
            illumination(&Vector3::new(-r, 0.0, 0.0), &sun),
            0.0,
            "anti-solar LEO point is in umbra"
        );
        assert_eq!(
            illumination(&Vector3::new(0.0, r, 0.0), &sun),
            1.0,
            "terminator-crossing 90 degrees away is lit (LEO shadow is a narrow cone)"
        );
        // Sweeping around the anti-solar side, illumination changes monotonically from 1 to 0 through penumbra.
        let mut prev = 1.0;
        let mut saw_penumbra = false;
        for k in 0..400 {
            let ang = (90.0 + 90.0 * k as f64 / 400.0).to_radians();
            let v = illumination(&Vector3::new(r * ang.cos(), r * ang.sin(), 0.0), &sun);
            assert!(v <= prev + 1e-9, "non-monotonic at step {k}");
            saw_penumbra |= v > 0.01 && v < 0.99;
            prev = v;
        }
        assert!(saw_penumbra, "a LEO satellite crosses a thin penumbra");
        assert_eq!(prev, 0.0);
    }

    #[test]
    fn inside_the_earth_is_dark() {
        assert_eq!(
            illumination(
                &Vector3::new(1000.0, 0.0, 0.0),
                &Vector3::new(AU_KM, 0.0, 0.0)
            ),
            0.0
        );
    }
}
