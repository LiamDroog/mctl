//! Polar sky plot: azimuth around, elevation inward, zenith at the centre.

use egui::{Align2, Painter, Pos2, Shape, Stroke};
use mctl_core::time::Utc;
use mctl_core::world::{LookView, PassView};

use crate::theme::Theme;

/// Elevation rings drawn, degrees.
const RINGS_DEG: [f32; 2] = [30.0, 60.0];

pub struct SkyInput<'a> {
    pub mask_deg: f32,
    /// The pass to draw: the one in progress or the next.
    pub pass: Option<&'a PassView>,
    pub now: Utc,
    pub look_now: Option<LookView>,
    pub sun_az_el: Option<(f64, f64)>,
    pub moon_az_el: Option<(f64, f64)>,
}

/// Screen position of an azimuth/elevation on a plot of radius `r` centred at `c`. Elevation may be
/// negative (below the horizon); such points sit outside the horizon ring and are not drawn by callers.
pub fn project(c: Pos2, r: f32, az_deg: f32, el_deg: f32) -> Pos2 {
    let rho = r * (90.0 - el_deg) / 90.0;
    let a = az_deg.to_radians();
    // North is up, east is right (looking up at the sky from below, east would be left; a sky plot
    // for a ground observer conventionally keeps N up, E right as on a map of the sky dome from above).
    Pos2::new(c.x + rho * a.sin(), c.y - rho * a.cos())
}

pub fn paint(painter: &Painter, theme: &Theme, rect_px: [u32; 4], inp: &SkyInput<'_>) {
    let [x, y, w, h] = rect_px;
    let radius = (w.min(h) as f32 / 2.0 - 22.0).max(0.0);
    if radius < 30.0 {
        return;
    }
    let c = Pos2::new(x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0);
    let p = &theme.palette;
    let (grid, dim, bright, info) = (
        p.border.to_egui(),
        p.dim.to_egui(),
        p.bright.to_egui(),
        p.info.to_egui(),
    );
    painter.circle_stroke(c, radius, Stroke::new(2.0, bright));
    for ring in RINGS_DEG {
        painter.circle_stroke(c, radius * (90.0 - ring) / 90.0, Stroke::new(1.0, grid));
        painter.text(
            c + egui::vec2(3.0, -radius * (90.0 - ring) / 90.0),
            Align2::LEFT_BOTTOM,
            format!("{ring:.0}"),
            crate::fonts::font_id(theme),
            dim,
        );
    }
    if inp.mask_deg > 0.5 {
        let mask_r = radius * (90.0 - inp.mask_deg) / 90.0;
        painter.add(Shape::dashed_line(
            &circle_points(c, mask_r),
            Stroke::new(1.0, p.caution.to_egui()),
            6.0,
            5.0,
        ));
    }
    for (az, label) in [(0.0, "N"), (90.0, "E"), (180.0, "S"), (270.0, "W")] {
        painter.line_segment(
            [
                project(c, radius, az, 0.0),
                project(c, radius, az, 0.0) + (c - project(c, radius, az, 0.0)).normalized() * 8.0,
            ],
            Stroke::new(1.0, bright),
        );
        painter.text(
            project(c, radius + 14.0, az, 0.0),
            Align2::CENTER_CENTER,
            label,
            crate::fonts::font_id(theme),
            bright,
        );
    }
    // Cross-hairs.
    painter.line_segment(
        [Pos2::new(c.x - radius, c.y), Pos2::new(c.x + radius, c.y)],
        Stroke::new(1.0, grid),
    );
    painter.line_segment(
        [Pos2::new(c.x, c.y - radius), Pos2::new(c.x, c.y + radius)],
        Stroke::new(1.0, grid),
    );

    if let Some((az, el)) = inp.sun_az_el.filter(|(_, el)| *el > -18.0) {
        let pos = project(c, radius, az as f32, (el as f32).max(0.0));
        let sun = if el > 0.0 { p.caution.to_egui() } else { dim };
        painter.circle_stroke(pos, 6.0, Stroke::new(2.0, sun));
        painter.text(
            pos + egui::vec2(9.0, 0.0),
            Align2::LEFT_CENTER,
            if el > 0.0 {
                "SUN"
            } else {
                "SUN (BELOW HORIZON)"
            },
            crate::fonts::font_id(theme),
            sun,
        );
    }

    if let Some((az, el)) = inp.moon_az_el.filter(|(_, el)| *el > -5.0) {
        let pos = project(c, radius, az as f32, (el as f32).max(0.0));
        let colour = if el > 0.0 { bright } else { dim };
        painter.circle_filled(pos, 5.0, colour);
        painter.text(
            pos + egui::vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            if el > 0.0 {
                "MOON"
            } else {
                "MOON (BELOW HORIZON)"
            },
            crate::fonts::font_id(theme),
            colour,
        );
    }
    if let Some(pass) = inp.pass {
        // Path samples are evenly spaced from AOS to LOS; split into flown (solid) and predicted (dashed).
        let n = pass.path.len();
        if n >= 2 {
            let span = pass.los.minus(pass.aos).max(1.0);
            let frac = (inp.now.minus(pass.aos) / span).clamp(0.0, 1.0);
            let split = ((n - 1) as f64 * frac) as usize;
            let pts: Vec<Pos2> = pass
                .path
                .iter()
                .map(|[az, el]| project(c, radius, *az, el.max(0.0)))
                .collect();
            if split >= 1 {
                painter.add(Shape::line(
                    pts[..=split].to_vec(),
                    Stroke::new(2.5, bright),
                ));
            }
            painter.add(Shape::dashed_line(
                &pts[split..],
                Stroke::new(2.0, info),
                8.0,
                6.0,
            ));
            for (pos, label) in [(pts[0], "AOS"), (pts[n - 1], "LOS"), (pts[n / 2], "")] {
                painter.circle_filled(pos, 3.0, bright);
                if !label.is_empty() {
                    painter.text(
                        pos + egui::vec2(6.0, -6.0),
                        Align2::LEFT_BOTTOM,
                        label,
                        crate::fonts::font_id(theme),
                        bright,
                    );
                }
            }
            let top = pass
                .path
                .iter()
                .enumerate()
                .max_by(|a, b| a.1[1].total_cmp(&b.1[1]))
                .map(|(i, _)| i)
                .unwrap_or(0);
            painter.text(
                pts[top] + egui::vec2(8.0, 8.0),
                Align2::LEFT_TOP,
                format!("TCA {:.0}", pass.max_el_deg),
                crate::fonts::font_id(theme),
                bright,
            );
        }
    }
    if let Some(l) = inp.look_now.filter(|l| l.el_deg >= 0.0) {
        let pos = project(c, radius, l.az_deg as f32, l.el_deg as f32);
        painter.circle_filled(pos, 6.0, p.warning.to_egui());
        painter.circle_stroke(pos, 9.0, Stroke::new(1.5, p.warning.to_egui()));
    }
}

fn circle_points(c: Pos2, r: f32) -> Vec<Pos2> {
    (0..=96)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / 96.0;
            Pos2::new(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zenith_is_the_centre_and_the_horizon_is_the_rim() {
        let c = Pos2::new(100.0, 100.0);
        assert_eq!(project(c, 50.0, 123.0, 90.0), c);
        let n = project(c, 50.0, 0.0, 0.0);
        assert!(
            (n.x - 100.0).abs() < 1e-4 && (n.y - 50.0).abs() < 1e-4,
            "north is up: {n:?}"
        );
        let e = project(c, 50.0, 90.0, 0.0);
        assert!(
            (e.x - 150.0).abs() < 1e-4 && (e.y - 100.0).abs() < 1e-4,
            "east is right: {e:?}"
        );
        let mid = project(c, 50.0, 180.0, 45.0);
        assert!(
            (mid.y - 125.0).abs() < 1e-4,
            "45 degrees is halfway out, south is down: {mid:?}"
        );
    }
}
