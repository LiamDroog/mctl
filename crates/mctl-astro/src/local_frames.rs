//! Local orbital frames (DESIGN.md FR-11, §6.4): RTN for residuals, VNB for maneuver input, and LVLH
//! for attitude and relative motion. Each is a rotation from the inertial frame the state is in.

use mctl_core::state::State;
use nalgebra::{Matrix3, Vector3};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalFrame {
    /// Radial, along-track (transverse), orbit normal.
    Rtn,
    /// Velocity, orbit normal, binormal (completes the right-handed set).
    Vnb,
    /// ISS convention: +X forward (along velocity for a circular orbit), +Y negative orbit normal,
    /// +Z nadir.
    Lvlh,
}

impl LocalFrame {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rtn => "RTN",
            Self::Vnb => "VNB",
            Self::Lvlh => "LVLH",
        }
    }
}

/// Rows are the frame's axes expressed in the inertial frame, so `m * v_inertial` gives components
/// in the local frame. `None` for a degenerate state (zero radius, or position parallel to velocity).
pub fn basis(frame: LocalFrame, s: &State) -> Option<Matrix3<f64>> {
    let (r, v) = (s.r, s.v);
    let h = r.cross(&v);
    if r.norm() < 1.0e-9 || h.norm() < 1.0e-12 {
        return None;
    }
    let n = h.normalize();
    let (x, y, z) = match frame {
        LocalFrame::Rtn => {
            let rhat = r.normalize();
            (rhat, n.cross(&rhat), n)
        }
        LocalFrame::Vnb => {
            let vhat = v.normalize();
            (vhat, n, vhat.cross(&n))
        }
        LocalFrame::Lvlh => {
            let nadir = -r.normalize();
            let y = -n;
            (y.cross(&nadir), y, nadir)
        }
    };
    Some(Matrix3::from_rows(&[
        x.transpose(),
        y.transpose(),
        z.transpose(),
    ]))
}

/// Components of an inertial vector in the local frame of `reference`.
pub fn to_local(frame: LocalFrame, reference: &State, v: &Vector3<f64>) -> Option<Vector3<f64>> {
    basis(frame, reference).map(|m| m * v)
}

/// The inertial vector for components given in the local frame of `reference` (e.g. a burn's delta-v).
pub fn from_local(
    frame: LocalFrame,
    reference: &State,
    local: &Vector3<f64>,
) -> Option<Vector3<f64>> {
    basis(frame, reference).map(|m| m.transpose() * local)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circular() -> State {
        // Equatorial circular orbit at the x axis moving along +y.
        State::new([7000.0, 0.0, 0.0], [0.0, 7.5, 0.0])
    }

    fn inclined() -> State {
        State::new([6000.0, 2000.0, 3000.0], [-1.5, 5.0, 4.0])
    }

    #[test]
    fn every_basis_is_orthonormal_and_right_handed() {
        for f in [LocalFrame::Rtn, LocalFrame::Vnb, LocalFrame::Lvlh] {
            for s in [circular(), inclined()] {
                let m = basis(f, &s).unwrap();
                assert!(
                    (m * m.transpose() - Matrix3::identity()).norm() < 1e-12,
                    "{f:?}"
                );
                assert!(
                    (m.determinant() - 1.0).abs() < 1e-12,
                    "{f:?} must be right-handed"
                );
            }
        }
    }

    #[test]
    fn rtn_on_a_circular_equatorial_orbit_is_x_y_z() {
        let s = circular();
        let m = basis(LocalFrame::Rtn, &s).unwrap();
        assert!(
            (m - Matrix3::identity()).norm() < 1e-12,
            "R = +x, T = +y, N = +z: {m}"
        );
        // An outward displacement is purely radial; a forward one purely along-track.
        assert!(
            (to_local(LocalFrame::Rtn, &s, &Vector3::new(1.0, 0.0, 0.0)).unwrap()
                - Vector3::new(1.0, 0.0, 0.0))
            .norm()
                < 1e-12
        );
        assert!(
            (to_local(LocalFrame::Rtn, &s, &Vector3::new(0.0, 2.0, 0.0)).unwrap()
                - Vector3::new(0.0, 2.0, 0.0))
            .norm()
                < 1e-12
        );
    }

    #[test]
    fn vnb_puts_velocity_first_and_the_orbit_normal_second() {
        let s = inclined();
        let m = basis(LocalFrame::Vnb, &s).unwrap();
        assert!((m.row(0).transpose() - s.v.normalize()).norm() < 1e-12);
        assert!((m.row(1).transpose() - s.r.cross(&s.v).normalize()).norm() < 1e-12);
        // A prograde burn is +V; the vector along velocity has no N or B components.
        let l = to_local(LocalFrame::Vnb, &s, &(s.v * 0.001)).unwrap();
        assert!((l.x - s.v.norm() * 0.001).abs() < 1e-12 && l.y.abs() < 1e-12 && l.z.abs() < 1e-12);
    }

    #[test]
    fn lvlh_follows_the_iss_convention() {
        let s = circular();
        let m = basis(LocalFrame::Lvlh, &s).unwrap();
        let z = m.row(2).transpose();
        assert!(
            (z - Vector3::new(-1.0, 0.0, 0.0)).norm() < 1e-12,
            "+Z is nadir"
        );
        assert!(
            (m.row(1).transpose() - Vector3::new(0.0, 0.0, -1.0)).norm() < 1e-12,
            "+Y is the negative orbit normal"
        );
        assert!(
            (m.row(0).transpose() - Vector3::new(0.0, 1.0, 0.0)).norm() < 1e-12,
            "+X is forward on a circular orbit"
        );
    }

    #[test]
    fn local_and_inertial_components_round_trip() {
        let s = inclined();
        let v = Vector3::new(0.3, -0.2, 0.9);
        for f in [LocalFrame::Rtn, LocalFrame::Vnb, LocalFrame::Lvlh] {
            let local = to_local(f, &s, &v).unwrap();
            assert!(
                (from_local(f, &s, &local).unwrap() - v).norm() < 1e-12,
                "{f:?}"
            );
            assert!(
                (local.norm() - v.norm()).abs() < 1e-12,
                "rotations preserve length"
            );
        }
    }

    #[test]
    fn degenerate_states_have_no_frame() {
        assert!(
            basis(
                LocalFrame::Rtn,
                &State::new([7000.0, 0.0, 0.0], [1.0, 0.0, 0.0])
            )
            .is_none(),
            "radial motion has no orbit normal"
        );
        assert!(basis(LocalFrame::Lvlh, &State::new([0.0; 3], [0.0, 1.0, 0.0])).is_none());
    }
}
