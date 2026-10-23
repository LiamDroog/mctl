//! Shared state type and the local-frame maths used to report residuals.

use nalgebra::{Matrix3, Vector3};

/// Inertial or Earth-fixed state, kilometres and kilometres per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct State {
    pub r: Vector3<f64>,
    pub v: Vector3<f64>,
}

impl State {
    pub fn new(r: [f64; 3], v: [f64; 3]) -> Self {
        Self {
            r: Vector3::from(r),
            v: Vector3::from(v),
        }
    }
}

/// Rotation taking inertial vectors into the radial / transverse(along-track) / normal frame of `reference`.
///
/// R = r/|r|, N = (r x v)/|r x v|, T = N x R. This is the CCSDS RTN convention (a.k.a. RSW, RIC).
pub fn rtn_matrix(reference: &State) -> Matrix3<f64> {
    let rhat = reference.r.normalize();
    let nhat = reference.r.cross(&reference.v).normalize();
    let that = nhat.cross(&rhat);
    Matrix3::from_rows(&[rhat.transpose(), that.transpose(), nhat.transpose()])
}

/// Residual `test - reference` of positions, expressed in the RTN frame of `reference`, kilometres.
pub fn rtn_residual(reference: &State, test: &State) -> Vector3<f64> {
    rtn_matrix(reference) * (test.r - reference.r)
}

/// Summary statistics of a set of 3-vectors, in metres.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct ResidualStats {
    pub samples: usize,
    pub rms_3d_m: f64,
    pub max_3d_m: f64,
    pub max_abs_r_m: f64,
    pub max_abs_t_m: f64,
    pub max_abs_n_m: f64,
    pub final_r_m: f64,
    pub final_t_m: f64,
    pub final_n_m: f64,
}

/// Statistics of RTN residuals given in kilometres.
pub fn residual_stats(rtn_km: &[Vector3<f64>]) -> ResidualStats {
    let Some(last) = rtn_km.last() else {
        return ResidualStats::default();
    };
    let m = |v: f64| v * 1000.0;
    let sumsq: f64 = rtn_km.iter().map(|v| v.norm_squared()).sum();
    let maxabs = |i: usize| rtn_km.iter().map(|v| v[i].abs()).fold(0.0, f64::max);
    ResidualStats {
        samples: rtn_km.len(),
        rms_3d_m: m((sumsq / rtn_km.len() as f64).sqrt()),
        max_3d_m: m(rtn_km.iter().map(|v| v.norm()).fold(0.0, f64::max)),
        max_abs_r_m: m(maxabs(0)),
        max_abs_t_m: m(maxabs(1)),
        max_abs_n_m: m(maxabs(2)),
        final_r_m: m(last[0]),
        final_t_m: m(last[1]),
        final_n_m: m(last[2]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtn_axes_for_a_circular_equatorial_orbit() {
        let s = State::new([7000.0, 0.0, 0.0], [0.0, 7.5, 0.0]);
        let m = rtn_matrix(&s);
        assert!(
            (m * Vector3::new(1.0, 0.0, 0.0) - Vector3::new(1.0, 0.0, 0.0)).norm() < 1e-12,
            "R is radial"
        );
        assert!(
            (m * Vector3::new(0.0, 1.0, 0.0) - Vector3::new(0.0, 1.0, 0.0)).norm() < 1e-12,
            "T is along-track"
        );
        assert!(
            (m * Vector3::new(0.0, 0.0, 1.0) - Vector3::new(0.0, 0.0, 1.0)).norm() < 1e-12,
            "N is orbit normal"
        );
    }

    #[test]
    fn residual_is_along_track_when_displaced_along_velocity() {
        let reference = State::new([7000.0, 0.0, 0.0], [0.0, 7.5, 0.0]);
        let test = State::new([7000.0, 2.0, 0.0], [0.0, 7.5, 0.0]);
        let r = rtn_residual(&reference, &test);
        assert!(r[0].abs() < 1e-12 && (r[1] - 2.0).abs() < 1e-12 && r[2].abs() < 1e-12);
    }

    #[test]
    fn empty_input_gives_default_stats() {
        assert_eq!(residual_stats(&[]).samples, 0);
    }
}
