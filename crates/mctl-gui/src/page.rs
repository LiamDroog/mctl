//! The MASTER page (DESIGN.md §9.5), built only from the engine's published snapshot.
//!
//! Rule (DESIGN.md §1.3): a quantity with no source is shown as `NO DATA`, never as a plausible
//! substitute. Panels whose data source belongs to a later milestone say so rather than inventing content.

use egui::{Painter, Pos2, Stroke};
use mctl_core::config::Limits;
use mctl_core::time::Utc;
use mctl_core::world::{EopView, Health, SourceStatus, SourceView, Track, VehicleView, World};

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
    pub vehicle_idx: usize,
    pub vehicle_count: usize,
    pub theme: &'a Theme,
    pub toggles: EffectToggles,
    pub camera: &'a Camera,
    pub show_ecef: bool,
    pub limits: &'a Limits,
    pub frame_ms: f32,
    pub gpu_ms: Option<f32>,
    pub adapter: &'a str,
    pub size_px: [u32; 2],
}

/// Panels that need pixel-level drawing.
pub struct Layout {
    pub orbit: CellRect,
    pub ground: CellRect,
    pub eclipse: CellRect,
}

/// A caution or warning line derived from source health (a stand-in for the M3 C&W engine).
#[derive(Debug, Clone, PartialEq)]
pub struct CwLine {
    pub severity: Option<Limit>,
    pub text: String,
}

/// `1d02h`, `15h22m`, `4m10s`, `37s`; negative or non-finite values render as `--`.
pub fn fmt_dur(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--".into();
    }
    let s = secs as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        3600..=86_399 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d{:02}h", s / 86_400, (s % 86_400) / 3600),
    }
}

fn quality_text(sol: &mctl_core::world::Solution) -> String {
    format!("{} ({})", sol.quality.label(), sol.source.label())
}

/// Derive the caution/warning list for the displayed vehicle from source health and residuals.
pub fn cw_lines(world: &World, v: Option<&VehicleView>, limits: &Limits) -> Vec<CwLine> {
    let mut out = Vec::new();
    let mut add = |severity, text: String| out.push(CwLine { severity, text });
    let Some(v) = v else {
        add(Some(Limit::Warning), "NO VEHICLE DATA YET".into());
        return out;
    };
    if let EopView::Assumed { .. } = world.eop {
        add(
            Some(Limit::Caution),
            "EOP ASSUMED 0 (UT1 UP TO 0.9 S)".into(),
        );
    }
    if v.solution.is_none() {
        add(
            Some(Limit::Warning),
            format!("{} NO TRAJECTORY SOLUTION", v.name),
        );
    }
    for s in &v.sources {
        match (&s.health, &s.status) {
            (_, SourceStatus::Failed(why)) => add(
                Some(Limit::Warning),
                format!("{} {} FAILED: {}", v.name, s.kind.label(), why),
            ),
            (Health::Failing(why), _) => add(
                Some(Limit::Caution),
                format!("{} {} FETCH FAILING: {}", v.name, s.kind.label(), why),
            ),
            (Health::Offline, _) => add(
                Some(Limit::Stale),
                format!("{} {} OFFLINE, CACHE ONLY", v.name, s.kind.label()),
            ),
            _ => {}
        }
        if s.status == SourceStatus::Stale {
            add(
                Some(Limit::Caution),
                format!(
                    "{} {} DATA STALE ({})",
                    v.name,
                    s.kind.label(),
                    s.age_s.map_or("--".into(), fmt_dur)
                ),
            );
        }
        if let Some(r) = s.residual {
            let (a, b) = (
                v.solution.as_ref().map_or("?", |x| x.source.label()),
                s.kind.label(),
            );
            if r.norm_km >= limits.residual_warning_km {
                add(
                    Some(Limit::Warning),
                    format!("{a} vs {b} DIFFER {:.1} KM", r.norm_km),
                );
            } else if r.norm_km >= limits.residual_caution_km {
                add(
                    Some(Limit::Caution),
                    format!("{a} vs {b} DIFFER {:.1} KM", r.norm_km),
                );
            }
        }
    }
    if let Some(sol) = &v.solution
        && sol.quality == mctl_core::state::Quality::Stale
    {
        add(
            Some(Limit::Caution),
            format!("{} SOLUTION IS STALE", v.name),
        );
    }
    out
}

/// Worst severity in `lines` (Warning > Caution > Stale > none).
pub fn worst(lines: &[CwLine]) -> Option<Limit> {
    [Limit::Warning, Limit::Caution, Limit::Stale]
        .into_iter()
        .find(|s| lines.iter().any(|l| l.severity == Some(*s)))
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

struct Lines<'a> {
    grid: &'a mut Grid,
    rect: CellRect,
    row: usize,
}

impl<'a> Lines<'a> {
    fn new(grid: &'a mut Grid, panel: CellRect) -> Self {
        Self {
            grid,
            rect: panel.inner(),
            row: 0,
        }
    }

    fn put(&mut self, col: usize, text: &str, style: Style) {
        if self.row < self.rect.rows && col < self.rect.cols {
            let room = self.rect.cols - col;
            let t: String = text.chars().take(room).collect();
            self.grid
                .put(self.rect.col + col, self.rect.row + self.row, &t, style);
        }
    }

    fn line(&mut self, text: &str, style: Style) {
        self.put(1, text, style);
        self.row += 1;
    }

    fn pair(&mut self, label: &str, value: &str, unit: &str, value_style: Style) {
        let dim = Style::new(Role::Dim);
        self.put(1, label, dim);
        self.put(8, value, value_style);
        self.put(8 + value.chars().count() + 1, unit, dim);
        self.row += 1;
    }

    fn no_data(&mut self, why: &str) {
        self.line("NO DATA", Style::new(Role::Dim));
        self.line(why, Style::new(Role::Dim));
    }
}

fn layout(cols: usize, rows: usize) -> [CellRect; 9] {
    let body_h = rows.saturating_sub(3);
    let (lw, mw) = (cols * 50 / 100, cols * 25 / 100);
    let rw = cols.saturating_sub(lw + mw);
    let (c1, c2) = (lw, lw + mw);
    let pct = |p: usize| body_h * p / 100;
    let (orbit_h, ground_h) = (pct(48), pct(33));
    let (state_h, elem_h) = (pct(34), pct(31));
    let (cw_h, src_h) = (pct(28), pct(38));
    [
        CellRect::new(0, 1, lw, orbit_h),
        CellRect::new(0, 1 + orbit_h, lw, ground_h),
        CellRect::new(
            0,
            1 + orbit_h + ground_h,
            lw,
            body_h.saturating_sub(orbit_h + ground_h),
        ),
        CellRect::new(c1, 1, mw, state_h),
        CellRect::new(c1, 1 + state_h, mw, elem_h),
        CellRect::new(
            c1,
            1 + state_h + elem_h,
            mw,
            body_h.saturating_sub(state_h + elem_h),
        ),
        CellRect::new(c2, 1, rw, cw_h),
        CellRect::new(c2, 1 + cw_h, rw, src_h),
        CellRect::new(
            c2,
            1 + cw_h + src_h,
            rw,
            body_h.saturating_sub(cw_h + src_h),
        ),
    ]
}

fn source_row(l: &mut Lines<'_>, s: &SourceView, caution_km: f64) {
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

/// Fill the grid with the MASTER page and return the pixel-drawn panels' rectangles.
pub fn build_master(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (fg, dim, bright, border) = (
        Style::new(Role::Fg),
        Style::new(Role::Dim),
        Style::new(Role::Bright),
        Style::new(Role::Border),
    );
    let cw = cw_lines(inp.world, inp.vehicle, inp.limits);
    let severity = worst(&cw);

    // Header.
    let (name, norad) = inp
        .vehicle
        .map_or(("--".to_owned(), 0), |v| (v.name.clone(), v.norad));
    let src = inp
        .vehicle
        .and_then(|v| v.solution.as_ref())
        .map_or("NO DATA".to_owned(), quality_text);
    let age = inp
        .vehicle
        .and_then(|v| v.sources.iter().find(|s| s.status == SourceStatus::Active))
        .and_then(|s| s.age_s)
        .map_or(String::new(), |a| format!(" AGE {}", fmt_dur(a)));
    let header = format!(
        " {name} ({norad}) | MASTER | GMT {} | LIVE 1x | SRC {src}{age} | {}",
        inp.world.t.gmt_doy(),
        inp.theme.name.to_uppercase()
    );
    let annunciator = match severity {
        Some(Limit::Warning) => " WARN ",
        Some(Limit::Caution) => " CAUT ",
        Some(Limit::Stale) => " STALE ",
        None => " NOM ",
    };
    let header_cols = cols.saturating_sub(annunciator.len() + 1);
    let header: String = header.chars().take(header_cols).collect();
    grid.put(0, 0, &header, bright);
    let ann_style = severity.map_or(Style::new(Role::Nominal), Style::limit);
    grid.put(header_cols + 1, 0, annunciator, ann_style);

    let [
        orbit,
        ground,
        timeline,
        state,
        elements,
        subsys,
        cwp,
        sources,
        eclipse,
    ] = layout(cols, rows);
    let sol = inp.vehicle.and_then(|v| v.solution.as_ref());
    let quality = sol.map_or("NO DATA".to_owned(), quality_text);
    let cam = inp.camera;
    let focus = if cam.focus_vehicle {
        "VEHICLE"
    } else {
        "EARTH"
    };
    let frame = if cam.earth_fixed {
        "EARTH-FIXED"
    } else {
        "INERTIAL"
    };
    grid.draw_box(
        orbit,
        &format!(
            "ORBIT MAP  ALT x{:.0}  {frame}  FOCUS {focus}",
            cam.exaggeration()
        ),
        border,
        bright,
    );
    grid.draw_box(
        ground,
        "GROUND TRACK  PAST SOLID / PREDICTED DASHED",
        border,
        bright,
    );
    grid.draw_box(
        timeline,
        "EVENT TIMELINE  FROM TRACK SAMPLES",
        border,
        bright,
    );
    grid.draw_box(
        state,
        &format!(
            "STATE VECTOR ({})  {quality}",
            if inp.show_ecef { "ITRF" } else { "GCRF" }
        ),
        border,
        bright,
    );
    grid.draw_box(elements, "ELEMENTS (OSCULATING)", border, bright);
    grid.draw_box(subsys, "SUBSYSTEM SUMMARY", border, bright);
    grid.draw_box(cwp, "CAUTION & WARNING  SOURCE HEALTH", border, bright);
    grid.draw_box(
        sources,
        "SOURCES  HEALTH / AGE / DIFFERENCE",
        border,
        bright,
    );
    grid.draw_box(eclipse, "ECLIPSE / BETA", border, bright);

    // Orbit map interior: the globe pass draws behind; with no solution say so.
    if sol.is_none() {
        let mut l = Lines::new(grid, orbit);
        l.no_data("NO TRAJECTORY SOLUTION YET");
    }

    // State vector.
    let mut l = Lines::new(grid, state);
    match sol {
        None => l.no_data("NO SOURCE HAS DELIVERED A STATE"),
        Some(s) => {
            let st = if inp.show_ecef {
                &s.state_itrf
            } else {
                &s.state_gcrf
            };
            for (lab, v) in [("X", st.r.x), ("Y", st.r.y), ("Z", st.r.z)] {
                l.pair(lab, &format!("{v:>13.3}"), "km", fg);
            }
            for (lab, v) in [("VX", st.v.x), ("VY", st.v.y), ("VZ", st.v.z)] {
                l.pair(lab, &format!("{v:>13.6}"), "km/s", fg);
            }
            l.pair("|R|", &format!("{:>13.3}", st.r.norm()), "km", fg);
            l.pair("|V|", &format!("{:>13.6}", st.v.norm()), "km/s", fg);
            l.pair("ALT", &format!("{:>13.3}", s.geodetic.alt_km), "km", fg);
            l.pair("LAT", &format!("{:>13.4}", s.geodetic.lat_deg), "deg", fg);
            l.pair("LON", &format!("{:>13.4}", s.geodetic.lon_deg), "deg", fg);
            if matches!(inp.world.eop, EopView::Assumed { .. }) {
                l.line(
                    "UT1-UTC ASSUMED 0 (ITRF +-30 M)",
                    Style::limit(Limit::Caution),
                );
            }
        }
    }

    // Elements.
    let mut l = Lines::new(grid, elements);
    match sol {
        None => l.no_data("NEEDS A STATE VECTOR"),
        Some(s) => {
            let e = &s.elements;
            for (lab, v, unit, p) in [
                ("SMA", e.sma_km, "km", 3),
                ("ECC", e.ecc, "", 6),
                ("INC", e.inc_deg, "deg", 4),
                ("RAAN", e.raan_deg, "deg", 4),
                ("ARGP", e.argp_deg, "deg", 4),
                ("TA", e.true_anom_deg, "deg", 3),
                ("MA", e.mean_anom_deg, "deg", 3),
            ] {
                l.pair(lab, &format!("{v:>13.p$}"), unit, fg);
            }
            l.pair("PERIOD", &format!("{:>13}", fmt_dur(e.period_s)), "", fg);
            l.pair("HA", &format!("{:>13.2}", e.apo_alt_km), "km", fg);
            l.pair("HP", &format!("{:>13.2}", e.peri_alt_km), "km", fg);
            l.pair("BETA", &format!("{:>13.2}", s.beta_deg), "deg", fg);
        }
    }

    // Subsystem summary: not available until the ISSLIVE adapter exists.
    let mut l = Lines::new(grid, subsys);
    l.no_data("ISSLIVE ADAPTER ARRIVES IN M3");

    // Caution & warning (source health only until the M3 rule engine).
    let mut l = Lines::new(grid, cwp);
    if cw.is_empty() {
        l.line("NOM  NO SOURCE ANOMALIES", Style::new(Role::Nominal));
    }
    for c in &cw {
        let (tag, style) = match c.severity {
            Some(Limit::Warning) => ("WARN ", Style::limit(Limit::Warning)),
            Some(Limit::Caution) => ("CAUT ", Style::limit(Limit::Caution)),
            Some(Limit::Stale) => ("STALE", Style::limit(Limit::Stale)),
            None => ("NOM  ", Style::new(Role::Nominal)),
        };
        l.line(&format!("{tag} {}", c.text), style);
    }
    l.row = l.rect.rows.saturating_sub(1);
    l.line("RULE ENGINE (TLE AGE, LIMITS): M3", dim);

    // Sources.
    let mut l = Lines::new(grid, sources);
    match inp.vehicle {
        None => l.no_data("ENGINE NOT STARTED"),
        Some(v) => {
            for s in &v.sources {
                source_row(&mut l, s, inp.limits.residual_caution_km);
            }
            let eop = match &inp.world.eop {
                EopView::Loaded { covers_to, .. } => {
                    (format!("EOP LOADED THRU {}", &covers_to.iso()[..10]), dim)
                }
                EopView::Assumed { .. } => {
                    ("EOP ASSUMED 0".to_owned(), Style::limit(Limit::Caution))
                }
            };
            l.put(1, &eop.0, eop.1);
        }
    }

    // Eclipse / beta (ring is painted over the lower part of the panel).
    let mut l = Lines::new(grid, eclipse);
    match sol {
        None => l.no_data("NEEDS A STATE VECTOR"),
        Some(s) => {
            let state = if s.illumination >= 0.999 {
                ("SUNLIT".to_owned(), fg)
            } else if s.illumination <= 0.001 {
                ("UMBRA".to_owned(), Style::limit(Limit::Stale))
            } else {
                (
                    format!("PENUMBRA {:.0}%", s.illumination * 100.0),
                    Style::limit(Limit::Stale),
                )
            };
            l.put(1, &state.0, state.1);
            l.put(20, &format!("BETA {:+.1} deg", s.beta_deg), fg);
        }
    }

    // Event timeline.
    let mut l = Lines::new(grid, timeline);
    match inp.vehicle.and_then(|v| v.track.as_deref()) {
        None => l.no_data("NEEDS A TRACK"),
        Some(tr) => {
            for e in upcoming_events(tr, inp.world.t).iter().take(l.rect.rows) {
                let dt = e.t.minus(inp.world.t);
                l.line(
                    &format!("T-{:<9} {:<16} GMT {}", fmt_dur(dt), e.label, e.t.gmt_doy()),
                    fg,
                );
            }
        }
    }

    // Footer: two rows (stats, then key legend).
    let t = inp.toggles;
    let on = |b: bool| if b { "ON " } else { "OFF" };
    let veh = if inp.vehicle_count > 1 {
        format!("V VEHICLE {}/{}  ", inp.vehicle_idx + 1, inp.vehicle_count)
    } else {
        String::new()
    };
    let legend = format!(
        " F1 MASTER {veh}X FRAME ARROWS ORBIT +/- ZOOM Z ALT F CAM C FOCUS T THEME 1 SCAN {} 2 BLOOM {} 3 PERSIST {} 4 CRT {} E ALL Q QUIT",
        on(t.scanlines),
        on(t.bloom),
        on(t.persistence),
        on(t.curvature)
    );
    let legend: String = legend.chars().take(cols).collect();
    grid.put(0, rows.saturating_sub(1), &legend, dim);
    let gpu = inp
        .gpu_ms
        .map_or_else(|| "GPU n/a".to_owned(), |g| format!("GPU {g:5.2}ms"));
    let stats = format!(
        "{}x{} FRAME {:5.2}ms {:5.1}fps {gpu} ",
        inp.size_px[0],
        inp.size_px[1],
        inp.frame_ms,
        if inp.frame_ms > 0.0 {
            1000.0 / inp.frame_ms
        } else {
            0.0
        }
    );
    let stats_row = rows.saturating_sub(2);
    grid.put(
        cols.saturating_sub(stats.len()),
        stats_row,
        &stats,
        Style::new(Role::Info),
    );
    let small = if cols < MIN_COLS || rows < MIN_ROWS {
        format!("WINDOW {cols}x{rows} CELLS < MIN {MIN_COLS}x{MIN_ROWS}: PANELS CLIPPED  ")
    } else {
        String::new()
    };
    let adapter: String = format!("{small}{}", inp.adapter)
        .chars()
        .take(cols.saturating_sub(stats.len() + 2))
        .collect();
    grid.put(
        1,
        stats_row,
        &adapter,
        if small.is_empty() {
            dim
        } else {
            Style::limit(Limit::Caution)
        },
    );

    Layout {
        orbit: orbit.inner(),
        ground: ground.inner(),
        eclipse: eclipse.inner(),
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
    let (cx, cy) = (
        x as f32 + w as f32 / 2.0,
        ring_top + (h as f32 - 52.0) / 2.0,
    );
    let radius = ((w as f32).min(h as f32 - 52.0) / 2.0 - 8.0).max(10.0);
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
        }
    }

    fn world_with(eop: EopView) -> World {
        World {
            t: Utc(1.0e9),
            seq: 1,
            eop,
            vehicles: vec![],
        }
    }

    #[test]
    fn durations_format_compactly() {
        assert_eq!(fmt_dur(37.0), "37s");
        assert_eq!(fmt_dur(250.0), "4m10s");
        assert_eq!(fmt_dur(55_320.0), "15h22m");
        assert_eq!(fmt_dur(100_000.0), "1d03h");
        assert_eq!(fmt_dur(-1.0), "--");
        assert_eq!(fmt_dur(f64::NAN), "--");
    }

    #[test]
    fn cw_lines_grade_residuals_by_the_configured_limits() {
        let loaded = EopView::Loaded {
            covers_to: Utc(2.0e9),
            fetched_at: None,
            from_cache: false,
        };
        let w = world_with(loaded.clone());
        assert!(
            cw_lines(
                &w,
                Some(&view(SourceStatus::Standby, Health::Ok, Some(1.0))),
                &limits()
            )
            .is_empty(),
            "small residual is nominal"
        );
        let caut = cw_lines(
            &w,
            Some(&view(SourceStatus::Standby, Health::Ok, Some(7.0))),
            &limits(),
        );
        assert_eq!(worst(&caut), Some(Limit::Caution));
        let warn = cw_lines(
            &w,
            Some(&view(SourceStatus::Standby, Health::Ok, Some(25.0))),
            &limits(),
        );
        assert_eq!(worst(&warn), Some(Limit::Warning));
    }

    #[test]
    fn cw_lines_report_health_staleness_failures_and_missing_eop() {
        let assumed = world_with(EopView::Assumed { reason: "x".into() });
        let l = cw_lines(
            &assumed,
            Some(&view(
                SourceStatus::Stale,
                Health::Failing("503".into()),
                None,
            )),
            &limits(),
        );
        let text: Vec<&str> = l.iter().map(|c| c.text.as_str()).collect();
        assert!(text.iter().any(|t| t.contains("EOP ASSUMED")));
        assert!(text.iter().any(|t| t.contains("STALE")));
        assert!(text.iter().any(|t| t.contains("FETCH FAILING: 503")));
        let f = cw_lines(
            &assumed,
            Some(&view(
                SourceStatus::Failed("decayed".into()),
                Health::Ok,
                None,
            )),
            &limits(),
        );
        assert_eq!(worst(&f), Some(Limit::Warning));
        let none = cw_lines(&assumed, None, &limits());
        assert_eq!(
            worst(&none),
            Some(Limit::Warning),
            "no vehicle data is never silent"
        );
        assert_eq!(worst(&[]), None);
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

    #[test]
    fn layout_panels_stay_inside_the_grid_at_reference_and_awkward_sizes() {
        for (cols, rows) in [
            (160, 45),
            (200, 56),
            (106, 30),
            (80, 24),
            (20, 10),
            (3, 3),
            (0, 0),
        ] {
            for r in layout(cols, rows) {
                assert!(
                    r.col + r.cols <= cols,
                    "{cols}x{rows}: {r:?} overflows columns"
                );
                assert!(
                    r.row + r.rows <= rows.max(1),
                    "{cols}x{rows}: {r:?} overflows rows"
                );
            }
        }
        let [orbit, ground, timeline, ..] = layout(160, 45);
        assert_eq!(
            orbit.row + orbit.rows,
            ground.row,
            "left column panels are contiguous"
        );
        assert_eq!(ground.row + ground.rows, timeline.row);
        assert!(
            timeline.row + timeline.rows <= 45 - 2,
            "footer rows are free"
        );
    }
}
