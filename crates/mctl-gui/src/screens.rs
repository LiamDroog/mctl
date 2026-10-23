//! Page composition: the header, footer, every page and sub-page of the page map (DESIGN.md §9.4),
//! the greyed `NO SOURCE` page and the help overlay. Panels are small functions so pages are layouts.

use mctl_core::time::Utc;
use mctl_core::world::{ClockMode, EopView, SourceStatus, SourceView};

use crate::commands::COMMANDS;
use crate::grid::{CellRect, Grid, Limit, Role, Style};
use crate::page::{
    Layout, Lines, MIN_COLS, MIN_ROWS, PageInput, cw_lines, fmt_dur, quality_text, source_row,
    upcoming_events, worst,
};
use crate::pages::{ALL_PAGES, Availability, Page, availability};

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
        format!(
            "{} / {}",
            inp.page.title(),
            subs[inp.sub.min(subs.len() - 1)]
        )
    } else {
        inp.page.title().to_owned()
    }
}

/// Row 0: vehicle, page, GMT, MET, clock, source and age, and the master annunciator (DESIGN.md §9.3).
fn header(grid: &mut Grid, inp: &PageInput<'_>) {
    let cols = grid.cols;
    let (bright, _dim) = (Style::new(Role::Bright), Style::new(Role::Dim));
    let cw = cw_lines(inp.world, inp.vehicle, inp.limits);
    let severity = worst(&cw);
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
        .map_or(String::new(), |a| format!(" AGE {}", fmt_dur(a.abs())));
    let met = inp
        .vehicle
        .and_then(|v| v.met_epoch)
        .map_or("--".to_owned(), |e| fmt_met(inp.world.t.minus(e)));
    let annunciator = match severity {
        Some(Limit::Warning) => " WARN ",
        Some(Limit::Caution) => " CAUT ",
        Some(Limit::Stale) => " STALE ",
        None => " NOM ",
    };
    let budget = cols.saturating_sub(annunciator.len() + 1);
    let (clock, clock_style) = clock_text(inp);
    let before = format!(
        " {name} ({norad}) | {} | GMT {} | MET {met} | ",
        sub_title(inp),
        inp.world.t.gmt_doy()
    );
    let after = format!(" | SRC {src}{age}");
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
        severity.map_or(Style::new(Role::Nominal), Style::limit),
    );
}

/// Page-specific key hints for the footer's left side.
fn page_hints(page: Page, sub: usize) -> &'static str {
    match (page, sub) {
        (Page::Master | Page::Traj, 0) | (Page::Master, _) => {
            "ARROWS/DRAG ORBIT +/- ZOOM Z ALT F CAM C FOCUS X FRAME"
        }
        (Page::Traj, 1) => "X FRAME  TAB SUBPAGE",
        (Page::Sys, _) => "TAB SUBPAGE",
        _ => "TAB SUBPAGE",
    }
}

/// Last two rows: stats and hints (or the status message), then the F-key legend or the command line.
fn footer(grid: &mut Grid, inp: &PageInput<'_>) {
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
        let common = "SPACE PAUSE [ ] WARP 0 LIVE T THEME V VEHICLE";
        (format!("{}  {common}", page_hints(inp.page, inp.sub)), dim)
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
        return;
    }
    let mut col = 1;
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
        col += text.chars().count() + 1;
    }
    let hint = ": CMD  ? HELP  Q QUIT";
    grid.put(cols.saturating_sub(hint.len() + 1), last, hint, dim);
}

// ---- panels --------------------------------------------------------------------------------------------

fn boxed(grid: &mut Grid, rect: CellRect, title: &str) {
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

fn state_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
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
    if !inp.world.clock.is_live() {
        l.line("NOT LIVE", Style::limit(Limit::Caution));
    }
    l.no_data(match availability(Page::Gnc, 0, inp.vehicle) {
        Availability::NoSource(why) => {
            Box::leak(format!("NO SOURCE: {}", why.to_uppercase()).into_boxed_str())
        }
        Availability::Available => "",
    });
}

fn cw_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
    let dim = Style::new(Role::Dim);
    let cw = cw_lines(inp.world, inp.vehicle, inp.limits);
    boxed(grid, rect, "CAUTION & WARNING  SOURCE HEALTH");
    let mut l = Lines::new(grid, rect);
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
}

fn sources_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
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

/// Placeholder panel for data that does not exist yet; always says what and when, never invents content.
fn pending_panel(grid: &mut Grid, rect: CellRect, title: &str, lines: &[&str]) {
    boxed(grid, rect, title);
    let mut l = Lines::new(grid, rect);
    l.line("NO DATA", Style::new(Role::Dim));
    for t in lines {
        l.line(t, Style::new(Role::Dim));
    }
}

// ---- pages ---------------------------------------------------------------------------------------------

/// (row, height) of the body between the header and the two footer rows.
fn body(rows: usize) -> (usize, usize) {
    (1, rows.saturating_sub(3))
}

/// Split `total` into parts by percentage, giving the remainder to the last part.
fn split(total: usize, pcts: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = pcts.iter().map(|p| total * p / 100).collect();
    if let Some((last, rest)) = out.split_last_mut() {
        *last = total.saturating_sub(rest.iter().sum::<usize>());
    }
    out
}

fn column(x: usize, w: usize, top: usize, heights: &[usize]) -> Vec<CellRect> {
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
    let lw = cols * 30 / 100;
    let left = column(0, lw, top, &split(h, &[45, 55]));
    elements_panel(grid, left[0], inp);
    state_panel(grid, left[1], inp);
    pending_panel(
        grid,
        CellRect::new(lw, top, cols - lw, h),
        "TRENDS  SMA / ALTITUDE / ECC / INC / RAAN / ARGP / BETA / SOURCE DIFFERENCE",
        &[
            "",
            "NEEDS THE RECORDING STORE (SQLITE), WHICH ARRIVES IN M3.",
            "",
            "WHEN PRESENT: WINDOW 6 H .. 90 D, ELEMENT-SET EPOCHS MARKED SO REBOOSTS SHOW AS STEPS (DESIGN.md 9.6).",
        ],
    );
    Layout::default()
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
        (Page::Traj, 3) => &[
            "BURN TABLE: TRIGGER, EPOCH, FRAME (VNB/RTN), DELTA-V.",
            "FORCE-MODEL TOGGLES, HORIZON, BALLISTIC COEFFICIENT.",
            "RESULTS: DELTA ELEMENTS, NEW HA/HP, DELTA PERIOD, DELTA PHASE, PASS SHIFTS.",
        ],
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

fn help_overlay(grid: &mut Grid) {
    let (cols, rows) = (grid.cols, grid.rows);
    let keys: &[&str] = &[
        "F1..F10        pages: MASTER TRAJ GNC EPS TCS COMM PASS PROX VIEW SYS",
        "TAB / SHIFT-TAB  next / previous sub-page",
        "V              next vehicle              T   next theme",
        "SPACE          pause / resume            [ ]  halve / double warp",
        "0              return to LIVE            :   command line",
        "ARROWS         orbit camera              +/-  zoom",
        "MOUSE          drag inside the ORBIT MAP to orbit; wheel there to zoom",
        "Z alt x1/2/5/10  F inertial/Earth-fixed  C focus Earth/vehicle  X GCRF/ITRF",
        "1 2 3 4        scanlines bloom persistence CRT      E  all effects",
        "?              close this help           Q   quit",
    ];
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

/// Draw the header, the current page, the footer, and the help overlay when open.
pub fn render(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    header(grid, inp);
    let layout = match availability(inp.page, inp.sub, inp.vehicle) {
        Availability::NoSource(why) => no_source(grid, inp, &why),
        Availability::Available => match (inp.page, inp.sub) {
            (Page::Master, _) => master(grid, inp),
            (Page::Traj, 0) => traj_orbit(grid, inp),
            (Page::Traj, 1) => traj_ground(grid, inp),
            (Page::Traj, _) => traj_elements(grid, inp),
            (Page::Sys, 0) => sys_events(grid, inp),
            (Page::Sys, 1) => sys_sources(grid, inp),
            (Page::Sys, _) => sys_settings(grid, inp),
            (p, _) => no_source(grid, inp, &format!("{} is not implemented", p.title())),
        },
    };
    footer(grid, inp);
    if inp.help {
        help_overlay(grid);
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
