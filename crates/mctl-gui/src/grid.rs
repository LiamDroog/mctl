//! Fixed character grid: the console's text model.
//!
//! Panels write styled characters into the grid; [`Grid::paint`] turns runs of identical
//! style into egui text at exact cell positions. Graphical panels are cell rectangles the
//! grid leaves untouched, so the wgpu passes underneath show through.

use egui::{Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Vec2};

use crate::theme::{LimitStyle, Rgb, Theme};

/// Box-drawing characters used for panel borders.
const BOX_TL: char = '┌';
const BOX_TR: char = '┐';
const BOX_BL: char = '└';
const BOX_BR: char = '┘';
const BOX_H: char = '─';
const BOX_V: char = '│';

/// Semantic colour roles resolved through the active theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Fg,
    Dim,
    Bright,
    Border,
    Nominal,
    Info,
}

/// Limit/data-quality states, encoded per theme without relying on hue alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Caution,
    Warning,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub role: Role,
    pub limit: Option<Limit>,
    /// Paint the theme background behind these cells (overlays that must hide what is underneath).
    pub solid: bool,
}

impl Style {
    pub const fn new(role: Role) -> Self {
        Self {
            role,
            limit: None,
            solid: false,
        }
    }

    pub const fn solid(self) -> Self {
        Self {
            solid: true,
            ..self
        }
    }
    pub const fn limit(limit: Limit) -> Self {
        Self {
            role: Role::Fg,
            limit: Some(limit),
            solid: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Cell {
    ch: char,
    style: Style,
}

/// A rectangle in cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
}

impl CellRect {
    pub const fn new(col: usize, row: usize, cols: usize, rows: usize) -> Self {
        Self {
            col,
            row,
            cols,
            rows,
        }
    }
    /// The same rectangle moved by (`dc`, `dr`) cells.
    pub fn shifted(self, dc: usize, dr: usize) -> Self {
        Self::new(self.col + dc, self.row + dr, self.cols, self.rows)
    }
    /// The area inside a one-cell border.
    pub fn inner(self) -> Self {
        Self::new(
            self.col + 1,
            self.row + 1,
            self.cols.saturating_sub(2),
            self.rows.saturating_sub(2),
        )
    }
}

/// A pixel-drawn element that sits in a cell rectangle the text grid leaves empty. Panels queue them
/// while laying out text; the console paints them over the same rectangle.
#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    /// Horizontal gauge. `frac` is 0..1 filled from the left; when `centered`, -1..1 from the middle.
    /// `ticks` are marks at 0..1 positions.
    Bar {
        rect: CellRect,
        frac: f32,
        centered: bool,
        limit: Option<Limit>,
        ticks: Vec<f32>,
    },
    /// A faint bar drawn behind a number: `frac` of the cell width, relative to its row's largest value.
    DataBar { rect: CellRect, frac: f32 },
    /// Line of recent values, scaled to its own min and max.
    Spark {
        rect: CellRect,
        values: std::sync::Arc<[f32]>,
        limit: Option<Limit>,
    },
    /// History chart: each series is evenly spaced samples ending now (`HISTORY_PERIOD_S` apart).
    Chart {
        rect: CellRect,
        series: Vec<(String, std::sync::Arc<[f32]>)>,
    },
    /// Rotating wireframe art of a vehicle (`id` is the vehicle id).
    Art { rect: CellRect, id: String },
    /// Stars behind the art, and the limb of the Earth (`earth`) or a small Earth far off (L2).
    Backdrop { rect: CellRect, earth: bool },
    /// A survey picture of a field (`key` is the field request key on the selected vehicle).
    Image { rect: CellRect, key: String },
    /// Circular angle dial with a needle at `deg` (0 at the top, clockwise).
    Dial {
        rect: CellRect,
        deg: f32,
        limit: Option<Limit>,
    },
    /// A plot drawn from the selected vehicle's data.
    Plot { rect: CellRect, kind: PlotKind },
}

impl Widget {
    /// This widget moved by (`dc`, `dr`) cells.
    pub fn shifted(mut self, dc: usize, dr: usize) -> Self {
        let r = match &mut self {
            Self::Bar { rect, .. }
            | Self::DataBar { rect, .. }
            | Self::Chart { rect, .. }
            | Self::Image { rect, .. }
            | Self::Art { rect, .. }
            | Self::Backdrop { rect, .. }
            | Self::Spark { rect, .. }
            | Self::Dial { rect, .. }
            | Self::Plot { rect, .. } => rect,
        };
        *r = r.shifted(dc, dr);
        self
    }

    pub fn rect(&self) -> CellRect {
        match self {
            Self::Bar { rect, .. }
            | Self::DataBar { rect, .. }
            | Self::Chart { rect, .. }
            | Self::Image { rect, .. }
            | Self::Art { rect, .. }
            | Self::Backdrop { rect, .. }
            | Self::Spark { rect, .. }
            | Self::Dial { rect, .. }
            | Self::Plot { rect, .. } => *rect,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlotKind {
    /// L2 halo orbit seen from Earth (Y-Z).
    Halo,
    /// L2 orbit from ecliptic north (X-Y), with the Moon's orbit for scale.
    TopView,
    /// Distance, off-axis angle, Y/Z offsets or range rate against time, from the track.
    DeepSeries(DeepSeries),
    /// A deep-space vehicle's path among the planets' orbits, from ecliptic north, on a square-root radial scale.
    SolarSystem,
    /// R-bar/V-bar relative motion of the visiting vehicles about the station.
    Prox,
    /// Altitude of the nominal and planned orbits against time.
    PlanAltitude,
    /// Distance between the planned and nominal vehicle against time.
    PlanSeparation,
    /// JWST visit timeline by instrument.
    Timeline,
    /// Hours of observing per instrument over the schedule.
    InstrumentShare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepSeries {
    Distance,
    OffAxis,
    Offsets,
    RangeRate,
    /// Deep space: distance from the Sun (AU), from Earth as light time (hours), and the heliocentric
    /// ecliptic longitude and latitude (degrees).
    SunDistance,
    EarthLight,
    EclLon,
    EclLat,
}

pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    cells: Vec<Cell>,
    widgets: Vec<Widget>,
}

/// A box title cut to `max` characters. Titles are a name and then asides after a double space ("GROUND TRACK
/// PAST SOLID / PREDICTED DASHED"); in a narrow box the asides go first, and only a name that is itself too
/// long is cut, ending in `~`.
pub(crate) fn fit_title(title: &str, max: usize) -> String {
    let mut shown = String::new();
    for (i, part) in title.split("  ").filter(|p| !p.is_empty()).enumerate() {
        let next = if i == 0 {
            part.to_owned()
        } else {
            format!("{shown}  {part}")
        };
        if next.chars().count() > max {
            break;
        }
        shown = next;
    }
    if shown.is_empty() {
        let n = title.chars().count();
        if n <= max {
            return title.to_owned();
        }
        return title
            .chars()
            .take(max.saturating_sub(1))
            .chain(['~'])
            .collect();
    }
    shown
}

impl Grid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let blank = Cell {
            ch: ' ',
            style: Style::new(Role::Fg),
        };
        Self {
            cols,
            rows,
            cells: vec![blank; cols * rows],
            widgets: Vec::new(),
        }
    }

    /// Copy `src` into this grid with its top-left corner at (`col`, `row`); what falls outside is clipped.
    pub fn blit(&mut self, src: &Grid, col: usize, row: usize) {
        for r in 0..src.rows {
            for c in 0..src.cols {
                let (dc, dr) = (col + c, row + r);
                if dc < self.cols && dr < self.rows {
                    self.cells[dr * self.cols + dc] = src.cells[r * src.cols + c];
                }
            }
        }
    }

    /// Queue a pixel-drawn widget for the console to paint.
    pub fn widget(&mut self, w: Widget) {
        self.widgets.push(w);
    }

    /// Hand over the queued widgets (once per page render).
    pub fn take_widgets(&mut self) -> Vec<Widget> {
        std::mem::take(&mut self.widgets)
    }

    /// Write a string starting at (col, row). Anything outside the grid is clipped, never a panic:
    /// panels must survive being laid out into a window that is too small.
    pub fn put(&mut self, col: usize, row: usize, text: &str, style: Style) {
        if row >= self.rows {
            return;
        }
        for (i, ch) in text.chars().enumerate() {
            let c = col + i;
            if c >= self.cols {
                break;
            }
            self.cells[row * self.cols + c] = Cell { ch, style };
        }
    }

    /// Fill a rectangle with spaces of a style (used to blank a region for an overlay).
    /// The characters of one row (for tests and diagnostics).
    #[cfg(test)]
    pub fn row_text(&self, row: usize) -> String {
        self.cells[row * self.cols..(row + 1) * self.cols]
            .iter()
            .map(|c| c.ch)
            .collect()
    }

    /// All rows joined with newlines.
    #[cfg(test)]
    pub fn text(&self) -> String {
        (0..self.rows)
            .map(|r| self.row_text(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn fill(&mut self, rect: CellRect, style: Style) {
        for r in rect.row..(rect.row + rect.rows).min(self.rows) {
            for c in rect.col..(rect.col + rect.cols).min(self.cols) {
                self.cells[r * self.cols + c] = Cell { ch: ' ', style };
            }
        }
    }

    /// Draw a titled box border. Rectangles smaller than 2×2 are ignored.
    pub fn draw_box(&mut self, rect: CellRect, title: &str, border: Style, title_style: Style) {
        if rect.cols < 2 || rect.rows < 2 {
            return;
        }
        let (right, bottom) = (rect.col + rect.cols - 1, rect.row + rect.rows - 1);
        let horiz: String = std::iter::repeat_n(BOX_H, rect.cols - 2).collect();
        self.put(
            rect.col,
            rect.row,
            &format!("{BOX_TL}{horiz}{BOX_TR}"),
            border,
        );
        self.put(
            rect.col,
            bottom,
            &format!("{BOX_BL}{horiz}{BOX_BR}"),
            border,
        );
        for r in rect.row + 1..bottom {
            self.put(rect.col, r, &BOX_V.to_string(), border);
            self.put(right, r, &BOX_V.to_string(), border);
        }
        if !title.is_empty() && rect.cols > 4 {
            let max = rect.cols - 4;
            let shown = fit_title(title, max);
            self.put(rect.col + 2, rect.row, &format!(" {shown} "), title_style);
        }
    }

    /// Paint the grid with egui. `origin` is the top-left of the grid in points; `time_s` drives blink.
    pub fn paint(&self, painter: &Painter, theme: &Theme, origin: Pos2, time_s: f64) {
        let family = match theme.font.family {
            crate::theme::FontFamily::Unscii => FontFamily::Name("unscii".into()),
            crate::theme::FontFamily::Jetbrains => FontFamily::Name("jetbrains".into()),
        };
        let font = FontId::new(theme.font.size_px, family);
        let (cw, ch) = (theme.font.cell_w_px, theme.font.cell_h_px);
        for row in 0..self.rows {
            let cells = &self.cells[row * self.cols..(row + 1) * self.cols];
            let mut col = 0;
            while col < self.cols {
                let style = cells[col].style;
                let mut end = col;
                while end < self.cols && cells[end].style == style {
                    end += 1;
                }
                let text: String = cells[col..end].iter().map(|c| c.ch).collect();
                if !text.trim().is_empty() || style.limit.is_some() || style.solid {
                    let top_left = origin + Vec2::new(col as f32 * cw, row as f32 * ch);
                    paint_run(
                        painter,
                        theme,
                        &font,
                        top_left,
                        Vec2::new((end - col) as f32 * cw, ch),
                        &text,
                        style,
                        time_s,
                    );
                }
                col = end;
            }
        }
    }
}

fn limit_style(theme: &Theme, limit: Limit) -> LimitStyle {
    match limit {
        Limit::Caution => theme.limit.caution,
        Limit::Warning => theme.limit.warning,
        Limit::Stale => theme.limit.stale,
    }
}

fn role_colour(theme: &Theme, role: Role) -> Rgb {
    let p = &theme.palette;
    match role {
        Role::Fg => p.foreground,
        Role::Dim => p.dim,
        Role::Bright => p.bright,
        Role::Border => p.border,
        Role::Nominal => p.nominal,
        Role::Info => p.info,
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_run(
    painter: &Painter,
    theme: &Theme,
    font: &FontId,
    top_left: Pos2,
    size: Vec2,
    text: &str,
    style: Style,
    time_s: f64,
) {
    let (mut fg, mut inverse) = (role_colour(theme, style.role), false);
    if let Some(limit) = style.limit {
        let ls = limit_style(theme, limit);
        if ls.blink_hz > 0.0 {
            // Blink toggles at `blink_hz` full on/off cycles per second.
            let phase = (time_s * f64::from(ls.blink_hz) * 2.0).floor() as i64;
            if phase % 2 != 0 {
                return;
            }
        }
        inverse = ls.inverse;
        fg = if ls.semantic_colour {
            match limit {
                Limit::Caution => theme.palette.caution,
                Limit::Warning => theme.palette.warning,
                Limit::Stale => fg,
            }
        } else {
            theme.palette.foreground
        };
        if ls.dim {
            fg = theme.palette.dim;
        }
    }
    let rect = Rect::from_min_size(top_left, size);
    if style.solid {
        painter.rect_filled(rect, 0.0, theme.palette.background.to_egui());
    }
    let text_colour: Color32 = if inverse {
        painter.rect_filled(rect, 0.0, fg.to_egui());
        theme.palette.background.to_egui()
    } else {
        fg.to_egui()
    };
    painter.text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        text,
        font.clone(),
        text_colour,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_clips_instead_of_panicking() {
        let mut g = Grid::new(4, 2);
        g.put(2, 0, "abcdef", Style::new(Role::Fg));
        g.put(0, 5, "x", Style::new(Role::Fg));
        assert_eq!(g.cells[2].ch, 'a');
        assert_eq!(g.cells[3].ch, 'b');
    }

    #[test]
    fn box_corners_land_where_expected() {
        let mut g = Grid::new(10, 5);
        g.draw_box(
            CellRect::new(1, 1, 6, 3),
            "T",
            Style::new(Role::Border),
            Style::new(Role::Bright),
        );
        assert_eq!(g.cells[10 + 1].ch, BOX_TL);
        assert_eq!(g.cells[10 + 6].ch, BOX_TR);
        assert_eq!(g.cells[3 * 10 + 1].ch, BOX_BL);
        assert_eq!(g.cells[3 * 10 + 6].ch, BOX_BR);
    }

    #[test]
    fn degenerate_box_is_ignored() {
        let mut g = Grid::new(3, 3);
        g.draw_box(
            CellRect::new(0, 0, 1, 3),
            "x",
            Style::new(Role::Border),
            Style::new(Role::Fg),
        );
        assert!(g.cells.iter().all(|c| c.ch == ' '));
    }
}
