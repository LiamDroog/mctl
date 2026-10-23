//! The MASTER page (DESIGN.md §9.5), built only from the engine's published snapshot.
//!
//! Rule (DESIGN.md §1.3): a quantity with no source is shown as `NO DATA`, never as a plausible
//! substitute. Panels whose data source belongs to a later milestone say so rather than inventing content.

use egui::{Painter, Pos2, Stroke};
use mctl_core::config::Limits;
use mctl_core::time::Utc;
use mctl_core::world::{Health, SourceStatus, SourceView, Track, VehicleView, World};

use crate::globe::Camera;
use crate::grid::{CellRect, Grid, Limit, Role, Style};
use crate::render::EffectToggles;
use crate::theme::Theme;

/// Smallest grid for which the layout is designed (DESIGN.md §9.5).
pub const MIN_COLS: usize = 160;
pub const MIN_ROWS: usize = 45;

/// Everything the page needs for one frame.
pub struct PageInput<'a> {
    pub world: &'a World,
    pub vehicle: Option<&'a VehicleView>,
    pub theme: &'a Theme,
    pub toggles: EffectToggles,
    pub camera: &'a Camera,
    pub show_ecef: bool,
    pub limits: &'a Limits,
    pub frame_ms: f32,
    pub gpu_ms: Option<f32>,
    pub adapter: &'a str,
    pub size_px: [u32; 2],
    pub page: crate::pages::Page,
    pub sub: usize,
    /// The command line, when open.
    pub command: Option<&'a CommandView>,
    /// Last command result or error, shown briefly in the footer.
    pub status: Option<&'a StatusMsg>,
    pub help: bool,
    /// Static key/value rows for the SETTINGS page (configuration echoes).
    pub settings: &'a [(String, String)],
}

pub struct CommandView {
    pub text: String,
    pub candidates: Vec<String>,
}

pub struct StatusMsg {
    pub text: String,
    pub error: bool,
}

/// Panels that need pixel-level drawing, when the current page has them.
#[derive(Default)]
pub struct Layout {
    pub globe: Option<CellRect>,
    pub ground: Option<CellRect>,
    pub eclipse: Option<CellRect>,
    pub adi: Option<CellRect>,
    pub sky: Option<CellRect>,
}

pub use mctl_core::time::fmt_dur;

pub(crate) fn quality_text(sol: &mctl_core::world::Solution) -> String {
    format!("{} ({})", sol.quality.label(), sol.source.label())
}

/// An upcoming event read off the sampled track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackEvent {
    pub t: Utc,
    pub label: &'static str,
}

/// Ascending/descending node crossings and eclipse entry/exit after `now`, interpolated linearly between
/// track samples (so accurate to a few seconds). Formal event detection with root finding is M4.
pub fn upcoming_events(track: &Track, now: Utc) -> Vec<TrackEvent> {
    let mut out = Vec::new();
    for w in track.points.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        let cross = |va: f64, vb: f64| {
            (va - vb != 0.0)
                .then(|| va / (va - vb))
                .filter(|f| (0.0..=1.0).contains(f))
        };
        let at = |f: f64| Utc(a.t.0 + (b.t.0 - a.t.0) * f);
        let (za, zb) = (a.r_gcrf_km[2], b.r_gcrf_km[2]);
        if za < 0.0 && zb >= 0.0 {
            if let Some(f) = cross(za, zb) {
                out.push(TrackEvent {
                    t: at(f),
                    label: "ASCENDING NODE",
                });
            }
        } else if za > 0.0
            && zb <= 0.0
            && let Some(f) = cross(za, zb)
        {
            out.push(TrackEvent {
                t: at(f),
                label: "DESCENDING NODE",
            });
        }
        let (ia, ib) = (
            f64::from(a.illumination) - 0.5,
            f64::from(b.illumination) - 0.5,
        );
        if ia >= 0.0 && ib < 0.0 {
            if let Some(f) = cross(ia, ib) {
                out.push(TrackEvent {
                    t: at(f),
                    label: "ECLIPSE ENTRY",
                });
            }
        } else if ia < 0.0
            && ib >= 0.0
            && let Some(f) = cross(ia, ib)
        {
            out.push(TrackEvent {
                t: at(f),
                label: "ECLIPSE EXIT",
            });
        }
    }
    out.retain(|e| e.t.0 > now.0);
    out.sort_by(|x, y| x.t.0.total_cmp(&y.t.0));
    out
}

/// Latitude (deg) of the day/night terminator at `lon_deg`, for a Sun at (`subsolar_lon_deg`, declination `dec_deg`).
pub fn terminator_lat_deg(lon_deg: f64, subsolar_lon_deg: f64, dec_deg: f64) -> f64 {
    // A tiny declination makes tan() blow up; the terminator is then the meridian pair, so clamp.
    let dec = dec_deg.to_radians();
    let dec = if dec.abs() < 1.0e-4 {
        1.0e-4_f64.copysign(if dec == 0.0 { 1.0 } else { dec })
    } else {
        dec
    };
    (-(lon_deg - subsolar_lon_deg).to_radians().cos() / dec.tan())
        .atan()
        .to_degrees()
}

pub(crate) struct Lines<'a> {
    pub(crate) grid: &'a mut Grid,
    pub(crate) rect: CellRect,
    pub(crate) row: usize,
}

impl<'a> Lines<'a> {
    pub(crate) fn new(grid: &'a mut Grid, panel: CellRect) -> Self {
        Self {
            grid,
            rect: panel.inner(),
            row: 0,
        }
    }

    pub(crate) fn put(&mut self, col: usize, text: &str, style: Style) {
        if self.row < self.rect.rows && col < self.rect.cols {
            let room = self.rect.cols - col;
            let t: String = text.chars().take(room).collect();
            self.grid
                .put(self.rect.col + col, self.rect.row + self.row, &t, style);
        }
    }

    pub(crate) fn line(&mut self, text: &str, style: Style) {
        self.put(1, text, style);
        self.row += 1;
    }

    pub(crate) fn pair(&mut self, label: &str, value: &str, unit: &str, value_style: Style) {
        let dim = Style::new(Role::Dim);
        self.put(1, label, dim);
        self.put(8, value, value_style);
        self.put(8 + value.chars().count() + 1, unit, dim);
        self.row += 1;
    }

    pub(crate) fn no_data(&mut self, why: &str) {
        self.line("NO DATA", Style::new(Role::Dim));
        self.line(why, Style::new(Role::Dim));
    }
}

pub(crate) fn source_row(l: &mut Lines<'_>, s: &SourceView, caution_km: f64) {
    let (status, style) = match &s.status {
        SourceStatus::Active => ("ACTIVE", Style::new(Role::Bright)),
        SourceStatus::Standby => ("STANDBY", Style::new(Role::Fg)),
        SourceStatus::Stale => ("STALE", Style::limit(Limit::Stale)),
        SourceStatus::OutOfSpan => ("OUT OF SPAN", Style::new(Role::Dim)),
        SourceStatus::NoData => ("NO DATA", Style::new(Role::Dim)),
        SourceStatus::Failed(_) => ("FAILED", Style::limit(Limit::Warning)),
    };
    let age = s.age_s.map_or("--".to_owned(), fmt_dur);
    l.put(1, s.kind.label(), Style::new(Role::Fg));
    l.put(10, status, style);
    l.put(24, &format!("AGE {age}"), Style::new(Role::Dim));
    l.row += 1;
    let health = match &s.health {
        Health::Starting => "STARTING".to_owned(),
        Health::Ok => "FEED OK".to_owned(),
        Health::Failing(_) => "FEED FAILING".to_owned(),
        Health::Offline => "OFFLINE".to_owned(),
    };
    let origin = match (s.fetched_at, s.from_cache) {
        (Some(_), true) => " (CACHE)",
        (Some(_), false) => "",
        (None, _) => "",
    };
    let hs = if matches!(s.health, Health::Failing(_)) {
        Style::limit(Limit::Caution)
    } else {
        Style::new(Role::Dim)
    };
    l.put(3, &format!("{health}{origin}"), hs);
    if let Some((a, b)) = s.validity {
        l.put(
            24,
            &format!("SPAN {}", fmt_dur(b.minus(a))),
            Style::new(Role::Dim),
        );
    }
    l.row += 1;
    if let Some(r) = s.residual {
        let st = if r.norm_km >= caution_km {
            Style::limit(Limit::Caution)
        } else {
            Style::new(Role::Fg)
        };
        l.put(
            3,
            &format!(
                "vs ACTIVE R{:+.1} T{:+.1} N{:+.1} KM",
                r.rtn_km[0], r.rtn_km[1], r.rtn_km[2]
            ),
            st,
        );
        l.row += 1;
        l.put(
            3,
            &format!("|DR| {:.2} KM  |DV| {:.1} MM/S", r.norm_km, r.dv_mm_s),
            st,
        );
        l.row += 1;
    }
}

// ---- pixel-drawn panels ---------------------------------------------------------------------------

/// Ground-track panel body: coastlines, day/night shading, the track and the vehicle.
pub fn paint_ground_track(
    painter: &Painter,
    theme: &Theme,
    rect_px: [u32; 4],
    coast: &[crate::coast::Line],
    v: &VehicleView,
    observer: &mctl_core::world::ObserverView,
) {
    let [px0, py0, pw, ph] = rect_px;
    if pw < 8 || ph < 8 {
        return;
    }
    // Equirectangular maps are 2:1; fit the largest such rectangle and centre it in the panel.
    let w = pw.min(ph * 2);
    let h = w / 2;
    let (x, y) = (px0 + (pw - w) / 2, py0 + (ph - h) / 2);
    let to_screen = |lon: f64, lat: f64| {
        Pos2::new(
            x as f32 + ((lon + 180.0) / 360.0) as f32 * w as f32,
            y as f32 + ((90.0 - lat) / 180.0) as f32 * h as f32,
        )
    };
    // Night side: per-column terminator latitude, shaded from the terminator to the night pole.
    if let Some(sol) = &v.solution {
        let m = sol.ecef_from_gcrf;
        let s = sol.sun_gcrf_km;
        let sun_ecef = [
            m[0][0] * s[0] + m[0][1] * s[1] + m[0][2] * s[2],
            m[1][0] * s[0] + m[1][1] * s[1] + m[1][2] * s[2],
            m[2][0] * s[0] + m[2][1] * s[1] + m[2][2] * s[2],
        ];
        let norm = (sun_ecef[0].powi(2) + sun_ecef[1].powi(2) + sun_ecef[2].powi(2)).sqrt();
        let (dec, lon_s) = (
            (sun_ecef[2] / norm).asin().to_degrees(),
            sun_ecef[1].atan2(sun_ecef[0]).to_degrees(),
        );
        let col_px = 4.0f32;
        let night = egui::Color32::from_black_alpha(120);
        let mut px = 0.0f32;
        while px < w as f32 {
            let lon = -180.0 + f64::from((px + col_px / 2.0) / w as f32) * 360.0;
            let lat_t = terminator_lat_deg(lon, lon_s, dec);
            let (a, b) = if dec >= 0.0 {
                (to_screen(lon, lat_t).y, (y + h) as f32)
            } else {
                (y as f32, to_screen(lon, lat_t).y)
            };
            let r = egui::Rect::from_min_max(
                Pos2::new(x as f32 + px, a.min(b)),
                Pos2::new((x as f32 + px + col_px).min((x + w) as f32), a.max(b)),
            );
            painter.rect_filled(r, 0.0, night);
            px += col_px;
        }
        let sub = to_screen(lon_s, dec);
        painter.circle_stroke(sub, 5.0, Stroke::new(1.5, theme.palette.bright.to_egui()));
        painter.line_segment(
            [sub - egui::vec2(8.0, 0.0), sub + egui::vec2(8.0, 0.0)],
            Stroke::new(1.0, theme.palette.bright.to_egui()),
        );
        painter.line_segment(
            [sub - egui::vec2(0.0, 8.0), sub + egui::vec2(0.0, 8.0)],
            Stroke::new(1.0, theme.palette.bright.to_egui()),
        );
    }
    let coast_stroke = Stroke::new(1.0, theme.palette.dim.to_egui());
    for line in coast {
        let mut run: Vec<Pos2> = Vec::new();
        for pair in line.windows(2) {
            // Break polylines that cross the antimeridian instead of striking across the map.
            if (pair[0].0 - pair[1].0).abs() > 180.0 {
                if run.len() > 1 {
                    painter.add(egui::Shape::line(std::mem::take(&mut run), coast_stroke));
                }
                run.clear();
                continue;
            }
            if run.is_empty() {
                run.push(to_screen(f64::from(pair[0].0), f64::from(pair[0].1)));
            }
            run.push(to_screen(f64::from(pair[1].0), f64::from(pair[1].1)));
        }
        if run.len() > 1 {
            painter.add(egui::Shape::line(run, coast_stroke));
        }
    }
    // Observer sites with the circle inside which the vehicle is above the elevation mask.
    let alt_km = v.solution.as_ref().map(|s| s.geodetic.alt_km);
    for (i, site) in observer.sites.iter().enumerate() {
        let active = i == observer.active;
        let colour = if active {
            theme.palette.bright.to_egui()
        } else {
            theme.palette.dim.to_egui()
        };
        if let Some(alt) = alt_km {
            let angle = visibility_central_angle_deg(alt, observer.mask_deg);
            let ring = circle_on_sphere(site.lat_deg, site.lon_deg, angle, 72);
            let mut run: Vec<Pos2> = Vec::new();
            let mut prev: Option<f64> = None;
            for (lon, lat) in ring {
                if prev.is_some_and(|p| (p - lon).abs() > 180.0) && run.len() > 1 {
                    painter.extend(egui::Shape::dashed_line(
                        &std::mem::take(&mut run),
                        Stroke::new(1.0, colour),
                        5.0,
                        4.0,
                    ));
                }
                run.push(to_screen(lon, lat));
                prev = Some(lon);
            }
            if run.len() > 1 {
                painter.extend(egui::Shape::dashed_line(
                    &run,
                    Stroke::new(1.0, colour),
                    5.0,
                    4.0,
                ));
            }
        }
        let at = to_screen(site.lon_deg, site.lat_deg);
        let r = egui::Rect::from_center_size(at, egui::vec2(8.0, 8.0));
        if active {
            painter.rect_filled(r, 0.0, colour);
        } else {
            painter.rect_stroke(r, 0.0, Stroke::new(1.5, colour), egui::StrokeKind::Middle);
        }
        painter.text(
            at + egui::vec2(7.0, -7.0),
            egui::Align2::LEFT_BOTTOM,
            &site.name,
            crate::fonts::font_id(theme),
            colour,
        );
    }
    let Some(track) = &v.track else { return };
    let (past_stroke, future_stroke) = (
        Stroke::new(2.0, theme.palette.foreground.to_egui()),
        Stroke::new(2.0, theme.palette.bright.to_egui()),
    );
    let draw = |seg: &mut Vec<Pos2>, future: bool| {
        if seg.len() > 1 {
            let pts = std::mem::take(seg);
            if future {
                painter.extend(egui::Shape::dashed_line(&pts, future_stroke, 8.0, 6.0));
            } else {
                painter.add(egui::Shape::line(pts, past_stroke));
            }
        } else {
            seg.clear();
        }
    };
    let mut seg: Vec<Pos2> = Vec::new();
    let mut seg_future = track.now_index == 0;
    let mut prev_lon: Option<f64> = None;
    for (i, p) in track.points.iter().enumerate() {
        let wrap = prev_lon.is_some_and(|l| (l - p.lon_deg).abs() > 180.0);
        let boundary = i == track.now_index && i > 0;
        if wrap || boundary {
            draw(&mut seg, seg_future);
            seg_future = i >= track.now_index;
            if boundary && !wrap {
                // Join the flown and predicted parts at the boundary so the line is continuous.
                let q = &track.points[i - 1];
                seg.push(to_screen(q.lon_deg, q.lat_deg));
            }
        }
        seg.push(to_screen(p.lon_deg, p.lat_deg));
        prev_lon = Some(p.lon_deg);
    }
    draw(&mut seg, seg_future);
    if let Some(sol) = &v.solution {
        let at = to_screen(sol.geodetic.lon_deg, sol.geodetic.lat_deg);
        painter.circle_stroke(at, 7.0, Stroke::new(2.5, theme.globe.vehicle.to_egui()));
        painter.circle_filled(at, 3.0, theme.globe.vehicle.to_egui());
        painter.text(
            at + egui::vec2(10.0, -10.0),
            egui::Align2::LEFT_BOTTOM,
            &v.name,
            crate::fonts::font_id(theme),
            theme.palette.bright.to_egui(),
        );
    }
}

/// Earth-central angle (degrees) from a site to the edge of the region where a vehicle at `alt_km`
/// is above elevation `mask_deg`: acos(Re cos(el) / (Re + h)) - el.
pub fn visibility_central_angle_deg(alt_km: f64, mask_deg: f64) -> f64 {
    let el = mask_deg.to_radians();
    let re = mctl_astro::sun::EARTH_RADIUS_KM;
    ((re * el.cos() / (re + alt_km)).acos() - el)
        .to_degrees()
        .max(0.0)
}

/// Points `(lon, lat)` in degrees on the small circle of angular radius `angle_deg` around a point.
pub fn circle_on_sphere(lat_deg: f64, lon_deg: f64, angle_deg: f64, n: usize) -> Vec<(f64, f64)> {
    let (lat0, lon0, d) = (
        lat_deg.to_radians(),
        lon_deg.to_radians(),
        angle_deg.to_radians(),
    );
    (0..=n)
        .map(|k| {
            let brg = std::f64::consts::TAU * k as f64 / n as f64;
            let lat = (lat0.sin() * d.cos() + lat0.cos() * d.sin() * brg.cos()).asin();
            let lon =
                lon0 + (brg.sin() * d.sin() * lat0.cos()).atan2(d.cos() - lat0.sin() * lat.sin());
            (
                ((lon.to_degrees() + 540.0) % 360.0) - 180.0,
                lat.to_degrees(),
            )
        })
        .collect()
}

/// Beta angle over +-30 days as a line, with zero and the present marked.
fn paint_beta_sparkline(
    painter: &Painter,
    theme: &Theme,
    rect: [f32; 4],
    series: &[(Utc, f64)],
    now: Utc,
) {
    let [x, y, w, h] = rect;
    if series.len() < 3 || w < 30.0 || h < 30.0 {
        return;
    }
    let max = series
        .iter()
        .map(|(_, b)| b.abs())
        .fold(10.0_f64, f64::max)
        .ceil();
    let (t0, t1) = (series[0].0.0, series[series.len() - 1].0.0);
    let to = |t: f64, b: f64| {
        Pos2::new(
            x + w * ((t - t0) / (t1 - t0)) as f32,
            y + h * (0.5 - (b / (2.0 * max)) as f32),
        )
    };
    let p = &theme.palette;
    painter.line_segment(
        [to(t0, 0.0), to(t1, 0.0)],
        Stroke::new(1.0, p.border.to_egui()),
    );
    let pts: Vec<Pos2> = series.iter().map(|(t, b)| to(t.0, *b)).collect();
    painter.add(egui::Shape::line(
        pts,
        Stroke::new(2.0, p.foreground.to_egui()),
    ));
    let nx = to(now.0.clamp(t0, t1), 0.0).x;
    painter.line_segment(
        [Pos2::new(nx, y), Pos2::new(nx, y + h)],
        Stroke::new(1.0, p.bright.to_egui()),
    );
    let font = crate::fonts::font_id(theme);
    painter.text(
        Pos2::new(x, y - 2.0),
        egui::Align2::LEFT_BOTTOM,
        format!("BETA +-30D  +-{max:.0}"),
        font,
        p.dim.to_egui(),
    );
}

/// One revolution as a ring starting at the top (now), coloured by illumination.
pub fn paint_eclipse_ring(
    painter: &Painter,
    theme: &Theme,
    rect_px: [u32; 4],
    v: &VehicleView,
    now: Utc,
) {
    let [x, y, w, h] = rect_px;
    let (Some(track), Some(sol)) = (&v.track, &v.solution) else {
        return;
    };
    if w < 40 || h < 60 {
        return;
    }
    // Lower part of the panel, below the two text rows.
    let ring_top = y as f32 + 52.0;
    // The ring takes the left part of the panel; the beta sparkline the right.
    let ring_w = w as f32 * 0.55;
    let (cx, cy) = (x as f32 + ring_w / 2.0, ring_top + (h as f32 - 52.0) / 2.0);
    let radius = (ring_w.min(h as f32 - 52.0) / 2.0 - 8.0).max(10.0);
    paint_beta_sparkline(
        painter,
        theme,
        [
            x as f32 + ring_w,
            ring_top,
            w as f32 - ring_w - 6.0,
            h as f32 - 60.0,
        ],
        &v.beta_series,
        now,
    );
    let period = sol.elements.period_s;
    if !(period.is_finite() && period > 600.0) {
        return;
    }
    let n = 120;
    let point = |phase: f32| {
        let a = phase * std::f32::consts::TAU;
        Pos2::new(cx + radius * a.sin(), cy - radius * a.cos())
    };
    let illum_at = |t: Utc| {
        let i = track
            .points
            .partition_point(|p| p.t.0 < t.0)
            .min(track.points.len().saturating_sub(1));
        track.points.get(i).map(|p| p.illumination)
    };
    for k in 0..n {
        let (p0, p1) = (k as f32 / n as f32, (k + 1) as f32 / n as f32);
        let Some(il) = illum_at(now.plus(f64::from(p0 + p1) / 2.0 * period)) else {
            continue;
        };
        let stroke = if il > 0.95 {
            Stroke::new(4.0, theme.palette.foreground.to_egui())
        } else if il < 0.05 {
            Stroke::new(2.0, theme.palette.dim.to_egui())
        } else {
            Stroke::new(3.0, theme.palette.border.to_egui())
        };
        painter.line_segment([point(p0), point(p1)], stroke);
    }
    painter.circle_filled(point(0.0), 5.0, theme.globe.vehicle.to_egui());
    painter.text(
        Pos2::new(cx, cy),
        egui::Align2::CENTER_CENTER,
        "ONE REV",
        crate::fonts::font_id(theme),
        theme.palette.dim.to_egui(),
    );
}

#[cfg(test)]
mod tests {
    use mctl_core::state::{Elements, Geodetic, Quality, SourceKind, State};
    use mctl_core::world::{Residual, Solution, TrackPoint};

    use super::*;

    fn limits() -> Limits {
        Limits {
            residual_caution_km: 5.0,
            residual_warning_km: 20.0,
        }
    }

    fn sol(source: SourceKind) -> Solution {
        Solution {
            source,
            quality: Quality::Pred,
            state_gcrf: State::new([7000.0, 0.0, 0.0], [0.0, 7.5, 0.0]),
            state_itrf: State::new([7000.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
            geodetic: Geodetic {
                lat_deg: 0.0,
                lon_deg: 0.0,
                alt_km: 400.0,
            },
            elements: Elements {
                sma_km: 6778.0,
                ecc: 0.001,
                inc_deg: 51.6,
                raan_deg: 10.0,
                argp_deg: 20.0,
                true_anom_deg: 30.0,
                mean_anom_deg: 30.0,
                period_s: 5560.0,
                apo_alt_km: 405.0,
                peri_alt_km: 398.0,
            },
            sun_gcrf_km: [1.5e8, 0.0, 0.0],
            beta_deg: 12.0,
            illumination: 1.0,
            gmst_rad: 0.0,
            ecef_from_gcrf: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    fn view(status: SourceStatus, health: Health, residual_km: Option<f64>) -> VehicleView {
        VehicleView {
            id: "iss".into(),
            name: "ISS".into(),
            norad: 25544,
            telemetry: None,
            met_epoch: None,
            sources: vec![SourceView {
                kind: SourceKind::Gp,
                health,
                status,
                data_epoch: None,
                fetched_at: None,
                from_cache: false,
                age_s: Some(3600.0),
                validity: None,
                residual: residual_km.map(|k| Residual {
                    vs: SourceKind::Gp,
                    rtn_km: [0.0, k, 0.0],
                    norm_km: k,
                    dv_mm_s: 1.0,
                }),
            }],
            solution: Some(sol(SourceKind::Oem)),
            track: None,
            passes: vec![],
            beta_series: Default::default(),
        }
    }

    fn point(t: f64, z: f64, illum: f32) -> TrackPoint {
        TrackPoint {
            t: Utc(t),
            r_gcrf_km: [7000.0, 0.0, z],
            lat_deg: 0.0,
            lon_deg: 0.0,
            alt_km: 400.0,
            illumination: illum,
        }
    }

    #[test]
    fn events_interpolate_node_and_eclipse_crossings_and_drop_the_past() {
        let pts = vec![
            point(0.0, -10.0, 1.0),
            point(10.0, 10.0, 1.0),
            point(20.0, 30.0, 0.0),
            point(30.0, -10.0, 0.0),
            point(40.0, -10.0, 1.0),
        ];
        let tr = Track {
            revision: 1,
            source: SourceKind::Gp,
            points: pts,
            now_index: 0,
        };
        let ev = upcoming_events(&tr, Utc(-1.0));
        let find = |l: &str| ev.iter().find(|e| e.label == l).map(|e| e.t.0);
        assert_eq!(find("ASCENDING NODE"), Some(5.0));
        assert_eq!(find("ECLIPSE ENTRY"), Some(15.0));
        assert_eq!(find("DESCENDING NODE"), Some(27.5));
        assert_eq!(find("ECLIPSE EXIT"), Some(35.0));
        assert!(ev.windows(2).all(|w| w[0].t.0 <= w[1].t.0), "sorted");
        let later = upcoming_events(&tr, Utc(20.0));
        assert!(later.iter().all(|e| e.t.0 > 20.0) && later.len() == 2);
    }

    #[test]
    fn terminator_geometry_is_physical() {
        // Equinox-like Sun on the equator (declination clamped): the terminator is nearly meridional, so it is
        // far from the equator everywhere except exactly 90 degrees from the Sun.
        assert!(terminator_lat_deg(30.0, 0.0, 0.0).abs() > 80.0);
        // Northern summer: at the sub-solar longitude the terminator is far south (night is the south pole region).
        let lat = terminator_lat_deg(0.0, 0.0, 23.4);
        assert!(lat < -60.0, "{lat}");
        // On the anti-solar meridian it is far north.
        assert!(terminator_lat_deg(180.0, 0.0, 23.4) > 60.0);
        // 90 degrees from the Sun the terminator crosses the equator.
        assert!(terminator_lat_deg(90.0, 0.0, 23.4).abs() < 1e-9);
    }

    use crate::globe::Camera;
    use crate::grid::Grid;
    use crate::pages::{ALL_PAGES, Page};
    use crate::render::EffectToggles;
    use crate::theme::Theme;
    use mctl_core::cw::{Condition, CwState, Latch, Severity};
    use mctl_core::world::{CwRuleView, CwView};

    fn bare_world() -> World {
        World::empty(Utc(1.0e9))
    }

    /// Render a page into a fresh grid of the given size.
    fn render_page(
        world: &World,
        vehicle: Option<&VehicleView>,
        page: Page,
        sub: usize,
        cols: usize,
        rows: usize,
    ) -> (Grid, crate::page::Layout) {
        let theme: Theme = crate::theme::embedded().unwrap().remove(0);
        let (camera, limits, settings) = (Camera::default(), limits(), vec![]);
        let mut grid = Grid::new(cols, rows);
        let layout = crate::screens::render(
            &mut grid,
            &PageInput {
                world,
                vehicle,
                theme: &theme,
                toggles: EffectToggles::from_theme(&theme.effects),
                camera: &camera,
                show_ecef: false,
                limits: &limits,
                frame_ms: 16.7,
                gpu_ms: None,
                adapter: "test",
                size_px: [(cols * 12) as u32, (rows * 24) as u32],
                page,
                sub,
                command: None,
                status: None,
                help: false,
                settings: &settings,
            },
        );
        (grid, layout)
    }

    #[test]
    fn every_page_renders_at_every_size_without_panicking_and_keeps_header_and_footer() {
        let mut v = view(SourceStatus::Active, Health::Ok, None);
        v.telemetry = Some("isslive".into());
        let w = bare_world();
        for (cols, rows) in [(160, 45), (100, 30), (60, 20), (40, 12), (20, 6), (3, 3)] {
            for page in ALL_PAGES {
                for sub in 0..page.subpages().len() {
                    for vehicle in [Some(&v), None] {
                        let (g, _) = render_page(&w, vehicle, page, sub, cols, rows);
                        if cols >= 100 {
                            assert!(
                                g.row_text(0).contains(page.title()),
                                "{page:?}/{sub} {cols}x{rows}: {}",
                                g.row_text(0)
                            );
                            assert!(
                                g.row_text(rows - 1).contains("F1 MASTER")
                                    || g.row_text(rows - 1).contains("[F1 MASTER]"),
                                "footer legend at {cols}x{rows}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn pages_without_a_source_say_why_and_never_show_invented_numbers() {
        let hubble = view(SourceStatus::Active, Health::Ok, None);
        let w = bare_world();
        let (g, layout) = render_page(&w, Some(&hubble), Page::Gnc, 0, 160, 45);
        let t = g.text();
        assert!(
            t.contains("NO SOURCE") && t.contains("HAS NO TELEMETRY SOURCE"),
            "{t}"
        );
        assert!(layout.adi.is_none(), "no ADI without a telemetry source");
        let (g, _) = render_page(&w, Some(&hubble), Page::View, 0, 160, 45);
        assert!(g.text().contains("M7"));
    }

    #[test]
    fn pages_that_draw_pixels_report_where_so_mouse_and_painters_agree() {
        let v = view(SourceStatus::Active, Health::Ok, None);
        let w = bare_world();
        let (_, master) = render_page(&w, Some(&v), Page::Master, 0, 160, 45);
        assert!(master.globe.is_some() && master.ground.is_some() && master.eclipse.is_some());
        let (_, orbit) = render_page(&w, Some(&v), Page::Traj, 0, 160, 45);
        assert!(orbit.globe.is_some() && orbit.ground.is_none());
        let (_, ground) = render_page(&w, Some(&v), Page::Traj, 1, 160, 45);
        assert!(ground.globe.is_none() && ground.ground.is_some());
        let (_, sys) = render_page(&w, Some(&v), Page::Sys, 0, 160, 45);
        assert!(
            sys.globe.is_none(),
            "text pages have no orbit map, so no mouse rotation target"
        );
    }

    #[test]
    fn the_cw_panel_shows_latched_conditions_and_active_rules() {
        let v = view(SourceStatus::Active, Health::Ok, None);
        let mut w = bare_world();
        w.cw = CwView {
            rules: vec![CwRuleView {
                id: "r".into(),
                title: "Cabin pressure".into(),
                state: CwState::Warning,
                value: Some(640.0),
                units: "mmHg".into(),
                caution: Some(700.0),
                warning: Some(650.0),
                basis: "illustrative".into(),
            }],
            conditions: vec![
                Condition {
                    vehicle: Some("iss".into()),
                    severity: Severity::Caution,
                    text: "ISS OEM FETCH FAILING: 503".into(),
                },
                Condition {
                    vehicle: Some("other".into()),
                    severity: Severity::Warning,
                    text: "OTHER VEHICLE PROBLEM".into(),
                },
            ],
            latch: Latch {
                caution: true,
                warning: true,
                acked_at: None,
            },
        };
        let (g, _) = render_page(&w, Some(&v), Page::Master, 0, 160, 45);
        let t = g.text();
        assert!(t.contains("MASTER WARNING LATCHED"), "{t}");
        assert!(t.contains("Cabin pressure 640.00 mmHg"));
        assert!(t.contains("FETCH FAILING: 503"));
        assert!(
            !t.contains("OTHER VEHICLE PROBLEM"),
            "other vehicles' conditions are not shown"
        );
        assert!(
            g.row_text(0).contains("WARN*"),
            "latched warning is starred: {}",
            g.row_text(0)
        );
        w.cw.latch = Latch::default();
        w.cw.rules[0].state = CwState::Nom;
        w.cw.conditions.clear();
        let (g, _) = render_page(&w, Some(&v), Page::Master, 0, 160, 45);
        assert!(g.text().contains("NO ACTIVE CAUTIONS OR WARNINGS"));
        assert!(g.row_text(0).contains("NOM"));
    }

    #[test]
    fn the_sys_rules_page_lists_each_rule_with_its_basis() {
        let v = view(SourceStatus::Active, Health::Ok, None);
        let mut w = bare_world();
        w.cw.rules.push(CwRuleView {
            id: "r".into(),
            title: "Lab ppCO2".into(),
            state: CwState::NoData,
            value: None,
            units: "mmHg".into(),
            caution: Some(6.0),
            warning: Some(8.0),
            basis: "illustrative, not a flight rule".into(),
        });
        let (g, _) = render_page(&w, Some(&v), Page::Sys, 3, 160, 45);
        let t = g.text();
        assert!(
            t.contains("Lab ppCO2")
                && t.contains("NO DATA")
                && t.contains("ILLUSTRATIVE, NOT A FLIGHT RULE"),
            "{t}"
        );
    }

    #[test]
    fn the_help_overlay_hides_what_is_under_it() {
        let v = view(SourceStatus::Active, Health::Ok, None);
        let w = bare_world();
        let theme: Theme = crate::theme::embedded().unwrap().remove(0);
        let (camera, limits) = (Camera::default(), limits());
        let mut grid = Grid::new(160, 45);
        crate::screens::render(
            &mut grid,
            &PageInput {
                world: &w,
                vehicle: Some(&v),
                theme: &theme,
                toggles: EffectToggles::from_theme(&theme.effects),
                camera: &camera,
                show_ecef: false,
                limits: &limits,
                frame_ms: 1.0,
                gpu_ms: None,
                adapter: "t",
                size_px: [1920, 1080],
                page: Page::Master,
                sub: 0,
                command: None,
                status: None,
                help: true,
                settings: &[],
            },
        );
        let t = grid.text();
        assert!(t.contains("KEYS") && t.contains("COMMANDS") && t.contains("acknowledge C&W"));
        assert!(
            t.contains("warp <rate>"),
            "every command is documented in the help"
        );
    }

    fn world_with_passes() -> (World, VehicleView) {
        use mctl_core::world::{LookView, ObserverView, PassSet, PassView, SiteView};
        let t = Utc(1.0e9);
        let pass = |dt: f64, max_el: f64, visible: Option<bool>| PassView {
            aos: t.plus(dt),
            tca: t.plus(dt + 180.0),
            los: t.plus(dt + 360.0),
            max_el_deg: max_el,
            aos_az_deg: 220.0,
            tca_az_deg: 140.0,
            los_az_deg: 50.0,
            started_before_window: false,
            ends_after_window: false,
            visible,
            path: vec![[220.0, 10.0], [140.0, max_el as f32], [50.0, 10.0]],
        };
        let mut v = view(SourceStatus::Active, Health::Ok, None);
        v.passes = vec![PassSet {
            site: "Home".into(),
            from: t,
            to: t.plus(7.0 * 86_400.0),
            source: "OEM then GP/SGP4".into(),
            passes: vec![
                pass(600.0, 66.6, Some(true)),
                pass(6_000.0, 12.3, Some(false)),
            ],
            look_now: Some(LookView {
                az_deg: 200.0,
                el_deg: -20.0,
                range_km: 6000.0,
            }),
        }];
        let mut w = bare_world();
        w.t = t;
        w.observer = ObserverView {
            sites: vec![
                SiteView {
                    name: "Home".into(),
                    lat_deg: 51.05,
                    lon_deg: -114.07,
                    alt_m: 1045.0,
                    origin: "CONFIGURED".into(),
                    sun_az_deg: Some(180.0),
                    sun_el_deg: Some(-30.0),
                },
                SiteView {
                    name: "NASA HQ".into(),
                    lat_deg: 38.883,
                    lon_deg: -77.0163,
                    alt_m: 10.0,
                    origin: "DEFAULT".into(),
                    sun_az_deg: None,
                    sun_el_deg: None,
                },
            ],
            active: 0,
            mask_deg: 10.0,
        };
        (w, v)
    }

    #[test]
    fn the_pass_pages_show_times_elevations_visibility_and_the_active_site() {
        let (w, v) = world_with_passes();
        let (g, layout) = render_page(&w, Some(&v), Page::Pass, 0, 160, 45);
        let t = g.text();
        assert!(layout.sky.is_some(), "the sky plot reports its rectangle");
        assert!(
            t.contains("AOS IN 10m00s")
                && t.contains("MAX EL 66.6")
                && t.contains("DURATION 6m00s"),
            "{t}"
        );
        assert!(
            t.contains("VISIBLE PASS") && t.contains("DARK SKY") && t.contains("BELOW HORIZON"),
            "{t}"
        );
        assert!(
            g.row_text(0).contains("SITE Home (CONFIGURED)"),
            "{}",
            g.row_text(0)
        );
        let (g, _) = render_page(&w, Some(&v), Page::Pass, 1, 160, 45);
        let t = g.text();
        assert!(
            t.contains(" 1  10m00s") && t.contains("66.6") && t.contains("YES") && t.contains("no"),
            "{t}"
        );
        let (g, _) = render_page(&w, Some(&v), Page::Pass, 2, 160, 45);
        let t = g.text();
        assert!(
            t.contains("> Home") && t.contains("NASA HQ") && t.contains("DEFAULT"),
            "{t}"
        );
        assert!(
            t.contains("RESOLUTION ORDER") && t.contains(":site save"),
            "{t}"
        );
    }

    #[test]
    fn a_default_site_says_it_is_not_the_users_location() {
        let (mut w, v) = world_with_passes();
        w.observer.active = 1;
        let (g, _) = render_page(&w, Some(&v), Page::Pass, 1, 160, 45);
        assert!(g.text().contains("DEFAULT, NOT YOUR LOCATION"));
        assert!(
            g.text().contains("NO PREDICTION YET"),
            "no passes for a site that has none, and no invented ones"
        );
    }

    #[test]
    fn the_visibility_circle_matches_known_geometry() {
        // At 400 km and a 0 degree mask the horizon is ~19.8 degrees of arc away; a higher mask shrinks it.
        let a0 = visibility_central_angle_deg(400.0, 0.0);
        assert!((a0 - 19.74).abs() < 0.1, "{a0}");
        let a10 = visibility_central_angle_deg(400.0, 10.0);
        assert!(a10 < a0 && (a10 - 12.2).abs() < 0.4, "{a10}");
        assert!(
            visibility_central_angle_deg(0.0, 0.0).abs() < 1e-9,
            "on the ground you see only your own point"
        );
    }

    #[test]
    fn circles_on_the_sphere_stay_at_the_requested_distance() {
        let hav = |lat1: f64, lon1: f64, lat2: f64, lon2: f64| {
            let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
            let dl = (lon2 - lon1).to_radians();
            (p1.sin() * p2.sin() + p1.cos() * p2.cos() * dl.cos())
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees()
        };
        for (lat, lon) in [(51.0, -114.0), (0.0, 179.0), (-80.0, 10.0)] {
            let ring = circle_on_sphere(lat, lon, 15.0, 36);
            assert_eq!(ring.len(), 37);
            for (rl, rb) in ring {
                assert!((hav(lat, lon, rb, rl) - 15.0).abs() < 1e-6, "{lat},{lon}");
                assert!((-180.0..=180.0).contains(&rl));
            }
        }
    }
}
