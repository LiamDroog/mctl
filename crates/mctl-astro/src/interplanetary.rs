//! Geometry of a vehicle far from Earth (Voyager, New Horizons), from its geocentric state and the Sun's.
//!
//! JPL Horizons gives geocentric vectors, so the heliocentric position is the vehicle's minus the Sun's, and
//! the heliocentric velocity needs the Sun's velocity relative to Earth (Earth's own, reversed). Positions are
//! then rotated to the J2000 ecliptic, where a solar-system plot and an ecliptic longitude make sense.

use nalgebra::Vector3;

use crate::l2::{C_KM_S, OBLIQUITY_J2000_DEG};
use crate::sun::AU_KM;

/// A GCRF vector expressed in the J2000 mean ecliptic frame (x to the vernal equinox, z ecliptic north).
pub fn to_ecliptic(v: &Vector3<f64>) -> Vector3<f64> {
    let (s, c) = OBLIQUITY_J2000_DEG.to_radians().sin_cos();
    Vector3::new(v.x, c * v.y + s * v.z, -s * v.y + c * v.z)
}

/// What the display shows about a deep-space vehicle at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    /// Distance from the Sun's centre, km.
    pub sun_dist_km: f64,
    /// Distance from Earth's centre, km.
    pub earth_dist_km: f64,
    /// Speed relative to the Sun, km/s.
    pub helio_speed_km_s: f64,
    /// d|r|/dt relative to Earth, km/s, positive when receding.
    pub range_rate_km_s: f64,
    /// One-way light time to Earth, seconds.
    pub light_time_s: f64,
    /// Heliocentric ecliptic longitude and latitude, degrees (longitude 0 to 360).
    pub ecl_lon_deg: f64,
    pub ecl_lat_deg: f64,
    /// Heliocentric position in the J2000 ecliptic frame, km.
    pub helio_ecl_km: [f64; 3],
}

/// `sun_geo` and `sun_vel_geo` are the Sun's position (km) and velocity (km/s) relative to Earth.
pub fn summary(
    r_geo: &Vector3<f64>,
    v_geo: &Vector3<f64>,
    sun_geo: &Vector3<f64>,
    sun_vel_geo: &Vector3<f64>,
) -> Summary {
    let helio = r_geo - sun_geo;
    let helio_v = v_geo - sun_vel_geo;
    let ecl = to_ecliptic(&helio);
    let earth_dist = r_geo.norm();
    Summary {
        sun_dist_km: helio.norm(),
        earth_dist_km: earth_dist,
        helio_speed_km_s: helio_v.norm(),
        range_rate_km_s: r_geo.dot(v_geo) / earth_dist,
        light_time_s: earth_dist / C_KM_S,
        ecl_lon_deg: ecl.y.atan2(ecl.x).to_degrees().rem_euclid(360.0),
        ecl_lat_deg: (ecl.z / ecl.norm()).clamp(-1.0, 1.0).asin().to_degrees(),
        helio_ecl_km: [ecl.x, ecl.y, ecl.z],
    }
}

/// Kilometres in an astronomical unit, for the display.
pub const KM_PER_AU: f64 = AU_KM;

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    #[test]
    fn the_ecliptic_pole_is_tilted_from_the_equatorial_pole_by_the_obliquity() {
        let pole_ecl = to_ecliptic(&Vector3::new(
            0.0,
            -OBLIQUITY_J2000_DEG.to_radians().sin(),
            OBLIQUITY_J2000_DEG.to_radians().cos(),
        ));
        assert!((pole_ecl - Vector3::new(0.0, 0.0, 1.0)).norm() < EPS);
        // The equinox direction is the same in both.
        assert!((to_ecliptic(&Vector3::x()) - Vector3::x()).norm() < EPS);
    }

    #[test]
    fn heliocentric_values_remove_the_sun_and_the_earth_motion() {
        // Earth 1 AU from the Sun on the ecliptic x axis, moving +30 km/s along y (so the Sun moves -30).
        let sun = Vector3::new(-AU_KM, 0.0, 0.0);
        let sun_v = Vector3::new(0.0, -30.0, 0.0);
        // A craft 100 AU out on the ecliptic y axis (written in GCRF), heliocentrically moving 17 km/s straight outward.
        let (se, ce) = OBLIQUITY_J2000_DEG.to_radians().sin_cos();
        let helio_r = Vector3::new(0.0, 100.0 * AU_KM * ce, 100.0 * AU_KM * se);
        let helio_v = Vector3::new(0.0, 17.0, 0.0);
        let r_geo = helio_r + sun;
        let v_geo = helio_v + sun_v;
        let s = summary(&r_geo, &v_geo, &sun, &sun_v);
        assert!((s.sun_dist_km / AU_KM - 100.0).abs() < 1e-9);
        assert!((s.helio_speed_km_s - 17.0).abs() < 1e-9);
        assert!((s.ecl_lon_deg - 90.0).abs() < 1e-9);
        assert!(s.ecl_lat_deg.abs() < 1e-9);
        // From Earth it is a little over 100 AU away, and the light takes about 13.9 hours.
        assert!((s.earth_dist_km / AU_KM - 100.005).abs() < 0.01);
        assert!((s.light_time_s / 3600.0 - 13.87).abs() < 0.05);
    }

    #[test]
    fn longitude_wraps_into_0_to_360_and_latitude_is_signed() {
        let zero = Vector3::zeros();
        let s = summary(&Vector3::new(1.0e9, -1.0e9, 0.0), &zero, &zero, &zero);
        assert!(
            s.ecl_lon_deg > 300.0 && s.ecl_lon_deg < 360.0,
            "{}",
            s.ecl_lon_deg
        );
        let s = summary(&Vector3::new(1.0e9, 0.0, -1.0e9), &zero, &zero, &zero);
        assert!(s.ecl_lat_deg < 0.0);
    }

    #[test]
    fn range_rate_is_positive_when_receding_from_earth() {
        let zero = Vector3::zeros();
        let r = Vector3::new(1.0e9, 0.0, 0.0);
        let s = summary(&r, &Vector3::new(5.0, 0.0, 0.0), &zero, &zero);
        assert!((s.range_rate_km_s - 5.0).abs() < EPS);
        let s = summary(&r, &Vector3::new(-5.0, 0.0, 0.0), &zero, &zero);
        assert!(s.range_rate_km_s < 0.0);
    }
}
