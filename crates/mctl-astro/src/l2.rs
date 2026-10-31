//! Geometry of a Sun-Earth L2 vehicle, from its geocentric position and the Sun's direction.
//!
//! The frame is the usual halo-orbit one, centred on Earth: `x` points away from the Sun (so L2 is at
//! about +1.5 million km), `z` is ecliptic north and `y` completes a right-handed set. Seen from
//! Earth looking along +x, the vehicle traces its halo in the (y, z) plane.

use nalgebra::Vector3;

/// Mean obliquity of the ecliptic at J2000, degrees (changes 47" per century: irrelevant here).
const OBLIQUITY_J2000_DEG: f64 = 23.439_291;
/// Speed of light, km/s.
pub const C_KM_S: f64 = 299_792.458;

/// Unit vectors (x away from the Sun, y, z ecliptic north) of the rotating frame, in GCRF.
pub fn axes(sun_gcrf: &Vector3<f64>) -> [Vector3<f64>; 3] {
    let eps = OBLIQUITY_J2000_DEG.to_radians();
    let z = Vector3::new(0.0, -eps.sin(), eps.cos());
    let x = -sun_gcrf.normalize();
    // Remove the (tiny) out-of-ecliptic part of the Sun direction so the set is orthonormal.
    let x = (x - z * x.dot(&z)).normalize();
    let y = z.cross(&x);
    [x, y, z]
}

/// A GCRF vector expressed in the rotating frame.
pub fn to_rotating(v: &Vector3<f64>, sun_gcrf: &Vector3<f64>) -> [f64; 3] {
    let [x, y, z] = axes(sun_gcrf);
    [v.dot(&x), v.dot(&y), v.dot(&z)]
}

/// What the display shows about an L2 vehicle at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub dist_km: f64,
    pub speed_km_s: f64,
    /// d|r|/dt, positive when receding.
    pub range_rate_km_s: f64,
    /// One-way light time to Earth centre, seconds.
    pub light_time_s: f64,
    /// Angle at Earth between the anti-Sun direction and the vehicle, degrees.
    pub off_axis_deg: f64,
    /// Rotating-frame position, km.
    pub rot_km: [f64; 3],
}

pub fn summary(r: &Vector3<f64>, v: &Vector3<f64>, sun_gcrf: &Vector3<f64>) -> Summary {
    let dist = r.norm();
    let rot = to_rotating(r, sun_gcrf);
    Summary {
        dist_km: dist,
        speed_km_s: v.norm(),
        range_rate_km_s: r.dot(v) / dist,
        light_time_s: dist / C_KM_S,
        off_axis_deg: (rot[0] / dist).clamp(-1.0, 1.0).acos().to_degrees(),
        rot_km: rot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_are_orthonormal_and_x_points_away_from_the_sun() {
        let sun = Vector3::new(1.2e8, -7.0e7, -3.0e7);
        let [x, y, z] = axes(&sun);
        for (a, b) in [(x, y), (y, z), (z, x)] {
            assert!(a.dot(&b).abs() < 1e-12);
        }
        assert!((x.norm() - 1.0).abs() < 1e-12);
        assert!(x.dot(&sun) < 0.0);
        assert!((x.cross(&y) - z).norm() < 1e-12);
    }

    #[test]
    fn a_point_on_the_anti_sun_line_has_no_y_or_z() {
        // A Sun direction in the ecliptic plane (z_ecl . sun = 0), as it always is to ~1e-4 degrees.
        let eps = OBLIQUITY_J2000_DEG.to_radians();
        let sun = Vector3::new(1.0e8, 1.0e8, 1.0e8 * eps.tan());
        let r = -sun.normalize() * 1.5e6;
        let s = summary(&r, &Vector3::zeros(), &sun);
        assert!((s.dist_km - 1.5e6).abs() < 1e-6);
        assert!(s.off_axis_deg < 0.5, "{}", s.off_axis_deg);
        // Light time at 1.5 million km is about 5 seconds.
        assert!((s.light_time_s - 5.0).abs() < 0.05);
        assert!(s.rot_km[1].abs() < 1e-3 * s.dist_km);
    }

    #[test]
    fn range_rate_is_the_radial_speed() {
        let sun = Vector3::new(1.5e8, 0.0, 0.0);
        let r = Vector3::new(-1.5e6, 0.0, 0.0);
        let s = summary(&r, &Vector3::new(-0.3, 0.4, 0.0), &sun);
        assert!((s.range_rate_km_s - 0.3).abs() < 1e-12);
        assert!((s.speed_km_s - 0.5).abs() < 1e-12);
    }
}
