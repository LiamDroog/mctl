//! Attitude director indicator and the quaternion maths behind it.
//!
//! The ISSLIVE attitude quaternion's component order and axis convention are NOT verified. This module assumes scalar-first and the aerospace 3-2-1 (yaw-pitch-roll) sequence, which
//! matches the live data (component 0 is ~0.997 in the nominal hold); the page shows that assumption
//! on screen until it is checked against a known attitude mode.

use egui::{Color32, Painter, Pos2, Shape, Stroke, Vec2};
use mctl_core::world::TelemetryView;

use crate::theme::Theme;

/// Telemetry item IDs of the current LVLH attitude quaternion (config/isslive.toml, from ISS Mimic).
pub const QUAT_ITEMS: [&str; 4] = ["USLAB000018", "USLAB000019", "USLAB000020", "USLAB000021"];
/// Commanded attitude quaternion items and the reported roll/pitch/yaw error items (deg).
pub const COMMANDED_ITEMS: [&str; 4] = ["USLAB000028", "USLAB000029", "USLAB000030", "USLAB000031"];
pub const ERROR_ITEMS: [&str; 3] = ["USLAB000022", "USLAB000023", "USLAB000024"];
/// Largest allowed deviation of the quaternion's norm from 1 before it is rejected as not a quaternion.
pub const NORM_TOLERANCE: f64 = 0.01;
/// Degrees of pitch from the ball centre to its edge.
const BALL_HALF_RANGE_DEG: f32 = 45.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Attitude {
    pub roll_deg: f64,
    pub pitch_deg: f64,
    pub yaw_deg: f64,
}

/// Roll, pitch, yaw (3-2-1) from a scalar-first quaternion. `None` if it is not a unit quaternion.
pub fn euler_deg(q: [f64; 4]) -> Option<Attitude> {
    let norm = q.iter().map(|c| c * c).sum::<f64>().sqrt();
    if !norm.is_finite() || (norm - 1.0).abs() > NORM_TOLERANCE {
        return None;
    }
    let [w, x, y, z] = q.map(|c| c / norm);
    let roll = (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y));
    let pitch = (2.0 * (w * y - z * x)).clamp(-1.0, 1.0).asin();
    let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z));
    Some(Attitude {
        roll_deg: roll.to_degrees(),
        pitch_deg: pitch.to_degrees(),
        yaw_deg: yaw.to_degrees(),
    })
}

/// The attitude from live telemetry, or the reason there is none.
pub fn attitude(t: Option<&TelemetryView>) -> Result<Attitude, String> {
    let t = t.ok_or("no telemetry feed")?;
    let mut q = [0.0; 4];
    for (slot, id) in q.iter_mut().zip(QUAT_ITEMS) {
        *slot = t
            .get(id)
            .and_then(|i| i.value)
            .ok_or_else(|| format!("{id} not received"))?;
    }
    euler_deg(q)
        .ok_or_else(|| format!("quaternion norm outside 1 +- {NORM_TOLERANCE}: not an attitude"))
}

/// Cross-check of the quaternion convention against the feed's own error items: the difference of the
/// current and commanded attitudes, in the assumed convention, should match the reported errors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Consistency {
    pub derived_deg: [f64; 3],
    pub reported_deg: [f64; 3],
}

impl Consistency {
    pub fn max_diff_deg(&self) -> f64 {
        self.derived_deg
            .iter()
            .zip(self.reported_deg)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max)
    }
}

fn wrap_deg(a: f64) -> f64 {
    (a + 180.0).rem_euclid(360.0) - 180.0
}

pub fn consistency(t: Option<&TelemetryView>) -> Option<Consistency> {
    let t = t?;
    let get =
        |ids: &[&str]| -> Option<Vec<f64>> { ids.iter().map(|id| t.get(id)?.value).collect() };
    let cur = euler_deg(get(&QUAT_ITEMS)?.try_into().ok()?)?;
    let cmd = euler_deg(get(&COMMANDED_ITEMS)?.try_into().ok()?)?;
    let rep = get(&ERROR_ITEMS)?;
    Some(Consistency {
        derived_deg: [
            wrap_deg(cur.roll_deg - cmd.roll_deg),
            wrap_deg(cur.pitch_deg - cmd.pitch_deg),
            wrap_deg(cur.yaw_deg - cmd.yaw_deg),
        ],
        reported_deg: [rep[0], rep[1], rep[2]],
    })
}

/// Draw the ball in `rect_px` (`[x, y, w, h]`). No attitude: draw the OFF flag instead, never a guess.
pub fn paint(painter: &Painter, theme: &Theme, rect_px: [u32; 4], att: Result<Attitude, String>) {
    let [x, y, w, h] = rect_px;
    let radius = (w.min(h) as f32 / 2.0 - 8.0).max(0.0);
    if radius < 20.0 {
        return;
    }
    let centre = Pos2::new(x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0);
    let p = &theme.palette;
    // Sky and ground differ in brightness only, so the split reads in monochrome themes too.
    let (sky, ground, line) = (
        p.foreground.to_egui().gamma_multiply(0.10),
        p.foreground.to_egui().gamma_multiply(0.45),
        p.bright.to_egui(),
    );
    let Ok(a) = att else {
        painter.circle_stroke(centre, radius, Stroke::new(2.0, p.dim.to_egui()));
        painter.text(
            centre,
            egui::Align2::CENTER_CENTER,
            "OFF",
            egui::FontId::monospace(radius * 0.4),
            p.warning.to_egui(),
        );
        return;
    };
    let ppd = radius / BALL_HALF_RANGE_DEG;
    // Rotate by the roll; pitch shifts the horizon along the rotated vertical.
    let (s, c) = (-(a.roll_deg as f32).to_radians()).sin_cos();
    let rot = |v: Vec2| Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c);
    let to = |v: Vec2| centre + rot(v);
    let shift = a.pitch_deg as f32 * ppd;
    // Disc polygons clipped analytically: sample the circle, split at the horizon line y = shift.
    let n = 96;
    let mut sky_pts = Vec::new();
    let mut gnd_pts = Vec::new();
    for i in 0..=n {
        let th = std::f32::consts::TAU * i as f32 / n as f32;
        let v = Vec2::new(radius * th.cos(), radius * th.sin());
        if v.y <= shift {
            sky_pts.push(to(v))
        } else {
            gnd_pts.push(to(v))
        }
    }
    let half = (radius * radius - shift * shift).max(0.0).sqrt();
    let (l, r) = (to(Vec2::new(-half, shift)), to(Vec2::new(half, shift)));
    for (pts, col) in [(&mut sky_pts, sky), (&mut gnd_pts, ground)] {
        pts.push(r);
        pts.push(l);
        // Both half-discs are convex only after ordering around the centre.
        let c0 = pts.iter().fold(Vec2::ZERO, |acc, q| acc + q.to_vec2()) / pts.len() as f32;
        pts.sort_by(|a, b| {
            (a.to_vec2() - c0)
                .angle()
                .total_cmp(&(b.to_vec2() - c0).angle())
        });
        painter.add(Shape::convex_polygon(pts.clone(), col, Stroke::NONE));
    }
    painter.line_segment([l, r], Stroke::new(2.0, line));
    // Pitch ladder every 10 degrees.
    for k in (-4..=4).filter(|k| *k != 0) {
        let dy = shift - k as f32 * 10.0 * ppd;
        let half_w = if k % 2 == 0 {
            radius * 0.28
        } else {
            radius * 0.18
        };
        if (dy * dy + half_w * half_w).sqrt() < radius * 0.95 {
            painter.line_segment(
                [to(Vec2::new(-half_w, dy)), to(Vec2::new(half_w, dy))],
                Stroke::new(1.0, line),
            );
            painter.text(
                to(Vec2::new(half_w + 10.0, dy)),
                egui::Align2::CENTER_CENTER,
                format!("{}", k * 10),
                egui::FontId::monospace(11.0),
                line,
            );
        }
    }
    painter.circle_stroke(centre, radius, Stroke::new(2.0, line));
    // Roll scale pointer at the top, fixed vehicle symbol at the centre.
    painter.add(Shape::convex_polygon(
        vec![
            centre + Vec2::new(0.0, -radius),
            centre + Vec2::new(-6.0, -radius + 12.0),
            centre + Vec2::new(6.0, -radius + 12.0),
        ],
        line,
        Stroke::NONE,
    ));
    let sym = Color32::from_rgb(255, 200, 0);
    painter.line_segment(
        [
            centre + Vec2::new(-radius * 0.3, 0.0),
            centre + Vec2::new(-radius * 0.1, 0.0),
        ],
        Stroke::new(3.0, sym),
    );
    painter.line_segment(
        [
            centre + Vec2::new(radius * 0.1, 0.0),
            centre + Vec2::new(radius * 0.3, 0.0),
        ],
        Stroke::new(3.0, sym),
    );
    painter.circle_filled(centre, 3.0, sym);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_level() {
        let a = euler_deg([1.0, 0.0, 0.0, 0.0]).unwrap();
        assert_eq!((a.roll_deg, a.pitch_deg, a.yaw_deg), (0.0, 0.0, 0.0));
    }

    #[test]
    fn single_axis_rotations_give_that_axis_only() {
        let half = 10f64.to_radians() / 2.0;
        let (c, s) = (half.cos(), half.sin());
        let r = euler_deg([c, s, 0.0, 0.0]).unwrap();
        assert!(
            (r.roll_deg - 10.0).abs() < 1e-9 && r.pitch_deg.abs() < 1e-9 && r.yaw_deg.abs() < 1e-9
        );
        let p = euler_deg([c, 0.0, s, 0.0]).unwrap();
        assert!((p.pitch_deg - 10.0).abs() < 1e-9 && p.roll_deg.abs() < 1e-9);
        let y = euler_deg([c, 0.0, 0.0, s]).unwrap();
        assert!((y.yaw_deg - 10.0).abs() < 1e-9 && y.pitch_deg.abs() < 1e-9);
    }

    #[test]
    fn live_sample_from_the_feed_is_a_small_pitch_and_yaw_offset() {
        // Captured 2026-10-23 from USLAB000018-21.
        let a = euler_deg([0.9971358, 0.0014797, -0.0674240, -0.0342406]).unwrap();
        assert!((a.pitch_deg + 7.7).abs() < 0.2, "{a:?}");
        assert!((a.yaw_deg + 3.9).abs() < 0.2, "{a:?}");
        assert!(a.roll_deg.abs() < 0.5, "{a:?}");
    }

    #[test]
    fn commanded_attitude_is_a_round_number_triple_in_the_assumed_convention() {
        // Captured live 2026-10-23 from USLAB000028-31. A convention mismatch would not give round
        // numbers: this is the evidence that the scalar-first 3-2-1 reading is right.
        let c = euler_deg([
            0.997064411640167,
            0.005457313731313,
            -0.068245224654675,
            -0.034283746033907,
        ])
        .unwrap();
        assert!(
            (c.roll_deg - 0.9).abs() < 1e-4
                && (c.pitch_deg + 7.8).abs() < 1e-4
                && (c.yaw_deg + 4.0).abs() < 1e-4,
            "{c:?}"
        );
    }

    #[test]
    fn wrap_keeps_angles_in_half_turn() {
        assert!((wrap_deg(359.0) + 1.0).abs() < 1e-9);
        assert!((wrap_deg(-181.0) - 179.0).abs() < 1e-9);
    }

    #[test]
    fn non_unit_or_non_finite_quaternions_are_rejected_not_normalised() {
        assert!(euler_deg([0.5, 0.0, 0.0, 0.0]).is_none());
        assert!(euler_deg([f64::NAN, 0.0, 0.0, 0.0]).is_none());
        assert!(euler_deg([0.0; 4]).is_none());
    }

    #[test]
    fn attitude_names_what_is_missing() {
        assert!(attitude(None).unwrap_err().contains("no telemetry"));
    }
}
