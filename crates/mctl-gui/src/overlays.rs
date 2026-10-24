//! Orbit-map overlays (DESIGN.md §9.6 F2.1): node and apsis markers with altitudes, the eclipse
//! segment of the orbit, the velocity vector, the nadir line and the Sun direction. Drawn with the
//! 2D painter by projecting the same scene the globe pass uses, hiding what the Earth occludes.

use egui::{Align2, Painter, Pos2, Shape, Stroke};
use glam::Vec3;
use mctl_core::plan::{PlanPoint, PlanView};
use mctl_core::time::Utc;
use mctl_core::world::{EventKind, Solution, Track};

use crate::globe::{GlobeScene, exaggerate, segment_hits_unit_sphere, to_vec3};
use crate::theme::Theme;

/// Length of the velocity arrow, Earth radii (before exaggeration it is a direction cue, not a scale).
const VELOCITY_ARROW_RE: f32 = 0.22;
/// Distance at which the Sun marker is drawn from the Earth's centre, Earth radii.
const SUN_MARKER_RE: f32 = 2.6;
/// Illumination below which an orbit sample counts as in eclipse.
const ECLIPSE_LEVEL: f32 = 0.5;

/// Project a scene point to pixels inside `rect_px`; `None` behind the camera.
pub fn project(scene: &GlobeScene, rect_px: [u32; 4], p: Vec3) -> Option<Pos2> {
    let clip = scene.view_proj * p.extend(1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let (nx, ny) = (clip.x / clip.w, clip.y / clip.w);
    Some(Pos2::new(
        rect_px[0] as f32 + (nx * 0.5 + 0.5) * rect_px[2] as f32,
        rect_px[1] as f32 + (0.5 - ny * 0.5) * rect_px[3] as f32,
    ))
}

fn visible(scene: &GlobeScene, p: Vec3) -> bool {
    !segment_hits_unit_sphere(scene.cam_pos, p)
}

/// Position (km, GCRF) and altitude of the track at time `t`, interpolated between its samples.
pub fn track_at(track: &Track, t: Utc) -> Option<([f64; 3], f64)> {
    let i = track.points.partition_point(|p| p.t.0 < t.0);
    if i == 0 || i >= track.points.len() {
        return None;
    }
    let (a, b) = (&track.points[i - 1], &track.points[i]);
    let f = (t.0 - a.t.0) / (b.t.0 - a.t.0).max(1.0e-9);
    let lerp = |x: f64, y: f64| x + (y - x) * f;
    Some((
        [
            lerp(a.r_gcrf_km[0], b.r_gcrf_km[0]),
            lerp(a.r_gcrf_km[1], b.r_gcrf_km[1]),
            lerp(a.r_gcrf_km[2], b.r_gcrf_km[2]),
        ],
        lerp(a.alt_km, b.alt_km),
    ))
}

pub fn paint(
    painter: &Painter,
    theme: &Theme,
    rect_px: [u32; 4],
    scene: &GlobeScene,
    track: &Track,
    sol: &Solution,
    exaggeration: f32,
) {
    let p = &theme.palette;
    let font = crate::fonts::font_id(theme);
    let (bright, dim, caution) = (p.bright.to_egui(), p.dim.to_egui(), p.caution.to_egui());
    // Eclipse part of the orbit, drawn over the ribbon in its own colour.
    let mut run: Vec<Pos2> = Vec::new();
    let flush = |run: &mut Vec<Pos2>| {
        if run.len() > 1 {
            painter.add(Shape::line(std::mem::take(run), Stroke::new(3.0, caution)));
        }
        run.clear();
    };
    for pt in &track.points {
        let w = exaggerate(to_vec3(pt.r_gcrf_km), exaggeration);
        match project(scene, rect_px, w) {
            Some(px) if pt.illumination < ECLIPSE_LEVEL && visible(scene, w) => run.push(px),
            _ => flush(&mut run),
        }
    }
    flush(&mut run);
    // Nodes and apsides with the altitude where the vehicle crosses them.
    for (t, kind) in &track.events {
        let (label, shape) = match kind {
            EventKind::AscendingNode => ("AN", 1),
            EventKind::DescendingNode => ("DN", 2),
            EventKind::Periapsis => ("Pe", 0),
            EventKind::Apoapsis => ("Ap", 0),
            _ => continue,
        };
        let Some((r, alt)) = track_at(track, *t) else {
            continue;
        };
        let w = exaggerate(to_vec3(r), exaggeration);
        let (Some(px), true) = (project(scene, rect_px, w), visible(scene, w)) else {
            continue;
        };
        let s = 5.0;
        match shape {
            1 => painter.add(Shape::convex_polygon(
                vec![
                    px + egui::vec2(0.0, -s),
                    px + egui::vec2(-s, s),
                    px + egui::vec2(s, s),
                ],
                bright,
                Stroke::NONE,
            )),
            2 => painter.add(Shape::convex_polygon(
                vec![
                    px + egui::vec2(0.0, s),
                    px + egui::vec2(-s, -s),
                    px + egui::vec2(s, -s),
                ],
                bright,
                Stroke::NONE,
            )),
            _ => painter.circle_stroke(px, s, Stroke::new(1.5, bright)),
        };
        let text = if shape == 0 {
            format!("{label} {alt:.0} km")
        } else {
            label.to_owned()
        };
        painter.text(
            px + egui::vec2(8.0, -2.0),
            Align2::LEFT_BOTTOM,
            text,
            font.clone(),
            dim,
        );
    }
    // Velocity vector and nadir line from the vehicle.
    let v = scene.vehicle;
    if let Some(vpx) = project(scene, rect_px, v) {
        let vel = to_vec3([sol.state_gcrf.v.x, sol.state_gcrf.v.y, sol.state_gcrf.v.z])
            .normalize_or_zero()
            * VELOCITY_ARROW_RE;
        if let Some(tip) = project(scene, rect_px, v + vel) {
            painter.line_segment([vpx, tip], Stroke::new(2.0, bright));
            painter.text(
                tip + egui::vec2(4.0, 0.0),
                Align2::LEFT_CENTER,
                "V",
                font.clone(),
                bright,
            );
        }
        let foot = v.normalize_or_zero();
        if let Some(fpx) = project(scene, rect_px, foot).filter(|_| !scene.vehicle_occulted) {
            painter.add(Shape::dashed_line(
                &[vpx, fpx],
                Stroke::new(1.0, dim),
                4.0,
                4.0,
            ));
        }
    }
    // Sun direction.
    if let Some(spx) = project(scene, rect_px, scene.sun_dir * SUN_MARKER_RE) {
        painter.circle_stroke(spx, 7.0, Stroke::new(2.0, caution));
        painter.text(
            spx + egui::vec2(10.0, 0.0),
            Align2::LEFT_CENTER,
            "SUN",
            font,
            caution,
        );
    }
}

/// The plan point nearest `t`.
pub fn plan_point_at(points: &[PlanPoint], t: Utc) -> Option<&PlanPoint> {
    points
        .iter()
        .min_by(|a, b| (a.t.0 - t.0).abs().total_cmp(&(b.t.0 - t.0).abs()))
}

/// The nominal and planned trajectories over the orbit map (DESIGN.md FR-31): nominal dim and dashed,
/// planned solid in the info colour, burns as diamonds numbered in time order.
pub fn paint_plan(
    painter: &Painter,
    theme: &Theme,
    rect_px: [u32; 4],
    scene: &GlobeScene,
    plan: &PlanView,
    exaggeration: f32,
) {
    let p = &theme.palette;
    let font = crate::fonts::font_id(theme);
    let line = |points: &[PlanPoint], stroke: Stroke, dashed: bool| {
        let mut run: Vec<Pos2> = Vec::new();
        let flush = |run: &mut Vec<Pos2>| {
            if run.len() > 1 {
                let pts = std::mem::take(run);
                if dashed {
                    painter.add(Shape::dashed_line(&pts, stroke, 5.0, 4.0));
                } else {
                    painter.add(Shape::line(pts, stroke));
                }
            }
            run.clear();
        };
        for pt in points {
            let w = exaggerate(to_vec3(pt.r_gcrf_km), exaggeration);
            match project(scene, rect_px, w) {
                Some(px) if visible(scene, w) => run.push(px),
                _ => flush(&mut run),
            }
        }
        flush(&mut run);
    };
    line(&plan.nominal, Stroke::new(1.5, p.dim.to_egui()), true);
    line(&plan.planned, Stroke::new(2.0, p.info.to_egui()), false);
    for (i, b) in plan.burns.iter().enumerate() {
        let Some(pt) = plan_point_at(&plan.planned, b.t) else {
            continue;
        };
        let w = exaggerate(to_vec3(pt.r_gcrf_km), exaggeration);
        let (Some(px), true) = (project(scene, rect_px, w), visible(scene, w)) else {
            continue;
        };
        let s = 6.0;
        painter.add(Shape::convex_polygon(
            vec![
                px + egui::vec2(0.0, -s),
                px + egui::vec2(s, 0.0),
                px + egui::vec2(0.0, s),
                px + egui::vec2(-s, 0.0),
            ],
            p.caution.to_egui(),
            Stroke::new(1.0, p.bright.to_egui()),
        ));
        painter.text(
            px + egui::vec2(9.0, -9.0),
            Align2::LEFT_BOTTOM,
            format!("B{}", i + 1),
            font.clone(),
            p.bright.to_egui(),
        );
    }
}

#[cfg(test)]
mod tests {
    use mctl_core::state::SourceKind;
    use mctl_core::world::TrackPoint;

    use super::*;

    fn track() -> Track {
        let pt = |t: f64, x: f64, alt: f64| TrackPoint {
            t: Utc(t),
            r_gcrf_km: [x, 0.0, 0.0],
            lat_deg: 0.0,
            lon_deg: 0.0,
            alt_km: alt,
            illumination: 1.0,
            rot_km: [0.0; 3],
        };
        Track {
            revision: 1,
            source: SourceKind::Gp,
            points: vec![
                pt(0.0, 7000.0, 400.0),
                pt(10.0, 7010.0, 410.0),
                pt(20.0, 7030.0, 430.0),
            ],
            now_index: 1,
            events: vec![],
        }
    }

    #[test]
    fn track_positions_interpolate_linearly_and_outside_the_span_there_is_none() {
        let (r, alt) = track_at(&track(), Utc(5.0)).unwrap();
        assert!((r[0] - 7005.0).abs() < 1e-9 && (alt - 405.0).abs() < 1e-9);
        let (r, alt) = track_at(&track(), Utc(15.0)).unwrap();
        assert!((r[0] - 7020.0).abs() < 1e-9 && (alt - 420.0).abs() < 1e-9);
        assert!(track_at(&track(), Utc(-1.0)).is_none());
        assert!(track_at(&track(), Utc(25.0)).is_none());
    }

    #[test]
    fn the_earths_centre_projects_to_the_middle_of_the_panel_and_points_behind_the_camera_do_not_project()
     {
        let sol_cam = crate::globe::Camera::default();
        let sol = {
            // A minimal solution: only the fields build_scene reads matter.
            use mctl_core::state::{Elements, Geodetic, Quality, State};
            Solution {
                source: SourceKind::Gp,
                quality: Quality::Pred,
                state_gcrf: State::new([7000.0, 0.0, 0.0], [0.0, 7.5, 0.0]),
                state_itrf: State::new([7000.0, 0.0, 0.0], [0.0; 3]),
                geodetic: Geodetic {
                    lat_deg: 0.0,
                    lon_deg: 0.0,
                    alt_km: 600.0,
                },
                elements: Elements {
                    sma_km: 7000.0,
                    ecc: 0.0,
                    inc_deg: 0.0,
                    raan_deg: 0.0,
                    argp_deg: 0.0,
                    true_anom_deg: 0.0,
                    mean_anom_deg: 0.0,
                    period_s: 5800.0,
                    apo_alt_km: 600.0,
                    peri_alt_km: 600.0,
                },
                sun_gcrf_km: [1.5e8, 0.0, 0.0],
                beta_deg: 0.0,
                illumination: 1.0,
                gmst_rad: 0.0,
                ecef_from_gcrf: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            }
        };
        let scene = crate::globe::build_scene(&sol_cam, &sol, 2.0);
        let rect = [100, 50, 800, 400];
        let c = project(&scene, rect, Vec3::ZERO).unwrap();
        assert!(
            (c.x - 500.0).abs() < 1.0 && (c.y - 250.0).abs() < 1.0,
            "{c:?}"
        );
        let behind = scene.cam_pos * 3.0;
        assert!(project(&scene, rect, behind).is_none());
    }
}
