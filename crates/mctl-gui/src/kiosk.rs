//! The Raspberry Pi / CRT display (`mctl --pi`): big pictures, few numbers, cycling through the vehicles.
//!
//! This is a screen for looking at rather than operating, so it is built from the same widgets as the
//! console but has no pages, no command line and no effects: the tube supplies those.

use mctl_core::config::Regime;
use mctl_core::schedule::Ops;
use mctl_core::world::{VehicleView, World};

use crate::art::{EXTRAS, Extra};
use crate::grid::{CellRect, Grid, Limit, Role, Style, Widget};
use crate::page::{Layout, fmt_dur};

/// Seconds each vehicle is shown, unless `--dwell` says otherwise.
pub const DEFAULT_DWELL_S: f64 = 20.0;
/// An interlude (art without a live vehicle behind it) is shown for this fraction of a vehicle's time.
pub const EXTRA_DWELL_FRACTION: f64 = 0.6;

/// Rows used above the picture (the header) and below it (rule, two status lines, the progress marks).
const HEADER_ROWS: usize = 1;
const FOOTER_ROWS: usize = 4;
/// A CRT hides the edge of the picture (overscan), so text keeps this many rows and columns clear.
const SAFE_ROWS: usize = 1;
const SAFE_COLS: usize = 2;

#[derive(Clone, Copy)]
pub enum Scene<'a> {
    Vehicle(&'a VehicleView),
    Extra(&'static Extra),
}

impl Scene<'_> {
    /// How long the scene stays up, given the dwell for a vehicle.
    pub fn dwell_s(&self, dwell_s: f64) -> f64 {
        match self {
            Self::Vehicle(_) => dwell_s,
            Self::Extra(_) => dwell_s * EXTRA_DWELL_FRACTION,
        }
    }
}

/// The cycle: every vehicle, each followed by one piece of gallery art until that runs out.
pub fn scenes(world: &World) -> Vec<Scene<'_>> {
    let mut out = Vec::new();
    let mut extras = EXTRAS.iter();
    for v in &world.vehicles {
        out.push(Scene::Vehicle(v));
        if let Some(e) = extras.next() {
            out.push(Scene::Extra(e));
        }
    }
    out.extend(extras.map(Scene::Extra));
    out
}

pub struct KioskInput<'a> {
    pub world: &'a World,
    pub scene: Scene<'a>,
    pub index: usize,
    pub total: usize,
    pub held: bool,
}

/// `text` cut to `width` characters.
fn fit(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

fn centre(grid: &mut Grid, row: usize, text: &str, style: Style) {
    let w = text.chars().count().min(grid.cols);
    grid.put((grid.cols - w) / 2, row, &fit(text, grid.cols), style);
}

fn right(grid: &mut Grid, row: usize, text: &str, style: Style) {
    let w = text
        .chars()
        .count()
        .min(grid.cols.saturating_sub(SAFE_COLS));
    grid.put(
        grid.cols.saturating_sub(w + SAFE_COLS),
        row,
        &fit(text, w),
        style,
    );
}

fn vehicle_name(v: &VehicleView) -> String {
    v.resolved_name
        .clone()
        .unwrap_or_else(|| v.name.clone())
        .to_uppercase()
}

/// Up to three (label, value) readouts down each side of the picture.
type Readouts = Vec<(&'static str, String)>;

fn readouts(v: &VehicleView) -> (Readouts, Readouts) {
    if let Some(d) = v.deep.as_ref().filter(|_| v.regime == Regime::L2) {
        return (
            vec![
                ("DISTANCE", format!("{:.2} M KM", d.dist_km / 1.0e6)),
                ("SPEED", format!("{:.2} KM/S", d.speed_km_s)),
                ("ORBIT", "L2 HALO".into()),
            ],
            vec![
                ("LIGHT TIME", format!("{:.1} S", d.light_time_s)),
                ("OFF AXIS", format!("{:.1} DEG", d.off_axis_deg)),
                ("SOURCE", "HORIZONS".into()),
            ],
        );
    }
    let Some(s) = v.solution.as_ref() else {
        return (Vec::new(), Vec::new());
    };
    let g = &s.geodetic;
    let ns = if g.lat_deg >= 0.0 { 'N' } else { 'S' };
    let ew = if g.lon_deg >= 0.0 { 'E' } else { 'W' };
    let light = if s.illumination > 0.99 {
        "SUNLIT"
    } else if s.illumination < 0.01 {
        "ECLIPSE"
    } else {
        "PENUMBRA"
    };
    (
        vec![
            ("ALTITUDE", format!("{:.0} KM", g.alt_km)),
            ("SPEED", format!("{:.2} KM/S", s.state_gcrf.v.norm())),
            ("LIGHT", light.into()),
        ],
        vec![
            ("LATITUDE", format!("{:.1} {ns}", g.lat_deg.abs())),
            ("LONGITUDE", format!("{:.1} {ew}", g.lon_deg.abs())),
            ("DATA", s.quality.label().into()),
        ],
    )
}

/// Two lines about what the vehicle is doing now, from whatever is known of it.
fn status(world: &World, v: &VehicleView, width: usize) -> [(String, Style); 2] {
    let (bright, fg, dim) = (
        Style::new(Role::Bright),
        Style::new(Role::Fg),
        Style::new(Role::Dim),
    );
    let now = world.t;
    match v.ops.as_ref() {
        Some(Ops::Jwst(s)) => {
            let first = match s.current(now) {
                Some(c) => (fit(&format!("OBSERVING  {}", c.target), width), bright),
                None => ("BETWEEN OBSERVATIONS".to_owned(), dim),
            };
            let second = match s.upcoming(now).next() {
                Some(n) => (
                    fit(
                        &format!("NEXT IN {}  {}", fmt_dur(n.start.minus(now)), n.target),
                        width,
                    ),
                    fg,
                ),
                None => (String::new(), dim),
            };
            return [first, second];
        }
        Some(Ops::Hst(r)) => {
            let first = (
                fit(
                    &format!("{} PROGRAMS IN TODAY'S REPORT", r.science.len()),
                    width,
                ),
                bright,
            );
            let second = match r.science.first() {
                Some(p) => (fit(&p.title.to_uppercase(), width), fg),
                None => (String::new(), dim),
            };
            return [first, second];
        }
        None => {}
    }
    if let Some(p) = world
        .prox
        .iter()
        .find(|p| p.chaser == v.id)
        .filter(|_| v.visits.is_some())
    {
        let target = world
            .vehicles
            .iter()
            .find(|t| t.id == p.target)
            .map_or(p.target.to_uppercase(), |t| t.name.to_uppercase());
        return [
            (
                fit(&format!("{}  {}", p.state.label(), target), width),
                bright,
            ),
            (
                if p.state == mctl_core::prox::ProxState::Docked {
                    String::new()
                } else {
                    fit(&format!("RANGE {:.1} KM", p.range_km), width)
                },
                fg,
            ),
        ];
    }
    let next = v
        .passes
        .first()
        .and_then(|set| set.passes.iter().find(|p| p.los.0 > now.0));
    if let Some(p) = next {
        let first = if p.aos.0 <= now.0 {
            ("OVERHEAD NOW".to_owned(), Style::limit(Limit::Caution))
        } else {
            (
                format!("NEXT PASS IN {}", fmt_dur(p.aos.minus(now))),
                bright,
            )
        };
        return [first, (format!("PEAKS AT {:.0} DEG", p.max_el_deg), fg)];
    }
    if !v.visitors.is_empty() {
        let names: Vec<String> = v
            .visitors
            .iter()
            .filter_map(|id| world.vehicles.iter().find(|t| t.id == *id))
            .map(|t| t.name.to_uppercase())
            .collect();
        return [
            (
                fit(&format!("VISITORS  {}", names.join("  ")), width),
                bright,
            ),
            (String::new(), dim),
        ];
    }
    [
        (
            v.solution
                .as_ref()
                .map_or("NO SOLUTION YET".to_owned(), |s| {
                    format!("TRACKED FROM {}", s.source.label())
                }),
            fg,
        ),
        (String::new(), dim),
    ]
}

pub fn render(grid: &mut Grid, inp: &KioskInput<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (bright, fg, dim, border) = (
        Style::new(Role::Bright),
        Style::new(Role::Fg),
        Style::new(Role::Dim),
        Style::new(Role::Border),
    );
    let bottom = rows.saturating_sub(SAFE_ROWS);
    let picture_rows = bottom.saturating_sub(SAFE_ROWS + HEADER_ROWS + FOOTER_ROWS);
    let picture = CellRect::new(0, SAFE_ROWS + HEADER_ROWS, cols, picture_rows);

    // Header: the product on the left, the UTC time on the right, the subject in the middle.
    grid.put(SAFE_COLS, SAFE_ROWS, "MCTL", fg);
    let utc = inp.world.t.iso();
    right(
        grid,
        SAFE_ROWS,
        &format!("{} UTC", utc.get(11..19).unwrap_or("--:--:--")),
        fg,
    );

    let (title, art_id, earth) = match inp.scene {
        Scene::Vehicle(v) => (vehicle_name(v), v.id.as_str(), v.regime == Regime::Earth),
        Scene::Extra(e) => (e.title.to_owned(), e.id, true),
    };
    centre(grid, SAFE_ROWS, &title, bright);

    grid.widget(Widget::Backdrop {
        rect: picture,
        earth,
    });
    grid.widget(Widget::Art {
        rect: picture,
        id: art_id.to_owned(),
    });

    let rule = "─".repeat(cols.saturating_sub(2 * SAFE_COLS));
    let footer = bottom.saturating_sub(FOOTER_ROWS);
    grid.put(SAFE_COLS, footer, &rule, border);
    match inp.scene {
        Scene::Vehicle(v) => {
            let (l, r) = readouts(v);
            for (i, (label, value)) in l.iter().enumerate() {
                let row = picture.row + 1 + i * 3;
                grid.put(SAFE_COLS, row, label, dim);
                grid.put(SAFE_COLS, row + 1, value, bright);
            }
            for (i, (label, value)) in r.iter().enumerate() {
                let row = picture.row + 1 + i * 3;
                right(grid, row, label, dim);
                right(grid, row + 1, value, bright);
            }
            let lines = status(inp.world, v, cols.saturating_sub(2 * SAFE_COLS));
            for (i, (text, style)) in lines.iter().enumerate() {
                centre(grid, footer + 1 + i, text, *style);
            }
        }
        Scene::Extra(e) => {
            centre(grid, footer + 1, e.caption, bright);
        }
    }

    // One mark per scene; the current one is filled. A held display says so.
    let marks: String = (0..inp.total)
        .map(|i| if i == inp.index { "■ " } else { "· " })
        .collect::<String>()
        .trim_end()
        .to_owned();
    let marks = if inp.held {
        format!("{marks}  HOLD")
    } else {
        marks
    };
    centre(grid, bottom.saturating_sub(1), &marks, dim);

    Layout {
        widgets: grid.take_widgets(),
        ..Layout::default()
    }
}

#[cfg(test)]
pub(crate) fn test_world(n: usize) -> World {
    let mut w = World::empty(mctl_core::time::Utc(1.0e9));
    for i in 0..n {
        w.vehicles.push(VehicleView {
            id: format!("v{i}"),
            name: format!("V{i}"),
            norad: 1,
            resolved_name: None,
            visits: None,
            visitors: vec![],
            telemetry: None,
            regime: Default::default(),
            schedule: None,
            deep: None,
            reference_burns: vec![],
            fields: vec![],
            ops: None,
            ops_health: None,
            met_epoch: None,
            sources: vec![],
            solution: None,
            track: None,
            passes: vec![],
            beta_series: Default::default(),
        });
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cycle_visits_every_vehicle_and_every_piece_of_art_once() {
        let world = test_world(3);
        let cycle = scenes(&world);
        let vehicles = cycle
            .iter()
            .filter(|s| matches!(s, Scene::Vehicle(_)))
            .count();
        let extras = cycle
            .iter()
            .filter(|s| matches!(s, Scene::Extra(_)))
            .count();
        assert_eq!(vehicles, world.vehicles.len());
        assert_eq!(extras, EXTRAS.len());
        assert!(
            matches!(cycle[0], Scene::Vehicle(_)),
            "it opens on a vehicle"
        );
    }

    #[test]
    fn fit_never_splits_a_character() {
        assert_eq!(fit("ÅÅÅ", 2), "ÅÅ");
        assert_eq!(fit("abc", 9), "abc");
    }
}
