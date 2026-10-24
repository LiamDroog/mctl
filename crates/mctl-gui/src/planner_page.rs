//! TRAJ / MANEUVER PLANNER (DESIGN.md 9.6 F2.4): the burn table, force-model controls, results and the
//! plan plots. The plan itself lives in the engine; this page shows the snapshot and says which
//! commands edit it.

use mctl_core::plan::{PlanStatus, PlanView};
use mctl_core::state::Elements;

use crate::grid::{CellRect, Grid, Limit, PlotKind, Role, Style, Widget};
use crate::page::{Layout, Lines, PageInput, fmt_dur};
use crate::screens::{body, boxed, column, split};

fn onoff(b: bool) -> &'static str {
    if b { "ON " } else { "OFF" }
}

fn plan_of<'a>(inp: &'a PageInput<'_>) -> Option<&'a PlanView> {
    let id = &inp.vehicle?.id;
    inp.world
        .plans
        .iter()
        .find(|(v, _)| v == id)
        .map(|(_, p)| &**p)
}

fn burns_panel(grid: &mut Grid, rect: CellRect, plan: Option<&PlanView>) {
    boxed(grid, rect, "BURNS");
    let mut l = Lines::new(grid, rect);
    let (dim, fg, bright) = (
        Style::new(Role::Dim),
        Style::new(Role::Fg),
        Style::new(Role::Bright),
    );
    let Some(p) = plan.filter(|p| !p.def.burns.is_empty()) else {
        l.line("NO BURNS DEFINED", dim);
        l.line(
            "burn add apo vnb 1 0 0      1 m/s prograde at the next apoapsis",
            dim,
        );
        l.line("burn add 2026-10-25T12:00:00 rtn 0 0.5 0", dim);
        l.line(
            "TRIGGERS: an dn apo peri (next after the previous burn) or an epoch.",
            dim,
        );
        l.line(
            "FRAMES: vnb (velocity, normal, binormal) or rtn (radial, along-track, normal).",
            dim,
        );
        return;
    };
    l.put(1, " #  TRIGGER               EPOCH (GMT)         FRM        DV1       DV2       DV3    |DV| m/s", dim);
    l.row += 1;
    // The resolved epochs exist once the plan has been computed; before that, show what was typed.
    for (i, b) in p.def.burns.iter().enumerate() {
        let epoch = p
            .burns
            .iter()
            .find(|r| r.trigger == b.trigger && (r.dv_m_s == b.dv_m_s))
            .map_or_else(|| "--".to_owned(), |r| r.t.gmt_doy()[..12].to_owned());
        l.put(
            1,
            &format!(
                "{:>2}  {:<20}  {:<19} {}  {:>9.3} {:>9.3} {:>9.3} {:>9.3}",
                i + 1,
                b.trigger,
                epoch,
                b.frame.label(),
                b.dv_m_s[0],
                b.dv_m_s[1],
                b.dv_m_s[2],
                b.dv_mag_m_s()
            ),
            bright,
        );
        l.row += 1;
    }
    l.row += 1;
    let d = &p.def;
    l.line(
        &format!(
            "HORIZON {:.2} REV   ZONAL {}  SUN {}  MOON {}  DRAG {} x{:.2}  SRP {}",
            d.horizon_revs,
            onoff(d.zonal),
            onoff(d.sun),
            onoff(d.moon),
            onoff(d.drag),
            d.drag_scale,
            onoff(d.srp)
        ),
        fg,
    );
    l.line(
        "EDIT: burn add|del|clear   plan horizon|force|drag|run|save|load",
        dim,
    );
}

fn elements_rows(l: &mut Lines<'_>, n: &Elements, p: &Elements) {
    let (dim, fg) = (Style::new(Role::Dim), Style::new(Role::Fg));
    l.put(1, "              NOMINAL       PLANNED         DELTA", dim);
    l.row += 1;
    let rows: [(&str, f64, f64, usize, &str); 6] = [
        ("SMA   km", n.sma_km, p.sma_km, 3, ""),
        ("ECC", n.ecc, p.ecc, 6, ""),
        ("INC  deg", n.inc_deg, p.inc_deg, 4, ""),
        ("HA    km", n.apo_alt_km, p.apo_alt_km, 2, ""),
        ("HP    km", n.peri_alt_km, p.peri_alt_km, 2, ""),
        ("PERIOD s", n.period_s, p.period_s, 3, ""),
    ];
    for (lab, a, b, prec, _) in rows {
        l.put(1, lab, dim);
        l.put(12, &format!("{a:>12.prec$}"), fg);
        l.put(26, &format!("{b:>12.prec$}"), fg);
        l.put(
            40,
            &format!("{:>+12.prec$}", b - a),
            Style::new(Role::Bright),
        );
        l.row += 1;
    }
}

fn results_panel(grid: &mut Grid, rect: CellRect, plan: Option<&PlanView>) {
    boxed(grid, rect, "RESULTS AT THE HORIZON");
    let mut l = Lines::new(grid, rect);
    let (dim, fg) = (Style::new(Role::Dim), Style::new(Role::Fg));
    let Some(p) = plan else {
        l.no_data("NO PLAN YET");
        return;
    };
    match &p.status {
        PlanStatus::Empty => {
            l.no_data("ADD A BURN: THE RESULT IS THE DIFFERENCE FROM COASTING");
            return;
        }
        PlanStatus::Computing => l.line("COMPUTING...", Style::limit(Limit::Caution)),
        PlanStatus::Failed(why) => {
            l.line("PLAN FAILED", Style::limit(Limit::Warning));
            l.line(&why.to_uppercase(), dim);
            return;
        }
        PlanStatus::Ready => {}
    }
    let Some(r) = &p.report else { return };
    l.line(
        &format!(
            "FROM {} TO {}  ({})",
            &p.t0.iso()[..19],
            &p.t_end.iso()[..19],
            fmt_dur(p.t_end.minus(p.t0))
        ),
        dim,
    );
    elements_rows(&mut l, &r.nominal, &r.planned);
    l.row += 1;
    l.pair(
        "D-PHASE",
        &format!("{:>+10.3}", r.d_phase_deg),
        "deg along the orbit (+ ahead)",
        fg,
    );
    l.pair(
        "SEPARATN",
        &format!("{:>10.2}", r.separation_km),
        "km at the horizon",
        fg,
    );
    l.row += 1;
    l.line("FIRST PASS, NOMINAL VS PLANNED", Style::new(Role::Bright));
    for s in &p.passes {
        let text = match (s.nominal_aos, s.planned_aos, s.shift_s()) {
            (Some(n), Some(_), d) => {
                let d = d.unwrap_or_default();
                format!("{}  AOS {}  SHIFT {:+.0} s", s.site, &n.gmt_doy()[..12], d)
            }
            (Some(n), None, _) => {
                format!("{}  AOS {}  LOST IN THE PLAN", s.site, &n.gmt_doy()[..12])
            }
            (None, Some(pl), _) => format!("{}  NEW PASS AOS {}", s.site, &pl.gmt_doy()[..12]),
            (None, None, _) => format!("{}  NO PASS IN THE HORIZON", s.site),
        };
        l.line(&text, fg);
    }
    for n in &p.notes {
        l.line(
            &format!("NOTE: {}", n.to_uppercase()),
            Style::limit(Limit::Caution),
        );
    }
    l.line(
        "THE PLAN IS NUMERICAL (DOP853); DRAG IS A STATIC ORDER-OF-MAGNITUDE MODEL.",
        dim,
    );
}

fn plot(grid: &mut Grid, rect: CellRect, title: &str, kind: PlotKind) {
    boxed(grid, rect, title);
    grid.widget(Widget::Plot {
        rect: rect.inner(),
        kind,
    });
}

pub(crate) fn planner(grid: &mut Grid, inp: &PageInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (top, h) = body(rows);
    let plan = plan_of(inp);
    let lw = cols * 58 / 100;
    let left = column(0, lw, top, &split(h, &[44, 56]));
    burns_panel(grid, left[0], plan);
    let half = lw / 2;
    plot(
        grid,
        CellRect::new(0, left[1].row, half, left[1].rows),
        "ALTITUDE",
        PlotKind::PlanAltitude,
    );
    plot(
        grid,
        CellRect::new(half, left[1].row, lw - half, left[1].rows),
        "SEPARATION",
        PlotKind::PlanSeparation,
    );
    results_panel(grid, CellRect::new(lw, top, cols - lw, h), plan);
    Layout::default()
}
