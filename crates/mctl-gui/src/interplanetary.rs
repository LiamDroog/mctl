//! Pages for vehicles on escape trajectories (Voyager 1 and 2, New Horizons).
//!
//! Their Earth-centred Kepler elements are meaningless, so the pages show the path among the planets'
//! orbits, distance from the Sun and light time to Earth, and the direction of travel on the ecliptic.
//! Everything comes from the engine's snapshot of the Horizons table.

use crate::grid::{CellRect, DeepSeries, Grid, PlotKind, Role, Style};
use crate::page::{Layout, Lines, PageInput, fmt_dur, quality_text};
use crate::screens::{body, boxed, column, cw_panel, sources_panel, split};
use mctl_astro::interplanetary::KM_PER_AU;

use crate::deep::plot;

/// Distance from the Sun (AU) for a full gauge: a little past the heliopause, where both Voyagers are.
const SUN_BAR_FULL_AU: f64 = 200.0;

/// Sun and Earth distances, light time, speed and the heliocentric direction.
pub(crate) fn state_panel(grid: &mut Grid, rect: CellRect, inp: &PageInput<'_>) {
    let fg = Style::new(Role::Fg);
    let sol = inp.vehicle.and_then(|v| v.solution.as_ref());
    let quality = sol.map_or("NO DATA".to_owned(), quality_text);
    boxed(grid, rect, &format!("HELIOCENTRIC STATE  {quality}"));
    let mut l = Lines::new(grid, rect);
    let Some(d) = inp.vehicle.and_then(|v| v.deep) else {
        l.no_data("NO SOURCE HAS DELIVERED A STATE");
        return;
    };
    let wide = l.rect.cols.saturating_sub(34).clamp(0, 24);
    let sun_au = d.sun_dist_km / KM_PER_AU;
    let lon = d.rot_km[1]
        .atan2(d.rot_km[0])
        .to_degrees()
        .rem_euclid(360.0);
    let rows = [
        (
            "SUN",
            format!("{sun_au:>13.3}"),
            "AU from the Sun",
            Some((sun_au / SUN_BAR_FULL_AU) as f32),
        ),
        (
            "EARTH",
            format!("{:>13.3}", d.dist_km / KM_PER_AU),
            "AU from Earth",
            None,
        ),
        (
            "LIGHT",
            format!("{:>13}", fmt_dur(d.light_time_s)),
            "one-way to Earth",
            None,
        ),
        (
            "SPEED",
            format!("{:>13.3}", d.speed_km_s),
            "km/s about the Sun",
            None,
        ),
        (
            "RATE",
            format!("{:>13.3}", d.range_rate_km_s),
            "km/s from Earth (+ away)",
            None,
        ),
        (
            "LON",
            format!("{lon:>13.2}"),
            "deg ecliptic longitude",
            None,
        ),
        (
            "LAT",
            format!("{:>13.2}", d.off_axis_deg),
            "deg ecliptic latitude",
            None,
        ),
    ];
    for (lab, val, unit, bar) in rows {
        l.pair(lab, &val, unit, fg);
        if let (Some(f), true) = (bar, wide >= 8) {
            l.bar(8, wide, f, None);
            l.row += 1;
        }
    }
    if let Some(s) = sol {
        l.line(
            &format!("SOURCE {}  JPL HORIZONS, ICRF", s.source.label()),
            Style::new(Role::Dim),
        );
    }
}

pub(crate) fn master(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 56 / 100;
    plot(
        grid,
        CellRect::new(0, top, lw, h),
        "SOLAR SYSTEM FROM ECLIPTIC NORTH  PAST SOLID / PREDICTED DASHED",
        PlotKind::SolarSystem,
    );
    let right = column(lw, cols - lw, top, &split(h, &[44, 22, 34]));
    state_panel(grid, right[0], inp);
    cw_panel(grid, right[1], inp);
    sources_panel(grid, right[2], inp);
    Layout::default()
}

pub(crate) fn traj_orbit(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 64 / 100;
    plot(
        grid,
        CellRect::new(0, top, lw, h),
        "SOLAR SYSTEM FROM ECLIPTIC NORTH  RADIUS GOES AS SQRT OF DISTANCE",
        PlotKind::SolarSystem,
    );
    let right = column(lw, cols - lw, top, &split(h, &[62, 38]));
    state_panel(grid, right[0], inp);
    sources_panel(grid, right[1], inp);
    Layout::default()
}

pub(crate) fn traj_series(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 74 / 100;
    let left = column(0, lw, top, &split(h, &[50, 50]));
    let half = lw / 2;
    let cells = [
        ("DISTANCE FROM THE SUN", DeepSeries::SunDistance),
        ("LIGHT TIME TO EARTH", DeepSeries::EarthLight),
        ("ECLIPTIC LONGITUDE", DeepSeries::EclLon),
        ("ECLIPTIC LATITUDE", DeepSeries::EclLat),
    ];
    for (i, (title, kind)) in cells.into_iter().enumerate() {
        let band = left[i / 2];
        let r = CellRect::new(
            (i % 2) * half,
            band.row,
            if i % 2 == 0 { half } else { lw - half },
            band.rows,
        );
        plot(grid, r, title, PlotKind::DeepSeries(kind));
    }
    let right = column(lw, cols - lw, top, &split(h, &[55, 45]));
    state_panel(grid, right[0], inp);
    sources_panel(grid, right[1], inp);
    Layout::default()
}
