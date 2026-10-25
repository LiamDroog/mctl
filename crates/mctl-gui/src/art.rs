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
    /// Enlargement over the fit to the rectangle, for long thin models; below 1 counts as 1.
    pub zoom: f32,
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

    /// A tube about the x axis between two stations, centred at (y, z) = `c`.
    fn tube_x(&mut self, n: usize, r: f32, x0: f32, x1: f32, c: (f32, f32)) {
        let ring = |x: f32| -> Vec<P3> {
            (0..n)
                .map(|i| {
                    let a = i as f32 / n as f32 * std::f32::consts::TAU;
                    [x, c.0 + r * a.cos(), c.1 + r * a.sin()]
                })
                .collect()
        };
        let (a, b) = (ring(x0), ring(x1));
        self.poly(&a, true, true);
        self.poly(&b, true, true);
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

    /// A latitude/longitude sphere centred at `c`.
    fn sphere(&mut self, c: P3, r: f32, nlat: usize, nlon: usize, major: bool) {
        for i in 1..nlat {
            let lat = (-90.0 + 180.0 * i as f32 / nlat as f32).to_radians();
            self.ring_y(
                28,
                r * lat.cos(),
                c[1] + r * lat.sin(),
                (c[0], c[2]),
                major && i == nlat / 2,
            );
        }
        for j in 0..nlon {
            let lon = j as f32 / nlon as f32 * std::f32::consts::TAU;
            let pts: Vec<P3> = (0..=14)
                .map(|k| {
                    let lat = (-90.0 + 180.0 * k as f32 / 14.0).to_radians();
                    [
                        c[0] + r * lat.cos() * lon.cos(),
                        c[1] + r * lat.sin(),
                        c[2] + r * lat.cos() * lon.sin(),
                    ]
                })
                .collect();
            self.poly(&pts, false, false);
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

/// The International Space Station: truss, four solar-array wing pairs (two blankets each, one wing up and
/// one down from the mast), the white radiators, the Russian segment's small arrays and the module stack.
fn iss() -> Model {
    let mut m = Model::default();
    // Integrated truss along x, a square section of 2.
    m.cuboid([-54.0, -1.0, -1.0], [54.0, 1.0, 1.0], true);
    // Solar arrays: at each of four stations a mast carries a wing up and a wing down, each of two
    // 35 x 5.5 m blankets either side of the mast.
    for x in [-47.0, -33.0, 33.0, 47.0] {
        for sign in [-1.0f32, 1.0] {
            let (z0, z1) = if sign < 0.0 {
                (-36.0, -2.0)
            } else {
                (2.0, 36.0)
            };
            for half in [-1.0f32, 1.0] {
                let x0 = x + half * 0.5 - if half < 0.0 { 5.7 } else { 0.0 };
                m.panel(
                    [x0, 0.0, z0],
                    [5.7, 0.0, 0.0],
                    [0.0, 0.0, z1 - z0],
                    2,
                    10,
                    sign > 0.0,
                );
            }
            // The mast between the blankets.
            m.line([x, 0.0, z0], [x, 0.0, z1], false);
        }
    }
    // Thermal radiators on the inboard truss: big white panels in the plane of the arrays.
    for side in [-1.0f32, 1.0] {
        for k in 0..3 {
            let x0 = side * (9.0 + k as f32 * 4.6);
            m.panel(
                [x0 - if side < 0.0 { 4.2 } else { 0.0 }, 0.0, 2.0],
                [4.2, 0.0, 0.0],
                [0.0, 0.0, 11.0],
                1,
                4,
                false,
            );
        }
    }
    // Pressurised stack along z, below the truss: Zarya and Zvezda one way, Unity, Destiny and Harmony
    // the other.
    m.tube_z(10, 2.0, -26.0, 24.0, (0.0, -5.0), true);
    m.tube_z(10, 2.4, -6.0, 4.0, (0.0, -5.0), false);
    m.tube_z(10, 2.0, -26.0, -20.0, (0.0, -5.0), true);
    // Zvezda's two small arrays at the far end of the stack.
    for side in [-1.0f32, 1.0] {
        m.line([side * 2.0, -5.0, -23.0], [side * 3.0, -5.0, -23.0], false);
        m.panel(
            [
                side * 3.0 - if side < 0.0 { 11.0 } else { 0.0 },
                -5.0,
                -24.7,
            ],
            [11.0, 0.0, 0.0],
            [0.0, 0.0, 3.4],
            4,
            1,
            false,
        );
    }
    // Cross modules at right angles: Columbus and Kibo, the nodes' side ports.
    m.tube_x(8, 1.6, 2.0, 9.0, (-5.0, -8.0));
    m.tube_x(8, 1.6, -9.0, -2.0, (-5.0, -8.0));
    m.tube_x(8, 1.9, 2.0, 10.0, (-5.0, 3.0));
    // The Cupola and docking ports.
    m.sphere([0.0, -8.0, -4.0], 1.0, 4, 6, false);
    m.line([0.0, -3.0, 0.0], [0.0, -1.0, 0.0], false);
    m
}

/// Hubble: the optical tube with its aperture door and light shield, the equipment section, two solar wings
/// of two long panels each, and the antenna masts.
fn hubble() -> Model {
    let mut m = Model::default();
    // Aft shroud, the equipment-bay section, and the forward light shield (narrower), nose toward +z.
    m.tube_z(12, 2.15, -6.6, -2.2, (0.0, 0.0), true);
    m.tube_z(8, 2.2, -2.2, 2.0, (0.0, 0.0), true);
    m.tube_z(12, 2.0, 2.0, 6.6, (0.0, 0.0), true);
    m.ring_z(12, 2.0, 6.6, (0.0, 0.0), true);
    m.ring_z(12, 2.15, -2.2, (0.0, 0.0), false);
    // Aperture door, open on its hinge.
    m.panel(
        [-2.0, 2.0, 6.6],
        [4.0, 0.0, 0.0],
        [0.0, 2.4, 1.6],
        3,
        3,
        false,
    );
    // Solar wings: a boom from the equipment section, then two panels end to end along the tube.
    for side in [-1.0f32, 1.0] {
        m.line([side * 2.2, 0.0, 0.5], [side * 3.2, 0.0, 0.5], true);
        for (z0, z1) in [(-6.4, 0.2), (0.8, 7.4)] {
            m.panel(
                [side * 3.2, -0.25, z0],
                [side * 2.7, 0.0, 0.0],
                [0.0, 0.0, z1 - z0],
                3,
                5,
                true,
            );
        }
    }
    // The two high-gain antenna masts with their dishes.
    for side in [-1.0f32, 1.0] {
        m.line([side * 1.4, 1.6, -4.0], [side * 2.2, 4.2, -4.6], false);
        m.ring_z(8, 0.7, -4.6, (side * 2.2, 4.2), false);
        m.ring_z(8, 0.3, -4.4, (side * 2.2, 4.2), false);
    }
    // Aft-bay doors and the radial instrument bays: a few longitudinal ribs.
    for a in [0.0f32, 90.0, 180.0, 270.0] {
        let (s, c) = a.to_radians().sin_cos();
        m.line([2.2 * c, 2.2 * s, -2.2], [2.2 * c, 2.2 * s, 2.0], false);
    }
    m
}

/// JWST: the five-layer kite sunshield, the 18-segment mirror on its tower, the secondary on its boom, and
/// the spacecraft bus with its solar panel, antenna and trim flap on the hot side.
fn jwst() -> Model {
    let mut m = Model::default();
    // Sunshield layers: a kite 21 x 14, five membranes with gaps between.
    for i in 0..5 {
        let y = -2.4 - i as f32 * 0.55;
        let k = 1.0 - i as f32 * 0.018;
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
    // The spreader booms and the mid-booms that hold the layers out.
    m.line([-10.5, -2.4, 0.0], [10.5, -2.4, 0.0], false);
    m.line([0.0, -2.4, -7.0], [0.0, -2.4, 7.0], false);
    for x in [-10.5, 10.5] {
        m.line([x, -2.4, 0.0], [x, -4.6, 0.0], false);
    }
    // Primary mirror: two rings of hexagons round the empty centre (6 + 12 = 18 segments).
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
    // Backplane: the frame behind the mirror, and the folding wing hinges.
    m.cuboid([-0.9, y - 1.2, -0.9], [0.9, y - 0.1, 0.9], false);
    // Secondary mirror on its three-strut boom.
    let tip = [0.0, y + 7.2, 0.0];
    for a in [90.0f32, 210.0, 330.0] {
        let (s, c) = a.to_radians().sin_cos();
        m.line([3.0 * c, y, 3.0 * s], tip, false);
    }
    m.ring_y(8, 0.5, y + 7.2, (0.0, 0.0), false);
    // Spacecraft bus under the shield (the hot side), with its trim flap.
    m.cuboid([-1.8, -9.0, -1.8], [1.8, -5.6, 1.8], true);
    m.panel(
        [1.8, -8.6, -2.6],
        [0.0, 0.0, 5.2],
        [3.2, 0.0, 0.0],
        5,
        2,
        false,
    );
    // The flat solar panel on the sun side of the bus, and the antenna.
    m.panel(
        [-1.9, -5.6, -2.4],
        [-0.0, 0.0, 4.8],
        [-3.6, 0.0, 0.0],
        4,
        2,
        true,
    );
    m.line([0.0, -9.0, 0.0], [0.0, -10.4, 0.0], false);
    m.ring_y(8, 0.6, -10.4, (0.0, 0.0), false);
    // Tower from the bus through the shield to the telescope.
    m.tube_y(6, 0.9, 0.9, -5.6, y - 1.2, (0.0, 0.0), false);
    m
}

/// Roman: the round telescope barrel under its aperture cover, the aft instrument carrier, the big fixed
/// solar panel, and the antenna dish on its mast.
fn roman() -> Model {
    let mut m = Model::default();
    // Outer barrel and aperture cover, nose toward +y.
    m.tube_y(16, 2.1, 2.1, 0.0, 6.5, (0.0, 0.0), true);
    m.ring_y(16, 2.1, 7.4, (0.0, 0.0), true);
    m.tube_y(16, 2.1, 2.1, 6.5, 7.4, (0.0, 0.0), false);
    m.ring_y(16, 1.2, 6.5, (0.0, 0.0), false);
    // Instrument carrier and spacecraft bus aft of the barrel.
    m.cuboid([-2.6, -4.0, -2.6], [2.6, 0.0, 2.6], true);
    m.cuboid([-2.8, -4.4, -2.8], [2.8, -4.0, 2.8], false);
    // The large solar panel: a roof over the barrel, on standoffs, canted toward the Sun.
    m.panel(
        [2.9, -3.0, -3.6],
        [0.4, 9.6, 0.0],
        [-0.8, 0.0, 7.2],
        6,
        8,
        true,
    );
    for (y, z) in [(-2.0, -2.0), (-2.0, 2.0), (4.5, -2.0), (4.5, 2.0)] {
        m.line([2.0, y, z * 0.7], [2.9, y + 0.1, z * 1.6], false);
    }
    // High-gain antenna on a mast off the bus.
    m.line([2.6, -2.0, 0.0], [4.2, -1.4, 0.0], false);
    m.ring_z(12, 1.1, -1.4, (4.2, -1.4), false);
    m.ring_z(12, 0.4, -1.1, (4.2, -1.4), false);
    // Radiator and sunshade vanes at the aft end.
    m.line([-2.6, -2.0, 2.6], [-3.8, -2.0, 3.8], false);
    m.line([2.6, -2.0, -2.6], [3.8, -2.0, -3.8], false);
    m
}

/// The Crew Dragon shape both Dragons share: a rounded cone over a 4 m skirt, the nose cone, the trunk with
/// its two fins, and the body-mounted array. Returns the model for the caller to add to.
fn dragon_body(m: &mut Model, nose_open: bool) {
    // Capsule: skirt, shoulder, cone. Radii in metres, 1 unit = 1 m.
    m.ring_y(16, 2.0, 0.0, (0.0, 0.0), true);
    m.ring_y(16, 2.0, 0.5, (0.0, 0.0), false);
    m.ring_y(16, 1.75, 1.6, (0.0, 0.0), true);
    m.ring_y(16, 1.3, 2.6, (0.0, 0.0), false);
    m.ring_y(16, 0.95, 3.4, (0.0, 0.0), true);
    for i in 0..16 {
        let a = i as f32 / 16.0 * std::f32::consts::TAU;
        let (s, c) = a.sin_cos();
        let pts = [(2.0, 0.0), (2.0, 0.5), (1.75, 1.6), (1.3, 2.6), (0.95, 3.4)];
        for w in pts.windows(2) {
            m.line(
                [w[0].0 * c, w[0].1, w[0].0 * s],
                [w[1].0 * c, w[1].1, w[1].0 * s],
                false,
            );
        }
    }
    // Nose cone: a dome over the docking hatch, hinged up when open.
    if nose_open {
        m.ring_y(12, 0.85, 3.45, (0.0, 0.0), false);
        m.line([0.85, 3.45, 0.0], [1.3, 4.7, 0.0], true);
        m.line([-0.85, 3.45, 0.0], [-1.3, 4.7, 0.0], true);
        m.line([1.3, 4.7, 0.0], [-1.3, 4.7, 0.0], true);
    } else {
        m.tube_y(12, 0.95, 0.7, 3.4, 3.9, (0.0, 0.0), true);
        m.tube_y(12, 0.7, 0.1, 3.9, 4.3, (0.0, 0.0), true);
    }
    // Trunk, 4 m across and 3.6 m long, with two fins flaring from the aft end.
    m.tube_y(16, 2.0, 2.0, -3.6, -0.2, (0.0, 0.0), true);
    for side in [-1.0f32, 1.0] {
        m.poly(
            &[
                [side * 2.0, -3.6, 0.0],
                [side * 3.0, -4.6, 0.0],
                [side * 3.0, -3.6, 0.0],
                [side * 2.0, -0.6, 0.0],
            ],
            true,
            true,
        );
    }
    // The body-mounted array: strips of cells up the trunk's sun-side half.
    for i in 0..9 {
        let a = (-60.0 + i as f32 * 15.0).to_radians();
        m.line(
            [2.02 * a.cos(), -3.4, 2.02 * a.sin()],
            [2.02 * a.cos(), -0.4, 2.02 * a.sin()],
            false,
        );
    }
}

/// Crew Dragon: capsule with nose cone, the four SuperDraco pods, windows, and the finned trunk.
fn dragon_crew() -> Model {
    let mut m = Model::default();
    dragon_body(&mut m, false);
    // Heat shield rings under the skirt.
    for r in [1.5, 0.9, 0.35] {
        m.ring_y(12, r, 0.0, (0.0, 0.0), false);
    }
    // SuperDraco pods: boxes in pairs on the skirt at four stations.
    for i in 0..4 {
        let a = (45.0 + i as f32 * 90.0).to_radians();
        let (c, s) = (1.95 * a.cos(), 1.95 * a.sin());
        m.cuboid([c - 0.3, 0.7, s - 0.3], [c + 0.3, 1.5, s + 0.3], false);
    }
    // The window row on the cone.
    for a in [-25.0f32, 0.0, 25.0] {
        let (s, c) = (a + 90.0).to_radians().sin_cos();
        m.line([1.5 * c, 2.0, 1.5 * s], [1.3 * c, 2.5, 1.3 * s], false);
    }
    m
}

/// Cargo Dragon: the same capsule and finned trunk, no SuperDracos or windows, with two array wings out.
fn dragon_cargo() -> Model {
    let mut m = Model::default();
    dragon_body(&mut m, false);
    // Two deployed wings, each a panel of cells on a short boom out of the trunk.
    for side in [-1.0f32, 1.0] {
        m.line([side * 2.0, -2.0, 0.0], [side * 2.8, -2.0, 0.0], true);
        m.panel(
            [side * 2.8 - if side < 0.0 { 6.5 } else { 0.0 }, -3.6, -0.02],
            [6.5, 0.0, 0.0],
            [0.0, 3.2, 0.0],
            5,
            2,
            true,
        );
    }
    m
}

/// Falcon 9: nine engines, grid fins, landing legs, the interstage, second stage and fairing.
fn falcon9() -> Model {
    let mut m = Model::default();
    let r = 0.5;
    // First stage, interstage, second stage, then the fairing ogive.
    m.tube_y(12, r, r, -9.6, 1.6, (0.0, 0.0), true);
    m.ring_y(12, r, 0.2, (0.0, 0.0), false);
    m.ring_y(12, r, -3.5, (0.0, 0.0), false);
    m.tube_y(12, r, r, 1.6, 2.5, (0.0, 0.0), false);
    m.tube_y(12, r, r, 2.5, 5.7, (0.0, 0.0), true);
    m.tube_y(12, r, 0.46, 5.7, 7.3, (0.0, 0.0), true);
    m.tube_y(12, 0.46, 0.2, 7.3, 8.6, (0.0, 0.0), true);
    m.tube_y(12, 0.2, 0.02, 8.6, 9.4, (0.0, 0.0), false);
    // The octaweb of engines: one centre, eight round it.
    m.ring_y(8, 0.12, -9.9, (0.0, 0.0), false);
    for i in 0..8 {
        let a = i as f32 / 8.0 * std::f32::consts::TAU;
        m.ring_y(6, 0.12, -9.9, (0.3 * a.cos(), 0.3 * a.sin()), false);
    }
    m.tube_y(10, 0.45, 0.4, -9.9, -9.6, (0.0, 0.0), false);
    // Four landing legs folded out from the base.
    for i in 0..4 {
        let a = (45.0 + i as f32 * 90.0).to_radians();
        let (c, s) = (a.cos(), a.sin());
        m.line([r * c, -8.0, r * s], [1.5 * c, -10.4, 1.5 * s], true);
        m.line([r * c, -9.6, r * s], [1.5 * c, -10.4, 1.5 * s], false);
    }
    // Grid fins near the top of the first stage.
    for i in 0..4 {
        let a = (i as f32 * 90.0).to_radians();
        let (c, s) = (a.cos(), a.sin());
        let (tx, tz) = (-s, c);
        let base = [(r + 0.02) * c, 1.3, (r + 0.02) * s];
        let out = [(r + 0.55) * c, 1.3, (r + 0.55) * s];
        let at = |p: P3, dy: f32, d: f32| [p[0] + tx * d, p[1] + dy, p[2] + tz * d];
        m.poly(
            &[
                at(base, -0.2, -0.3),
                at(out, -0.2, -0.3),
                at(out, 0.2, 0.3),
                at(base, 0.2, 0.3),
            ],
            true,
            false,
        );
    }
    m
}

/// The Space Shuttle stack: the orbiter on the orange tank's flank between the two boosters.
/// One unit is about 3 m.
fn shuttle() -> Model {
    let mut m = Model::default();
    // External tank: 8.4 m across and 47 m long, with the ogive nose and the intertank ring.
    m.tube_y(16, 1.4, 1.4, -8.0, 5.6, (0.0, 0.0), true);
    m.tube_y(16, 1.4, 0.05, 5.6, 9.2, (0.0, 0.0), true);
    m.ring_y(16, 1.4, 1.6, (0.0, 0.0), false);
    m.ring_y(16, 1.4, 3.6, (0.0, 0.0), false);
    // Solid boosters: 3.7 m across, nose cones, and the engine skirts below the tank.
    for side in [-1.0f32, 1.0] {
        let c = (side * 2.3, 0.0);
        m.tube_y(12, 0.62, 0.62, -7.8, 5.0, c, true);
        m.tube_y(12, 0.62, 0.05, 5.0, 7.4, c, true);
        m.tube_y(10, 0.45, 0.62, -8.6, -7.8, c, false);
        m.ring_y(12, 0.62, -3.0, c, false);
    }
    // The orbiter on the tank's front face, nose up: fuselage, cockpit, payload-bay doors, delta wings.
    let z = 2.1;
    m.tube_y(12, 0.85, 0.85, -7.0, 2.4, (0.0, z), true);
    m.tube_y(12, 0.85, 0.5, 2.4, 3.8, (0.0, z), true);
    m.tube_y(12, 0.5, 0.05, 3.8, 4.5, (0.0, z), true);
    m.line([-0.45, -5.8, z + 0.85], [-0.45, 2.0, z + 0.85], false);
    m.line([0.45, -5.8, z + 0.85], [0.45, 2.0, z + 0.85], false);
    for side in [-1.0f32, 1.0] {
        m.poly(
            &[
                [side * 0.85, 0.2, z],
                [side * 1.6, -2.6, z],
                [side * 4.0, -6.6, z],
                [side * 4.0, -7.0, z],
                [side * 0.85, -6.8, z],
            ],
            true,
            true,
        );
        // Elevons along the trailing edge.
        m.line([side * 1.2, -6.9, z], [side * 4.0, -6.9, z], false);
    }
    // Vertical tail and the OMS pods and main engines at the aft end.
    m.poly(
        &[
            [0.0, -4.4, z + 0.85],
            [0.0, -6.9, z + 2.9],
            [0.0, -7.2, z + 2.9],
            [0.0, -6.6, z + 0.85],
        ],
        true,
        true,
    );
    for side in [-1.0f32, 1.0] {
        m.cuboid(
            [side * 0.6 - 0.25, -7.0, z + 0.4],
            [side * 0.6 + 0.25, -5.4, z + 1.0],
            false,
        );
    }
    for (x, dz) in [(0.0, 0.3), (-0.4, -0.25), (0.4, -0.25)] {
        m.ring_y(6, 0.2, -7.5, (x, z + dz), false);
    }
    // The struts holding the orbiter and boosters to the tank.
    m.line([0.0, 2.0, 1.4], [0.0, 2.0, z], false);
    m.line([-0.8, -5.0, 1.45], [-1.7, -5.0, 0.0], false);
    m.line([0.8, -5.0, 1.45], [1.7, -5.0, 0.0], false);
    m
}

/// Earth: the sphere, the equator, and an inclined orbit with a satellite on it.
fn earth() -> Model {
    let mut m = Model::default();
    m.sphere([0.0; 3], 5.0, 8, 12, true);
    // The orbit: a ring tilted 51.6 degrees, 1.3 radii up.
    let tilt = 51.6f32.to_radians();
    let (st, ct) = tilt.sin_cos();
    let orbit: Vec<P3> = (0..64)
        .map(|i| {
            let a = i as f32 / 64.0 * std::f32::consts::TAU;
            let (x, z) = (6.6 * a.cos(), 6.6 * a.sin());
            [x, -z * st, z * ct]
        })
        .collect();
    m.poly(&orbit, true, true);
    // The satellite: a tiny box with panels.
    let p = orbit[9];
    m.cuboid(
        [p[0] - 0.15, p[1] - 0.15, p[2] - 0.15],
        [p[0] + 0.15, p[1] + 0.15, p[2] + 0.15],
        true,
    );
    m.line([p[0] - 0.9, p[1], p[2]], [p[0] + 0.9, p[1], p[2]], true);
    m
}

/// Art that is not tied to a configured vehicle: the gallery and the kiosk's interludes. Only vehicles
/// the program has a use for are drawn; more come with the features that need them.
#[derive(Debug, Clone, Copy)]
pub struct Extra {
    pub id: &'static str,
    pub title: &'static str,
    pub caption: &'static str,
}

pub const EXTRAS: &[Extra] = &[
    Extra {
        id: "falcon9",
        title: "FALCON 9",
        caption: "LAUNCHES DRAGON",
    },
    Extra {
        id: "shuttle",
        title: "SPACE SHUTTLE",
        caption: "1981 TO 2011",
    },
    Extra {
        id: "earth",
        title: "EARTH",
        caption: "ISS ORBIT 51.6 DEG",
    },
];

/// The art for a vehicle id, if there is some.
pub fn model(id: &str) -> Option<Model> {
    let mut m = match id {
        "iss" => iss(),
        "hubble" => hubble(),
        "jwst" => jwst(),
        "roman" => roman(),
        "dragon-crew" => dragon_crew(),
        "dragon-cargo" => dragon_cargo(),
        "falcon9" => falcon9(),
        "shuttle" => shuttle(),
        "earth" => earth(),
        _ => return None,
    };
    m.zoom = match id {
        "falcon9" => 1.3,
        _ => 1.0,
    };
    Some(m)
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
    let fit = (w.min(h) * 0.5 * 0.8) / r * m.zoom.max(1.0);
    let c = Pos2::new(x + w / 2.0, y + h / 2.0);
    let yaw = (t_s as f32 * 0.35) % std::f32::consts::TAU;
    let tilt = 0.45;
    let p = &theme.palette;
    let (bright, mid) = (p.bright.to_egui(), p.foreground.to_egui());
    // Lines thicken with the cell size, so the art holds up on a low-resolution display.
    let k = (theme.font.cell_h_px / 24.0).clamp(1.0, 3.0);
    let with_alpha =
        |col: Color32, a: u8| Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a);
    painter.rect_filled(
        Rect::from_min_size(Pos2::new(x, y), vec2(w, h)),
        0.0,
        Color32::TRANSPARENT,
    );
    // A turntable under the model: a flat ring with ticks, so it stands on something.
    let floor = m
        .segs
        .iter()
        .flat_map(|s| [s.a[1], s.b[1]])
        .fold(f32::MAX, f32::min);
    let (lo, hi) = m
        .segs
        .iter()
        .flat_map(|s| [s.a[1], s.b[1]])
        .fold((f32::MAX, f32::MIN), |(l, h), y| (l.min(y), h.max(y)));
    let ring_r = r * 0.95;
    let ring_y = floor - r * 0.06;
    let ring: Vec<Pos2> = (0..=48)
        .map(|i| {
            let a = i as f32 / 48.0 * std::f32::consts::TAU;
            project(
                [ring_r * a.cos(), ring_y, ring_r * a.sin()],
                yaw,
                tilt,
                fit,
                r,
                c,
            )
            .0
        })
        .collect();
    painter.add(Shape::line(ring, Stroke::new(k, with_alpha(mid, 70))));
    for i in 0..12 {
        let a = i as f32 / 12.0 * std::f32::consts::TAU + yaw;
        let (s, co) = a.sin_cos();
        let p0 = project([ring_r * co, ring_y, ring_r * s], 0.0, tilt, fit, r, c).0;
        let p1 = project(
            [ring_r * 1.08 * co, ring_y, ring_r * 1.08 * s],
            0.0,
            tilt,
            fit,
            r,
            c,
        )
        .0;
        painter.line_segment([p0, p1], Stroke::new(k, with_alpha(mid, 90)));
    }
    // A bright band sweeps up the model every few seconds, lighting the lines it passes.
    let sweep = lo + (hi - lo) * ((t_s as f32 * 0.25) % 1.4 - 0.2);
    let band = (hi - lo).max(1.0) * 0.07;
    for s in &m.segs {
        let ((a, za), (b, zb)) = (
            project(s.a, yaw, tilt, fit, r, c),
            project(s.b, yaw, tilt, fit, r, c),
        );
        // Far lines fade: depth in model units over the model radius.
        let depth = ((za + zb) * 0.5 / r).clamp(-1.0, 1.0);
        let near = 0.55 + 0.45 * (depth * 0.5 + 0.5);
        let glint = (1.0 - ((s.a[1] + s.b[1]) * 0.5 - sweep).abs() / band).clamp(0.0, 1.0);
        let col = if glint > 0.0 {
            Color32::WHITE
        } else if s.major {
            bright
        } else {
            mid
        };
        let alpha = (255.0 * near * if s.major { 1.0 } else { 0.75 }).max(255.0 * glint) as u8;
        painter.add(Shape::line_segment(
            [a, b],
            Stroke::new(
                k * if s.major { 5.0 } else { 3.5 },
                with_alpha(col, alpha / 7),
            ),
        ));
        painter.add(Shape::line_segment(
            [a, b],
            Stroke::new(k * if s.major { 1.8 } else { 1.1 }, with_alpha(col, alpha)),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: [&str; 9] = [
        "iss",
        "hubble",
        "jwst",
        "roman",
        "dragon-crew",
        "dragon-cargo",
        "falcon9",
        "shuttle",
        "earth",
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
    fn every_extra_has_art() {
        for e in EXTRAS {
            assert!(model(e.id).is_some(), "{}", e.id);
            assert!(IDS.contains(&e.id), "{} is not in the test list", e.id);
        }
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
