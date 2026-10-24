//! Page composition: the header, footer, every page and sub-page of the page map (DESIGN.md §9.4),
//! the greyed `NO SOURCE` page and the help overlay. Panels are small functions so pages are layouts.

use mctl_core::cw::{CwState, Severity};
use mctl_core::time::{Utc, fmt_dur};
use mctl_core::world::{ClockMode, EopView, SourceStatus, SourceView};

use crate::commands::COMMANDS;
use crate::grid::{CellRect, Grid, Limit, Role, Style};
use crate::keys::{KeyAction, KeyMap};
use crate::page::{
    Layout, Lines, MIN_COLS, MIN_ROWS, PageInput, quality_text, source_row, upcoming_events,
};
use crate::pages::{ALL_PAGES, Availability, Page, availability};
use crate::telemetry_view;

/// `DDDD:HH:MM:SS` mission elapsed time; negative (before the epoch) is shown as `--`.
pub fn fmt_met(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--".into();
    }
    let s = secs as u64;
    format!(
        "{:04}:{:02}:{:02}:{:02}",
        s / 86_400,
        (s % 86_400) / 3600,
        (s % 3600) / 60,
        s % 60
    )
}

fn clock_text(inp: &PageInput<'_>) -> (String, Style) {
    let c = inp.world.clock;
    match c.mode {
        ClockMode::Live => ("LIVE 1x".into(), Style::new(Role::Bright)),
        ClockMode::Sim if c.paused => (
            format!("SIM PAUSED {:+}x", c.rate),
            Style::limit(Limit::Caution),
        ),
        ClockMode::Sim => (format!("SIM {:+}x", c.rate), Style::limit(Limit::Caution)),
    }
}

fn sub_title(inp: &PageInput<'_>) -> String {
    let subs = inp.page.subpages();
    if subs.len() > 1 {
        let sub = subs[inp.sub.min(subs.len() - 1)];
        // At L2 the orbit-map and elements sub-pages show different things under the same keys.
        let sub = match (is_l2(inp), inp.page, inp.sub) {
            (true, Page::Traj, 0) => "HALO ORBIT",
            (true, Page::Traj, 2) => "DISTANCE AND ANGLE",
            _ => sub,
        };
        format!("{} / {sub}", inp.page.title())
    } else {
        inp.page.title().to_owned()
    }
}

/// Row 0: vehicle, page, GMT, MET, clock, source and age, and the master annunciator (DESIGN.md §9.3).
fn header(grid: &mut Grid, inp: &PageInput<'_>) {
    let cols = grid.cols;
    let (bright, _dim) = (Style::new(Role::Bright), Style::new(Role::Dim));
    let (severity, latched) = master_state(inp);
    let (name, norad) = inp.vehicle.map_or(("--".to_owned(), String::new()), |v| {
        // A vehicle with no catalogue number (JWST, Roman) shows just its name.
        (
            v.name.clone(),
            if v.norad == 0 {
                String::new()
            } else {
                format!(" ({})", v.norad)
            },
        )
    });
    let src = inp
        .vehicle
        .and_then(|v| v.solution.as_ref())
        .map_or("NO DATA".to_owned(), quality_text);
    let age = inp
        .vehicle
        .and_then(|v| v.sources.iter().find(|s| s.status == SourceStatus::Active))
        .and_then(|s| s.age_s)
        .map_or(String::new(), |a| format!(" AGE {}", fmt_dur(a.abs())));
    let met = inp
        .vehicle
        .and_then(|v| v.met_epoch)
        .map_or("--".to_owned(), |e| fmt_met(inp.world.t.minus(e)));
    let annunciator = match (severity, latched) {
        (Some(Severity::Warning), true) => " WARN* ",
        (Some(Severity::Warning), false) => " WARN ",
        (Some(Severity::Caution), true) => " CAUT* ",
        (Some(Severity::Caution), false) => " CAUT ",
        (Some(Severity::Stale), _) => " STALE ",
        (None, _) => " NOM ",
    };
    let budget = cols.saturating_sub(annunciator.len() + 1);
    let (clock, clock_style) = clock_text(inp);
    let before = format!(
        " {name}{norad} | {} | GMT {} | MET {met} | ",
        sub_title(inp),
        inp.world.t.gmt_doy()
    );
    let site = inp.world.observer.active_site().map_or(String::new(), |s| {
        format!(" | SITE {} ({})", s.name, s.origin)
    });
    let after = format!(" | SRC {src}{age}{site}");
    let mut col = 0;
    for (text, style) in [(before, bright), (clock, clock_style), (after, bright)] {
        let room = budget.saturating_sub(col);
        let t: String = text.chars().take(room).collect();
        grid.put(col, 0, &t, style);
        col += t.chars().count();
    }
    grid.put(
        budget + 1,
        0,
        annunciator,
        severity.map_or(Style::new(Role::Nominal), |s| Style::limit(limit_of(s))),
    );
}

/// Page-specific key hints for the footer's left side, with the user's actual bindings.
fn page_hints(page: Page, sub: usize, k: &KeyMap) -> String {
    let l = |a| k.label(a);
    match (page, sub) {
        (Page::Master | Page::Traj, 0) | (Page::Master, _) => format!(
            "ARROWS/DRAG ORBIT {}/{} ZOOM {} ALT {} CAM {} FOCUS {} FRAME",
            l(KeyAction::ZoomIn).replace(' ', ""),
            l(KeyAction::ZoomOut).replace(' ', ""),
            l(KeyAction::Altitude),
            l(KeyAction::CameraFrame),
            l(KeyAction::Focus),
            l(KeyAction::Frame)
        ),
        (Page::Traj, 1) => format!("{} FRAME  TAB SUBPAGE", l(KeyAction::Frame)),
        (Page::Traj, 2) => format!("{} WINDOW  TAB SUBPAGE", l(KeyAction::TrendWindow)),
        _ => "TAB SUBPAGE".to_owned(),
    }
}

/// Last two rows: stats and hints (or the status message), then the F-key legend or the command line.
fn footer(grid: &mut Grid, inp: &PageInput<'_>) -> Vec<(usize, usize, usize, Page)> {
    let (cols, rows) = (grid.cols, grid.rows);
    let (dim, info) = (Style::new(Role::Dim), Style::new(Role::Info));
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
    grid.put(cols.saturating_sub(stats.len()), stats_row, &stats, info);
    let room = cols.saturating_sub(stats.len() + 2);
    let small = cols < MIN_COLS || rows < MIN_ROWS;
    let (left, style) = if let (Some(cmd), true) = (
        inp.command,
        inp.command.is_some_and(|c| !c.candidates.is_empty()),
    ) {
        (format!("TAB: {}", cmd.candidates.join("  ")), info)
    } else if let Some(m) = inp.status {
        (
            m.text.clone(),
            if m.error {
                Style::limit(Limit::Caution)
            } else {
                info
            },
        )
    } else if small {
        (
            format!("WINDOW {cols}x{rows} CELLS < MIN {MIN_COLS}x{MIN_ROWS}: PANELS CLIPPED"),
            Style::limit(Limit::Caution),
        )
    } else {
        let k = inp.keys;
        let common = format!(
            "{} PAUSE {} {} WARP {} LIVE {} THEME {} VEHICLE",
            k.label(KeyAction::Pause),
            k.label(KeyAction::WarpDown),
            k.label(KeyAction::WarpUp),
            k.label(KeyAction::Live),
            k.label(KeyAction::Theme),
            k.label(KeyAction::Vehicle)
        );
        (
            format!(
                "{}  {common}",
                if is_l2(inp) && matches!(inp.page, Page::Master | Page::Traj) {
                    "TAB SUBPAGE".to_owned()
                } else {
                    page_hints(inp.page, inp.sub, k)
                }
            ),
            dim,
        )
    };
    grid.put(
        1,
        stats_row,
        &left.chars().take(room).collect::<String>(),
        style,
    );

    let last = rows.saturating_sub(1);
    if let Some(cmd) = inp.command {
        let line: String = format!(":{}_", cmd.text).chars().take(cols).collect();
        grid.put(0, last, &line, Style::new(Role::Bright));
        return Vec::new();
    }
    let mut col = 1;
    let mut hits = Vec::new();
    for p in ALL_PAGES {
        let label = format!("F{} {}", p.fkey(), p.title());
        let (text, style) = if p == inp.page {
            (format!("[{label}]"), Style::new(Role::Bright))
        } else if matches!(availability(p, 0, inp.vehicle), Availability::NoSource(_)) {
            (label, dim)
        } else {
            (label, Style::new(Role::Fg))
        };
        grid.put(col, last, &text, style);
        let width = text.chars().count();
        hits.push((col, col + width - 1, last, p));
        col += width + 1;
    }
    let hint = ": CMD  ? HELP  Q QUIT";
    grid.put(cols.saturating_sub(hint.len() + 1), last, hint, dim);
    hits
}

// ---- panels --------------------------------------------------------------------------------------------

pub(crate) fn boxed(grid: &mut Grid, rect: CellRect, title: &str) {
    grid.draw_box(
        rect,
        title,
        Style::new(Role::Border),
        Style::new(Role::Bright),
    );
}

fn orbit_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) -> CellRect {
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
    boxed(
        grid,
        rect,
        &format!(
            "ORBIT MAP  ALT x{:.0}  {frame}  FOCUS {focus}",
            cam.exaggeration()
        ),
    );
    if inp.vehicle.and_then(|v| v.solution.as_ref()).is_none() {
        Lines::new(grid, rect).no_data("NO TRAJECTORY SOLUTION YET");
    }
    rect.inner()
}

fn ground_panel(grid: &mut Grid, rect: CellRect) -> CellRect {
    boxed(grid, rect, "GROUND TRACK  PAST SOLID / PREDICTED DASHED");
    rect.inner()
}

pub(crate) fn state_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
    let fg = Style::new(Role::Fg);
    let sol = inp.vehicle.and_then(|v| v.solution.as_ref());
    let quality = sol.map_or("NO DATA".to_owned(), quality_text);
    boxed(
        grid,
        rect,
        &format!(
            "STATE VECTOR ({})  {quality}",
            if inp.show_ecef { "ITRF" } else { "GCRF" }
        ),
    );
    let mut l = Lines::new(grid, rect);
    let Some(s) = sol else {
        l.no_data("NO SOURCE HAS DELIVERED A STATE");
        return;
    };
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

fn elements_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
    let fg = Style::new(Role::Fg);
    boxed(grid, rect, "ELEMENTS (OSCULATING)");
    let mut l = Lines::new(grid, rect);
    let Some(s) = inp.vehicle.and_then(|v| v.solution.as_ref()) else {
        l.no_data("NEEDS A STATE VECTOR");
        return;
    };
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

fn subsystem_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
    boxed(grid, rect, "SUBSYSTEM SUMMARY");
    let mut l = Lines::new(grid, rect);
    telemetry_banner(&mut l, inp);
    // The feed belongs to the vehicle that lists it; never show the ISS feed on another vehicle's page.
    let feed = inp
        .vehicle
        .filter(|v| v.telemetry.is_some())
        .and(inp.world.telemetry.as_ref());
    let Some(t) = feed else {
        let why = match availability(Page::Gnc, 0, inp.vehicle) {
            Availability::NoSource(why) => why.to_uppercase(),
            Availability::Available => "NO TELEMETRY FEED".to_owned(),
        };
        l.no_data(&format!("NO SOURCE: {why}"));
        return;
    };
    for (group, title) in TELEMETRY_GROUPS {
        let got = t.items.iter().filter(|i| i.group == *group).count();
        let total = t.total_in(group);
        let style = if got == 0 {
            Style::limit(Limit::Stale)
        } else {
            Style::new(Role::Fg)
        };
        l.line(
            &format!("{title:<10} {got:>3}/{total:<3} ITEMS RECEIVED"),
            style,
        );
    }
}

fn limit_of(s: Severity) -> Limit {
    match s {
        Severity::Warning => Limit::Warning,
        Severity::Caution => Limit::Caution,
        Severity::Stale => Limit::Stale,
    }
}

/// Severity to annunciate (live, or latched until acknowledged) and whether it is only latched.
fn master_state(inp: &PageInput<'_>) -> (Option<Severity>, bool) {
    let cw = &inp.world.cw;
    let live = cw.worst_now(inp.vehicle.map(|v| v.id.as_str()));
    let latched = if cw.latch.warning {
        Some(Severity::Warning)
    } else if cw.latch.caution {
        Some(Severity::Caution)
    } else {
        None
    };
    let shown = live.max(latched);
    (shown, latched.is_some())
}

pub(crate) fn cw_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
    let dim = Style::new(Role::Dim);
    let cw = &inp.world.cw;
    boxed(grid, rect, "CAUTION & WARNING");
    let mut l = Lines::new(grid, rect);
    let vehicle = inp.vehicle.map(|v| v.id.as_str());
    if cw.latch.warning || cw.latch.caution {
        let which = if cw.latch.warning {
            "WARNING"
        } else {
            "CAUTION"
        };
        l.line(
            &format!("MASTER {which} LATCHED  PRESS A TO ACKNOWLEDGE"),
            Style::limit(if cw.latch.warning {
                Limit::Warning
            } else {
                Limit::Caution
            }),
        );
    }
    let mut shown = 0;
    for c in cw
        .conditions
        .iter()
        .filter(|c| c.vehicle.is_none() || c.vehicle.as_deref() == vehicle)
    {
        l.line(
            &format!("{:<5} {}", c.severity.label(), c.text),
            Style::limit(limit_of(c.severity)),
        );
        shown += 1;
    }
    for r in cw
        .rules
        .iter()
        .filter(|r| r.applies_to(vehicle) && r.state != CwState::Nom)
    {
        let v = r
            .value
            .map_or(String::new(), |v| format!(" {v:.2} {}", r.units));
        let style = r
            .state
            .severity()
            .map_or(dim, |s| Style::limit(limit_of(s)));
        l.line(&format!("{:<5} {}{v}", r.state.label(), r.title), style);
        shown += 1;
    }
    if shown == 0 {
        l.line(
            "NOM   NO ACTIVE CAUTIONS OR WARNINGS",
            Style::new(Role::Nominal),
        );
    }
    l.row = l.rect.rows.saturating_sub(1);
    let mine: Vec<_> = cw.rules.iter().filter(|r| r.applies_to(vehicle)).collect();
    let nom = mine.iter().filter(|r| r.state == CwState::Nom).count();
    l.line(
        &format!("{nom}/{} RULES NOM  (F10 TAB: DETAIL)", mine.len()),
        dim,
    );
}

pub(crate) fn sources_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
    let dim = Style::new(Role::Dim);
    boxed(grid, rect, "SOURCES  HEALTH / AGE / DIFFERENCE");
    let mut l = Lines::new(grid, rect);
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
}

fn eclipse_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) -> CellRect {
    boxed(grid, rect, "ECLIPSE / BETA");
    let mut l = Lines::new(grid, rect);
    match inp.vehicle.and_then(|v| v.solution.as_ref()) {
        None => l.no_data("NEEDS A STATE VECTOR"),
        Some(s) => {
            let state = if s.illumination >= 0.999 {
                ("SUNLIT".to_owned(), Style::new(Role::Fg))
            } else if s.illumination <= 0.001 {
                ("UMBRA".to_owned(), Style::limit(Limit::Stale))
            } else {
                (
                    format!("PENUMBRA {:.0}%", s.illumination * 100.0),
                    Style::limit(Limit::Stale),
                )
            };
            l.put(1, &state.0, state.1);
            l.put(
                20,
                &format!("BETA {:+.1} deg", s.beta_deg),
                Style::new(Role::Fg),
            );
        }
    }
    rect.inner()
}

fn timeline_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
    boxed(grid, rect, "EVENT TIMELINE  FROM TRACK SAMPLES");
    let mut l = Lines::new(grid, rect);
    match inp.vehicle.and_then(|v| v.track.as_deref()) {
        None => l.no_data("NEEDS A TRACK"),
        Some(tr) => {
            let rows = l.rect.rows;
            for e in upcoming_events(tr, inp.world.t).iter().take(rows) {
                let dt = e.t.minus(inp.world.t);
                l.line(
                    &format!("T-{:<9} {:<16} GMT {}", fmt_dur(dt), e.label, e.t.gmt_doy()),
                    Style::new(Role::Fg),
                );
            }
        }
    }
}

// ---- pages ---------------------------------------------------------------------------------------------

/// (row, height) of the body between the header and the two footer rows.
pub(crate) fn body(rows: usize) -> (usize, usize) {
    (1, rows.saturating_sub(3))
}

/// Split `total` into parts by percentage, giving the remainder to the last part.
pub(crate) fn split(total: usize, pcts: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = pcts.iter().map(|p| total * p / 100).collect();
    if let Some((last, rest)) = out.split_last_mut() {
        *last = total.saturating_sub(rest.iter().sum::<usize>());
    }
    out
}

pub(crate) fn column(x: usize, w: usize, top: usize, heights: &[usize]) -> Vec<CellRect> {
    let mut y = top;
    heights
        .iter()
        .map(|h| {
            let r = CellRect::new(x, y, w, *h);
            y += h;
            r
        })
        .collect()
}

fn master(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let (lw, mw) = (cols * 50 / 100, cols * 25 / 100);
    let rw = cols.saturating_sub(lw + mw);
    let left = column(0, lw, top, &split(h, &[48, 33, 19]));
    let mid = column(lw, mw, top, &split(h, &[34, 31, 35]));
    let right = column(lw + mw, rw, top, &split(h, &[28, 38, 34]));
    let globe = orbit_panel(grid, left[0], inp);
    let ground = ground_panel(grid, left[1]);
    timeline_panel(grid, left[2], inp);
    state_panel(grid, mid[0], inp);
    elements_panel(grid, mid[1], inp);
    subsystem_panel(grid, mid[2], inp);
    cw_panel(grid, right[0], inp);
    sources_panel(grid, right[1], inp);
    let eclipse = eclipse_panel(grid, right[2], inp);
    Layout {
        globe: Some(globe),
        ground: Some(ground),
        eclipse: Some(eclipse),
        ..Layout::default()
    }
}

fn traj_orbit(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 68 / 100;
    let globe = orbit_panel(grid, CellRect::new(0, top, lw, h), inp);
    let right = column(lw, cols - lw, top, &split(h, &[36, 33, 31]));
    state_panel(grid, right[0], inp);
    elements_panel(grid, right[1], inp);
    sources_panel(grid, right[2], inp);
    Layout {
        globe: Some(globe),
        ..Layout::default()
    }
}

fn traj_ground(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 72 / 100;
    let left = column(0, lw, top, &split(h, &[78, 22]));
    let ground = ground_panel(grid, left[0]);
    timeline_panel(grid, left[1], inp);
    let right = column(lw, cols - lw, top, &split(h, &[40, 60]));
    state_panel(grid, right[0], inp);
    sources_panel(grid, right[1], inp);
    Layout {
        ground: Some(ground),
        ..Layout::default()
    }
}

fn traj_elements(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 24 / 100;
    let left = column(0, lw, top, &split(h, &[45, 55]));
    elements_panel(grid, left[0], inp);
    state_panel(grid, left[1], inp);
    let region = CellRect::new(lw, top, cols - lw, h);
    let set = inp
        .world
        .trends
        .as_ref()
        .filter(|s| inp.vehicle.is_some_and(|v| v.id == s.vehicle));
    let window = crate::trendplot::window_label(inp.trend_window_s);
    let Some(set) = set else {
        boxed(grid, region, &format!("TRENDS  {window}"));
        let mut l = Lines::new(grid, region);
        l.line("LOADING FROM THE RECORDING...", Style::new(Role::Dim));
        l.line(
            "(NEEDS THE RECORDING: IT IS OFF WITH --no-record OR [store] enabled = false)",
            Style::new(Role::Dim),
        );
        return Layout::default();
    };
    // Header line, then a grid of plots, two columns by four rows at full size.
    grid.put(
        lw + 1,
        top,
        &format!(
            "TRENDS {window}   {} STATE ROWS   {} GP ELEMENT SETS IN WINDOW (DASHED)   W CYCLES THE WINDOW, :trend <6h..90d>",
            set.rows,
            set.element_marks.len()
        ),
        Style::new(Role::Bright),
    );
    let area = CellRect::new(region.col, top + 1, region.cols, h.saturating_sub(1));
    let (ncols, nrows) = if area.cols >= 90 { (2, 4) } else { (1, 8) };
    let (cw, ch) = (area.cols / ncols, area.rows / nrows);
    let mut plots = Vec::new();
    for (i, plot) in set.plots.iter().enumerate().take(ncols * nrows) {
        let rect = CellRect::new(
            area.col + (i % ncols) * cw,
            area.row + (i / ncols) * ch,
            cw,
            ch,
        );
        let unit = if plot.units.is_empty() {
            String::new()
        } else {
            format!(" ({})", plot.units)
        };
        boxed(grid, rect, &format!("{}{unit}", plot.title.to_uppercase()));
        if crate::trendplot::has_data(plot) {
            plots.push((rect, i));
        } else {
            let mut l = Lines::new(grid, rect);
            l.line("NO RECORDED DATA IN THIS WINDOW", Style::new(Role::Dim));
        }
    }
    Layout {
        plots,
        ..Layout::default()
    }
}

/// Largest disagreement between the quaternion-derived and the reported attitude errors that still
/// counts as consistent. The two are sampled up to a few seconds apart at rates up to ~0.07 deg/s, and
/// the feed rounds, so 0.5 deg is generous for a hold and tight enough to catch a wrong convention.
const ADI_CONSISTENCY_TOL_DEG: f64 = 0.5;

/// Telemetry older than this is shown STALE. Labelled constant: the feed updates items every few
/// seconds at most, so this is generous (DESIGN.md FR-6 defines the STALE quality).
const TELEMETRY_STALE_S: f64 = 30.0;

/// Whether an item shows as stale on this page: the signal is lost, or (without the stream's clock)
/// the item itself is old.
fn stale_fn<'a>(inp: &'a PageInput<'_>) -> impl Fn(&mctl_core::world::TelemetryItem) -> bool + 'a {
    move |i| {
        inp.world
            .telemetry
            .as_ref()
            .is_some_and(|t| t.is_stale(i, TELEMETRY_STALE_S))
    }
}

fn telemetry_banner(l: &mut Lines<'_>, inp: &PageInput<'_>) {
    match inp.world.telemetry.as_ref().map(|t| &t.health) {
        None => l.line("NO TELEMETRY FEED", Style::limit(Limit::Caution)),
        Some(mctl_core::world::Health::Ok) => {}
        Some(h) => l.line(
            &format!("FEED {h:?}").to_uppercase(),
            Style::limit(Limit::Caution),
        ),
    }
    if let Some(t) = inp.world.telemetry.as_ref().filter(|t| t.signal_lost()) {
        l.line(
            &format!(
                "SIGNAL LOST: VALUES FROZEN {} AGO",
                fmt_dur(t.feed_age_s.unwrap_or(0.0))
            ),
            Style::limit(Limit::Caution),
        );
    }
    if !inp.world.clock.is_live() {
        l.line(
            "NOT LIVE: TELEMETRY IS CURRENT, NOT AT SIM TIME",
            Style::limit(Limit::Caution),
        );
    }
}

fn gnc_adi(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 45 / 100;
    let ball = CellRect::new(0, top, lw, h);
    boxed(grid, ball, "ADI  LVLH");
    let att = crate::adi::attitude(inp.world.telemetry.as_ref());
    let inner = ball.inner();
    let mut l = Lines::new(grid, ball);
    l.row = inner.rows.saturating_sub(3);
    match &att {
        Ok(a) => {
            l.line(
                &format!(
                    "ROLL {:+7.2}  PITCH {:+7.2}  YAW {:+7.2} DEG",
                    a.roll_deg, a.pitch_deg, a.yaw_deg
                ),
                Style::new(Role::Fg),
            );
            match crate::adi::consistency(inp.world.telemetry.as_ref()) {
                Some(c) if c.max_diff_deg() <= ADI_CONSISTENCY_TOL_DEG => {
                    l.line(
                        &format!(
                            "CONVENTION CHECK OK (QUAT DIFF VS REPORTED ERROR, MAX {:.2} DEG)",
                            c.max_diff_deg()
                        ),
                        Style::new(Role::Dim),
                    );
                }
                Some(c) => l.line(
                    &format!(
                        "CONVENTION CHECK FAILED: MAX DIFF {:.2} DEG; ADI MAY BE WRONG",
                        c.max_diff_deg()
                    ),
                    Style::limit(Limit::Warning),
                ),
                None => l.line(
                    "CONVENTION CHECK: COMMANDED OR ERROR ITEMS NOT RECEIVED",
                    Style::limit(Limit::Caution),
                ),
            }
        }
        Err(why) => l.line(
            &format!("OFF: {}", why.to_uppercase()),
            Style::limit(Limit::Caution),
        ),
    }
    let right = CellRect::new(lw, top, cols - lw, h);
    let items = inp
        .world
        .telemetry
        .as_ref()
        .map_or(&[][..], |t| &t.items[..]);
    let secs = telemetry_view::sections(items, "attitude", &stale_fn(inp));
    for (idx, rect) in telemetry_view::pack(&secs, right, 1) {
        telemetry_view::render(grid, rect, &secs[idx]);
    }
    if secs.is_empty() {
        boxed(grid, right, "ATTITUDE");
        let mut l = Lines::new(grid, right);
        telemetry_banner(&mut l, inp);
        l.no_data("NO ATTITUDE ITEMS RECEIVED");
    }
    Layout {
        adi: Some(inner),
        ..Layout::default()
    }
}

/// Telemetry groups in display order, with their titles.
const TELEMETRY_GROUPS: &[(&str, &str)] = &[
    ("attitude", "ATTITUDE"),
    ("cmg", "CMG"),
    ("eps", "EPS"),
    ("tcs", "TCS"),
    ("eclss", "ECLSS"),
    ("comm", "COMM"),
];

/// A telemetry group as titled sections (matrices where items share rows and columns), packed into
/// one or two columns, under a one-line status.
fn group_table(
    grid: &mut Grid,
    inp: &PageInput<'_>,
    title: &str,
    group: &str,
    note: &str,
) -> Layout {
    group_table_reserved(grid, inp, title, group, note, 0)
}

/// As [`group_table`], leaving `reserve` rows free at the bottom of the body for another panel.
fn group_table_reserved(
    grid: &mut Grid,
    inp: &PageInput<'_>,
    title: &str,
    group: &str,
    note: &str,
    reserve: usize,
) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let h = h.saturating_sub(reserve);
    let t = inp.world.telemetry.as_ref();
    let (got, total) = t.map_or((0, 0), |t| {
        (
            t.items.iter().filter(|i| i.group == group).count(),
            t.total_in(group),
        )
    });
    let lost = t.is_some_and(|t| t.signal_lost());
    let feed = match t.map(|t| &t.health) {
        None => ("NO TELEMETRY FEED".to_owned(), Style::limit(Limit::Caution)),
        Some(mctl_core::world::Health::Ok) if lost => {
            ("SIGNAL LOST".to_owned(), Style::limit(Limit::Caution))
        }
        Some(mctl_core::world::Health::Ok) => ("FEED OK".to_owned(), Style::new(Role::Nominal)),
        Some(h) => (
            format!("FEED {h:?}").to_uppercase(),
            Style::limit(Limit::Caution),
        ),
    };
    let ages: Vec<f64> = t
        .map(|t| {
            t.items
                .iter()
                .filter(|i| i.group == group)
                .map(|i| i.age_s.max(0.0))
                .collect()
        })
        .unwrap_or_default();
    // The stream's own clock says whether data is flowing; per-item ages only say when a value last
    // changed, and many items never change for hours.
    let fresh = match (
        t.and_then(|t| t.feed_age_s),
        ages.iter().cloned().reduce(f64::min),
        ages.iter().cloned().reduce(f64::max),
    ) {
        (Some(a), Some(_), _) if lost => format!("VALUES FROZEN {} AGO", fmt_dur(a)),
        (Some(a), Some(lo), Some(hi)) => format!(
            "STREAM CLOCK {} BEHIND  LAST CHANGE {} .. {} AGO",
            fmt_dur(a.max(0.0)),
            fmt_dur(lo),
            fmt_dur(hi)
        ),
        (None, Some(lo), Some(hi)) => format!("UPDATED {} .. {} AGO", fmt_dur(lo), fmt_dur(hi)),
        _ => "NOTHING RECEIVED".to_owned(),
    };
    let live = if inp.world.clock.is_live() {
        String::new()
    } else {
        "  NOT LIVE: TELEMETRY IS CURRENT, NOT AT SIM TIME".to_owned()
    };
    let line = format!("{title}   {got}/{total} ITEMS   {fresh}{live}");
    grid.put(1, top, &line, Style::new(Role::Bright));
    grid.put(1 + line.chars().count() + 3, top, &feed.0, feed.1);
    let items = t.map_or(&[][..], |t| &t.items[..]);
    let secs = telemetry_view::sections(items, group, &stale_fn(inp));
    let region = CellRect::new(0, top + 1, cols, h.saturating_sub(2));
    let columns = if cols >= 120 { 2 } else { 1 };
    let placed = telemetry_view::pack(&secs, region, columns);
    for (idx, rect) in &placed {
        telemetry_view::render(grid, *rect, &secs[*idx]);
    }
    // History charts fill what the section boxes leave free, below them.
    let used = placed
        .iter()
        .map(|(_, r)| r.row + r.rows)
        .max()
        .unwrap_or(region.row);
    let free = CellRect::new(
        0,
        used,
        cols,
        (region.row + region.rows).saturating_sub(used),
    );
    telemetry_view::render_charts(grid, free, &telemetry_view::charts(&secs));
    if placed.len() < secs.len() {
        grid.put(
            1,
            top + h - 2,
            &format!(
                "{} MORE SECTION(S) DO NOT FIT AT THIS WINDOW SIZE",
                secs.len() - placed.len()
            ),
            Style::limit(Limit::Caution),
        );
    }
    if secs.is_empty() {
        grid.put(
            1,
            top + 2,
            "NO DATA: NOTHING RECEIVED FOR THIS PAGE YET",
            Style::new(Role::Dim),
        );
    }
    if !note.is_empty() {
        grid.put(1, top + h - 1, note, Style::new(Role::Dim));
    }
    Layout::default()
}

fn active_pass_set<'a>(inp: &'a PageInput<'_>) -> Option<&'a mctl_core::world::PassSet> {
    let site = inp.world.observer.active_site()?;
    inp.vehicle?.passes.iter().find(|s| s.site == site.name)
}

/// The pass in progress, else the next one.
fn current_or_next_pass(
    set: &mctl_core::world::PassSet,
    now: Utc,
) -> Option<&mctl_core::world::PassView> {
    set.passes.iter().find(|p| p.los.0 > now.0)
}

fn pass_state(l: &mut Lines<'_>, inp: &PageInput<'_>) {
    match inp.world.observer.active_site() {
        None => l.no_data("NO OBSERVER SITE"),
        Some(s) => {
            let src = match s.origin.as_str() {
                "DEFAULT" => "DEFAULT, NOT YOUR LOCATION: SET [observer] active",
                "ADDED" => "ADDED THIS SESSION (NOT SAVED)",
                _ => "CONFIGURED",
            };
            l.line(
                &format!("SITE {}  ({src})", s.name),
                Style::new(Role::Bright),
            );
            l.line(
                &format!(
                    "{:+.4} {:+.4} {:.0} M  MASK {:.0} DEG",
                    s.lat_deg, s.lon_deg, s.alt_m, inp.world.observer.mask_deg
                ),
                Style::new(Role::Dim),
            );
        }
    }
    if !inp.world.clock.is_live() {
        l.line(
            "SIM CLOCK: PASSES ARE PREDICTED FROM THE CLOCK TIME",
            Style::limit(Limit::Caution),
        );
    }
}

fn pass_sky(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 58 / 100;
    let plot = CellRect::new(0, top, lw, h);
    boxed(grid, plot, "SKY PLOT  N UP, E RIGHT, ZENITH CENTRE");
    let right = CellRect::new(lw, top, cols - lw, h);
    boxed(grid, right, "NEXT PASS");
    let mut l = Lines::new(grid, right);
    pass_state(&mut l, inp);
    l.row += 1;
    let Some(set) = active_pass_set(inp) else {
        l.no_data("NO PREDICTION YET (NEEDS A TRAJECTORY SOURCE)");
        return Layout {
            sky: Some(plot.inner()),
            ..Layout::default()
        };
    };
    let now = inp.world.t;
    if let Some(n) = set.look_now {
        let above = n.el_deg >= inp.world.observer.mask_deg;
        l.line(
            &format!(
                "NOW  AZ {:5.1}  EL {:+5.1}  RANGE {:.0} KM  {}",
                n.az_deg,
                n.el_deg,
                n.range_km,
                if above {
                    "ABOVE MASK"
                } else if n.el_deg >= 0.0 {
                    "BELOW MASK"
                } else {
                    "BELOW HORIZON"
                }
            ),
            if above {
                Style::new(Role::Bright)
            } else {
                Style::new(Role::Fg)
            },
        );
    }
    match current_or_next_pass(set, now) {
        None => l.line(
            "NO PASS IN THE PREDICTION WINDOW",
            Style::limit(Limit::Caution),
        ),
        Some(p) => {
            let in_progress = p.aos.0 <= now.0;
            let fg = Style::new(Role::Fg);
            l.line(
                &if in_progress {
                    format!("IN PROGRESS  LOS IN {}", fmt_dur(p.los.minus(now)))
                } else {
                    format!("AOS IN {}", fmt_dur(p.aos.minus(now)))
                },
                Style::new(Role::Bright),
            );
            l.line(
                &format!("AOS {}  AZ {:5.1}", p.aos.gmt_doy(), p.aos_az_deg),
                fg,
            );
            l.line(
                &format!(
                    "TCA {}  AZ {:5.1}  MAX EL {:.1}",
                    p.tca.gmt_doy(),
                    p.tca_az_deg,
                    p.max_el_deg
                ),
                fg,
            );
            l.line(
                &format!(
                    "LOS {}  AZ {:5.1}  DURATION {}",
                    p.los.gmt_doy(),
                    p.los_az_deg,
                    fmt_dur(p.duration_s())
                ),
                fg,
            );
            l.line(
                match p.visible {
                    Some(true) => "VISIBLE PASS: DARK SKY AND VEHICLE SUNLIT AT CULMINATION",
                    Some(false) => "NOT VISIBLE (DAYLIGHT OR VEHICLE IN SHADOW AT CULMINATION)",
                    None => "VISIBILITY UNKNOWN (NO SUN POSITION)",
                },
                Style::new(Role::Dim),
            );
            if p.started_before_window || p.ends_after_window {
                l.line(
                    "PASS CLIPPED BY THE PREDICTION WINDOW",
                    Style::limit(Limit::Caution),
                );
            }
        }
    }
    l.row += 1;
    if let Some(el) = inp.world.observer.active_site().and_then(|s| s.sun_el_deg) {
        let sky = if el <= mctl_astro::observer::DARK_SKY_SUN_ELEVATION_DEG {
            "DARK SKY"
        } else if el > 0.0 {
            "DAYTIME"
        } else {
            "TWILIGHT"
        };
        let moon = inp
            .world
            .observer
            .active_site()
            .and_then(|s| s.moon_el_deg)
            .map_or(String::new(), |m| format!("   MOON EL {m:+.1}"));
        l.line(
            &format!("SUN EL {el:+.1}  {sky}{moon}"),
            Style::new(Role::Dim),
        );
    }
    l.line(
        &format!(
            "SOURCE {}  WINDOW {} DAYS",
            set.source.to_uppercase(),
            ((set.to.minus(set.from)) / 86_400.0).round()
        ),
        Style::new(Role::Dim),
    );
    Layout {
        sky: Some(plot.inner()),
        ..Layout::default()
    }
}

fn pass_list(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (top, h) = body(grid.rows);
    let rect = CellRect::new(0, top, grid.cols, h);
    let name = inp
        .world
        .observer
        .active_site()
        .map_or("--", |s| s.name.as_str());
    boxed(
        grid,
        rect,
        &format!(
            "PASS LIST  {name}  MASK {:.0} DEG",
            inp.world.observer.mask_deg
        ),
    );
    let mut l = Lines::new(grid, rect);
    pass_state(&mut l, inp);
    let Some(set) = active_pass_set(inp) else {
        l.no_data("NO PREDICTION YET (NEEDS A TRAJECTORY SOURCE)");
        return Layout::default();
    };
    l.line(
        " #  IN         AOS GMT          DURATION  MAX EL  AOS AZ -> LOS AZ  VISIBLE",
        Style::new(Role::Dim),
    );
    let now = inp.world.t;
    let rows = l.rect.rows.saturating_sub(l.row);
    for (i, p) in set
        .passes
        .iter()
        .filter(|p| p.los.0 > now.0)
        .take(rows)
        .enumerate()
    {
        let until = if p.aos.0 <= now.0 {
            "NOW".to_owned()
        } else {
            fmt_dur(p.aos.minus(now))
        };
        let vis = match p.visible {
            Some(true) => "YES",
            Some(false) => "no",
            None => "?",
        };
        let clip = if p.started_before_window || p.ends_after_window {
            "*"
        } else {
            ""
        };
        let style = if p.visible == Some(true) {
            Style::new(Role::Bright)
        } else {
            Style::new(Role::Fg)
        };
        l.line(
            &format!(
                "{:>2}  {until:<9}  {}  {:>8}{clip:<1}  {:>6.1}  {:>6.0} -> {:<6.0}  {vis}",
                i + 1,
                p.aos.gmt_doy(),
                fmt_dur(p.duration_s()),
                p.max_el_deg,
                p.aos_az_deg,
                p.los_az_deg
            ),
            style,
        );
    }
    Layout::default()
}

fn pass_sites(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (top, h) = body(grid.rows);
    let rect = CellRect::new(0, top, grid.cols, h);
    boxed(grid, rect, "OBSERVER SITES");
    let mut l = Lines::new(grid, rect);
    l.line(
        "   NAME             ORIGIN       LAT       LON      ALT M  NEXT PASS (IN)   MAX EL",
        Style::new(Role::Dim),
    );
    let now = inp.world.t;
    let active = inp.world.observer.active;
    for (i, s) in inp.world.observer.sites.iter().enumerate() {
        let next = inp
            .vehicle
            .and_then(|v| v.passes.iter().find(|p| p.site == s.name))
            .and_then(|set| current_or_next_pass(set, now));
        let (when, el) = next.map_or(("--".to_owned(), "--".to_owned()), |p| {
            (
                if p.aos.0 <= now.0 {
                    "NOW".into()
                } else {
                    fmt_dur(p.aos.minus(now))
                },
                format!("{:.0}", p.max_el_deg),
            )
        });
        l.line(
            &format!(
                "{} {:<16} {:<10} {:>9.4} {:>9.4} {:>7.0}  {when:<15}  {el}",
                if i == active { ">" } else { " " },
                s.name,
                s.origin,
                s.lat_deg,
                s.lon_deg,
                s.alt_m
            ),
            if i == active {
                Style::new(Role::Bright)
            } else {
                Style::new(Role::Fg)
            },
        );
    }
    l.row += 1;
    for t in [
        "`:site <name>` SELECTS A SITE; `:site add <name> <lat> <lon> [alt_m]` ADDS ONE; `:site save [name]` KEEPS THE ACTIVE ONE IN THE CONFIG.",
        "SITES LIVE IN ~/.config/mctl/config.toml AS [[site]] ENTRIES. RESOLUTION ORDER: CONFIGURED ACTIVE SITE, GEOCLUE2, IP LOOKUP (OFF BY DEFAULT), FALLBACK.",
        "THE DEFAULT SITE (NASA HQ) COORDINATES ARE FROM DESIGN.md AND ARE NOT VERIFIED AGAINST A SURVEY SOURCE.",
    ] {
        l.line(t, Style::new(Role::Dim));
    }
    Layout::default()
}

/// COMM links: the telemetry sections above, and below them the observer site's next AOS and LOS
/// (the vehicle's own link schedule is not published; TDRS coverage is a v2 feature).
fn comm_links(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    const AOS_PANEL_ROWS: usize = 6;
    let layout = group_table_reserved(
        grid,
        inp,
        "COMM  S-BAND, KU-BAND, UHF",
        "comm",
        "",
        AOS_PANEL_ROWS,
    );
    let (cols, rows) = (grid.cols, grid.rows);
    let (_, h) = body(rows);
    let rect = CellRect::new(
        0,
        1 + h.saturating_sub(AOS_PANEL_ROWS),
        cols,
        AOS_PANEL_ROWS,
    );
    let site = inp
        .world
        .observer
        .active_site()
        .map_or("--", |s| s.name.as_str());
    boxed(
        grid,
        rect,
        &format!("AOS / LOS  OBSERVER SITE {site}  (NOT A GROUND-STATION OR TDRS SCHEDULE)"),
    );
    let mut l = Lines::new(grid, rect);
    let now = inp.world.t;
    match active_pass_set(inp).and_then(|set| current_or_next_pass(set, now)) {
        None => l.line("NO PASS PREDICTED YET", Style::new(Role::Dim)),
        Some(p) => {
            let state = if p.aos.0 <= now.0 {
                format!("IN PROGRESS, LOS IN {}", fmt_dur(p.los.minus(now)))
            } else {
                format!("AOS IN {}", fmt_dur(p.aos.minus(now)))
            };
            l.line(&state, Style::new(Role::Bright));
            l.line(
                &format!("AOS {}  AZ {:.0}", p.aos.gmt_doy(), p.aos_az_deg),
                Style::new(Role::Fg),
            );
            l.line(
                &format!(
                    "LOS {}  AZ {:.0}  MAX EL {:.1}",
                    p.los.gmt_doy(),
                    p.los_az_deg,
                    p.max_el_deg
                ),
                Style::new(Role::Fg),
            );
        }
    }
    layout
}

fn sys_events(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (top, h) = body(grid.rows);
    let rect = CellRect::new(0, top, grid.cols, h);
    boxed(
        grid,
        rect,
        &format!("EVENT LOG  {} ENTRIES, NEWEST FIRST", inp.world.log.len()),
    );
    let mut l = Lines::new(grid, rect);
    if inp.world.log.is_empty() {
        l.line("NO EVENTS YET", Style::new(Role::Dim));
    }
    let rows = l.rect.rows;
    for e in inp.world.log.iter().rev().take(rows) {
        l.line(
            &format!("{}  {}", e.at.gmt_doy(), e.text),
            Style::new(Role::Fg),
        );
    }
    Layout::default()
}

fn iso_short(t: Utc) -> String {
    t.iso()[..19].to_owned()
}

fn source_detail(l: &mut Lines<'_>, s: &SourceView, now: Utc) {
    let (fg, dim) = (Style::new(Role::Fg), Style::new(Role::Dim));
    let status = match &s.status {
        SourceStatus::Active => "ACTIVE".to_owned(),
        SourceStatus::Standby => "STANDBY".to_owned(),
        SourceStatus::Stale => "STALE".to_owned(),
        SourceStatus::OutOfSpan => "OUT OF SPAN".to_owned(),
        SourceStatus::NoData => "NO DATA".to_owned(),
        SourceStatus::Failed(w) => format!("FAILED: {w}"),
    };
    l.line(
        &format!("{}  [{status}]", s.kind.label()),
        Style::new(Role::Bright),
    );
    let feed = match &s.health {
        mctl_core::world::Health::Starting => ("STARTING".to_owned(), dim),
        mctl_core::world::Health::Ok => ("OK".to_owned(), fg),
        mctl_core::world::Health::Failing(w) => {
            (format!("FAILING: {w}"), Style::limit(Limit::Caution))
        }
        mctl_core::world::Health::Offline => (
            "OFFLINE (CACHE ONLY)".to_owned(),
            Style::limit(Limit::Stale),
        ),
    };
    l.put(3, "FEED", dim);
    l.put(10, &feed.0, feed.1);
    l.row += 1;
    let row = |l: &mut Lines<'_>, k: &str, v: String| {
        l.put(3, k, dim);
        l.put(10, &v, fg);
        l.row += 1;
    };
    row(l, "EPOCH", s.data_epoch.map_or("--".into(), iso_short));
    row(
        l,
        "AGE",
        s.age_s.map_or("--".into(), |a| {
            format!("{}{}", if a < 0.0 { "-" } else { "" }, fmt_dur(a.abs()))
        }),
    );
    row(
        l,
        "FETCHED",
        s.fetched_at.map_or("--".into(), |f| {
            format!(
                "{} ({} ago, {})",
                iso_short(f),
                fmt_dur(now.minus(f).abs()),
                if s.from_cache {
                    "from cache"
                } else {
                    "network"
                }
            )
        }),
    );
    if let Some((a, b)) = s.validity {
        row(l, "SPAN", format!("{} .. {}", iso_short(a), iso_short(b)));
    }
    if let Some(r) = s.residual {
        row(
            l,
            "DIFF",
            format!(
                "R{:+.3} T{:+.3} N{:+.3} KM  |DR| {:.3} KM  |DV| {:.1} MM/S vs ACTIVE",
                r.rtn_km[0], r.rtn_km[1], r.rtn_km[2], r.norm_km, r.dv_mm_s
            ),
        );
    }
    l.row += 1;
}

fn sys_sources(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 62 / 100;
    let name = inp.vehicle.map_or("--", |v| v.name.as_str());
    let rect = CellRect::new(0, top, lw, h);
    boxed(grid, rect, &format!("SOURCE HEALTH  {name}"));
    let mut l = Lines::new(grid, rect);
    match inp.vehicle {
        None => l.no_data("ENGINE NOT STARTED"),
        Some(v) => {
            for s in &v.sources {
                source_detail(&mut l, s, inp.world.t);
            }
        }
    }
    let erect = CellRect::new(lw, top, cols - lw, h);
    boxed(grid, erect, "EARTH ORIENTATION (IERS EOP)");
    let mut l = Lines::new(grid, erect);
    match &inp.world.eop {
        EopView::Loaded {
            covers_to,
            fetched_at,
            from_cache,
        } => {
            l.line("LOADED", Style::new(Role::Fg));
            l.line(
                &format!("COVERS TO {}", &covers_to.iso()[..10]),
                Style::new(Role::Fg),
            );
            if let Some(f) = fetched_at {
                l.line(
                    &format!(
                        "FETCHED {} ({})",
                        iso_short(*f),
                        if *from_cache { "cache" } else { "network" }
                    ),
                    Style::new(Role::Dim),
                );
            }
        }
        EopView::Assumed { reason } => {
            l.line("ASSUMED ZERO", Style::limit(Limit::Caution));
            l.line("UT1-UTC AND POLAR MOTION ARE 0;", Style::new(Role::Dim));
            l.line("ITRF POSITIONS CAN BE OFF BY ~30 M.", Style::new(Role::Dim));
            l.line(&reason.to_uppercase(), Style::new(Role::Dim));
        }
    }
    Layout::default()
}

fn sys_rules(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (top, h) = body(grid.rows);
    let rect = CellRect::new(0, top, grid.cols, h);
    let cw = &inp.world.cw;
    boxed(
        grid,
        rect,
        &format!(
            "C&W RULES  {}  EDIT IN ~/.config/mctl/config.toml",
            cw.rules.len()
        ),
    );
    let mut l = Lines::new(grid, rect);
    l.line(
        "STATE   RULE                          VALUE        CAUTION    WARNING    BASIS",
        Style::new(Role::Dim),
    );
    for r in &cw.rules {
        let th = |t: Option<f64>| t.map_or("--".to_owned(), |v| format!("{v}"));
        let value = r
            .value
            .map_or("--".to_owned(), |v| format!("{v:.2} {}", r.units));
        let style = r
            .state
            .severity()
            .map_or(Style::new(Role::Fg), |s| Style::limit(limit_of(s)));
        l.line(
            &format!(
                "{:<7} {:<29} {value:<12} {:<10} {:<10} {}",
                r.state.label(),
                r.title,
                th(r.caution),
                th(r.warning),
                r.basis.to_uppercase()
            ),
            style,
        );
    }
    l.row += 1;
    l.line(
        &format!(
            "{} STANDING CONDITIONS (SOURCE HEALTH):",
            cw.conditions.len()
        ),
        Style::new(Role::Dim),
    );
    for c in &cw.conditions {
        l.line(
            &format!("  {:<5} {}", c.severity.label(), c.text),
            Style::limit(limit_of(c.severity)),
        );
    }
    Layout::default()
}

fn sys_settings(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (top, h) = body(grid.rows);
    let rect = CellRect::new(0, top, grid.cols, h);
    boxed(
        grid,
        rect,
        "SETTINGS  READ-ONLY; CHANGE WITH COMMANDS OR ~/.config/mctl/config.toml",
    );
    let on = |b: bool| if b { "ON" } else { "OFF" };
    let t = inp.toggles;
    let cam = inp.camera;
    let live: Vec<(String, String)> = vec![
        ("THEME".into(), inp.theme.name.clone()),
        (
            "EFFECTS".into(),
            format!(
                "SCANLINES {} BLOOM {} PERSISTENCE {} CRT {}",
                on(t.scanlines),
                on(t.bloom),
                on(t.persistence),
                on(t.curvature)
            ),
        ),
        (
            "CAMERA".into(),
            format!(
                "AZ {:.0} EL {:.0} {} FOCUS {} ALT x{}",
                cam.azimuth_deg,
                cam.elevation_deg,
                if cam.earth_fixed {
                    "EARTH-FIXED"
                } else {
                    "INERTIAL"
                },
                if cam.focus_vehicle {
                    "VEHICLE"
                } else {
                    "EARTH"
                },
                cam.exaggeration()
            ),
        ),
        (
            "STATE FRAME".into(),
            if inp.show_ecef {
                "ITRF".into()
            } else {
                "GCRF".into()
            },
        ),
        ("GPU".into(), inp.adapter.to_owned()),
    ];
    let mut l = Lines::new(grid, rect);
    for (k, v) in live.iter().chain(inp.settings.iter()) {
        l.put(1, k, Style::new(Role::Dim));
        l.put(18, v, Style::new(Role::Fg));
        l.row += 1;
    }
    Layout::default()
}

/// What each greyed page will contain, from DESIGN.md §9.6.
fn describe(page: Page, sub: usize) -> &'static [&'static str] {
    match (page, sub) {
        (Page::Gnc, 0) => &[
            "SHADED 8-BALL WITH SKY/GROUND SPLIT, PITCH LADDER, HEADING MERIDIANS, ROLL SCALE,",
            "RATE AND ERROR NEEDLES. REFERENCE LVLH OR INERTIAL. NEVER SYNTHESISED WITHOUT AN ATTITUDE SOURCE.",
        ],
        (Page::Gnc, 1) => &["ATTITUDE RATES AND ERRORS FROM TELEMETRY."],
        (Page::Gnc, _) => &["CMG MOMENTUM AND TORQUE FROM TELEMETRY."],
        (Page::Eps, 0) => &["SOLAR ARRAY BETA GIMBALS AND SARJ ANGLES FROM TELEMETRY."],
        (Page::Eps, _) => &["BATTERY AND BUS STATE FROM TELEMETRY."],
        (Page::Tcs, 0) => &["THERMAL LOOPS AND RADIATOR ROTATION FROM TELEMETRY."],
        (Page::Tcs, _) => &["CABIN ATMOSPHERE FROM TELEMETRY."],
        (Page::Comm, 0) => &["LINK STATE AND AOS/LOS FROM TELEMETRY."],
        (Page::Comm, _) => &["TDRS COVERAGE (v2)."],
        (Page::Pass, 0) => {
            &["POLAR AZ/EL PLOT FOR THE ACTIVE SITE: NEXT PASS, AOS/TCA/LOS, SUN, ILLUMINATION."]
        }
        (Page::Pass, 1) => &["PREDICTED PASSES FOR EACH SITE, 7 DAYS, WITH ELEVATION MASK."],
        (Page::Pass, _) => &["OBSERVER SITES: CONFIGURED, GEOCLUE, FALLBACK (NASA HQ)."],
        (Page::Prox, _) => &[
            "HILL-FRAME RELATIVE MOTION OF A VISITING VEHICLE: R-BAR/V-BAR, KEEP-OUT SPHERE, CORRIDOR.",
        ],
        (Page::View, _) => &["PERSPECTIVE VIEW FROM THE VEHICLE: LIMB, TERMINATOR, SUN AND MOON."],
        _ => &[],
    }
}

fn no_source(grid: &mut Grid, inp: &PageInput<'_>, reason: &str) -> Layout {
    let (top, h) = body(grid.rows);
    let rect = CellRect::new(0, top, grid.cols, h);
    boxed(grid, rect, &format!("{}  NO SOURCE", sub_title(inp)));
    let mut l = Lines::new(grid, rect);
    l.line("NO SOURCE", Style::new(Role::Dim));
    l.line(&reason.to_uppercase(), Style::new(Role::Dim));
    l.row += 1;
    l.line("THIS PAGE WILL SHOW:", Style::new(Role::Dim));
    for t in describe(inp.page, inp.sub) {
        l.line(t, Style::new(Role::Dim));
    }
    Layout::default()
}

fn help_overlay(grid: &mut Grid, k: &KeyMap) {
    let (cols, rows) = (grid.cols, grid.rows);
    let mut keys: Vec<String> = vec![
        "F1..F11        pages: MASTER TRAJ GNC EPS TCS COMM PASS PROX VIEW SYS OPS".into(),
        "TAB / SHIFT-TAB  next / previous sub-page".into(),
        "ARROWS         orbit camera".into(),
        "MOUSE          drag inside the ORBIT MAP to orbit; wheel there to zoom".into(),
    ];
    keys.extend(
        crate::keys::ACTIONS
            .iter()
            .map(|(a, _, text)| format!("{:<14} {text}", k.label(*a))),
    );
    keys.push(
        "(keys are remappable under [keys] in the config; F-keys, Tab, arrows are fixed)".into(),
    );
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    let w = 100.min(cols.saturating_sub(4));
    let h = (keys.len() + COMMANDS.len() + 7).min(rows.saturating_sub(4));
    if w < 20 || h < 6 {
        return;
    }
    let rect = CellRect::new((cols - w) / 2, (rows - h) / 2, w, h);
    let (solid_dim, solid_fg) = (Style::new(Role::Dim).solid(), Style::new(Role::Fg).solid());
    grid.fill(rect, solid_fg);
    grid.draw_box(
        rect,
        "HELP",
        Style::new(Role::Border).solid(),
        Style::new(Role::Bright).solid(),
    );
    let mut l = Lines::new(grid, rect);
    l.line("KEYS", Style::new(Role::Bright).solid());
    for k in keys {
        l.line(k, solid_fg);
    }
    l.row += 1;
    l.line(
        "COMMANDS (press : then type; TAB completes)",
        Style::new(Role::Bright).solid(),
    );
    for (_, usage) in COMMANDS {
        l.line(usage, solid_dim);
    }
}

fn is_l2(inp: &PageInput<'_>) -> bool {
    inp.vehicle
        .is_some_and(|v| v.regime == mctl_core::config::Regime::L2)
}

/// Draw the header, the current page, the footer, and the help overlay when open.
pub fn render(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    header(grid, inp);
    let mut layout = match availability(inp.page, inp.sub, inp.vehicle) {
        Availability::NoSource(why) => no_source(grid, inp, &why),
        Availability::Available => match (inp.page, inp.sub) {
            (Page::Master, _) if is_l2(inp) => crate::deep::master(grid, inp),
            (Page::Traj, 0) if is_l2(inp) => crate::deep::traj_orbit(grid, inp),
            (Page::Traj, 2) if is_l2(inp) => crate::deep::traj_series(grid, inp),
            (Page::Traj, 3) => crate::planner_page::planner(grid, inp),
            (Page::Ops, _) => crate::deep::ops(grid, inp),
            (Page::Master, _) => master(grid, inp),
            (Page::Traj, 0) => traj_orbit(grid, inp),
            (Page::Traj, 1) => traj_ground(grid, inp),
            (Page::Traj, _) => traj_elements(grid, inp),
            (Page::Pass, 0) => pass_sky(grid, inp),
            (Page::Pass, 1) => pass_list(grid, inp),
            (Page::Pass, _) => pass_sites(grid, inp),
            (Page::Gnc, 0) => gnc_adi(grid, inp),
            (Page::Gnc, 1) => group_table(
                grid,
                inp,
                "GNC  ATTITUDE ITEMS",
                "attitude",
                "ERROR AND RATE ITEMS ARE deg AND deg/s (CHECKED: docs/step3/attitude-verification.md), NOT THE RAD THE SPREADSHEET SAYS.",
            ),
            (Page::Gnc, _) => group_table(
                grid,
                inp,
                "GNC  CMG",
                "cmg",
                "THE PUBLIC FEED GIVES CMG MOMENTUM AS A MAGNITUDE ONLY; NO PER-AXIS MOMENTUM VECTOR IS PUBLISHED.",
            ),
            (Page::Eps, _) => group_table(
                grid,
                inp,
                "EPS  ARRAYS, SARJ, BGA",
                "eps",
                "PVCU ARRAY NAMES ARE AS THE SPREADSHEET PUBLISHES THEM; A REPEATED NAME SHOWS AS #2, NOT CORRECTED.",
            ),
            (Page::Tcs, 0) => group_table(grid, inp, "TCS  LOOPS AND COOLING", "tcs", ""),
            (Page::Tcs, _) => group_table(
                grid,
                inp,
                "ECLSS  ATMOSPHERE, WATER, O2",
                "eclss",
                "UNITS: THE SPREADSHEET SAYS PSI/CNT; THE VALUES ARE mmHg AND degC (docs/step3/attitude-verification.md).",
            ),
            (Page::Comm, _) => comm_links(grid, inp),
            (Page::Sys, 0) => sys_events(grid, inp),
            (Page::Sys, 1) => sys_sources(grid, inp),
            (Page::Sys, 2) => sys_settings(grid, inp),
            (Page::Sys, _) => sys_rules(grid, inp),
            (p, _) => no_source(grid, inp, &format!("{} is not implemented", p.title())),
        },
    };
    layout.legend = footer(grid, inp);
    layout.widgets = grid.take_widgets();
    if inp.help {
        help_overlay(grid, inp.keys);
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn met_formats_days_hours_minutes_seconds() {
        assert_eq!(fmt_met(0.0), "0000:00:00:00");
        assert_eq!(fmt_met(86_400.0 * 10_000.0 + 3661.0), "10000:01:01:01");
        assert_eq!(fmt_met(-1.0), "--");
        assert_eq!(fmt_met(f64::NAN), "--");
    }

    #[test]
    fn split_gives_the_remainder_to_the_last_part_and_never_loses_rows() {
        for total in [0, 1, 7, 42, 100] {
            let parts = split(total, &[34, 31, 35]);
            assert_eq!(parts.iter().sum::<usize>(), total);
        }
        assert_eq!(split(42, &[48, 33, 19]), vec![20, 13, 9]);
    }

    #[test]
    fn column_stacks_rectangles_without_gaps() {
        let c = column(5, 10, 1, &[3, 4, 2]);
        assert_eq!((c[0].row, c[1].row, c[2].row), (1, 4, 8));
        assert!(c.iter().all(|r| r.col == 5 && r.cols == 10));
    }
}
