//! PROX (R-bar/V-bar of visiting vehicles about the station) and SYS / VEHICLE (the art and the facts).

use mctl_core::prox::{ProxState, ProxView};
use mctl_core::world::{SourceStatus, VehicleView};

use crate::grid::{CellRect, Grid, Limit, PlotKind, Role, Style, Widget};
use crate::page::{Layout, Lines, PageInput, fmt_dur};
use crate::screens::{body, boxed, column, split};

fn name_of(inp: &PageInput<'_>, id: &str) -> String {
    inp.world.vehicles.iter().find(|v| v.id == id).map_or_else(
        || id.to_uppercase(),
        |v| v.resolved_name.clone().unwrap_or_else(|| v.name.clone()),
    )
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match out.last_mut() {
            Some(l) if l.chars().count() + 1 + word.chars().count() <= width => {
                l.push(' ');
                l.push_str(word);
            }
            _ => out.push(word.chars().take(width).collect()),
        }
    }
    out
}

fn view_lines(l: &mut Lines<'_>, inp: &PageInput<'_>, v: &ProxView) {
    let (dim, fg, bright) = (
        Style::new(Role::Dim),
        Style::new(Role::Fg),
        Style::new(Role::Bright),
    );
    let state_style = match v.state {
        ProxState::Docked => bright,
        ProxState::Near => Style::limit(Limit::Caution),
        ProxState::Far => dim,
    };
    l.put(1, &name_of(inp, &v.chaser), bright);
    l.put(
        30.min(l.rect.cols.saturating_sub(8)),
        v.state.label(),
        state_style,
    );
    l.row += 1;
    if v.state == ProxState::Docked {
        l.line(
            &format!(
                "SAME ELEMENT SET AS {}: NO RELATIVE MOTION",
                name_of(inp, &v.target).to_uppercase()
            ),
            dim,
        );
    } else {
        l.pair(
            "V-BAR",
            &format!("{:>+10.3}", v.rel_lvlh_km[0]),
            "km along-track",
            fg,
        );
        l.pair(
            "H-BAR",
            &format!("{:>+10.3}", v.rel_lvlh_km[1]),
            "km out of plane",
            fg,
        );
        l.pair(
            "R-BAR",
            &format!("{:>+10.3}", v.rel_lvlh_km[2]),
            "km (+ toward Earth)",
            fg,
        );
        l.pair("RANGE", &format!("{:>10.3}", v.range_km), "km", fg);
        l.pair(
            "RATE",
            &format!("{:>+10.2}", v.range_rate_km_s * 1000.0),
            "m/s (+ opening)",
            fg,
        );
    }
    let w = l.rect.cols.saturating_sub(2).max(8);
    for line in wrap(&v.source_note, w) {
        l.line(&line, dim);
    }
    l.row += 1;
}

pub(crate) fn prox(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let id = inp.vehicle.map(|v| v.id.as_str()).unwrap_or_default();
    let views: Vec<&ProxView> = inp
        .world
        .prox
        .iter()
        .filter(|p| p.chaser == id || p.target == id)
        .map(|p| &**p)
        .collect();
    let lw = cols * 62 / 100;
    let plot = CellRect::new(0, top, lw, h);
    boxed(grid, plot, "R-BAR / V-BAR  PAST SOLID, PREDICTED DASHED");
    let right = column(lw, cols - lw, top, &split(h, &[58, 42]));
    boxed(
        grid,
        right[0],
        &format!("RELATIVE STATE  {}", crate::flags::TLE),
    );
    {
        let mut l = Lines::new(grid, right[0]);
        if views.is_empty() {
            l.no_data("NO RELATIVE STATE YET: A VEHICLE HAS NO SOLUTION");
        }
        for v in &views {
            view_lines(&mut l, inp, v);
        }
    }
    if !views.is_empty() {
        grid.widget(Widget::Plot {
            rect: plot.inner(),
            kind: PlotKind::Prox,
        });
    }
    // Art: the target and the first visitor, side by side.
    boxed(grid, right[1], "VEHICLES");
    let inner = right[1].inner();
    let half = inner.cols / 2;
    let art = |grid: &mut Grid, rect: CellRect, id: &str, name: String| {
        grid.widget(Widget::Art {
            rect: CellRect::new(rect.col, rect.row, rect.cols, rect.rows.saturating_sub(1)),
            id: id.to_owned(),
        });
        grid.put(
            rect.col + 1,
            rect.row + rect.rows.saturating_sub(1),
            &name
                .chars()
                .take(rect.cols.saturating_sub(2))
                .collect::<String>(),
            Style::new(Role::Dim),
        );
    };
    let target = views
        .first()
        .map_or_else(|| id.to_owned(), |v| v.target.clone());
    let chaser = views
        .first()
        .map_or_else(|| id.to_owned(), |v| v.chaser.clone());
    art(
        grid,
        CellRect::new(inner.col, inner.row, half, inner.rows),
        &target,
        name_of(inp, &target),
    );
    art(
        grid,
        CellRect::new(inner.col + half, inner.row, inner.cols - half, inner.rows),
        &chaser,
        name_of(inp, &chaser),
    );
    Layout::default()
}

fn source_line(s: &mctl_core::world::SourceView) -> String {
    let status = match &s.status {
        SourceStatus::Active => "ACTIVE",
        SourceStatus::Standby => "STANDBY",
        SourceStatus::Stale => "STALE",
        SourceStatus::OutOfSpan => "OUT OF SPAN",
        SourceStatus::NoData => "NO DATA",
        SourceStatus::Failed(_) => "FAILED",
    };
    format!(
        "{:<9} {:<11} AGE {}",
        s.kind.label(),
        status,
        s.age_s.map_or("--".to_owned(), |a| fmt_dur(a.abs()))
    )
}

fn facts(l: &mut Lines<'_>, v: &VehicleView, now: mctl_core::time::Utc) {
    let (fg, bright) = (Style::new(Role::Fg), Style::new(Role::Bright));
    l.line(
        &v.resolved_name.clone().unwrap_or_else(|| v.name.clone()),
        bright,
    );
    l.line(&format!("ID           {}", v.id), fg);
    l.line(
        &format!(
            "ORBIT        {}",
            match v.regime {
                mctl_core::config::Regime::Earth => "EARTH ORBIT",
                mctl_core::config::Regime::L2 => "SUN-EARTH L2 (HALO)",
            }
        ),
        fg,
    );
    if v.norad != 0 {
        l.line(&format!("NORAD        {}", v.norad), fg);
    }
    if let Some(e) = v.met_epoch {
        l.line(&format!("LAUNCH       {}", &e.iso()[..19]), fg);
        l.line(
            &format!("MET          {}", crate::screens::fmt_met(now.minus(e))),
            fg,
        );
    }
    l.row += 1;
    l.line("TRAJECTORY SOURCES", bright);
    for s in &v.sources {
        l.line(&source_line(s), fg);
    }
    l.row += 1;
    l.line(
        &format!(
            "TELEMETRY    {}",
            v.telemetry
                .as_deref()
                .map_or("NONE PUBLIC".to_owned(), str::to_uppercase)
        ),
        fg,
    );
    l.line(
        &format!(
            "SCHEDULE     {}",
            v.schedule
                .as_deref()
                .map_or("NONE FOUND".to_owned(), str::to_uppercase)
        ),
        fg,
    );
    if let Some(t) = &v.visits {
        l.line(&format!("VISITS       {}", t.to_uppercase()), fg);
    }
    if !v.visitors.is_empty() {
        l.line(
            &format!("VISITORS     {}", v.visitors.join(" ").to_uppercase()),
            fg,
        );
    }
}

pub(crate) fn vehicle_page(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let lw = cols * 62 / 100;
    let art = CellRect::new(0, top, lw, h);
    boxed(
        grid,
        art,
        &inp.vehicle.map_or("VEHICLE".to_owned(), |v| v.name.clone()),
    );
    let facts_rect = CellRect::new(lw, top, cols - lw, h);
    boxed(grid, facts_rect, "VEHICLE FACTS");
    match inp.vehicle {
        None => Lines::new(grid, facts_rect).no_data("NO VEHICLE"),
        Some(v) => {
            if crate::art::model(&v.id).is_some() {
                grid.widget(Widget::Art {
                    rect: art.inner(),
                    id: v.id.clone(),
                });
            } else {
                Lines::new(grid, art).no_data("NO ARTWORK FOR THIS VEHICLE");
            }
            let mut l = Lines::new(grid, facts_rect);
            facts(&mut l, v, inp.world.t);
        }
    }
    Layout::default()
}

/// SYS / GALLERY: the line art that is not tied to a configured vehicle, all turning together.
pub(crate) fn gallery(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let _ = inp;
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let per_row = 3;
    let lines = crate::art::EXTRAS.len().div_ceil(per_row);
    let (tile_w, tile_h) = (cols / per_row, h / lines.max(1));
    for (i, e) in crate::art::EXTRAS.iter().enumerate() {
        let rect = CellRect::new(
            (i % per_row) * tile_w,
            top + (i / per_row) * tile_h,
            tile_w,
            tile_h,
        );
        boxed(grid, rect, e.title);
        let inner = rect.inner();
        grid.widget(Widget::Art {
            rect: CellRect::new(
                inner.col,
                inner.row,
                inner.cols,
                inner.rows.saturating_sub(1),
            ),
            id: e.id.to_owned(),
        });
        grid.put(
            inner.col + 1,
            inner.row + inner.rows.saturating_sub(1),
            &e.caption
                .chars()
                .take(inner.cols.saturating_sub(2))
                .collect::<String>(),
            Style::new(Role::Dim),
        );
    }
    Layout::default()
}
