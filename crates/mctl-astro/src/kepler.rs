//! Osculating elements, beta angle, and geodetic conversion.

use mctl_core::state::{Elements, Geodetic, State};
use nalgebra::Vector3;

use crate::sun::EARTH_RADIUS_KM;

/// Earth GM, km^3/s^2 (WGS-84).
pub const MU_KM3_S2: f64 = 398_600.441_8;
/// WGS-84 flattening.
pub const FLATTENING: f64 = 1.0 / 298.257_223_563;
const TAU: f64 = std::f64::consts::TAU;

/// Threshold below which an orbit is treated as circular / equatorial when choosing angle references.
const SMALL: f64 = 1.0e-9;

/// Classical elements from an inertial state. `None` for degenerate or unbound states.
pub fn elements(s: &State) -> Option<Elements> {
    let (r, v) = (s.r, s.v);
    let (rn, vn) = (r.norm(), v.norm());
    if rn < 1.0 || vn < 1.0e-6 {
        return None;
    }
    let h = r.cross(&v);
    let hn = h.norm();
    if hn < 1.0e-9 {
        return None;
    }
    let n_vec = Vector3::z().cross(&h);
    let nn = n_vec.norm();
    let e_vec = ((vn * vn - MU_KM3_S2 / rn) * r - r.dot(&v) * v) / MU_KM3_S2;
    let e = e_vec.norm();
    let energy = vn * vn / 2.0 - MU_KM3_S2 / rn;
    if energy >= 0.0 {
        return None; // not bound
    }
    let a = -MU_KM3_S2 / (2.0 * energy);
    let inc = (h.z / hn).clamp(-1.0, 1.0).acos();
    let angle = |from: &Vector3<f64>, to: &Vector3<f64>, flip: bool| {
        let a = (from.dot(to) / (from.norm() * to.norm()))
            .clamp(-1.0, 1.0)
            .acos();
        if flip { TAU - a } else { a }
    };
    let equatorial = nn < SMALL * hn;
    let circular = e < SMALL;
    let raan = if equatorial {
        0.0
    } else {
        angle(&Vector3::x(), &n_vec, n_vec.y < 0.0)
    };
    let argp = match (circular, equatorial) {
        (true, _) => 0.0,
        (false, true) => {
            let w = e_vec.y.atan2(e_vec.x);
            (if h.z < 0.0 { TAU - w } else { w }).rem_euclid(TAU)
        }
        (false, false) => angle(&n_vec, &e_vec, e_vec.z < 0.0),
    };
    let nu = if circular {
        // Argument of latitude (from the node, or from +x if equatorial).
        if equatorial {
            let l = r.y.atan2(r.x);
            (if h.z < 0.0 { TAU - l } else { l }).rem_euclid(TAU)
        } else {
            angle(&n_vec, &r, r.z < 0.0)
        }
    } else {
        angle(&e_vec, &r, r.dot(&v) < 0.0)
    };
    // Mean anomaly via eccentric anomaly.
    let ecc_anom = 2.0 * (((1.0 - e) / (1.0 + e)).sqrt() * (nu / 2.0).tan()).atan();
    let mean = (ecc_anom - e * ecc_anom.sin()).rem_euclid(TAU);
    Some(Elements {
        sma_km: a,
        ecc: e,
        inc_deg: inc.to_degrees(),
        raan_deg: raan.to_degrees(),
        argp_deg: argp.to_degrees(),
        true_anom_deg: nu.to_degrees(),
        mean_anom_deg: mean.to_degrees(),
        period_s: TAU * (a.powi(3) / MU_KM3_S2).sqrt(),
        apo_alt_km: a * (1.0 + e) - EARTH_RADIUS_KM,
        peri_alt_km: a * (1.0 - e) - EARTH_RADIUS_KM,
    })
}

/// Beta angle: the angle between the orbit plane and the Sun direction, degrees (signed).
pub fn beta_deg(s: &State, sun_gcrf_km: &Vector3<f64>) -> Option<f64> {
    let h = s.r.cross(&s.v);
    (h.norm() > 1.0e-9 && sun_gcrf_km.norm() > 0.0).then(|| {
        (h.normalize().dot(&sun_gcrf_km.normalize()))
            .clamp(-1.0, 1.0)
            .asin()
            .to_degrees()
    })
}

/// WGS-84 geodetic coordinates of an ITRF position (km), by fixed-point iteration on latitude.
pub fn geodetic(r_itrf: &Vector3<f64>) -> Geodetic {
    let e2 = FLATTENING * (2.0 - FLATTENING);
    let p = r_itrf.x.hypot(r_itrf.y);
    let mut lat = r_itrf.z.atan2(p * (1.0 - e2));
    let mut n = EARTH_RADIUS_KM;
    for _ in 0..8 {
        n = EARTH_RADIUS_KM / (1.0 - e2 * lat.sin().powi(2)).sqrt();
        lat = (r_itrf.z + e2 * n * lat.sin()).atan2(p);
    }
    let alt = if lat.cos().abs() > 1.0e-6 {
        p / lat.cos() - n
    } else {
        r_itrf.z.abs() / lat.sin().abs() - n * (1.0 - e2)
    };
    Geodetic {
        lat_deg: lat.to_degrees(),
        lon_deg: r_itrf.y.atan2(r_itrf.x).to_degrees(),
        alt_km: alt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_elements(a: f64, e: f64, i: f64, raan: f64, argp: f64, nu: f64) -> State {
        // Perifocal -> inertial, textbook construction.
        let p = a * (1.0 - e * e);
        let r_pf = p / (1.0 + e * nu.cos()) * Vector3::new(nu.cos(), nu.sin(), 0.0);
        let v_pf = (MU_KM3_S2 / p).sqrt() * Vector3::new(-nu.sin(), e + nu.cos(), 0.0);
        let rot = nalgebra::Rotation3::from_axis_angle(&Vector3::z_axis(), raan)
            * nalgebra::Rotation3::from_axis_angle(&Vector3::x_axis(), i)
            * nalgebra::Rotation3::from_axis_angle(&Vector3::z_axis(), argp);
        State {
            r: rot * r_pf,
            v: rot * v_pf,
        }
    }

    #[test]
    fn elements_roundtrip_a_general_orbit() {
        let s = from_elements(
            7000.0,
            0.05,
            51.6f64.to_radians(),
            40f64.to_radians(),
            70f64.to_radians(),
            100f64.to_radians(),
        );
        let e = elements(&s).unwrap();
        assert!((e.sma_km - 7000.0).abs() < 1e-6 && (e.ecc - 0.05).abs() < 1e-9);
        assert!(
            (e.inc_deg - 51.6).abs() < 1e-9
                && (e.raan_deg - 40.0).abs() < 1e-9
                && (e.argp_deg - 70.0).abs() < 1e-9
                && (e.true_anom_deg - 100.0).abs() < 1e-9
        );
        assert!((e.apo_alt_km - (7000.0 * 1.05 - EARTH_RADIUS_KM)).abs() < 1e-6);
        assert!((e.period_s - TAU * (7000.0f64.powi(3) / MU_KM3_S2).sqrt()).abs() < 1e-6);
        assert!(e.mean_anom_deg > 0.0 && e.mean_anom_deg < 360.0);
    }

    #[test]
    fn retrograde_and_degenerate_orbits_do_not_produce_nonsense() {
        let s = from_elements(
            7000.0,
            0.1,
            120f64.to_radians(),
            300f64.to_radians(),
            200f64.to_radians(),
            250f64.to_radians(),
        );
        let e = elements(&s).unwrap();
        assert!(
            (e.inc_deg - 120.0).abs() < 1e-9
                && (e.raan_deg - 300.0).abs() < 1e-8
                && (e.argp_deg - 200.0).abs() < 1e-8
                && (e.true_anom_deg - 250.0).abs() < 1e-8
        );
        let circ_eq = State::new([7000.0, 0.0, 0.0], [0.0, (MU_KM3_S2 / 7000.0).sqrt(), 0.0]);
        let e = elements(&circ_eq).unwrap();
        assert!(
            e.ecc < 1e-9
                && e.inc_deg.abs() < 1e-9
                && e.true_anom_deg.abs() < 1e-6
                && e.raan_deg == 0.0
                && e.argp_deg == 0.0
        );
        assert!(elements(&State::new([7000.0, 0.0, 0.0], [0.0, 0.0, 0.0])).is_none());
        assert!(
            elements(&State::new([7000.0, 0.0, 0.0], [0.0, 20.0, 0.0])).is_none(),
            "unbound"
        );
        assert!(
            elements(&State::new([7000.0, 0.0, 0.0], [7.0, 0.0, 0.0])).is_none(),
            "radial"
        );
    }

    #[test]
    fn beta_is_zero_in_plane_and_ninety_normal() {
        let s = State::new([7000.0, 0.0, 0.0], [0.0, 7.5, 0.0]);
        assert!(beta_deg(&s, &Vector3::new(1.0e8, 0.0, 0.0)).unwrap().abs() < 1e-12);
        assert!((beta_deg(&s, &Vector3::new(0.0, 0.0, 1.0e8)).unwrap() - 90.0).abs() < 1e-9);
        assert!((beta_deg(&s, &Vector3::new(0.0, 0.0, -1.0e8)).unwrap() + 90.0).abs() < 1e-9);
    }

    #[test]
    fn geodetic_known_points() {
        let g = geodetic(&Vector3::new(EARTH_RADIUS_KM + 400.0, 0.0, 0.0));
        assert!(
            g.lat_deg.abs() < 1e-9 && g.lon_deg.abs() < 1e-9 && (g.alt_km - 400.0).abs() < 1e-6
        );
        let b = 6_356.752_314_245;
        let pole = geodetic(&Vector3::new(0.0, 0.0, b + 100.0));
        assert!(
            (pole.lat_deg - 90.0).abs() < 1e-9 && (pole.alt_km - 100.0).abs() < 1e-6,
            "{pole:?}"
        );
        let g = geodetic(&Vector3::new(0.0, -(EARTH_RADIUS_KM + 10.0), 0.0));
        assert!((g.lon_deg + 90.0).abs() < 1e-9);
        // Round-trip a mid-latitude point through the forward formula.
        let (lat, lon, h) = (38.883f64.to_radians(), -77.0163f64.to_radians(), 0.010);
        let e2 = FLATTENING * (2.0 - FLATTENING);
        let n = EARTH_RADIUS_KM / (1.0 - e2 * lat.sin().powi(2)).sqrt();
        let r = Vector3::new(
            (n + h) * lat.cos() * lon.cos(),
            (n + h) * lat.cos() * lon.sin(),
            (n * (1.0 - e2) + h) * lat.sin(),
        );
        let g = geodetic(&r);
        assert!(
            (g.lat_deg - 38.883).abs() < 1e-9
                && (g.lon_deg + 77.0163).abs() < 1e-9
                && (g.alt_km - h).abs() < 1e-6
        );
    }
}
