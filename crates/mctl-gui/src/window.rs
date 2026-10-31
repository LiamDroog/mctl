//! The window view: the Earth as seen from the vehicle, with the limb curving at its
//! true altitude, the terminator, the Sun and Moon, stars, and the sub-satellite point.
//!
//! The Earth itself is the globe pass drawn from the vehicle's position instead of from outside; this
//! module builds that camera and paints what goes over it.

use egui::{Align2, Painter, Pos2, Shape, Stroke};
use glam::Vec3;
use mctl_core::world::Solution;

use crate::globe::{EARTH_RADIUS_KM, GlobeScene, ecef_of, segment_hits_unit_sphere, to_vec3};
use crate::overlays::project;
use crate::theme::Theme;

/// Vertical field of view of the window, degrees: wide enough to see the curve of the limb.
const FOV_DEG: f32 = 75.0;
/// Near plane in Earth radii (5 km): well inside the lowest orbit's altitude.
const NEAR_RE: f32 = 0.000_8;
const FAR_RE: f32 = 60.0;
/// Distance at which directions to stars, Sun and Moon are projected, Earth radii.
const SKY_RE: f32 = 20.0;
/// Fixed stars scattered over the sky.
const STARS: u32 = 320;
/// Angular radii of the Sun and Moon as drawn, pixels at least.
const BODY_MIN_RADIUS_PX: f32 = 6.0;
/// Pitch of the free-look camera is the orbit-map elevation, inverted so dragging up looks up.
const PITCH_LIMIT_DEG: f32 = 89.0;

/// Where the window faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Look {
    Nadir,
    /// Along the velocity: ahead.
    Ram,
    /// Against the velocity: behind.
    Wake,
    /// `yaw` from ram toward the right, `pitch` above the local horizontal.
    Free {
        yaw_deg: f32,
        pitch_deg: f32,
    },
}

impl Look {
    /// The look for a sub-page (0 nadir, 1 ram, 2 wake, 3 free) and the console's drag camera.
    pub fn from_sub(sub: usize, camera: &crate::globe::Camera) -> Self {
        match sub {
            0 => Self::Nadir,
            1 => Self::Ram,
            2 => Self::Wake,
            _ => Self::Free {
                yaw_deg: camera.azimuth_deg,
                pitch_deg: (-camera.elevation_deg).clamp(-PITCH_LIMIT_DEG, PITCH_LIMIT_DEG),
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Nadir => "NADIR",
            Self::Ram => "RAM (AHEAD)",
            Self::Wake => "WAKE (BEHIND)",
            Self::Free { .. } => "FREE LOOK",
        }
    }
}

/// The local axes at the vehicle: up (away from the Earth), ram (along the velocity, horizontal) and
/// right (completing the set).
fn local_axes(sol: &Solution) -> (Vec3, Vec3, Vec3) {
    let r = to_vec3(sol.state_gcrf.r.into()).normalize_or_zero();
    let v = Vec3::new(
        sol.state_gcrf.v.x as f32,
        sol.state_gcrf.v.y as f32,
        sol.state_gcrf.v.z as f32,
    );
    let ram = (v - r * v.dot(r)).normalize_or_zero();
    (r, ram, ram.cross(r))
}

/// Viewing direction and up vector for a look.
fn direction(look: Look, sol: &Solution) -> (Vec3, Vec3) {
    let (up, ram, right) = local_axes(sol);
    match look {
        Look::Nadir => (-up, ram),
        Look::Ram => (ram, up),
        Look::Wake => (-ram, up),
        Look::Free { yaw_deg, pitch_deg } => {
            let (y, p) = (yaw_deg.to_radians(), pitch_deg.to_radians());
            let level = ram * y.cos() + right * y.sin();
            (level * p.cos() + up * p.sin(), up)
        }
    }
}

/// The globe scene seen from the vehicle: camera at its position, looking per `look`.
pub fn build_scene(look: Look, sol: &Solution, aspect: f32) -> GlobeScene {
    let cam_pos = to_vec3(sol.state_gcrf.r.into());
    let (dir, mut up) = direction(look, sol);
    if dir.dot(up).abs() > 0.999 {
        up = local_axes(sol).1;
    }
    let view = glam::camera::rh::view::look_at_mat4(cam_pos, cam_pos + dir, up);
    let proj = glam::camera::rh::proj::directx::perspective(
        FOV_DEG.to_radians(),
        aspect.max(0.01),
        NEAR_RE,
        FAR_RE,
    );
    GlobeScene {
        view_proj: proj * view,
        cam_pos,
        sun_dir: to_vec3(sol.sun_gcrf_km).normalize_or_zero(),
        ecef: ecef_of(sol),
        // The vehicle is the camera: nothing to mark.
        vehicle: Vec3::ZERO,
        vehicle_occulted: true,
    }
}

/// Horizon distance from the vehicle, km.
pub fn horizon_km(alt_km: f64) -> f64 {
    let r = EARTH_RADIUS_KM as f64;
    ((r + alt_km).powi(2) - r * r).max(0.0).sqrt()
}

/// Angular diameter of the Earth seen from the vehicle, degrees.
pub fn earth_diameter_deg(alt_km: f64) -> f64 {
    let r = EARTH_RADIUS_KM as f64;
    2.0 * (r / (r + alt_km)).asin().to_degrees()
}

/// Paint stars, Sun, Moon and the sub-satellite point over the globe pass.
pub fn paint(
    painter: &Painter,
    theme: &Theme,
    rect_px: [u32; 4],
    scene: &GlobeScene,
    sol: &Solution,
    moon_gcrf_km: Option<[f64; 3]>,
) {
    let p = &theme.palette;
    let font = crate::fonts::font_id(theme);
    let k = (theme.font.cell_h_px / 24.0).clamp(1.0, 3.0);
    let [rx, ry, rw, rh] = rect_px.map(|v| v as f32);
    let inside = |q: Pos2| q.x >= rx && q.x <= rx + rw && q.y >= ry && q.y <= ry + rh;
    let sky = |dir: Vec3| -> Option<Pos2> {
        let at = scene.cam_pos + dir * SKY_RE;
        if segment_hits_unit_sphere(scene.cam_pos, at) {
            return None;
        }
        project(scene, rect_px, at).filter(|q| inside(*q))
    };
    // Stars: fixed directions in the inertial frame, hidden behind the Earth.
    for i in 0..STARS {
        let z = 2.0 * crate::widgets::hash01(i, 11) - 1.0;
        let a = crate::widgets::hash01(i, 12) * std::f32::consts::TAU;
        let s = (1.0 - z * z).sqrt();
        let Some(q) = sky(Vec3::new(s * a.cos(), s * a.sin(), z)) else {
            continue;
        };
        let b = crate::widgets::hash01(i, 13);
        let col = if b > 0.9 { p.bright } else { p.dim }.to_egui();
        let side = if b > 0.9 { 2.0 * k } else { k };
        painter.rect_filled(
            egui::Rect::from_center_size(q, egui::vec2(side, side)),
            0.0,
            col,
        );
    }
    // Sun: a disc with a ring and rays; Moon: an outline. Both said in words.
    let sun = to_vec3(sol.sun_gcrf_km).normalize_or_zero();
    if let Some(q) = sky(sun) {
        let c = p.caution.to_egui();
        painter.circle_filled(q, BODY_MIN_RADIUS_PX * k, p.bright.to_egui());
        painter.circle_stroke(q, BODY_MIN_RADIUS_PX * 1.9 * k, Stroke::new(k, c));
        for i in 0..12 {
            let a = i as f32 / 12.0 * std::f32::consts::TAU;
            let (sn, cs) = a.sin_cos();
            painter.line_segment(
                [
                    q + egui::vec2(cs, sn) * BODY_MIN_RADIUS_PX * 2.3 * k,
                    q + egui::vec2(cs, sn) * BODY_MIN_RADIUS_PX * 3.2 * k,
                ],
                Stroke::new(k, c),
            );
        }
        painter.text(
            q + egui::vec2(BODY_MIN_RADIUS_PX * 3.6 * k, 0.0),
            Align2::LEFT_CENTER,
            "SUN",
            font.clone(),
            c,
        );
    }
    if let Some(m) = moon_gcrf_km {
        let rel = Vec3::new(
            (m[0] - sol.state_gcrf.r.x) as f32,
            (m[1] - sol.state_gcrf.r.y) as f32,
            (m[2] - sol.state_gcrf.r.z) as f32,
        )
        .normalize_or_zero();
        if let Some(q) = sky(rel) {
            painter.circle_stroke(
                q,
                BODY_MIN_RADIUS_PX * k,
                Stroke::new(1.5 * k, p.foreground.to_egui()),
            );
            painter.text(
                q + egui::vec2(BODY_MIN_RADIUS_PX * 1.6 * k, 0.0),
                Align2::LEFT_CENTER,
                "MOON",
                font.clone(),
                p.foreground.to_egui(),
            );
        }
    }
    // The sub-satellite point, where straight down meets the surface.
    let below = to_vec3(sol.state_gcrf.r.into()).normalize_or_zero();
    if let Some(q) = project(scene, rect_px, below).filter(|q| inside(*q)) {
        let (c, r) = (p.bright.to_egui(), 9.0 * k);
        painter.circle_stroke(q, r, Stroke::new(1.5 * k, c));
        painter.add(Shape::line_segment(
            [q - egui::vec2(r * 1.6, 0.0), q + egui::vec2(r * 1.6, 0.0)],
            Stroke::new(k, c),
        ));
        painter.add(Shape::line_segment(
            [q - egui::vec2(0.0, r * 1.6), q + egui::vec2(0.0, r * 1.6)],
            Stroke::new(k, c),
        ));
        painter.text(
            q + egui::vec2(r * 1.8, r * 1.8),
            Align2::LEFT_TOP,
            "SUB-SATELLITE",
            font,
            c,
        );
    }
    // A small reticle at the centre of the window.
    let c = Pos2::new(rx + rw / 2.0, ry + rh / 2.0);
    let col = p.dim.to_egui();
    for d in [-1.0f32, 1.0] {
        painter.line_segment(
            [
                c + egui::vec2(d * 6.0 * k, 0.0),
                c + egui::vec2(d * 14.0 * k, 0.0),
            ],
            Stroke::new(k, col),
        );
        painter.line_segment(
            [
                c + egui::vec2(0.0, d * 6.0 * k),
                c + egui::vec2(0.0, d * 14.0 * k),
            ],
            Stroke::new(k, col),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizon_and_disc_match_the_geometry_at_iss_altitude() {
        // 420 km up the horizon is ~2350 km away and the Earth spans ~140 degrees.
        assert!((horizon_km(420.0) - 2352.0).abs() < 10.0);
        assert!((earth_diameter_deg(420.0) - 140.0).abs() < 1.5);
        assert_eq!(horizon_km(0.0), 0.0);
        assert!((earth_diameter_deg(0.0) - 180.0).abs() < 1e-9);
    }
}
