//! The Raspberry Pi / CRT display (`mctl --kiosk`): big pictures, few numbers, cycling through the vehicles.
//!
//! This is a screen for looking at rather than operating, so it is built from the same widgets as the
//! console but has no pages, no command line and no effects: the tube supplies those.

use mctl_core::config::Regime;
use mctl_core::schedule::Ops;
use mctl_core::world::{VehicleView, World};

use crate::art::{EXTRAS, Extra};
use crate::grid::{CellRect, Grid, Limit, PlotKind, Role, Style, Widget};
use crate::page::{Layout, PageInput, fmt_dur};
use crate::pages::Page;
use crate::screens::boxed;

/// Seconds each vehicle is shown, unless `--dwell` says otherwise.
pub const DEFAULT_DWELL_S: f64 = 20.0;
/// An interlude (art without a live vehicle behind it) is shown for this fraction of a vehicle's time.
pub const EXTRA_DWELL_FRACTION: f64 = 0.6;

/// Rows used above the picture (the header) and below it (rule, two status lines, the progress marks).
const HEADER_ROWS: usize = 1;
const FOOTER_ROWS: usize = 4;
/// Rows and columns of text kept clear at the edges. The video mode's own margins (in `cmdline.txt`) take
/// care of the overscan at the top; the bottom and sides keep a cell of their own.
const SAFE_TOP: usize = 0;
const SAFE_BOTTOM: usize = 1;
const SAFE_COLS: usize = 2;

/// Which scenes the display cycles through.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mix {
    /// Pictures and console pages in turn.
    #[default]
    Mixed,
    /// Pictures only.
    Pretty,
    /// Console pages only.
    Dense,
}

#[derive(Clone, Debug)]
pub struct KioskConfig {
    /// Seconds a vehicle's scene stays up.
    pub dwell_s: f64,
    pub mix: Mix,
    pub order: Order,
    /// Vehicle ids to show; empty means every vehicle.
    pub vehicles: Vec<String>,
}

/// The order the display takes its scenes in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Order {
    /// As configured: each vehicle's scenes in turn, vehicles in the configuration's order.
    #[default]
    Sequential,
    /// Every scene once per cycle in a random order, and a different random order each cycle.
    Random,
    /// One vehicle at a time in a random order, all of its scenes before the next vehicle; the vehicle
    /// order is different each cycle.
    Vehicle,
}

/// A small xorshift generator: the display needs variety, not statistical quality, and a seed that tests can
/// fix, so no dependency is warranted.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Zero is the one state xorshift cannot leave.
        Self(seed.max(1))
    }

    /// A generator seeded from the clock.
    pub fn from_clock() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Self::new(nanos)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Fisher-Yates.
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, (self.next() % (i as u64 + 1)) as usize);
        }
    }
}

/// Which of `scenes` to show, in what order, for one pass of the display. Indices into `scenes`.
///
/// `only` limits the vehicles shown (empty: all of them); a gallery interlude follows the vehicle whose art
/// it came after, so it is dropped with that vehicle. `previous` is the scene shown last, which the new pass
/// does not start with, so a cycle never opens on what the last one closed on.
pub fn plan(
    scenes: &[Scene<'_>],
    order: Order,
    only: &[String],
    previous: Option<usize>,
    rng: &mut Rng,
) -> Vec<usize> {
    // Groups of consecutive scenes belonging to one vehicle. An interlude straight after a vehicle's art
    // rides with that vehicle; the gallery left over at the end of the cycle is a group of its own.
    let mut groups: Vec<(Option<&str>, Vec<usize>)> = Vec::new();
    for (i, sc) in scenes.iter().enumerate() {
        match sc {
            Scene::Vehicle(v, _)
                if groups
                    .last()
                    .is_none_or(|(id, _)| *id != Some(v.id.as_str())) =>
            {
                groups.push((Some(v.id.as_str()), vec![i]));
            }
            Scene::Extra(_) if i > 0 && matches!(scenes[i - 1], Scene::Vehicle(_, View::Art)) => {
                groups.last_mut().expect("a vehicle precedes").1.push(i);
            }
            Scene::Extra(_) => match groups.last_mut() {
                Some((None, g)) => g.push(i),
                _ => groups.push((None, vec![i])),
            },
            Scene::Vehicle(_, _) => groups.last_mut().expect("same vehicle").1.push(i),
        }
    }
    // The leftover gallery belongs to no vehicle, so a selection of vehicles leaves it out.
    groups.retain(|(id, _)| match id {
        Some(id) => only.is_empty() || only.iter().any(|o| o == id),
        None => only.is_empty(),
    });
    let mut out: Vec<usize> = match order {
        Order::Sequential => groups.into_iter().flat_map(|(_, g)| g).collect(),
        Order::Random => {
            let mut all: Vec<usize> = groups.into_iter().flat_map(|(_, g)| g).collect();
            rng.shuffle(&mut all);
            all
        }
        Order::Vehicle => {
            rng.shuffle(&mut groups);
            // A different vehicle from the last pass's first, when there is a choice.
            if let (Some(prev), true) = (previous, groups.len() > 1)
                && groups[0].1.contains(&prev)
            {
                groups.swap(0, 1);
            }
            groups.into_iter().flat_map(|(_, g)| g).collect()
        }
    };
    if order == Order::Random && out.len() > 1 && Some(out[0]) == previous {
        out.swap(0, 1);
    }
    out
}

/// A dense scene stays up this much longer than a picture, since there is more to read.
const DENSE_DWELL_FACTOR: f64 = 1.5;

/// What a vehicle's scene shows: line art or a picture page, or one of the console's pages at 80 x 30.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Art,
    Orbit,
    Ground,
    Sky,
    Prox,
    Ops,
    /// The console's MASTER page.
    DenseMaster,
    /// TRAJ orbit map with the state vector, elements and sources (the halo orbit at L2).
    DenseTraj,
    /// PASS sky plot and the next pass.
    DensePass,
    DenseProx,
    /// OPS: timeline, visit table, pictures; or the Hubble report.
    DenseOps,
    /// SYS event log.
    DenseEvents,
}

impl View {
    fn title(self, regime: Regime) -> &'static str {
        match (self, regime) {
            (Self::Art, _) => "",
            (Self::Orbit, Regime::L2) => "HALO ORBIT",
            (Self::Orbit, Regime::Deep) => "SOLAR SYSTEM",
            (Self::Orbit, _) => "ORBIT MAP",
            (Self::Ground, _) => "GROUND TRACK",
            (Self::Sky, _) => "SKY PLOT",
            (Self::Prox, _) => "R-BAR / V-BAR",
            (Self::Ops, _) => "OBSERVING  [PLAN]",
            (Self::DenseMaster, _) => "MASTER",
            (Self::DenseTraj, Regime::L2) => "HALO ORBIT",
            (Self::DenseTraj, Regime::Deep) => "SOLAR SYSTEM",
            (Self::DenseTraj, _) => "ORBIT MAP",
            (Self::DensePass, _) => "PASS",
            (Self::DenseProx, _) => "PROX",
            (Self::DenseOps, _) => "OPS",
            (Self::DenseEvents, _) => "EVENT LOG",
        }
    }

    pub fn dense(self) -> bool {
        matches!(
            self,
            Self::DenseMaster
                | Self::DenseTraj
                | Self::DensePass
                | Self::DenseProx
                | Self::DenseOps
                | Self::DenseEvents
        )
    }

    /// The console page a dense view draws.
    fn page(self) -> Option<(Page, usize)> {
        Some(match self {
            Self::DenseMaster => (Page::Master, 0),
            Self::DenseTraj => (Page::Traj, 0),
            Self::DensePass => (Page::Pass, 0),
            Self::DenseProx => (Page::Prox, 0),
            Self::DenseOps => (Page::Ops, 0),
            Self::DenseEvents => (Page::Sys, 0),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
pub enum Scene<'a> {
    Vehicle(&'a VehicleView, View),
    Extra(&'static Extra),
}

impl Scene<'_> {
    /// How long the scene stays up, given the dwell for a vehicle.
    pub fn dwell_s(&self, dwell_s: f64) -> f64 {
        match self {
            Self::Vehicle(_, v) if v.dense() => dwell_s * DENSE_DWELL_FACTOR,
            Self::Vehicle(..) => dwell_s,
            Self::Extra(_) => dwell_s * EXTRA_DWELL_FRACTION,
        }
    }

    /// Whether the scene wants the dense (small) font.
    pub fn dense(&self) -> bool {
        matches!(self, Self::Vehicle(_, v) if v.dense())
    }
}

/// The views a vehicle has data for, in the order shown: pictures and console pages interleaved. Pages
/// that are only telemetry (GNC, EPS, TCS, COMM) have no place on a display meant to be looked at.
fn views_of(world: &World, v: &VehicleView, mix: Mix) -> Vec<View> {
    let mut out = vec![View::Art];
    // A docked vehicle shares its host's orbit: the same maps would be shown twice.
    let docked = v.visits.is_some()
        && world
            .prox
            .iter()
            .any(|p| p.chaser == v.id && p.state == mctl_core::prox::ProxState::Docked);
    let placed = !docked
        && match v.regime {
            Regime::Earth => v.solution.is_some(),
            Regime::L2 | Regime::Deep => v.deep.is_some(),
        };
    let passes = !docked && v.passes.iter().any(|set| !set.passes.is_empty());
    // A plot of vehicles that are all docked has nothing to show.
    let relative = world.prox.iter().any(|p| {
        (p.chaser == v.id || p.target == v.id) && p.state != mctl_core::prox::ProxState::Docked
    });
    // Pictures are the point of the picture view: without one there is nothing to look at.
    let pictures = v
        .fields
        .iter()
        .any(|f| matches!(f.state, mctl_core::schedule::FieldState::Ready { .. }));
    let earth = v.regime == Regime::Earth;
    let wanted = [
        (View::DenseMaster, placed),
        (View::Orbit, placed),
        (View::Ground, placed && earth),
        (View::DenseTraj, placed),
        (View::Sky, passes),
        (View::DensePass, passes),
        (View::Prox, relative),
        (View::DenseProx, relative),
        (View::Ops, pictures),
        (View::DenseOps, v.ops.is_some()),
        (View::DenseEvents, v.telemetry.is_some()),
    ];
    out.extend(
        wanted
            .into_iter()
            .filter(|(_, ok)| *ok)
            .map(|(view, _)| view),
    );
    out.retain(|view| match mix {
        Mix::Mixed => true,
        Mix::Pretty => !view.dense(),
        Mix::Dense => view.dense(),
    });
    out
}

/// The cycle: each vehicle's views in turn, one piece of gallery art after each vehicle's art until the
/// gallery runs out (never in dense-only mode, which has no art).
pub fn scenes(world: &World, mix: Mix) -> Vec<Scene<'_>> {
    let mut out = Vec::new();
    let mut extras = EXTRAS.iter();
    for v in &world.vehicles {
        for view in views_of(world, v, mix) {
            out.push(Scene::Vehicle(v, view));
            if view == View::Art
                && let Some(e) = extras.next()
            {
                out.push(Scene::Extra(e));
            }
        }
    }
    if mix != Mix::Dense {
        out.extend(extras.map(Scene::Extra));
    }
    out
}

pub struct KioskInput<'a> {
    pub world: &'a World,
    pub scene: Scene<'a>,
    pub held: bool,
    /// The console's page input, which dense scenes re-aim at the page they draw.
    pub base: &'a PageInput<'a>,
}

/// `text` cut to `width` characters.
fn fit(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

/// How fast a line too long for the screen scrolls, in characters a second.
const TICKER_CPS: f64 = 5.0;
/// What separates the end of a scrolling line from its start coming round again.
const TICKER_GAP: &str = "     ";

/// `text` as it is if it fits in `width` characters; otherwise the `width` characters of it, looping, that
/// are in view `t` seconds into its scroll, like a news ticker.
fn ticker(text: &str, width: usize, t: f64) -> String {
    let n = text.chars().count();
    if n <= width || width == 0 {
        return text.to_owned();
    }
    let looped: Vec<char> = text.chars().chain(TICKER_GAP.chars()).collect();
    let start = (t.max(0.0) * TICKER_CPS) as usize % looped.len();
    (0..width)
        .map(|i| looped[(start + i) % looped.len()])
        .collect()
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
    if let Some(d) = v.deep.as_ref().filter(|_| v.regime == Regime::Deep) {
        let au = d.sun_dist_km / mctl_astro::interplanetary::KM_PER_AU;
        return (
            vec![
                ("FROM SUN", format!("{au:.1} AU")),
                ("SPEED", format!("{:.2} KM/S", d.speed_km_s)),
                ("ORBIT", "ESCAPE".into()),
            ],
            vec![
                ("LIGHT TIME", format!("{:.1} H", d.light_time_s / 3600.0)),
                ("ECL LAT", format!("{:.1} DEG", d.off_axis_deg)),
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
fn status(world: &World, v: &VehicleView) -> [(String, Style); 2] {
    let (bright, fg, dim) = (
        Style::new(Role::Bright),
        Style::new(Role::Fg),
        Style::new(Role::Dim),
    );
    let now = world.t;
    match v.ops.as_ref() {
        Some(Ops::Jwst(s)) => {
            let first = match s.current(now) {
                Some(c) => (format!("OBSERVING  {}", c.target), bright),
                None => ("BETWEEN OBSERVATIONS".to_owned(), dim),
            };
            let second = match s.upcoming(now).next() {
                Some(n) => (
                    format!("NEXT IN {}  {}", fmt_dur(n.start.minus(now)), n.target),
                    fg,
                ),
                None => (String::new(), dim),
            };
            return [first, second];
        }
        Some(Ops::Hst(r)) => {
            let first = (
                format!("{} PROGRAMS IN TODAY'S REPORT", r.science.len()),
                bright,
            );
            let second = match r.science.first() {
                Some(p) => (p.title.to_uppercase(), fg),
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
            (format!("{}  {}", p.state.label(), target), bright),
            (
                if p.state == mctl_core::prox::ProxState::Docked {
                    String::new()
                } else {
                    format!("RANGE {:.1} KM", p.range_km)
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
            (format!("VISITORS  {}", names.join("  ")), bright),
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

/// Position lines for the map views: where the vehicle is.
fn position_lines(v: &VehicleView) -> [(String, Style); 2] {
    let (bright, fg) = (Style::new(Role::Bright), Style::new(Role::Fg));
    if let Some(d) = v.deep.as_ref().filter(|_| v.regime == Regime::L2) {
        return [
            (
                format!(
                    "{:.2} M KM FROM EARTH   {:.2} KM/S",
                    d.dist_km / 1.0e6,
                    d.speed_km_s
                ),
                bright,
            ),
            (format!("LIGHT TIME {:.1} S", d.light_time_s), fg),
        ];
    }
    if let Some(d) = v.deep.as_ref().filter(|_| v.regime == Regime::Deep) {
        return [
            (
                format!(
                    "{:.1} AU FROM THE SUN   {:.2} KM/S",
                    d.sun_dist_km / mctl_astro::interplanetary::KM_PER_AU,
                    d.speed_km_s
                ),
                bright,
            ),
            (format!("LIGHT TIME {:.1} H", d.light_time_s / 3600.0), fg),
        ];
    }
    let Some(s) = v.solution.as_ref() else {
        return [("NO SOLUTION YET".to_owned(), fg), (String::new(), fg)];
    };
    let g = &s.geodetic;
    [
        (
            format!("{:.0} KM UP   {:.2} KM/S", g.alt_km, s.state_gcrf.v.norm()),
            bright,
        ),
        (
            format!(
                "{:.1} {}   {:.1} {}",
                g.lat_deg.abs(),
                if g.lat_deg >= 0.0 { 'N' } else { 'S' },
                g.lon_deg.abs(),
                if g.lon_deg >= 0.0 { 'E' } else { 'W' }
            ),
            fg,
        ),
    ]
}

/// The body of a pictorial view: the console's own panels, in its own frames, at this size.
fn view_body(grid: &mut Grid, rect: CellRect, v: &VehicleView, view: View) -> Layout {
    let mut layout = Layout::default();
    match view {
        View::Art => {}
        // Dense views are drawn by `render_dense`, never here.
        View::DenseMaster
        | View::DenseTraj
        | View::DensePass
        | View::DenseProx
        | View::DenseOps
        | View::DenseEvents => {}
        View::Orbit if v.regime == Regime::L2 => {
            crate::deep::plot(grid, rect, "HALO ORBIT  VIEW FROM EARTH", PlotKind::Halo);
        }
        View::Orbit if v.regime == Regime::Deep => {
            crate::deep::plot(
                grid,
                rect,
                "SOLAR SYSTEM FROM ECLIPTIC NORTH",
                PlotKind::SolarSystem,
            );
        }
        View::Orbit => {
            boxed(grid, rect, view.title(v.regime));
            layout.globe = Some(rect.inner());
        }
        View::Ground => {
            boxed(grid, rect, view.title(v.regime));
            layout.ground = Some(rect.inner());
        }
        View::Sky => {
            boxed(grid, rect, view.title(v.regime));
            layout.sky = Some(rect.inner());
        }
        View::Prox => {
            boxed(grid, rect, view.title(v.regime));
            grid.widget(Widget::Plot {
                rect: rect.inner(),
                kind: PlotKind::Prox,
            });
        }
        View::Ops => match v.ops.as_ref() {
            Some(Ops::Jwst(_)) => {
                let fields: Vec<_> = v.fields.iter().take(2).cloned().collect();
                crate::deep::field_strip(
                    grid,
                    rect,
                    &fields,
                    &|i, f| {
                        let what = f.label.split_whitespace().next().unwrap_or_default();
                        format!("{} {what}", if i == 0 { "NOW" } else { "NEXT" })
                    },
                    true,
                );
            }
            _ => {
                let fields: Vec<_> = v.fields.iter().take(2).cloned().collect();
                crate::deep::field_strip(grid, rect, &fields, &|_, f| f.label.clone(), true);
            }
        },
    }
    layout
}

/// One of the console's own pages, drawn inside the safe area with a header and a status line of the
/// display's own.
fn render_dense(grid: &mut Grid, inp: &KioskInput<'_>, v: &VehicleView, view: View) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let (fg, bright, dim) = (
        Style::new(Role::Fg),
        Style::new(Role::Bright),
        Style::new(Role::Dim),
    );
    let (inner_cols, inner_rows) = (
        cols.saturating_sub(2 * SAFE_COLS),
        rows.saturating_sub(SAFE_TOP + SAFE_BOTTOM),
    );
    let mut sub = Grid::new(inner_cols, inner_rows);
    let (page, subpage) = view.page().expect("a dense view has a page");
    let page_inp = inp.base.with_page(page, subpage);
    // MASTER is laid out for a much wider screen; Earth orbit gets a version for this one.
    let layout =
        if view == View::DenseMaster && v.regime == Regime::Earth && v.solution.is_some() {
            let mut layout = crate::screens::master_dense(&mut sub, &page_inp);
            layout.widgets = sub.take_widgets();
            layout
        } else {
            crate::screens::render_body(&mut sub, &page_inp)
        }
        .shifted(SAFE_COLS, SAFE_TOP);
    grid.blit(&sub, SAFE_COLS, SAFE_TOP);

    grid.put(SAFE_COLS, SAFE_TOP, "MCTL", fg);
    centre(
        grid,
        SAFE_TOP,
        &format!("{}  {}", vehicle_name(v), view.title(v.regime)),
        bright,
    );
    let utc = inp.world.t.iso();
    right(
        grid,
        SAFE_TOP,
        &format!("{} UTC", utc.get(11..19).unwrap_or("--:--:--")),
        fg,
    );
    // Below the page: one status line and the marks, in the rows its body leaves free.
    let status_row = rows.saturating_sub(SAFE_BOTTOM + 2);
    let [(line, style), _] = status(inp.world, v);
    centre(
        grid,
        status_row,
        &ticker(&line, inner_cols, inp.world.t.0),
        style,
    );
    centre(
        grid,
        status_row + 1,
        &marks(inp.world, Some(&v.id), inp.held),
        dim,
    );
    layout
}

/// One mark per vehicle, the current one filled; a held display says so.
fn marks(world: &World, current: Option<&str>, held: bool) -> String {
    let marks = world
        .vehicles
        .iter()
        .map(|w| {
            if Some(w.id.as_str()) == current {
                "■ "
            } else {
                "· "
            }
        })
        .collect::<String>()
        .trim_end()
        .to_owned();
    if held {
        format!("{marks}  HOLD")
    } else {
        marks
    }
}

pub fn render(grid: &mut Grid, inp: &KioskInput<'_>) -> Layout {
    if let Scene::Vehicle(v, view) = inp.scene
        && view.dense()
    {
        return render_dense(grid, inp, v, view);
    }
    let (cols, rows) = (grid.cols, grid.rows);
    let (bright, fg, dim, border) = (
        Style::new(Role::Bright),
        Style::new(Role::Fg),
        Style::new(Role::Dim),
        Style::new(Role::Border),
    );
    let bottom = rows.saturating_sub(SAFE_BOTTOM);
    let picture_rows = bottom.saturating_sub(SAFE_TOP + HEADER_ROWS + FOOTER_ROWS);
    let picture = CellRect::new(0, SAFE_TOP + HEADER_ROWS, cols, picture_rows);
    let framed = CellRect::new(
        SAFE_COLS,
        picture.row,
        cols.saturating_sub(2 * SAFE_COLS),
        picture_rows,
    );

    // Header: the product on the left, the UTC time on the right, the subject in the middle.
    grid.put(SAFE_COLS, SAFE_TOP, "MCTL", fg);
    let utc = inp.world.t.iso();
    right(
        grid,
        SAFE_TOP,
        &format!("{} UTC", utc.get(11..19).unwrap_or("--:--:--")),
        fg,
    );
    let (title, art_id, earth) = match inp.scene {
        Scene::Vehicle(v, _) => (vehicle_name(v), v.id.as_str(), v.regime == Regime::Earth),
        Scene::Extra(e) => (e.title.to_owned(), e.id, true),
    };
    centre(grid, SAFE_TOP, &title, bright);

    let mut layout = Layout::default();
    let art_scene = matches!(inp.scene, Scene::Extra(_) | Scene::Vehicle(_, View::Art));
    if art_scene {
        grid.widget(Widget::Backdrop {
            rect: picture,
            earth,
        });
        grid.widget(Widget::Art {
            rect: picture,
            id: art_id.to_owned(),
        });
    } else if let Scene::Vehicle(v, view) = inp.scene {
        layout = view_body(grid, framed, v, view);
    }

    let footer = bottom.saturating_sub(FOOTER_ROWS);
    grid.put(
        SAFE_COLS,
        footer,
        &"─".repeat(cols.saturating_sub(2 * SAFE_COLS)),
        border,
    );
    match inp.scene {
        Scene::Vehicle(v, View::Art) => {
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
            let lines = status(inp.world, v);
            for (i, (text, style)) in lines.iter().enumerate() {
                let text = ticker(text, cols.saturating_sub(2 * SAFE_COLS), inp.world.t.0);
                centre(grid, footer + 1 + i, &text, *style);
            }
        }
        Scene::Vehicle(v, View::Orbit | View::Ground) => {
            for (i, (text, style)) in position_lines(v).iter().enumerate() {
                centre(grid, footer + 1 + i, text, *style);
            }
        }
        Scene::Vehicle(v, _) => {
            let lines = status(inp.world, v);
            for (i, (text, style)) in lines.iter().enumerate() {
                let text = ticker(text, cols.saturating_sub(2 * SAFE_COLS), inp.world.t.0);
                centre(grid, footer + 1 + i, &text, *style);
            }
        }
        Scene::Extra(e) => {
            centre(grid, footer + 1, e.caption, bright);
        }
    }

    let current = match inp.scene {
        Scene::Vehicle(v, _) => Some(v.id.as_str()),
        Scene::Extra(_) => None,
    };
    centre(
        grid,
        bottom.saturating_sub(1),
        &marks(inp.world, current, inp.held),
        dim,
    );

    layout.widgets = grid.take_widgets();
    layout
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
        let cycle = scenes(&world, Mix::Mixed);
        let vehicles = cycle
            .iter()
            .filter(|s| matches!(s, Scene::Vehicle(..)))
            .count();
        let extras = cycle
            .iter()
            .filter(|s| matches!(s, Scene::Extra(_)))
            .count();
        assert_eq!(vehicles, world.vehicles.len());
        assert_eq!(extras, EXTRAS.len());
        assert!(
            matches!(cycle[0], Scene::Vehicle(..)),
            "it opens on a vehicle"
        );
    }

    #[test]
    fn a_vehicle_gets_only_the_views_it_has_data_for() {
        let mut w = test_world(1);
        assert_eq!(
            views_of(&w, &w.vehicles[0], Mix::Mixed),
            [View::Art],
            "no data, no maps"
        );
        w.vehicles[0].schedule = Some("jwst".into());
        assert_eq!(
            views_of(&w, &w.vehicles[0], Mix::Mixed),
            [View::Art],
            "a schedule with no picture is not worth a scene"
        );
    }

    #[test]
    fn the_mix_picks_pictures_console_pages_or_both() {
        let mut w = test_world(1);
        w.vehicles[0].telemetry = Some("isslive".into());
        let v = &w.vehicles[0];
        assert_eq!(
            views_of(&w, v, Mix::Mixed),
            [View::Art, View::DenseEvents],
            "a vehicle with telemetry has an event log"
        );
        assert_eq!(views_of(&w, v, Mix::Pretty), [View::Art]);
        assert_eq!(views_of(&w, v, Mix::Dense), [View::DenseEvents]);
        assert!(
            scenes(&w, Mix::Dense)
                .iter()
                .all(|s| matches!(s, Scene::Vehicle(_, v) if v.dense())),
            "dense-only has no art and no gallery"
        );
        assert!(Scene::Vehicle(v, View::DenseEvents).dense());
        assert!(
            Scene::Vehicle(v, View::DenseEvents).dwell_s(10.0)
                > Scene::Vehicle(v, View::Art).dwell_s(10.0)
        );
    }

    #[test]
    fn fit_never_splits_a_character() {
        assert_eq!(fit("ÅÅÅ", 2), "ÅÅ");
        assert_eq!(fit("abc", 9), "abc");
    }

    fn vehicle_of(scenes: &[Scene<'_>], i: usize) -> Option<String> {
        match scenes[i] {
            Scene::Vehicle(v, _) => Some(v.id.clone()),
            Scene::Extra(_) => None,
        }
    }

    #[test]
    fn sequential_keeps_the_configured_order() {
        let w = test_world(3);
        let scenes = scenes(&w, Mix::Mixed);
        let p = plan(&scenes, Order::Sequential, &[], None, &mut Rng::new(7));
        assert_eq!(p, (0..scenes.len()).collect::<Vec<_>>());
    }

    #[test]
    fn random_shows_every_scene_once_per_pass_and_a_different_order_each_pass() {
        let w = test_world(4);
        let scenes = scenes(&w, Mix::Mixed);
        let mut rng = Rng::new(42);
        let mut last = None;
        let mut passes = Vec::new();
        for _ in 0..5 {
            let p = plan(&scenes, Order::Random, &[], last, &mut rng);
            let mut sorted = p.clone();
            sorted.sort_unstable();
            assert_eq!(
                sorted,
                (0..scenes.len()).collect::<Vec<_>>(),
                "each scene once"
            );
            assert_ne!(
                Some(p[0]),
                last,
                "never opens on what the last pass closed on"
            );
            last = p.last().copied();
            passes.push(p);
        }
        assert!(
            passes.windows(2).all(|w| w[0] != w[1]),
            "a new order each pass: {passes:?}"
        );
    }

    #[test]
    fn vehicle_order_runs_a_whole_vehicle_before_the_next_and_reshuffles_each_pass() {
        let w = test_world(5);
        let scenes = scenes(&w, Mix::Mixed);
        let mut rng = Rng::new(3);
        let mut orders = Vec::new();
        let mut last = None;
        for _ in 0..4 {
            let p = plan(&scenes, Order::Vehicle, &[], last, &mut rng);
            assert_eq!(p.len(), scenes.len());
            // Walking the plan, a vehicle's scenes are contiguous: its id never comes back after another's.
            let mut seen: Vec<String> = Vec::new();
            for &i in &p {
                if let Some(id) = vehicle_of(&scenes, i)
                    && seen.last() != Some(&id)
                {
                    assert!(!seen.contains(&id), "{id} split up in {p:?}");
                    seen.push(id);
                }
            }
            assert_eq!(seen.len(), 5);
            last = p.last().copied();
            orders.push(seen);
        }
        assert!(orders.windows(2).all(|w| w[0] != w[1]), "{orders:?}");
    }

    #[test]
    fn a_selection_limits_the_vehicles_and_takes_their_interludes_with_them() {
        let w = test_world(4);
        let scenes = scenes(&w, Mix::Mixed);
        let only = vec!["v1".to_owned(), "v3".to_owned()];
        for order in [Order::Sequential, Order::Random, Order::Vehicle] {
            let p = plan(&scenes, order, &only, None, &mut Rng::new(9));
            let ids: std::collections::BTreeSet<_> =
                p.iter().filter_map(|&i| vehicle_of(&scenes, i)).collect();
            assert_eq!(
                ids.into_iter().collect::<Vec<_>>(),
                ["v1", "v3"],
                "{order:?}"
            );
            // An interlude is shown only if it followed a selected vehicle's art.
            for &i in p.iter().filter(|&&i| vehicle_of(&scenes, i).is_none()) {
                let before = vehicle_of(&scenes, i - 1).expect("an interlude follows art");
                assert!(only.contains(&before), "{order:?}");
            }
        }
    }

    #[test]
    fn a_line_that_fits_stays_put_and_a_longer_one_scrolls_and_loops() {
        assert_eq!(ticker("SHORT", 9, 123.0), "SHORT");
        let long = "ABCDEFGHIJKL";
        assert_eq!(ticker(long, 5, 0.0), "ABCDE");
        assert_eq!(ticker(long, 5, 1.0), "FGHIJ");
        assert_eq!(ticker(long, 5, 2.0), "KL   ");
        assert_eq!(ticker(long, 5, 2.6), "    A"); // into the gap, then round to the start
        assert_eq!(ticker(long, 5, 3.4), "ABCDE"); // a whole lap is 17 characters
    }
}
