//! Pixel-drawn widgets that sit in cell rectangles the text grid leaves empty: gauges, sparklines,
//! dials, the L2 orbit plots and the JWST schedule timeline. Colours come from the active theme; a
//! limit state changes colour here and is always also said in text beside the widget (DESIGN.md §9.2:
//! no state relies on hue alone).

use egui::{Align2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, vec2};
use mctl_core::schedule::Ops;
use mctl_core::time::Utc;
use mctl_core::world::{TrackPoint, VehicleView};

use crate::grid::{DeepSeries, Limit, PlotKind, Widget};
use crate::theme::Theme;

/// Mean distance of the Moon, km: the scale bar of the L2 top view.
const MOON_ORBIT_KM: f64 = 384_400.0;
/// Hours of the JWST timeline before and after now.
const TIMELINE_PAST_H: f64 = 6.0;
const TIMELINE_FUTURE_H: f64 = 42.0;
/// Hours of schedule summed in the instrument-share bars.
const SHARE_WINDOW_H: f64 = 168.0;

fn limit_colour(theme: &Theme, limit: Option<Limit>, normal: Color32) -> Color32 {
    let p = &theme.palette;
    match limit {
        Some(Limit::Caution) => p.caution.to_egui(),
        Some(Limit::Warning) => p.warning.to_egui(),
        Some(Limit::Stale) => p.dim.to_egui(),
        None => normal,
    }
}

fn fade(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

fn rect_of(px: [u32; 4]) -> Rect {
    let [x, y, w, h] = px.map(|v| v as f32);
    Rect::from_min_size(Pos2::new(x, y), vec2(w, h))
}

/// Paint one widget. `px` is its pixel rectangle `[x, y, w, h]`.
pub fn paint(
    painter: &Painter,
    theme: &Theme,
    px: [u32; 4],
    w: &Widget,
    vehicle: Option<&VehicleView>,
    now: Utc,
) {
    match w {
        Widget::Bar {
            frac,
            centered,
            limit,
            ticks,
            ..
        } => bar(painter, theme, px, *frac, *centered, *limit, ticks),
        Widget::Chart { series, .. } => chart(painter, theme, px, series),
        Widget::DataBar { frac, .. } => data_bar(painter, theme, px, *frac),
        Widget::Spark { values, limit, .. } => spark(painter, theme, px, values, *limit),
        Widget::Dial { deg, limit, .. } => dial(painter, theme, px, *deg, *limit),
        Widget::Plot { kind, .. } => {
            let Some(v) = vehicle else { return };
            match kind {
                PlotKind::Halo => halo(painter, theme, px, v, now),
                PlotKind::TopView => top_view(painter, theme, px, v, now),
                PlotKind::DeepSeries(s) => deep_series(painter, theme, px, v, now, *s),
                PlotKind::Timeline => timeline(painter, theme, px, v, now),
                PlotKind::InstrumentShare => instrument_share(painter, theme, px, v, now),
            }
        }
    }
}

// ---- gauges -----------------------------------------------------------------------------------------

fn bar(
    painter: &Painter,
    theme: &Theme,
    px: [u32; 4],
    frac: f32,
    centered: bool,
    limit: Option<Limit>,
    ticks: &[f32],
) {
    let r = rect_of(px).shrink2(vec2(2.0, (px[3] as f32 * 0.28).max(2.0)));
    if r.width() < 6.0 || r.height() < 3.0 {
        return;
    }
    let p = &theme.palette;
    painter.rect_filled(r, 0.0, fade(p.border.to_egui(), 60));
    painter.rect_stroke(
        r,
        0.0,
        Stroke::new(1.0, p.border.to_egui()),
        StrokeKind::Inside,
    );
    let colour = limit_colour(theme, limit, p.foreground.to_egui());
    let f = if frac.is_finite() { frac } else { 0.0 };
    let fill = if centered {
        let mid = r.center().x;
        let x = mid + f.clamp(-1.0, 1.0) * r.width() * 0.5;
        painter.line_segment(
            [
                Pos2::new(mid, r.top() - 2.0),
                Pos2::new(mid, r.bottom() + 2.0),
            ],
            Stroke::new(1.0, p.dim.to_egui()),
        );
        Rect::from_x_y_ranges(mid.min(x)..=mid.max(x), r.y_range())
    } else {
        let x = r.left() + f.clamp(0.0, 1.0) * r.width();
        Rect::from_x_y_ranges(r.left()..=x, r.y_range())
    };
    painter.rect_filled(fill.shrink(1.0), 0.0, colour);
    for t in ticks {
        let x = r.left() + t.clamp(0.0, 1.0) * r.width();
        painter.line_segment(
            [Pos2::new(x, r.top()), Pos2::new(x, r.bottom())],
            Stroke::new(1.0, p.bright.to_egui()),
        );
    }
}

/// Recent history of several series on shared axes, oldest on the left, "now" at the right edge.
fn chart(
    painter: &Painter,
    theme: &Theme,
    px: [u32; 4],
    series: &[(String, std::sync::Arc<[f32]>)],
) {
    let r = rect_of(px).shrink2(vec2(6.0, 4.0));
    if r.width() < 80.0 || r.height() < 50.0 || series.is_empty() {
        return;
    }
    let p = &theme.palette;
    let font = crate::fonts::font_id(theme);
    let (mn, mx) = series
        .iter()
        .flat_map(|(_, s)| s.iter().copied())
        .filter(|v| v.is_finite())
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), v| {
            (a.min(v), b.max(v))
        });
    if !mn.is_finite() {
        return;
    }
    let (lo, hi) = crate::trendplot::axis_range(mn, mx);
    let longest = series
        .iter()
        .map(|(_, s)| s.len())
        .max()
        .unwrap_or(2)
        .max(2);
    let area = Rect::from_min_max(r.min + vec2(0.0, 26.0), r.max - vec2(0.0, 26.0));
    painter.rect_stroke(
        area,
        0.0,
        Stroke::new(1.0, p.border.to_egui()),
        StrokeKind::Inside,
    );
    let colours = [
        p.foreground.to_egui(),
        p.info.to_egui(),
        p.bright.to_egui(),
        p.nominal.to_egui(),
    ];
    // Series are right-aligned on "now"; a shorter one starts later.
    let x_of = |i: usize, len: usize| {
        let from_end = (len - 1 - i) as f32 / (longest - 1) as f32;
        area.right() - area.width() * from_end
    };
    let mut legend_x = r.left() + 2.0;
    for (k, (name, s)) in series.iter().enumerate() {
        let colour = colours[k % colours.len()];
        let pts: Vec<Pos2> = s
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_finite())
            .map(|(i, v)| {
                Pos2::new(
                    x_of(i, s.len()),
                    area.bottom() - area.height() * (v - lo) / (hi - lo),
                )
            })
            .collect();
        if pts.len() >= 2 {
            let stroke = Stroke::new(1.5, colour);
            if k >= colours.len() {
                painter.add(Shape::dashed_line(&pts, stroke, 5.0, 3.0));
            } else {
                painter.add(Shape::line(pts, stroke));
            }
        }
        if legend_x < r.right() - 30.0 {
            let galley = painter.text(
                Pos2::new(legend_x, r.top()),
                Align2::LEFT_TOP,
                name,
                font.clone(),
                colour,
            );
            legend_x += galley.width() + 12.0;
        }
    }
    painter.text(
        area.left_top() + vec2(3.0, 1.0),
        Align2::LEFT_TOP,
        crate::trendplot::axis_label(hi),
        font.clone(),
        p.dim.to_egui(),
    );
    painter.text(
        area.left_bottom() + vec2(3.0, -1.0),
        Align2::LEFT_BOTTOM,
        crate::trendplot::axis_label(lo),
        font.clone(),
        p.dim.to_egui(),
    );
    let minutes = (longest - 1) as f64 * mctl_core::world::HISTORY_PERIOD_S / 60.0;
    painter.text(
        Pos2::new(area.left(), r.bottom()),
        Align2::LEFT_BOTTOM,
        format!("-{minutes:.0} min"),
        font.clone(),
        p.dim.to_egui(),
    );
    painter.text(
        Pos2::new(area.right(), r.bottom()),
        Align2::RIGHT_BOTTOM,
        "now",
        font,
        p.dim.to_egui(),
    );
}

/// A translucent fill behind a number; the number stays readable on top of it.
fn data_bar(painter: &Painter, theme: &Theme, px: [u32; 4], frac: f32) {
    let r = rect_of(px).shrink2(vec2(0.0, 3.0));
    if r.width() < 4.0 || !frac.is_finite() {
        return;
    }
    let w = r.width() * frac.clamp(0.0, 1.0);
    painter.rect_filled(
        Rect::from_min_size(r.min, vec2(w, r.height())),
        0.0,
        fade(theme.palette.info.to_egui(), 70),
    );
}

fn spark(painter: &Painter, theme: &Theme, px: [u32; 4], values: &[f32], limit: Option<Limit>) {
    let r = rect_of(px).shrink2(vec2(2.0, 3.0));
    if r.width() < 8.0 || r.height() < 4.0 || values.len() < 2 {
        return;
    }
    let p = &theme.palette;
    let (lo, hi) = values
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), v| {
            (a.min(v), b.max(v))
        });
    if !lo.is_finite() {
        return;
    }
    // A flat line is drawn mid-height rather than dividing by a zero span.
    let span = (hi - lo).max(1.0e-9);
    let flat = hi - lo < 1.0e-9;
    let n = values.len() as f32 - 1.0;
    let pts: Vec<Pos2> = values
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite())
        .map(|(i, v)| {
            let y = if flat { 0.5 } else { (v - lo) / span };
            Pos2::new(
                r.left() + r.width() * i as f32 / n,
                r.bottom() - y * r.height(),
            )
        })
        .collect();
    painter.line_segment(
        [r.left_bottom(), r.right_bottom()],
        Stroke::new(1.0, fade(p.border.to_egui(), 120)),
    );
    let colour = limit_colour(theme, limit, p.info.to_egui());
    if let Some(&last) = pts.last() {
        painter.add(Shape::line(pts, Stroke::new(1.25, colour)));
        painter.circle_filled(last, 2.0, p.bright.to_egui());
    }
}

fn dial(painter: &Painter, theme: &Theme, px: [u32; 4], deg: f32, limit: Option<Limit>) {
    let r = rect_of(px);
    let radius = (r.width().min(r.height()) / 2.0 - 3.0).max(0.0);
    if radius < 8.0 {
        return;
    }
    let p = &theme.palette;
    let c = r.center();
    painter.circle_stroke(c, radius, Stroke::new(1.5, p.border.to_egui()));
    for k in 0..12 {
        let a = (k as f32 * 30.0).to_radians();
        let (s, co) = a.sin_cos();
        let inner = radius * if k % 3 == 0 { 0.78 } else { 0.88 };
        painter.line_segment(
            [
                c + vec2(s * inner, -co * inner),
                c + vec2(s * radius, -co * radius),
            ],
            Stroke::new(1.0, p.dim.to_egui()),
        );
    }
    if deg.is_finite() {
        let a = deg.to_radians();
        let (s, co) = a.sin_cos();
        let colour = limit_colour(theme, limit, p.bright.to_egui());
        painter.line_segment(
            [c, c + vec2(s * radius * 0.85, -co * radius * 0.85)],
            Stroke::new(2.0, colour),
        );
        painter.circle_filled(c, 2.5, colour);
    }
}

// ---- L2 orbit plots ---------------------------------------------------------------------------------

/// A "nice" ring spacing (1, 2, 2.5, 5 x 10^n) giving about three rings out to `max`.
fn nice_step(max: f64) -> f64 {
    let raw = max / 3.0;
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag)
}

fn km_label(km: f64) -> String {
    if km >= 1.0e6 {
        format!("{:.1}M km", km / 1.0e6)
    } else {
        format!("{:.0}k km", km / 1.0e3)
    }
}

/// The track point nearest `t`.
fn nearest(points: &[TrackPoint], t: Utc) -> Option<&TrackPoint> {
    points
        .iter()
        .min_by(|a, b| (a.t.0 - t.0).abs().total_cmp(&(b.t.0 - t.0).abs()))
}

/// Draw a polyline: solid through the past, dashed through the predicted part.
fn past_future(painter: &Painter, theme: &Theme, pts: &[Pos2], now_index: usize) {
    let p = &theme.palette;
    let split = now_index.min(pts.len());
    if split >= 2 {
        painter.add(Shape::line(
            pts[..split].to_vec(),
            Stroke::new(1.5, p.foreground.to_egui()),
        ));
    }
    let tail = &pts[split.saturating_sub(1)..];
    if tail.len() >= 2 {
        painter.add(Shape::dashed_line(
            tail,
            Stroke::new(1.5, p.info.to_egui()),
            6.0,
            4.0,
        ));
    }
}

fn day_ticks(
    painter: &Painter,
    theme: &Theme,
    points: &[TrackPoint],
    now: Utc,
    to_screen: &dyn Fn(&TrackPoint) -> Pos2,
) {
    let p = &theme.palette;
    let font = crate::fonts::font_id(theme);
    for d in [-60.0, -30.0, 30.0, 60.0] {
        if let Some(pt) = nearest(points, now.plus(d * 86_400.0))
            && (pt.t.0 - now.0 - d * 86_400.0).abs() < 2.0 * 86_400.0
        {
            let at = to_screen(pt);
            painter.circle_filled(at, 2.5, p.dim.to_egui());
            painter.text(
                at + vec2(5.0, -2.0),
                Align2::LEFT_BOTTOM,
                format!("{d:+.0}d"),
                font.clone(),
                p.dim.to_egui(),
            );
        }
    }
}

fn now_marker(painter: &Painter, theme: &Theme, at: Pos2, name: &str) {
    let p = &theme.palette;
    painter.circle_filled(at, 5.0, p.bright.to_egui());
    painter.circle_stroke(at, 8.0, Stroke::new(1.0, p.bright.to_egui()));
    painter.text(
        at + vec2(11.0, 0.0),
        Align2::LEFT_CENTER,
        name,
        crate::fonts::font_id(theme),
        p.bright.to_egui(),
    );
}

/// View from Earth along the Sun-Earth line: the vehicle's halo in (Y, Z), with Earth's motion to the left.
fn halo(painter: &Painter, theme: &Theme, px: [u32; 4], v: &VehicleView, now: Utc) {
    let Some(track) = v.track.as_deref().filter(|t| t.points.len() >= 2) else {
        return;
    };
    let r = rect_of(px).shrink(8.0);
    let side = r.width().min(r.height());
    if side < 80.0 {
        return;
    }
    let p = &theme.palette;
    let font = crate::fonts::font_id(theme);
    let c = r.center();
    let max_km = track
        .points
        .iter()
        .flat_map(|q| [q.rot_km[1].abs(), q.rot_km[2].abs()])
        .fold(100_000.0, f64::max)
        * 1.15;
    let half = side / 2.0 - 14.0;
    let scale = half as f64 / max_km;
    let at = |y: f64, z: f64| Pos2::new(c.x - (y * scale) as f32, c.y - (z * scale) as f32);
    // Rings are radii from the Sun-Earth line; the crosshair is that line seen end-on.
    let step = nice_step(max_km);
    let mut ring = step;
    while ring < max_km {
        let rad = (ring * scale) as f32;
        painter.circle_stroke(c, rad, Stroke::new(1.0, fade(p.border.to_egui(), 160)));
        painter.text(
            c + vec2(3.0, -rad),
            Align2::LEFT_BOTTOM,
            km_label(ring),
            font.clone(),
            p.dim.to_egui(),
        );
        ring += step;
    }
    let cross = Stroke::new(1.0, p.dim.to_egui());
    painter.line_segment([c - vec2(half, 0.0), c + vec2(half, 0.0)], cross);
    painter.line_segment([c - vec2(0.0, half), c + vec2(0.0, half)], cross);
    painter.text(
        c + vec2(4.0, 4.0),
        Align2::LEFT_TOP,
        "SUN-EARTH LINE",
        font.clone(),
        p.dim.to_egui(),
    );
    painter.text(
        Pos2::new(c.x, r.top()),
        Align2::CENTER_TOP,
        "ECLIPTIC NORTH",
        font.clone(),
        p.dim.to_egui(),
    );
    painter.text(
        Pos2::new(r.left(), c.y - 14.0),
        Align2::LEFT_BOTTOM,
        "<- EARTH'S MOTION",
        font.clone(),
        p.dim.to_egui(),
    );
    let pts: Vec<Pos2> = track
        .points
        .iter()
        .map(|q| at(q.rot_km[1], q.rot_km[2]))
        .collect();
    past_future(painter, theme, &pts, track.now_index);
    day_ticks(painter, theme, &track.points, now, &|q| {
        at(q.rot_km[1], q.rot_km[2])
    });
    if let Some(d) = v.deep {
        now_marker(painter, theme, at(d.rot_km[1], d.rot_km[2]), &v.name);
    }
}

/// View from ecliptic north: Earth at the origin, the Sun to the left, the Moon's orbit for scale.
fn top_view(painter: &Painter, theme: &Theme, px: [u32; 4], v: &VehicleView, now: Utc) {
    let Some(track) = v.track.as_deref().filter(|t| t.points.len() >= 2) else {
        return;
    };
    let r = rect_of(px).shrink(8.0);
    if r.width() < 100.0 || r.height() < 60.0 {
        return;
    }
    let p = &theme.palette;
    let font = crate::fonts::font_id(theme);
    let (xmin, xmax) = (
        -1.25 * MOON_ORBIT_KM,
        track
            .points
            .iter()
            .map(|q| q.rot_km[0])
            .fold(1.0e6, f64::max)
            * 1.1,
    );
    let ymax = track
        .points
        .iter()
        .map(|q| q.rot_km[1].abs())
        .fold(MOON_ORBIT_KM * 1.1, f64::max)
        * 1.1;
    let scale = (r.width() as f64 / (xmax - xmin)).min(r.height() as f64 / (2.0 * ymax));
    let origin = Pos2::new(r.left() + (-xmin * scale) as f32, r.center().y);
    let at =
        |x: f64, y: f64| Pos2::new(origin.x + (x * scale) as f32, origin.y - (y * scale) as f32);
    // Earth-Sun line.
    painter.line_segment(
        [at(xmin, 0.0), at(xmax, 0.0)],
        Stroke::new(1.0, fade(p.border.to_egui(), 160)),
    );
    let moon = (MOON_ORBIT_KM * scale) as f32;
    if moon > 6.0 {
        painter.add(Shape::dashed_line(
            &circle_points(origin, moon),
            Stroke::new(1.0, p.dim.to_egui()),
            4.0,
            4.0,
        ));
        painter.text(
            origin + vec2(0.0, -moon - 2.0),
            Align2::CENTER_BOTTOM,
            "MOON ORBIT",
            font.clone(),
            p.dim.to_egui(),
        );
    }
    painter.circle_filled(origin, ((6378.0 * scale) as f32).max(3.5), p.info.to_egui());
    painter.text(
        origin + vec2(0.0, 7.0),
        Align2::CENTER_TOP,
        "EARTH",
        font.clone(),
        p.info.to_egui(),
    );
    painter.text(
        Pos2::new(r.left() + 2.0, origin.y - 4.0),
        Align2::LEFT_BOTTOM,
        "<- SUN",
        font.clone(),
        p.dim.to_egui(),
    );
    let l2 = at(1.5e6, 0.0);
    painter.line_segment(
        [l2 - vec2(0.0, 4.0), l2 + vec2(0.0, 4.0)],
        Stroke::new(1.0, p.dim.to_egui()),
    );
    painter.text(
        l2 + vec2(0.0, 6.0),
        Align2::CENTER_TOP,
        "~L2 1.5M km",
        font.clone(),
        p.dim.to_egui(),
    );
    let pts: Vec<Pos2> = track
        .points
        .iter()
        .map(|q| at(q.rot_km[0], q.rot_km[1]))
        .collect();
    past_future(painter, theme, &pts, track.now_index);
    day_ticks(painter, theme, &track.points, now, &|q| {
        at(q.rot_km[0], q.rot_km[1])
    });
    if let Some(d) = v.deep {
        now_marker(painter, theme, at(d.rot_km[0], d.rot_km[1]), &v.name);
    }
}

fn circle_points(c: Pos2, r: f32) -> Vec<Pos2> {
    (0..=96)
        .map(|i| {
            let a = i as f32 / 96.0 * std::f32::consts::TAU;
            c + vec2(r * a.cos(), r * a.sin())
        })
        .collect()
}

/// Time series from the track window, with the present marked.
fn deep_series(
    painter: &Painter,
    theme: &Theme,
    px: [u32; 4],
    v: &VehicleView,
    now: Utc,
    which: DeepSeries,
) {
    let Some(track) = v.track.as_deref().filter(|t| t.points.len() >= 3) else {
        return;
    };
    let pts = &track.points;
    let dist = |q: &TrackPoint| q.r_gcrf_km.iter().map(|c| c * c).sum::<f64>().sqrt();
    let series: Vec<(&str, Vec<(f64, f32)>)> = match which {
        DeepSeries::Distance => vec![(
            "DISTANCE FROM EARTH (1000 km)",
            pts.iter()
                .map(|q| (q.t.0, (dist(q) / 1.0e3) as f32))
                .collect(),
        )],
        DeepSeries::OffAxis => vec![(
            "ANGLE FROM ANTI-SUN LINE (deg)",
            pts.iter()
                .map(|q| {
                    (
                        q.t.0,
                        (q.rot_km[0] / dist(q)).clamp(-1.0, 1.0).acos().to_degrees() as f32,
                    )
                })
                .collect(),
        )],
        DeepSeries::Offsets => vec![
            (
                "Y OFFSET (1000 km)",
                pts.iter()
                    .map(|q| (q.t.0, (q.rot_km[1] / 1.0e3) as f32))
                    .collect(),
            ),
            (
                "Z OFFSET (1000 km)",
                pts.iter()
                    .map(|q| (q.t.0, (q.rot_km[2] / 1.0e3) as f32))
                    .collect(),
            ),
        ],
        DeepSeries::RangeRate => vec![(
            "RANGE RATE, FROM TRACK (m/s)",
            pts.windows(2)
                .map(|w| {
                    (
                        0.5 * (w[0].t.0 + w[1].t.0),
                        ((dist(&w[1]) - dist(&w[0])) / (w[1].t.0 - w[0].t.0) * 1000.0) as f32,
                    )
                })
                .collect(),
        )],
    };
    time_plot(painter, theme, px, &series, now);
}

fn time_plot(
    painter: &Painter,
    theme: &Theme,
    px: [u32; 4],
    series: &[(&str, Vec<(f64, f32)>)],
    now: Utc,
) {
    let r = rect_of(px).shrink2(vec2(6.0, 4.0));
    if r.width() < 60.0 || r.height() < 40.0 {
        return;
    }
    let p = &theme.palette;
    let font = crate::fonts::font_id(theme);
    let (t0, t1) = series
        .iter()
        .flat_map(|(_, s)| [s.first().map(|q| q.0), s.last().map(|q| q.0)])
        .flatten()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), t| {
            (a.min(t), b.max(t))
        });
    let (mn, mx) = series
        .iter()
        .flat_map(|(_, s)| s.iter().map(|q| q.1))
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), v| {
            (a.min(v), b.max(v))
        });
    if t1 <= t0 || !mn.is_finite() {
        return;
    }
    let (lo, hi) = crate::trendplot::axis_range(mn, mx);
    let at = |t: f64, v: f32| {
        Pos2::new(
            r.left() + r.width() * ((t - t0) / (t1 - t0)) as f32,
            r.bottom() - r.height() * (v - lo) / (hi - lo),
        )
    };
    painter.rect_stroke(
        r,
        0.0,
        Stroke::new(1.0, p.border.to_egui()),
        StrokeKind::Inside,
    );
    if (lo..=hi).contains(&0.0) {
        painter.line_segment(
            [at(t0, 0.0), at(t1, 0.0)],
            Stroke::new(1.0, fade(p.border.to_egui(), 140)),
        );
    }
    let nx = at(now.0, lo).x;
    painter.line_segment(
        [Pos2::new(nx, r.top()), Pos2::new(nx, r.bottom())],
        Stroke::new(1.0, p.bright.to_egui()),
    );
    let colours = [p.foreground.to_egui(), p.info.to_egui()];
    for (i, (_, s)) in series.iter().enumerate() {
        let line: Vec<Pos2> = s.iter().map(|(t, v)| at(*t, *v)).collect();
        painter.add(Shape::line(line, Stroke::new(1.5, colours[i % 2])));
    }
    let title: Vec<&str> = series.iter().map(|(n, _)| *n).collect();
    painter.text(
        r.left_top() + vec2(4.0, 2.0),
        Align2::LEFT_TOP,
        title.join(" / "),
        font.clone(),
        p.bright.to_egui(),
    );
    painter.text(
        r.right_top() + vec2(-4.0, 2.0),
        Align2::RIGHT_TOP,
        crate::trendplot::axis_label(hi),
        font.clone(),
        p.dim.to_egui(),
    );
    painter.text(
        r.right_bottom() + vec2(-4.0, -2.0),
        Align2::RIGHT_BOTTOM,
        crate::trendplot::axis_label(lo),
        font.clone(),
        p.dim.to_egui(),
    );
    painter.text(
        Pos2::new(nx + 3.0, r.bottom() - 2.0),
        Align2::LEFT_BOTTOM,
        "NOW",
        font.clone(),
        p.bright.to_egui(),
    );
    for (t, align, pos) in [
        (t0, Align2::LEFT_BOTTOM, r.left() + 4.0),
        (t1, Align2::RIGHT_BOTTOM, r.right() - 4.0),
    ] {
        painter.text(
            Pos2::new(
                pos,
                r.bottom() - 2.0 - if (pos - nx).abs() < 60.0 { 14.0 } else { 0.0 },
            ),
            align,
            format!("{:+.0}d", (t - now.0) / 86_400.0),
            font.clone(),
            p.dim.to_egui(),
        );
    }
}

// ---- JWST schedule ----------------------------------------------------------------------------------

/// Lane order of the timeline.
const LANES: [&str; 6] = ["NIRCam", "NIRSpec", "MIRI", "NIRISS", "FGS", "MAINT"];

fn lane_of(v: &mctl_core::schedule::Visit) -> usize {
    if v.is_maintenance() {
        return LANES.len() - 1;
    }
    LANES
        .iter()
        .position(|l| l.eq_ignore_ascii_case(v.instrument_family()))
        .unwrap_or(LANES.len() - 1)
}

fn timeline(painter: &Painter, theme: &Theme, px: [u32; 4], v: &VehicleView, now: Utc) {
    let Some(Ops::Jwst(s)) = v.ops.as_ref() else {
        return;
    };
    let r = rect_of(px).shrink2(vec2(4.0, 4.0));
    let label_w = 96.0;
    if r.width() < label_w + 80.0 || r.height() < 60.0 {
        return;
    }
    let p = &theme.palette;
    let font = crate::fonts::font_id(theme);
    let area = Rect::from_min_max(
        Pos2::new(r.left() + label_w, r.top() + 24.0),
        Pos2::new(r.right(), r.bottom() - 16.0),
    );
    let (t0, t1) = (
        now.0 - TIMELINE_PAST_H * 3600.0,
        now.0 + TIMELINE_FUTURE_H * 3600.0,
    );
    let x_of = |t: f64| area.left() + area.width() * ((t - t0) / (t1 - t0)).clamp(0.0, 1.0) as f32;
    let lane_h = area.height() / LANES.len() as f32;
    let current = s.current(now).map(|c| c.id.clone());
    for (i, name) in LANES.iter().enumerate() {
        let y = area.top() + i as f32 * lane_h;
        painter.line_segment(
            [Pos2::new(area.left(), y), Pos2::new(area.right(), y)],
            Stroke::new(1.0, fade(p.border.to_egui(), 140)),
        );
        painter.text(
            Pos2::new(r.left(), y + lane_h / 2.0),
            Align2::LEFT_CENTER,
            *name,
            font.clone(),
            p.dim.to_egui(),
        );
    }
    painter.rect_stroke(
        area,
        0.0,
        Stroke::new(1.0, p.border.to_egui()),
        StrokeKind::Inside,
    );
    // Hour ticks every 6 h of GMT.
    let first_tick = (t0 / 21_600.0).ceil() * 21_600.0;
    let mut t = first_tick;
    while t < t1 {
        let x = x_of(t);
        painter.line_segment(
            [Pos2::new(x, area.top()), Pos2::new(x, area.bottom() + 3.0)],
            Stroke::new(1.0, fade(p.border.to_egui(), 120)),
        );
        let (_, _, _, h, _, _) = Utc(t).to_ymd_hms();
        let label = if h == 0 {
            Utc(t).iso()[5..10].to_owned()
        } else {
            format!("{h:02}h")
        };
        painter.text(
            Pos2::new(x, area.bottom() + 3.0),
            Align2::CENTER_TOP,
            label,
            font.clone(),
            p.dim.to_egui(),
        );
        t += 21_600.0;
    }
    for vis in &s.visits {
        let (a, b) = (vis.start.0, vis.end().0);
        if b < t0 || a > t1 {
            continue;
        }
        let lane = lane_of(vis);
        let (xa, xb) = (x_of(a), x_of(b));
        let y = area.top() + lane as f32 * lane_h;
        let blk = Rect::from_min_max(
            Pos2::new(xa, y + 2.0),
            Pos2::new(xb.max(xa + 2.0), y + lane_h - 2.0),
        );
        let is_current = current.as_deref() == Some(vis.id.as_str());
        let colour = if vis.is_maintenance() {
            p.caution.to_egui()
        } else if is_current {
            p.bright.to_egui()
        } else if a < now.0 {
            fade(p.foreground.to_egui(), 110)
        } else {
            fade(p.info.to_egui(), 190)
        };
        painter.rect_filled(blk, 0.0, colour);
        if blk.width() > 50.0 && !vis.target.is_empty() {
            let text: String = vis
                .target
                .chars()
                .take(((blk.width() - 6.0) / 7.0) as usize)
                .collect();
            painter.with_clip_rect(blk).text(
                blk.left_center() + vec2(3.0, 0.0),
                Align2::LEFT_CENTER,
                text,
                font.clone(),
                p.background.to_egui(),
            );
        }
    }
    let nx = x_of(now.0);
    painter.line_segment(
        [
            Pos2::new(nx, area.top() - 6.0),
            Pos2::new(nx, area.bottom()),
        ],
        Stroke::new(2.0, p.bright.to_egui()),
    );
    painter.text(
        Pos2::new(nx, area.top() - 8.0),
        Align2::CENTER_BOTTOM,
        "NOW",
        font,
        p.bright.to_egui(),
    );
}

fn instrument_share(painter: &Painter, theme: &Theme, px: [u32; 4], v: &VehicleView, now: Utc) {
    let Some(Ops::Jwst(s)) = v.ops.as_ref() else {
        return;
    };
    let r = rect_of(px).shrink2(vec2(4.0, 4.0));
    if r.width() < 120.0 || r.height() < 30.0 {
        return;
    }
    let p = &theme.palette;
    let font = crate::fonts::font_id(theme);
    let hours = s.hours_by_instrument(now, now.plus(SHARE_WINDOW_H * 3600.0));
    let total: f64 = hours.iter().map(|(_, h)| h).sum();
    if total <= 0.0 {
        painter.text(
            r.left_top(),
            Align2::LEFT_TOP,
            "NO VISITS SCHEDULED AHEAD",
            font,
            p.dim.to_egui(),
        );
        return;
    }
    let max = hours.iter().map(|(_, h)| *h).fold(0.0, f64::max);
    let row_h = (r.height() / hours.len().max(1) as f32).min(26.0);
    let (label_w, value_w) = (110.0, 170.0);
    for (i, (name, h)) in hours.iter().enumerate() {
        let y = r.top() + i as f32 * row_h;
        let mid = y + row_h / 2.0;
        painter.text(
            Pos2::new(r.left(), mid),
            Align2::LEFT_CENTER,
            name,
            font.clone(),
            p.foreground.to_egui(),
        );
        let bar = Rect::from_min_max(
            Pos2::new(r.left() + label_w, y + 3.0),
            Pos2::new(
                r.left() + label_w + (r.width() - label_w - value_w) * (*h / max) as f32,
                y + row_h - 3.0,
            ),
        );
        let colour = if name == "MAINT" {
            p.caution.to_egui()
        } else {
            p.info.to_egui()
        };
        painter.rect_filled(bar, 0.0, colour);
        painter.text(
            Pos2::new(r.right(), mid),
            Align2::RIGHT_CENTER,
            format!("{h:6.1} h {:3.0}%", h / total * 100.0),
            font.clone(),
            p.dim.to_egui(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_steps_are_round_numbers() {
        assert_eq!(nice_step(900_000.0), 500_000.0);
        assert_eq!(nice_step(300_000.0), 100_000.0);
        assert_eq!(nice_step(1_200_000.0), 500_000.0);
    }
}
