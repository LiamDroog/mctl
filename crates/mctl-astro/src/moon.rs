//! Geocentric Moon position, analytic (arcminute level is sufficient). Vallado
//! Algorithm 31, the truncated lunar series, precessed to J2000.

use mctl_core::time::Utc;
use nalgebra::Vector3;

use crate::frames::{centuries_tt, mod_to_j2000};
use crate::sun::EARTH_RADIUS_KM;

/// Geocentric Moon position in GCRF, km.
pub fn moon_gcrf(t: Utc) -> Result<Vector3<f64>, mctl_core::time::TimeError> {
    let tt = centuries_tt(t)?;
    let d = |a: f64, b: f64| (a + b * tt).to_radians();
    let lambda = (218.32 + 481_267.881_3 * tt + 6.29 * d(134.9, 477_198.85).sin()
        - 1.27 * d(259.2, -413_335.38).sin()
        + 0.66 * d(235.7, 890_534.23).sin()
        + 0.21 * d(269.9, 954_397.70).sin()
        - 0.19 * d(357.5, 35_999.05).sin()
        - 0.11 * d(186.6, 966_404.05).sin())
    .to_radians();
    let phi = (5.13 * d(93.3, 483_202.03).sin() + 0.28 * d(228.2, 960_400.87).sin()
        - 0.28 * d(318.3, 6_003.18).sin()
        - 0.17 * d(217.6, -407_332.20).sin())
    .to_radians();
    let parallax = (0.9508
        + 0.0518 * d(134.9, 477_198.85).cos()
        + 0.0095 * d(259.2, -413_335.38).cos()
        + 0.0078 * d(235.7, 890_534.23).cos()
        + 0.0028 * d(269.9, 954_397.70).cos())
    .to_radians();
    let eps = (23.439_291 - 0.013_004_2 * tt).to_radians();
    let r = EARTH_RADIUS_KM / parallax.sin();
    let r_mod = r * Vector3::new(
        phi.cos() * lambda.cos(),
        eps.cos() * phi.cos() * lambda.sin() - eps.sin() * phi.sin(),
        eps.sin() * phi.cos() * lambda.sin() + eps.cos() * phi.sin(),
    );
    Ok(mod_to_j2000(tt) * r_mod)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JPL Horizons geocentric ICRF vectors of the Moon (km), fetched 2026-10-23 for these UTC epochs.
    const HORIZONS: [(&str, [f64; 3]); 5] = [
        (
            "2026-01-01T00:00:00",
            [
                1.443_257_274_919_273e5,
                2.895_841_575_449_329e5,
                1.601_589_230_016_842e5,
            ],
        ),
        (
            "2026-04-10T06:00:00",
            [
                1.399_473_813_328_118e5,
                -3.323_488_081_229_235e5,
                -1.729_069_185_193_463e5,
            ],
        ),
        (
            "2026-07-20T12:00:00",
            [
                -3.744_321_916_151_715e5,
                -9.023_943_499_130_209e4,
                -6.719_284_959_958_088e4,
            ],
        ),
        (
            "2026-10-23T18:00:00",
            [
                3.794_746_591_544_031e5,
                -1.570_235_931_761_262e4,
                1.206_938_579_186_284e4,
            ],
        ),
        (
            "2027-02-02T03:00:00",
            [
                -4.628_360_333_438_645e4,
                -3.558_174_511_372_232e5,
                -1.883_148_120_851_699e5,
            ],
        ),
    ];

    #[test]
    fn matches_horizons_to_the_accuracy_of_the_truncated_series() {
        for (when, truth) in HORIZONS {
            let m = moon_gcrf(Utc::parse(when).unwrap()).unwrap();
            let t = Vector3::from(truth);
            let ang = m.angle(&t).to_degrees();
            let dist = (m.norm() - t.norm()).abs() / t.norm();
            // Vallado's series is good to a fraction of a degree; this is what it actually achieves.
            assert!(ang < 0.35, "{when}: {ang} deg off");
            assert!(dist < 0.01, "{when}: distance off by {:.2} %", dist * 100.0);
        }
    }

    #[test]
    fn the_distance_stays_within_the_lunar_perigee_and_apogee() {
        let t0 = Utc::parse("2026-01-01T00:00:00").unwrap();
        for k in 0..60 {
            let r = moon_gcrf(t0.plus(f64::from(k) * 86_400.0)).unwrap().norm();
            assert!((356_000.0..=407_500.0).contains(&r), "day {k}: {r}");
        }
    }
}
