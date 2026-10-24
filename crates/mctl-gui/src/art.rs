//! Wireframe line art of each vehicle, drawn in the console's own vector style: thin bright lines with a
//! faint glow, slowly turning. The models are stylised from public descriptions of each vehicle's general
//! layout, and are not to scale or to engineering accuracy.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, vec2};

use crate::theme::Theme;

type P3 = [f32; 3];

/// One line of the model; `major` lines are the vehicle's main structure, the rest detail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seg {
    pub a: P3,
    pub b: P3,
    pub major: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Model {
    pub segs: Vec<Seg>,
}

impl Model {
    fn line(&mut self, a: P3, b: P3, major: bool) {
        self.segs.push(Seg { a, b, major });
    }

    fn poly(&mut self, pts: &[P3], closed: bool, major: bool) {
        for w in pts.windows(2) {
            self.line(w[0], w[1], major);
        }
        if closed && pts.len() > 2 {
            self.line(pts[pts.len() - 1], pts[0], major);
        }
    }

    /// A ring of `n` points about the y axis at height `y`, centred at (cx, cz).
    fn ring_y(&mut self, n: usize, r: f32, y: f32, c: (f32, f32), major: bool) -> Vec<P3> {
        let pts: Vec<P3> = (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                [c.0 + r * a.cos(), y, c.1 + r * a.sin()]
            })
            .collect();
        self.poly(&pts, true, major);
        pts
    }

    /// A ring about the z axis (for tubes lying along z).
    fn ring_z(&mut self, n: usize, r: f32, z: f32, c: (f32, f32), major: bool) -> Vec<P3> {
        let pts: Vec<P3> = (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                [c.0 + r * a.cos(), c.1 + r * a.sin(), z]
            })
            .collect();
        self.poly(&pts, true, major);
        pts
    }

    /// A tube about the y axis between two heights, with longitudinal lines.
    #[allow(clippy::too_many_arguments)]
    fn tube_y(&mut self, n: usize, r0: f32, r1: f32, y0: f32, y1: f32, c: (f32, f32), major: bool) {
        let a = self.ring_y(n, r0, y0, c, major);
        let b = self.ring_y(n, r1, y1, c, major);
        for (p, q) in a.iter().zip(&b) {
            self.line(*p, *q, false);
        }
    }

    /// A tube about the z axis between two stations.
    fn tube_z(&mut self, n: usize, r: f32, z0: f32, z1: f32, c: (f32, f32), major: bool) {
        let a = self.ring_z(n, r, z0, c, major);
        let b = self.ring_z(n, r, z1, c, major);
        for (p, q) in a.iter().zip(&b) {
            self.line(*p, *q, false);
        }
    }

    /// A flat rectangle spanned by `u` and `v` from `origin`, with `du` x `dv` grid divisions.
    fn panel(&mut self, origin: P3, u: P3, v: P3, du: usize, dv: usize, major: bool) {
        let at = |s: f32, t: f32| -> P3 {
            [
                origin[0] + u[0] * s + v[0] * t,
                origin[1] + u[1] * s + v[1] * t,
                origin[2] + u[2] * s + v[2] * t,
            ]
        };
        self.poly(
            &[at(0.0, 0.0), at(1.0, 0.0), at(1.0, 1.0), at(0.0, 1.0)],
            true,
            major,
        );
        for i in 1..du {
            let s = i as f32 / du as f32;
            self.line(at(s, 0.0), at(s, 1.0), false);
        }
        for j in 1..dv {
            let t = j as f32 / dv as f32;
            self.line(at(0.0, t), at(1.0, t), false);
        }
    }

    /// An axis-aligned box.
    fn cuboid(&mut self, lo: P3, hi: P3, major: bool) {
        let c = |x: bool, y: bool, z: bool| {
            [
                if x { hi[0] } else { lo[0] },
                if y { hi[1] } else { lo[1] },
                if z { hi[2] } else { lo[2] },
            ]
        };
        for (x, y) in [(false, false), (true, false), (true, true), (false, true)] {
            self.line(c(x, y, false), c(x, y, true), major);
        }
        for z in [false, true] {
            self.poly(
                &[
                    c(false, false, z),
                    c(true, false, z),
                    c(true, true, z),
                    c(false, true, z),
                ],
                true,
                major,
            );
        }
    }

    /// A hexagon in the xz plane at height `y`.
    fn hex_xz(&mut self, cx: f32, cz: f32, y: f32, r: f32, major: bool) {
        let pts: Vec<P3> = (0..6)
            .map(|i| {
                let a = (i as f32 * 60.0 + 30.0).to_radians();
                [cx + r * a.cos(), y, cz + r * a.sin()]
            })
            .collect();
        self.poly(&pts, true, major);
    }
}

/// The International Space Station: truss, four solar-array pairs, radiators and the module stack.
fn iss() -> Model {
    let mut m = Model::default();
    // Integrated truss along x, a square section of 2.
    m.cuboid([-54.0, -1.0, -1.0], [54.0, 1.0, 1.0], true);
    // Four solar-array wing pairs, two blades each, flat in the truss-radial plane.
    for x in [-48.0, -30.0, 30.0, 48.0] {
        for sign in [-1.0f32, 1.0] {
            let (z0, z1) = if sign < 0.0 {
                (-35.0, -2.0)
            } else {
                (2.0, 35.0)
            };
            m.panel(
                [x - 5.5, 0.0, z0],
                [11.0, 0.0, 0.0],
                [0.0, 0.0, z1 - z0],
                4,
                8,
                sign > 0.0,
            );
        }
    }
    // Radiators, offset along the truss.
    for x in [-12.0, 12.0] {
        m.panel(
            [x - 3.0, -1.0, -1.0],
            [6.0, 0.0, 0.0],
            [0.0, -12.0, 0.0],
            3,
            4,
            false,
        );
    }
    // Pressurised stack along z, below the truss.
    m.tube_z(8, 2.0, -26.0, 24.0, (0.0, -5.0), true);
    m.tube_z(8, 2.4, -6.0, 4.0, (0.0, -5.0), false);
    // Cross modules (nodes and laboratories) at right angles.
    m.tube_z(6, 1.6, -10.0, -2.0, (6.0, -5.0), false);
    m.tube_z(6, 1.6, -10.0, -2.0, (-6.0, -5.0), false);
    m.line([0.0, -3.0, 0.0], [0.0, -1.0, 0.0], false);
    m
}

/// Hubble: the optical tube with its aperture door and light shield, two solar wings and the antennas.
fn hubble() -> Model {
    let mut m = Model::default();
    m.tube_z(10, 2.1, -6.6, 6.6, (0.0, 0.0), true);
    m.tube_z(10, 2.3, 4.0, 6.6, (0.0, 0.0), false);
    // Aperture door, half open.
    m.panel(
        [-2.2, 2.2, 6.6],
        [4.4, 0.0, 0.0],
        [0.0, 2.6, 1.5],
        3,
        3,
        false,
    );
    // Solar wings.
    for side in [-1.0f32, 1.0] {
        m.line([side * 2.1, 0.0, 0.0], [side * 3.0, 0.0, 0.0], true);
        m.panel(
            [side * 3.0, -1.3, -3.6],
            [side * 7.1, 0.0, 0.0],
            [0.0, 2.6, 0.0],
            4,
            2,
            true,
        );
        m.panel(
            [side * 3.0, -1.3, 0.0],
            [side * 7.1, 0.0, 0.0],
            [0.0, 2.6, 0.0],
            4,
            2,
            false,
        );
    }
    // High-gain antenna booms and dishes.
    for side in [-1.0f32, 1.0] {
        m.line([side * 1.4, 1.4, -2.0], [side * 3.2, 4.2, -2.8], false);
        m.ring_z(8, 0.6, -2.8, (side * 3.2, 4.2), false);
    }
    m
}

/// JWST: the five-layer sunshield, the 18-segment mirror on its tripod, and the spacecraft bus.
fn jwst() -> Model {
    let mut m = Model::default();
    // Sunshield layers: a kite, 21 x 14.
    for i in 0..5 {
        let y = -2.5 - i as f32 * 0.5;
        let k = 1.0 - i as f32 * 0.012;
        m.poly(
            &[
                [-10.5 * k, y, 0.0],
                [0.0, y, -7.0 * k],
                [10.5 * k, y, 0.0],
                [0.0, y, 7.0 * k],
            ],
            true,
            i == 0 || i == 4,
        );
    }
    m.line([-10.5, -2.5, 0.0], [10.5, -2.5, 0.0], false);
    m.line([0.0, -2.5, -7.0], [0.0, -2.5, 7.0], false);
    // Primary mirror: three rings of hexagons (1 + 6 + 12 = 19 sites, the centre is the empty hole).
    let size = 0.72f32;
    let y = 2.6;
    for q in -2i32..=2 {
        for r in -2i32..=2 {
            if (q + r).abs() > 2 || (q == 0 && r == 0) {
                continue;
            }
            let x = size * 3.0f32.sqrt() * (q as f32 + r as f32 / 2.0);
            let z = size * 1.5 * r as f32;
            m.hex_xz(x, z, y, size * 0.98, true);
        }
    }
    // Secondary mirror on its three-strut boom.
    let tip = [0.0, y + 7.2, 0.0];
    for a in [90.0f32, 210.0, 330.0] {
        let (s, c) = a.to_radians().sin_cos();
        m.line([3.0 * c, y, 3.0 * s], tip, false);
    }
    m.ring_y(8, 0.5, y + 7.2, (0.0, 0.0), false);
    // Spacecraft bus and its trim flap.
    m.cuboid([-1.8, -2.4, -1.8], [1.8, 1.0, 1.8], true);
    m.panel(
        [1.8, -1.6, -2.8],
        [0.0, 0.0, 5.6],
        [3.0, 0.0, 0.0],
        5,
        2,
        false,
    );
    // Tower joining the bus to the telescope.
    m.tube_y(6, 1.0, 1.0, 1.0, y, (0.0, 0.0), false);
    m
}

/// Roman: the hexagonal telescope barrel, sunshade, bus, solar panel and the antenna dish.
fn roman() -> Model {
    let mut m = Model::default();
    m.tube_y(6, 1.4, 1.4, 0.0, 5.0, (0.0, 0.0), true);
    m.tube_y(6, 1.9, 1.9, 4.2, 6.6, (0.0, 0.0), false);
    // Instrument carrier and bus below the barrel.
    m.cuboid([-2.2, -3.0, -2.2], [2.2, 0.0, 2.2], true);
    // Large sun-facing solar panel, canted.
    m.panel(
        [-5.5, -1.6, -3.4],
        [11.0, 0.0, 0.0],
        [0.0, -1.1, 6.8],
        6,
        4,
        true,
    );
    m.line([-2.2, -1.5, 0.0], [-5.5, -1.6, 0.0], false);
    // High-gain antenna dish on a short mast.
    m.line([2.2, -1.0, 0.0], [3.6, 0.2, 0.0], false);
    m.ring_z(10, 1.0, 0.2, (3.6, 0.2), false);
    m.ring_z(10, 0.45, 0.45, (3.6, 0.2), false);
    m
}

/// Crew Dragon: capsule with nose cone, heat-shield base, and the trunk with its body-mounted array.
fn dragon_crew() -> Model {
    let mut m = Model::default();
    // Capsule: a truncated cone, base 3.7 across.
    m.tube_y(12, 1.85, 0.8, 0.0, 3.4, (0.0, 0.0), true);
    // Nose cone, swung open to the front: a shallow cone.
    m.tube_y(12, 0.8, 0.06, 3.4, 4.3, (0.0, 0.0), true);
    // Trunk below the heat shield.
    m.tube_y(12, 1.85, 1.85, -3.4, -0.2, (0.0, 0.0), true);
    // Body-mounted solar array on one side of the trunk: vertical strips.
    for i in 0..8 {
        let a = (-50.0 + i as f32 * 14.0).to_radians();
        m.line(
            [1.88 * a.cos(), -3.2, 1.88 * a.sin()],
            [1.88 * a.cos(), -0.4, 1.88 * a.sin()],
            false,
        );
    }
    // Windows and thruster pods on the capsule.
    for a in [20.0f32, 60.0, 100.0] {
        let (s, c) = a.to_radians().sin_cos();
        m.line([1.5 * c, 2.2, 1.5 * s], [1.0 * c, 2.9, 1.0 * s], false);
    }
    m
}

/// Cargo Dragon: the same capsule over a longer trunk with two deployed array wings.
fn dragon_cargo() -> Model {
    let mut m = Model::default();
    m.tube_y(12, 1.85, 0.8, 0.0, 3.0, (0.0, 0.0), true);
    m.tube_y(12, 0.8, 0.06, 3.0, 3.9, (0.0, 0.0), false);
    m.tube_y(12, 1.85, 1.85, -4.6, -0.2, (0.0, 0.0), true);
    for side in [-1.0f32, 1.0] {
        m.line([side * 1.85, -2.4, 0.0], [side * 2.4, -2.4, 0.0], true);
        m.panel(
            [side * 2.4, -4.2, -1.2],
            [side * 8.0, 0.0, 0.0],
            [0.0, 3.4, 0.0],
            5,
            3,
            true,
        );
    }
    m
}

/// The art for a vehicle id, if there is some.
pub fn model(id: &str) -> Option<Model> {
    Some(match id {
        "iss" => iss(),
        "hubble" => hubble(),
        "jwst" => jwst(),
        "roman" => roman(),
        "dragon-crew" => dragon_crew(),
        "dragon-cargo" => dragon_cargo(),
        _ => return None,
    })
}

/// Largest distance of any model point from the origin, for fitting it to a rectangle.
pub fn radius(m: &Model) -> f32 {
    m.segs
        .iter()
        .flat_map(|s| [s.a, s.b])
        .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
        .fold(1.0, f32::max)
}

/// Turn about the vertical axis by `yaw`, tip toward the viewer by `tilt`, then project with weak perspective.
fn project(p: P3, yaw: f32, tilt: f32, fit: f32, radius: f32, c: Pos2) -> (Pos2, f32) {
    let (sy, cy) = yaw.sin_cos();
    let (x, z) = (p[0] * cy + p[2] * sy, -p[0] * sy + p[2] * cy);
    let (st, ct) = tilt.sin_cos();
    let (y, z) = (p[1] * ct - z * st, p[1] * st + z * ct);
    // Camera 4 radii away: nearer points larger.
    let k = 4.0 * radius / (4.0 * radius - z).max(radius);
    (Pos2::new(c.x + x * fit * k, c.y - y * fit * k), z)
}

/// Draw `m` in `rect_px` (`[x, y, w, h]`), turned by `t_s` seconds of slow rotation.
pub fn paint(painter: &Painter, theme: &Theme, rect_px: [u32; 4], m: &Model, t_s: f64) {
    let [x, y, w, h] = rect_px.map(|v| v as f32);
    if w < 40.0 || h < 40.0 || m.segs.is_empty() {
        return;
    }
    let r = radius(m);
    let fit = (w.min(h) * 0.5 * 0.8) / r;
    let c = Pos2::new(x + w / 2.0, y + h / 2.0);
    let yaw = (t_s as f32 * 0.35) % std::f32::consts::TAU;
    let tilt = 0.45;
    let p = &theme.palette;
    let (bright, mid) = (p.bright.to_egui(), p.foreground.to_egui());
    let with_alpha =
        |col: Color32, a: u8| Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a);
    painter.rect_filled(
        Rect::from_min_size(Pos2::new(x, y), vec2(w, h)),
        0.0,
        Color32::TRANSPARENT,
    );
    for s in &m.segs {
        let ((a, za), (b, zb)) = (
            project(s.a, yaw, tilt, fit, r, c),
            project(s.b, yaw, tilt, fit, r, c),
        );
        // Far lines fade: depth in model units over the model radius.
        let depth = ((za + zb) * 0.5 / r).clamp(-1.0, 1.0);
        let near = 0.55 + 0.45 * (depth * 0.5 + 0.5);
        let col = if s.major { bright } else { mid };
        let alpha = (255.0 * near * if s.major { 1.0 } else { 0.75 }) as u8;
        painter.add(Shape::line_segment(
            [a, b],
            Stroke::new(if s.major { 5.0 } else { 3.5 }, with_alpha(col, alpha / 7)),
        ));
        painter.add(Shape::line_segment(
            [a, b],
            Stroke::new(if s.major { 1.8 } else { 1.1 }, with_alpha(col, alpha)),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: [&str; 6] = [
        "iss",
        "hubble",
        "jwst",
        "roman",
        "dragon-crew",
        "dragon-cargo",
    ];

    #[test]
    fn every_configured_vehicle_has_finite_art_of_a_sensible_size() {
        for id in IDS {
            let m = model(id).unwrap_or_else(|| panic!("no art for {id}"));
            assert!(m.segs.len() > 40, "{id} has only {} lines", m.segs.len());
            assert!(
                m.segs
                    .iter()
                    .all(|s| s.a.iter().chain(&s.b).all(|v| v.is_finite())),
                "{id}"
            );
            let r = radius(&m);
            assert!((3.0..120.0).contains(&r), "{id} radius {r}");
            assert!(
                m.segs.iter().any(|s| s.major) && m.segs.iter().any(|s| !s.major),
                "{id}"
            );
            assert!(
                m.segs.iter().all(|s| s.a != s.b),
                "{id} has a zero-length line"
            );
        }
        assert!(model("unknown").is_none());
    }

    #[test]
    fn the_models_differ_from_each_other() {
        let sizes: Vec<usize> = IDS.iter().map(|i| model(i).unwrap().segs.len()).collect();
        for (i, a) in IDS.iter().enumerate() {
            for b in &IDS[i + 1..] {
                assert_ne!(
                    format!("{:?}", model(a).unwrap().segs),
                    format!("{:?}", model(b).unwrap().segs),
                    "{a} and {b} are the same drawing"
                );
            }
        }
        assert!(sizes.iter().all(|n| *n > 0));
    }

    #[test]
    fn projection_keeps_the_origin_at_the_centre_and_fits_the_box() {
        let c = Pos2::new(100.0, 100.0);
        let (o, _) = project([0.0; 3], 1.2, 0.45, 5.0, 10.0, c);
        assert!((o - c).length() < 1e-3);
        for id in IDS {
            let m = model(id).unwrap();
            let fit = 80.0 / radius(&m);
            for yaw in [0.0, 1.0, 2.0, 3.0, 4.0, 5.0] {
                for s in &m.segs {
                    let (p, _) = project(s.a, yaw, 0.45, fit, radius(&m), c);
                    assert!((p - c).length() < 80.0 * 2.2, "{id} yaw {yaw}: {p:?}");
                }
            }
        }
    }
}
