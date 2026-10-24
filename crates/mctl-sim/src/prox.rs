//! Relative motion of a visiting vehicle about its target, in the target's LVLH frame.

use std::sync::Arc;

use mctl_astro::local_frames::{LocalFrame, to_local};
use mctl_astro::trajectory::Trajectory;
use mctl_core::config::ProxConfig;
use mctl_core::prox::{ProxPoint, ProxState, ProxView};
use mctl_core::state::State;
use mctl_core::time::Utc;

/// Spacing of predicted and recorded relative positions, seconds.
pub const STEP_S: f64 = 60.0;

/// Chaser position relative to the target in the target's LVLH frame (km), the separation (km) and its
/// rate (km/s).
pub fn relative(target: &State, chaser: &State) -> Option<([f64; 3], f64, f64)> {
    let d = chaser.r - target.r;
    let l = to_local(LocalFrame::Lvlh, target, &d)?;
    let range = d.norm();
    let rate = if range > 0.0 {
        d.dot(&(chaser.v - target.v)) / range
    } else {
        0.0
    };
    Some(([l.x, l.y, l.z], range, rate))
}

/// Relative positions from `t0` forward at `STEP_S` for `minutes`, skipping instants either source cannot
/// answer.
pub fn predict(
    target: &dyn Trajectory,
    chaser: &dyn Trajectory,
    t0: Utc,
    minutes: f64,
) -> Vec<ProxPoint> {
    let n = (minutes * 60.0 / STEP_S) as usize;
    (0..=n)
        .filter_map(|k| {
            let t = t0.plus(k as f64 * STEP_S);
            let (a, b) = (target.state_gcrf(t).ok()?, chaser.state_gcrf(t).ok()?);
            relative(&a, &b).map(|(l, _, _)| ProxPoint { t, lvlh_km: l })
        })
        .collect()
}

pub fn classify(range_km: f64, cfg: &ProxConfig) -> ProxState {
    if range_km * 1000.0 < cfg.docked_range_m {
        ProxState::Docked
    } else if range_km < cfg.near_km {
        ProxState::Near
    } else {
        ProxState::Far
    }
}

/// Assemble the view from the current states and the path arrays.
#[allow(clippy::too_many_arguments)]
pub fn view(
    chaser_id: &str,
    target_id: &str,
    target: &State,
    chaser: &State,
    note: String,
    history: Arc<Vec<ProxPoint>>,
    predicted: Arc<Vec<ProxPoint>>,
    cfg: &ProxConfig,
) -> Option<ProxView> {
    let (rel, range, rate) = relative(target, chaser)?;
    Some(ProxView {
        chaser: chaser_id.to_owned(),
        target: target_id.to_owned(),
        state: classify(range, cfg),
        rel_lvlh_km: rel,
        range_km: range,
        range_rate_km_s: rate,
        source_note: note,
        // A TLE-derived visitor is never ephemeris-grade, which is the only kind this build has for one.
        ephemeris_grade: false,
        history,
        predicted,
        keep_out_km: cfg.keep_out_m / 1000.0,
        ellipsoid_semi_km: cfg.ellipsoid_semi_km,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mctl_astro::numerical::{ForceModel, MU_EARTH, RE_KM, Table, propagate};
    use nalgebra::Vector3;

    fn circular(alt_km: f64, phase_rad: f64) -> State {
        let r = RE_KM + alt_km;
        let v = (MU_EARTH / r).sqrt();
        let (s, c) = phase_rad.sin_cos();
        State::new([r * c, r * s, 0.0], [-v * s, v * c, 0.0])
    }

    #[test]
    fn a_visitor_ahead_on_v_bar_and_below_on_r_bar_has_the_expected_signs() {
        let target = circular(420.0, 0.0);
        // 1 km ahead along the orbit, and (separately) 2 km lower.
        let ahead = circular(420.0, 1.0 / (RE_KM + 420.0));
        let (rel, range, _) = relative(&target, &ahead).unwrap();
        assert!((rel[0] - 1.0).abs() < 1e-3, "{rel:?}");
        assert!(rel[2].abs() < 1e-3 && rel[1].abs() < 1e-9);
        assert!((range - 1.0).abs() < 1e-3);
        let below = circular(418.0, 0.0);
        let (rel, _, _) = relative(&target, &below).unwrap();
        assert!((rel[2] - 2.0).abs() < 1e-9, "nadir is +Z: {rel:?}");
    }

    #[test]
    fn identical_states_are_docked_and_the_thresholds_classify() {
        let cfg = ProxConfig::default();
        let s = circular(420.0, 0.3);
        let (rel, range, rate) = relative(&s, &s).unwrap();
        assert_eq!((rel, range, rate), ([0.0; 3], 0.0, 0.0));
        assert_eq!(classify(range, &cfg), ProxState::Docked);
        assert_eq!(classify(0.049, &cfg), ProxState::Docked);
        assert_eq!(classify(0.051, &cfg), ProxState::Near);
        assert_eq!(classify(99.0, &cfg), ProxState::Near);
        assert_eq!(classify(101.0, &cfg), ProxState::Far);
    }

    #[test]
    fn the_prediction_of_a_phasing_orbit_drifts_along_v_bar() {
        // A visitor 1 km lower circles faster, so it gains on the target along the V-bar.
        let t0 = Utc::parse("2026-10-24T00:00:00").unwrap();
        let tab = |s: State| {
            Table::new(
                propagate(
                    &ForceModel::TWO_BODY,
                    &Vector3::z(),
                    t0,
                    &s,
                    t0.plus(7200.0),
                    30.0,
                )
                .unwrap(),
            )
            .unwrap()
        };
        let (target, chaser) = (tab(circular(420.0, 0.0)), tab(circular(419.0, 0.0)));
        let path = predict(&target, &chaser, t0, 90.0);
        assert!(path.len() > 80);
        assert!(
            path[0].lvlh_km[2] > 0.99 && path[0].lvlh_km[2] < 1.01,
            "starts 1 km below: {:?}",
            path[0]
        );
        let last = path.last().unwrap();
        assert!(
            last.lvlh_km[0] > 5.0,
            "gained several km on the V-bar in 90 min: {:?}",
            last.lvlh_km
        );
        // Outside the trajectories' span nothing is invented.
        assert!(predict(&target, &chaser, t0.plus(7000.0), 90.0).len() < 6);
    }
}
