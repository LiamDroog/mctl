//! Pages for vehicles at Sun-Earth L2 (JWST, Roman) and the OPS schedule page.
//!
//! An Earth-centred Kepler orbit means nothing 1.5 million km out, so these vehicles get the halo
//! orbit seen from Earth and from above the ecliptic, distance and light time, and (for JWST) the
//! visit schedule. Everything is drawn from the engine's snapshot.

use mctl_core::schedule::{HstProgram, JwstSchedule, Ops};
use mctl_core::time::Utc;
use mctl_core::world::{Health, VehicleView};

use crate::grid::{CellRect, DeepSeries, Grid, Limit, PlotKind, Role, Style, Widget};
use crate::page::{Layout, Lines, PageInput, fmt_dur, quality_text};
use crate::screens::{body, boxed, column, cw_panel, sources_panel, split};

/// Largest distance (km) for which a bar is full: Sun-Earth L2 sits at about 1.5 million km and the
/// halo reaches somewhat beyond it. Used only to scale the gauge, not as a limit.
const DISTANCE_BAR_FULL_KM: f64 = 2.0e6;
/// Off-axis angle (deg) for a full gauge; JWST's halo reaches about 30 degrees from Earth.
const OFF_AXIS_BAR_FULL_DEG: f64 = 45.0;

fn plot(grid: &mut Grid, rect: CellRect, title: &str, kind: PlotKind) {
    boxed(grid, rect, title);
    grid.widget(Widget::Plot {
        rect: rect.inner(),
        kind,
    });
}

/// Distance, light time, speed and halo coordinates.
pub(crate) fn state_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
    let fg = Style::new(Role::Fg);
    let sol = inp.vehicle.and_then(|v| v.solution.as_ref());
    let quality = sol.map_or("NO DATA".to_owned(), quality_text);
    boxed(grid, rect, &format!("L2 STATE (EARTH-CENTRED)  {quality}"));
    let mut l = Lines::new(grid, rect);
    let Some(d) = inp.vehicle.and_then(|v| v.deep) else {
        l.no_data("NO SOURCE HAS DELIVERED A STATE");
        return;
    };
    let wide = l.rect.cols.saturating_sub(34).clamp(0, 24);
    let rows = [
        (
            "DIST",
            format!("{:>13.0}", d.dist_km),
            "km",
            Some((d.dist_km / DISTANCE_BAR_FULL_KM) as f32),
        ),
        (
            "LIGHT",
            format!("{:>13.3}", d.light_time_s),
            "s one-way",
            None,
        ),
        ("SPEED", format!("{:>13.4}", d.speed_km_s), "km/s", None),
        (
            "RATE",
            format!("{:>13.1}", d.range_rate_km_s * 1000.0),
            "m/s range rate (+ away)",
            None,
        ),
        (
            "ANGLE",
            format!("{:>13.2}", d.off_axis_deg),
            "deg off the anti-Sun line",
            Some((d.off_axis_deg / OFF_AXIS_BAR_FULL_DEG) as f32),
        ),
        (
            "X",
            format!("{:>13.0}", d.rot_km[0]),
            "km beyond Earth",
            None,
        ),
        ("Y", format!("{:>13.0}", d.rot_km[1]), "km prograde", None),
        ("Z", format!("{:>13.0}", d.rot_km[2]), "km ecliptic N", None),
    ];
    for (lab, val, unit, bar) in rows {
        l.pair(lab, &val, unit, fg);
        if let (Some(f), true) = (bar, wide >= 8) {
            // The gauge goes on its own row under the value so it never overlaps the unit text.
            l.bar(8, wide, f, None);
            l.row += 1;
        }
    }
    if let Some(s) = sol {
        l.line(
            &format!("SOURCE {}  ICRF, KM", s.source.label()),
            Style::new(Role::Dim),
        );
    }
}

fn ops_one_liner(l: &mut Lines<'_>, v: &VehicleView, now: Utc) {
    let dim = Style::new(Role::Dim);
    match v.ops.as_ref() {
        None => match &v.ops_health {
            Some(Health::Failing(why)) => {
                l.line("SCHEDULE FEED FAILING", Style::limit(Limit::Caution));
                l.line(&why.chars().take(60).collect::<String>(), dim);
            }
            Some(Health::Offline) => l.no_data("OFFLINE, NOTHING CACHED"),
            _ => l.no_data("SCHEDULE NOT YET LOADED"),
        },
        Some(Ops::Jwst(s)) => match s.current(now) {
            Some(c) => {
                l.line(
                    &format!("NOW  {}  {}", c.instrument, c.target),
                    Style::new(Role::Bright),
                );
                let done = ((now.0 - c.start.0) / c.duration_s).clamp(0.0, 1.0) as f32;
                l.bar(1, l.rect.cols.saturating_sub(12).min(30), done, None);
                l.put(
                    l.rect.cols.saturating_sub(10).min(32),
                    &format!("{:>3.0}%", done * 100.0),
                    dim,
                );
                l.row += 1;
                if let Some(n) = s.upcoming(now).next() {
                    l.line(
                        &format!("NEXT T-{}  {}", fmt_dur(n.start.minus(now)), n.target),
                        Style::new(Role::Fg),
                    );
                }
            }
            None => l.line("NO VISIT IN PROGRESS IN THE PLAN", dim),
        },
        Some(Ops::Hst(r)) => {
            l.line(
                &format!(
                    "REPORT #{}  {} SCIENCE / {} CAL / {} OTHER",
                    r.number,
                    r.science.len(),
                    r.calibration.len(),
                    r.other.len()
                ),
                Style::new(Role::Fg),
            );
            if let Some(p) = r.science.first() {
                l.line(&format!("{} {}", p.id, p.title), dim);
            }
        }
    }
}

fn ops_summary_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
    boxed(grid, rect, "OBSERVING  (PLAN, NOT TELEMETRY)");
    let mut l = Lines::new(grid, rect);
    match inp.vehicle {
        Some(v) if v.schedule.is_some() => ops_one_liner(&mut l, v, inp.world.t),
        _ => l.no_data("NO PUBLIC OBSERVING SCHEDULE FOUND"),
    }
}

pub(crate) fn master(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 50 / 100;
    let left = column(0, lw, top, &split(h, &[58, 42]));
    plot(
        grid,
        left[0],
        "HALO ORBIT  VIEW FROM EARTH ALONG THE SUN-EARTH LINE",
        PlotKind::Halo,
    );
    plot(
        grid,
        left[1],
        "FROM ECLIPTIC NORTH  PAST SOLID / PREDICTED DASHED",
        PlotKind::TopView,
    );
    let right = column(lw, cols - lw, top, &split(h, &[36, 20, 22, 22]));
    state_panel(grid, right[0], inp);
    ops_summary_panel(grid, right[1], inp);
    cw_panel(grid, right[2], inp);
    sources_panel(grid, right[3], inp);
    Layout::default()
}

pub(crate) fn traj_orbit(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 62 / 100;
    plot(
        grid,
        CellRect::new(0, top, lw, h),
        "HALO ORBIT  VIEW FROM EARTH  PAST SOLID / PREDICTED DASHED  TICKS 30 D",
        PlotKind::Halo,
    );
    let right = column(lw, cols - lw, top, &split(h, &[38, 36, 26]));
    plot(grid, right[0], "FROM ECLIPTIC NORTH", PlotKind::TopView);
    state_panel(grid, right[1], inp);
    sources_panel(grid, right[2], inp);
    Layout::default()
}

pub(crate) fn traj_series(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 74 / 100;
    let left = column(0, lw, top, &split(h, &[50, 50]));
    let half = lw / 2;
    for (i, (title, kind)) in [
        ("DISTANCE", DeepSeries::Distance),
        ("OFF-AXIS ANGLE", DeepSeries::OffAxis),
    ]
    .into_iter()
    .enumerate()
    {
        let r = CellRect::new(
            i * half,
            left[0].row,
            if i == 0 { half } else { lw - half },
            left[0].rows,
        );
        plot(grid, r, title, PlotKind::DeepSeries(kind));
    }
    for (i, (title, kind)) in [
        ("HALO OFFSETS", DeepSeries::Offsets),
        ("RANGE RATE", DeepSeries::RangeRate),
    ]
    .into_iter()
    .enumerate()
    {
        let r = CellRect::new(
            i * half,
            left[1].row,
            if i == 0 { half } else { lw - half },
            left[1].rows,
        );
        plot(grid, r, title, PlotKind::DeepSeries(kind));
    }
    let right = column(lw, cols - lw, top, &split(h, &[55, 45]));
    state_panel(grid, right[0], inp);
    sources_panel(grid, right[1], inp);
    Layout::default()
}

// ---- OPS --------------------------------------------------------------------------------------------

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n.saturating_sub(1)).chain(['~']).collect()
    }
}

fn feed_line(l: &mut Lines<'_>, inp: &PageInput<'_>, fetched: Utc, what: &str) {
    let dim = Style::new(Role::Dim);
    let health = match inp.vehicle.and_then(|v| v.ops_health.as_ref()) {
        Some(Health::Ok) => "FEED OK".to_owned(),
        Some(Health::Failing(_)) => "FEED FAILING".to_owned(),
        Some(Health::Offline) => "OFFLINE (CACHE)".to_owned(),
        _ => "STARTING".to_owned(),
    };
    let style = if matches!(
        inp.vehicle.and_then(|v| v.ops_health.as_ref()),
        Some(Health::Failing(_))
    ) {
        Style::limit(Limit::Caution)
    } else {
        dim
    };
    l.line(
        &format!(
            "{what}  {health}  FETCHED {} AGO",
            fmt_dur(inp.world.t.minus(fetched).abs())
        ),
        style,
    );
}

fn visit_table(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>, s: &JwstSchedule) {
    boxed(grid, rect, "VISITS  IN PROGRESS AND NEXT");
    let mut l = Lines::new(grid, rect);
    let (dim, fg) = (Style::new(Role::Dim), Style::new(Role::Fg));
    let now = inp.world.t;
    feed_line(&mut l, inp, s.fetched_at, "STSCI WEEKLY SCHEDULE");
    l.line("TIMES ARE THE PLAN; EXECUTED OBSERVATIONS CAN DIFFER (UNPLANNED EVENTS, TARGETS OF OPPORTUNITY).", dim);
    l.row += 1;
    l.put(
        1,
        "  GMT START      DUR     INSTRUMENT / MODE                      TARGET",
        dim,
    );
    l.row += 1;
    let target_col = 64usize;
    let tw = l.rect.cols.saturating_sub(target_col + 1);
    let current = s.current(now).cloned();
    let rows_left = l.rect.rows.saturating_sub(l.row);
    let visits = current.iter().chain(s.upcoming(now)).take(rows_left);
    for (i, v) in visits.enumerate() {
        let is_now = i == 0 && current.is_some();
        let style = if is_now { Style::new(Role::Bright) } else { fg };
        let par = if v.parallels.is_empty() {
            String::new()
        } else {
            format!(" +{}", v.parallels.len())
        };
        let what = if v.is_maintenance() {
            v.kind.clone()
        } else {
            format!("{}{par}", v.instrument)
        };
        l.put(
            1,
            &format!(
                "{} {} {:>7}  {}",
                if is_now { ">" } else { " " },
                &v.start.gmt_doy()[..12],
                fmt_dur(v.duration_s),
                truncate(&what, 38),
            ),
            style,
        );
        l.put(target_col, &truncate(&v.target, tw), style);
        l.row += 1;
    }
}

fn program_list(grid: &mut Grid, rect: CellRect, title: &str, programs: &[HstProgram]) {
    boxed(grid, rect, &format!("{title}  {}", programs.len()));
    let mut l = Lines::new(grid, rect);
    let (dim, fg) = (Style::new(Role::Dim), Style::new(Role::Fg));
    if programs.is_empty() {
        l.line("NONE IN THIS REPORT", dim);
        return;
    }
    let w = l.rect.cols.saturating_sub(8);
    for p in programs {
        if l.row + 1 >= l.rect.rows {
            l.line(&format!("... {} MORE", programs.len()), dim);
            break;
        }
        l.put(1, &p.id, Style::new(Role::Bright));
        l.put(7, &truncate(&p.title, w), fg);
        l.row += 1;
        l.put(7, &truncate(&p.investigator, w), dim);
        l.row += 1;
    }
}

pub(crate) fn ops(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let all = CellRect::new(0, top, cols, h);
    match inp.vehicle.and_then(|v| v.ops.as_ref()) {
        Some(Ops::Jwst(s)) => {
            let split_rows = column(0, cols, top, &split(h, &[42, 58]));
            plot(
                grid,
                split_rows[0],
                "TIMELINE  -6 H TO +42 H  BY INSTRUMENT  (PLAN)",
                PlotKind::Timeline,
            );
            let lw = cols * 66 / 100;
            visit_table(
                grid,
                CellRect::new(0, split_rows[1].row, lw, split_rows[1].rows),
                inp,
                s,
            );
            plot(
                grid,
                CellRect::new(lw, split_rows[1].row, cols - lw, split_rows[1].rows),
                "NEXT 7 DAYS BY INSTRUMENT",
                PlotKind::InstrumentShare,
            );
        }
        Some(Ops::Hst(r)) => {
            let head = CellRect::new(0, top, cols, 5);
            boxed(grid, head, "HUBBLE DAILY STATUS REPORT");
            let mut l = Lines::new(grid, head);
            l.line(
                &format!("REPORT #{}   PERIOD {}", r.number, r.period),
                Style::new(Role::Bright),
            );
            let fetched = r.fetched_at;
            feed_line(&mut l, inp, fetched, "STSCI DAILY REPORT");
            l.line(
                "PROGRAMS SCHEDULED IN THE PERIOD; THE REPORT GIVES NO VISIT TIMES AND NO ENGINEERING TELEMETRY.",
                Style::new(Role::Dim),
            );
            let lists = CellRect::new(0, top + 5, cols, h - 5);
            let w = lists.cols / 3;
            program_list(
                grid,
                CellRect::new(0, lists.row, w, lists.rows),
                "SCIENCE",
                &r.science,
            );
            program_list(
                grid,
                CellRect::new(w, lists.row, w, lists.rows),
                "CALIBRATION",
                &r.calibration,
            );
            program_list(
                grid,
                CellRect::new(2 * w, lists.row, cols - 2 * w, lists.rows),
                "OTHER",
                &r.other,
            );
        }
        None => {
            boxed(grid, all, "OBSERVING SCHEDULE");
            let mut l = Lines::new(grid, all);
            match inp.vehicle.and_then(|v| v.ops_health.as_ref()) {
                Some(Health::Failing(why)) => {
                    l.line(
                        "SCHEDULE FEED FAILING, NOTHING CACHED",
                        Style::limit(Limit::Caution),
                    );
                    l.line(why, Style::new(Role::Dim));
                }
                Some(Health::Offline) => l.no_data("OFFLINE AND NOTHING CACHED"),
                _ => l.no_data("SCHEDULE NOT YET LOADED"),
            }
        }
    }
    Layout::default()
}
