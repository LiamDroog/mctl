//! Telemetry laid out for reading: titled sections, matrices where items share rows and columns
//! (CMG 1-4 by metric, solar arrays by metric), and numbers formatted by magnitude instead of a flat
//! list of IDs. Pure data shaping lives here so it is unit-tested without a window.

use std::sync::Arc;

use mctl_core::world::TelemetryItem;

use crate::grid::{CellRect, Grid, Limit, Role, Style, Widget};
use crate::page::Lines;

/// Widest row label shown before it is truncated, characters.
const MAX_LABEL: usize = 30;
/// Narrowest value column, characters.
const MIN_CELL: usize = 9;

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub text: String,
    pub stale: bool,
    pub value: Option<f64>,
    /// Recent values, oldest first (empty for text items).
    pub history: Arc<[f32]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: String,
    pub units: String,
    pub cells: Vec<Option<Cell>>,
}

/// Sections whose values are signed about zero (errors, rates, torques): drawn as centred bars.
const CENTERED_WORDS: [&str; 3] = ["error", "rate", "torque"];
/// Rows of a dial strip: dials are this many cells tall and twice as wide.
const DIAL_ROWS: usize = 3;
/// Narrowest sparkline worth drawing, cells.
const MIN_SPARK_CELLS: usize = 10;
/// Width of a centred bar, cells.
const CENTER_BAR_CELLS: usize = 14;

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: String,
    /// Column headings; a single empty heading means a plain list.
    pub cols: Vec<String>,
    pub rows: Vec<Row>,
}

impl Section {
    pub fn is_list(&self) -> bool {
        self.cols.len() == 1 && self.cols[0].is_empty()
    }

    /// Rows the box needs: content, a heading row for matrices, the dial strip if any, and the border.
    pub fn height(&self) -> usize {
        let strip = if self.dial_strip().is_some() {
            DIAL_ROWS + 1
        } else {
            0
        };
        self.rows.len() + usize::from(!self.is_list()) + strip + 2
    }

    /// One value column (a plain list, or a matrix with a single column such as "Error").
    pub fn is_single(&self) -> bool {
        self.cols.len() == 1
    }

    /// Whether this section's values are signed about zero, so bars start from the middle.
    pub fn is_centered(&self) -> bool {
        let t = self.title.to_ascii_lowercase();
        self.is_single() && CENTERED_WORDS.iter().any(|w| t.contains(w))
    }

    /// Angle readings to draw as dials: a list section titled "... angle" whose rows are all degrees,
    /// or a matrix row labelled "position" in degrees (one dial per column). Captions and degrees.
    pub fn dial_strip(&self) -> Option<Vec<(String, f64)>> {
        let t = self.title.to_ascii_lowercase();
        if self.is_single() && t.contains("angle") && self.rows.iter().all(|r| r.units == "deg") {
            let dials: Vec<(String, f64)> = self
                .rows
                .iter()
                .filter_map(|r| Some((r.label.clone(), r.cells.first()?.as_ref()?.value?)))
                .collect();
            return (!dials.is_empty()).then_some(dials);
        }
        let row = self
            .rows
            .iter()
            .find(|r| r.units == "deg" && r.label.to_ascii_lowercase().contains("position"))?;
        let dials: Vec<(String, f64)> = self
            .cols
            .iter()
            .zip(&row.cells)
            .filter_map(|(c, cell)| Some((c.clone(), cell.as_ref()?.value?)))
            .collect();
        (dials.len() >= 2).then_some(dials)
    }
}

/// Smallest 1, 2 or 5 times a power of ten that is at least `x` (scale for a centred bar).
pub fn nice_ceiling(x: f64) -> f64 {
    if !(x.is_finite() && x > 0.0) {
        return 1.0;
    }
    let mag = 10f64.powf(x.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|v| *v >= x * (1.0 - 1e-12))
        .unwrap_or(10.0 * mag)
}

/// Fill fractions for the numeric cells of a matrix row, each relative to the largest magnitude in
/// the row, so a bar says "how this compares with its neighbours", not "how close to a limit".
pub fn row_fractions(row: &Row) -> Vec<Option<f32>> {
    let values: Vec<f64> = row.cells.iter().flatten().filter_map(|c| c.value).collect();
    let min = values
        .iter()
        .copied()
        .map(f64::abs)
        .fold(f64::INFINITY, f64::min);
    let max = row
        .cells
        .iter()
        .flatten()
        .filter_map(|c| c.value)
        .map(f64::abs)
        .fold(0.0, f64::max);
    // An angle is not a magnitude, and a row whose values all agree has nothing to compare.
    let informative =
        row.units != "deg" && values.len() >= 2 && max > 0.0 && (max - min) / max > 0.02;
    row.cells
        .iter()
        .map(|c| {
            let v = c.as_ref()?.value?;
            informative.then(|| (v.abs() / max) as f32)
        })
        .collect()
}

/// A number for people: integers stay integers when there is no unit (state codes), everything else
/// gets enough digits to be useful and no more.
pub fn fmt_value(v: f64, units: &str) -> String {
    if !v.is_finite() {
        return "--".into();
    }
    if units.is_empty() && (v - v.round()).abs() < 1e-9 && v.abs() < 1.0e9 {
        return format!("{v:.0}");
    }
    if v == 0.0 {
        return "0.00".into();
    }
    match v.abs() {
        a if a >= 10_000.0 => format!("{v:.0}"),
        a if a >= 1_000.0 => format!("{v:.1}"),
        a if a >= 100.0 => format!("{v:.1}"),
        a if a >= 10.0 => format!("{v:.2}"),
        a if a >= 1.0 => format!("{v:.3}"),
        _ => format!("{v:.4}"),
    }
}

/// Sections of `group` in the order their first item appears. Items that have not arrived are absent;
/// a matrix cell with no item is `None` (shown as a dash, never as zero).
pub fn sections(
    items: &[TelemetryItem],
    group: &str,
    is_stale: &dyn Fn(&TelemetryItem) -> bool,
) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let in_group: Vec<&TelemetryItem> = items.iter().filter(|i| i.group == group).collect();
    let mut titles: Vec<&str> = Vec::new();
    for i in &in_group {
        if !titles.contains(&i.section.as_str()) {
            titles.push(&i.section);
        }
    }
    for title in titles {
        let its: Vec<&&TelemetryItem> = in_group.iter().filter(|i| i.section == title).collect();
        let mut cols: Vec<String> = Vec::new();
        let mut rows: Vec<(String, String)> = Vec::new();
        for i in &its {
            if !cols.contains(&i.col) {
                cols.push(i.col.clone());
            }
            if !rows.iter().any(|(r, _)| *r == i.row) {
                rows.push((i.row.clone(), i.units.clone()));
            }
        }
        let rows = rows
            .into_iter()
            .map(|(label, units)| Row {
                cells: cols
                    .iter()
                    .map(|c| {
                        its.iter()
                            .find(|i| i.row == label && i.col == *c)
                            .map(|i| Cell {
                                text: i.value.map_or_else(
                                    || i.text.clone().unwrap_or_else(|| "--".into()),
                                    |v| fmt_value(v, &i.units),
                                ),
                                stale: is_stale(i),
                                value: i.value,
                                history: i.history.clone(),
                            })
                    })
                    .collect(),
                label,
                units,
            })
            .collect();
        out.push(Section {
            title: title.to_owned(),
            cols,
            rows,
        });
    }
    out
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n.saturating_sub(1)).chain(['~']).collect()
    }
}

/// Draw one section as a titled box in `rect`.
pub fn render(grid: &mut Grid, rect: CellRect, sec: &Section) {
    grid.draw_box(
        rect,
        &sec.title.to_uppercase(),
        Style::new(Role::Border),
        Style::new(Role::Bright),
    );
    let mut l = Lines::new(grid, rect);
    let label_w = sec
        .rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0)
        .min(MAX_LABEL);
    let cell_w = sec
        .rows
        .iter()
        .flat_map(|r| r.cells.iter().flatten().map(|c| c.text.chars().count()))
        .chain(sec.cols.iter().map(|c| c.chars().count()))
        .max()
        .unwrap_or(0)
        .max(MIN_CELL)
        + 1;
    // Lay out: 1 margin, label, gap, value columns (right-aligned), gap, units.
    let units_x = 1 + label_w + 2 + cell_w * sec.cols.len() + 1;
    let units_w = sec
        .rows
        .iter()
        .map(|r| r.units.chars().count())
        .max()
        .unwrap_or(0);
    let (dim, fg, stale) = (
        Style::new(Role::Dim),
        Style::new(Role::Fg),
        Style::limit(Limit::Stale),
    );
    if !sec.is_list() {
        for (k, c) in sec.cols.iter().enumerate() {
            let x = 1 + label_w + 2 + cell_w * k;
            l.put(x + cell_w.saturating_sub(c.chars().count() + 1), c, dim);
        }
        l.row += 1;
    }
    let centered = sec.is_centered();
    let scale = nice_ceiling(
        sec.rows
            .iter()
            .flat_map(|r| r.cells.iter().flatten().filter_map(|c| c.value))
            .map(f64::abs)
            .fold(0.0, f64::max)
            * 1.25,
    );
    let after_units = units_x + units_w + 2;
    for r in &sec.rows {
        l.put(1, &truncate(&r.label, label_w), fg);
        let fracs = row_fractions(r);
        for (k, c) in r.cells.iter().enumerate() {
            let x = 1 + label_w + 2 + cell_w * k;
            // A value bar behind the number in matrix rows that carry units (a unitless value is a code).
            if let (false, Some(Some(f)), true) =
                (sec.is_single(), fracs.get(k), !r.units.is_empty())
                && l.row < l.rect.rows
            {
                l.grid.widget(Widget::DataBar {
                    rect: CellRect::new(
                        l.rect.col + x,
                        l.rect.row + l.row,
                        cell_w.saturating_sub(1),
                        1,
                    ),
                    frac: *f,
                });
            }
            let (text, style) = match c {
                Some(c) if c.stale => (format!("{}!", c.text), stale),
                Some(c) => (c.text.clone(), Style::new(Role::Bright)),
                None => ("--".to_owned(), dim),
            };
            l.put(
                x + cell_w.saturating_sub(text.chars().count() + 1),
                &text,
                style,
            );
        }
        l.put(units_x, &r.units, dim);
        if sec.is_single()
            && let Some(Some(cell)) = r.cells.first()
            && !r.units.is_empty()
            && l.row < l.rect.rows
        {
            let mut x = after_units;
            if let (true, Some(v)) = (centered, cell.value)
                && x + CENTER_BAR_CELLS + 6 < l.rect.cols
            {
                l.grid.widget(Widget::Bar {
                    rect: CellRect::new(l.rect.col + x, l.rect.row + l.row, CENTER_BAR_CELLS, 1),
                    frac: (v / scale) as f32,
                    centered: true,
                    limit: None,
                    ticks: Vec::new(),
                });
                let label = format!("+-{}", fmt_value(scale, ""));
                l.put(x + CENTER_BAR_CELLS + 1, &label, dim);
                x += CENTER_BAR_CELLS + label.chars().count() + 2;
            }
            let room = l.rect.cols.saturating_sub(x + 1);
            if room >= MIN_SPARK_CELLS && cell.history.len() >= 3 {
                l.grid.widget(Widget::Spark {
                    rect: CellRect::new(l.rect.col + x, l.rect.row + l.row, room, 1),
                    values: cell.history.clone(),
                    limit: None,
                });
            }
        }
        l.row += 1;
    }
    if let Some(dials) = sec.dial_strip() {
        let slot = DIAL_ROWS * 2 + 5;
        let usable = l.rect.cols.saturating_sub(1) / slot;
        let base = l.row;
        for (k, (caption, deg)) in dials.iter().take(usable).enumerate() {
            let x = 1 + k * slot;
            if base + DIAL_ROWS + 1 > l.rect.rows {
                break;
            }
            l.grid.widget(Widget::Dial {
                rect: CellRect::new(l.rect.col + x, l.rect.row + base, DIAL_ROWS * 2, DIAL_ROWS),
                deg: *deg as f32,
                limit: None,
            });
            l.row = base + DIAL_ROWS;
            l.put(x, &truncate(caption, slot - 1), dim);
        }
    }
}

/// Natural width of a section box, characters (borders included).
pub fn width(sec: &Section) -> usize {
    let label_w = sec
        .rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0)
        .min(MAX_LABEL);
    let units_w = sec
        .rows
        .iter()
        .map(|r| r.units.chars().count())
        .max()
        .unwrap_or(0);
    let cell_w = sec
        .rows
        .iter()
        .flat_map(|r| r.cells.iter().flatten().map(|c| c.text.chars().count()))
        .chain(sec.cols.iter().map(|c| c.chars().count()))
        .max()
        .unwrap_or(0)
        .max(MIN_CELL)
        + 1;
    2 + 1 + label_w + 2 + cell_w * sec.cols.len() + 1 + units_w + 1
}

/// Place sections into `columns` columns of a region, each section in the currently shortest column.
/// A section too wide for one column goes first, across the whole region. Returns (section index,
/// rectangle); sections that do not fit vertically are left out.
pub fn pack(secs: &[Section], region: CellRect, columns: usize) -> Vec<(usize, CellRect)> {
    let columns = columns.max(1);
    let col_w = region.cols / columns;
    let mut out = Vec::new();
    let mut top = 0usize;
    if columns > 1 {
        for (idx, s) in secs.iter().enumerate() {
            if width(s) > col_w && top + s.height() <= region.rows {
                out.push((
                    idx,
                    CellRect::new(region.col, region.row + top, region.cols, s.height()),
                ));
                top += s.height();
            }
        }
    }
    let placed: Vec<usize> = out.iter().map(|(i, _)| *i).collect();
    let mut heights = vec![top; columns];
    for (idx, s) in secs.iter().enumerate() {
        if placed.contains(&idx) || (columns > 1 && width(s) > col_w) {
            continue;
        }
        let c = (0..columns).min_by_key(|c| heights[*c]).unwrap_or(0);
        let h = s.height();
        if heights[c] + h > region.rows {
            continue;
        }
        let w = if c + 1 == columns {
            region.cols - col_w * c
        } else {
            col_w
        };
        out.push((
            idx,
            CellRect::new(region.col + c * col_w, region.row + heights[c], w, h),
        ));
        heights[c] += h;
    }
    out
}

/// One history chart: several series sharing units.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartSpec {
    pub title: String,
    pub series: Vec<(String, Arc<[f32]>)>,
}

/// Charts worth drawing for these sections. A single-column section charts its rows together, one
/// chart per distinct unit; a matrix charts each row that has units, one series per column. Series
/// with fewer than three samples are dropped.
pub fn charts(secs: &[Section]) -> Vec<ChartSpec> {
    let usable = |h: &Arc<[f32]>| h.len() >= 3;
    let mut out = Vec::new();
    for sec in secs {
        if sec.is_single() {
            let mut units: Vec<&str> = Vec::new();
            for r in sec.rows.iter().filter(|r| !r.units.is_empty()) {
                if !units.contains(&r.units.as_str()) {
                    units.push(&r.units);
                }
            }
            for u in units {
                let series: Vec<(String, Arc<[f32]>)> = sec
                    .rows
                    .iter()
                    .filter(|r| r.units == u)
                    .filter_map(|r| {
                        let h = &r.cells.first()?.as_ref()?.history;
                        usable(h).then(|| (r.label.clone(), h.clone()))
                    })
                    .collect();
                if !series.is_empty() {
                    out.push(ChartSpec {
                        title: format!("{} ({u})", sec.title),
                        series,
                    });
                }
            }
        } else {
            for r in sec
                .rows
                .iter()
                .filter(|r| !r.units.is_empty() && r.units != "deg")
            {
                let series: Vec<(String, Arc<[f32]>)> = sec
                    .cols
                    .iter()
                    .zip(&r.cells)
                    .filter_map(|(c, cell)| {
                        let h = &cell.as_ref()?.history;
                        usable(h).then(|| (c.clone(), h.clone()))
                    })
                    .collect();
                if !series.is_empty() {
                    out.push(ChartSpec {
                        title: format!("{} ({})", r.label, r.units),
                        series,
                    });
                }
            }
        }
    }
    out
}

/// Most charts drawn on one page, and their grid shape.
const MAX_CHARTS: usize = 6;
const CHART_COLUMNS: usize = 3;
/// Shortest chart box worth drawing, rows.
const MIN_CHART_ROWS: usize = 8;

/// Draw `specs` as a grid of titled chart boxes filling `region` (nothing if it is too small).
pub fn render_charts(grid: &mut Grid, region: CellRect, specs: &[ChartSpec]) {
    if region.rows < MIN_CHART_ROWS || region.cols < 30 {
        return;
    }
    let cols = CHART_COLUMNS.min(specs.len());
    // As many charts as fit, in rows of `cols`, each at least MIN_CHART_ROWS tall.
    let n = specs
        .len()
        .min(MAX_CHARTS)
        .min(region.rows / MIN_CHART_ROWS * cols);
    if n == 0 {
        return;
    }
    let row_h = region.rows / n.div_ceil(cols);
    let col_w = region.cols / cols;
    for (i, spec) in specs.iter().take(n).enumerate() {
        let (c, r) = (i % cols, i / cols);
        let w = if c + 1 == cols {
            region.cols - col_w * c
        } else {
            col_w
        };
        let rect = CellRect::new(region.col + c * col_w, region.row + r * row_h, w, row_h);
        grid.draw_box(
            rect,
            &spec.title.to_uppercase(),
            Style::new(Role::Border),
            Style::new(Role::Bright),
        );
        grid.widget(Widget::Chart {
            rect: rect.inner(),
            series: spec.series.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use mctl_core::time::Utc;

    use super::*;

    fn cell(v: Option<f64>, hist: &[f32]) -> Option<Cell> {
        Some(Cell {
            text: v.map_or("--".into(), |v| v.to_string()),
            stale: false,
            value: v,
            history: hist.to_vec().into(),
        })
    }

    fn section(title: &str, cols: &[&str], rows: Vec<Row>) -> Section {
        Section {
            title: title.into(),
            cols: cols.iter().map(|c| (*c).into()).collect(),
            rows,
        }
    }

    fn row(label: &str, units: &str, cells: Vec<Option<Cell>>) -> Row {
        Row {
            label: label.into(),
            units: units.into(),
            cells,
        }
    }

    #[test]
    fn centred_scales_are_one_two_five_steps() {
        assert_eq!(nice_ceiling(0.15), 0.2);
        assert_eq!(nice_ceiling(0.2), 0.2);
        assert_eq!(nice_ceiling(0.21), 0.5);
        assert_eq!(nice_ceiling(3.0), 5.0);
        assert_eq!(nice_ceiling(7.0), 10.0);
        assert_eq!(nice_ceiling(0.0), 1.0);
        assert_eq!(nice_ceiling(f64::NAN), 1.0);
    }

    #[test]
    fn row_bars_compare_neighbours_and_skip_angles_codes_and_flat_rows() {
        let r = row(
            "Temp",
            "degC",
            vec![
                cell(Some(20.0), &[]),
                cell(Some(40.0), &[]),
                cell(None, &[]),
            ],
        );
        assert_eq!(row_fractions(&r), vec![Some(0.5), Some(1.0), None]);
        let flat = row(
            "V",
            "V",
            vec![cell(Some(151.2), &[]), cell(Some(151.3), &[])],
        );
        assert_eq!(
            row_fractions(&flat),
            vec![None, None],
            "values that agree carry no comparison"
        );
        let angle = row(
            "BGA position",
            "deg",
            vec![cell(Some(10.0), &[]), cell(Some(300.0), &[])],
        );
        assert!(
            row_fractions(&angle).iter().all(Option::is_none),
            "an angle is not a magnitude"
        );
        let neg = row(
            "Rate",
            "deg/s",
            vec![cell(Some(-2.0), &[]), cell(Some(1.0), &[])],
        );
        assert_eq!(
            row_fractions(&neg),
            vec![Some(1.0), Some(0.5)],
            "bars show magnitude"
        );
    }

    #[test]
    fn sections_pick_their_visual_from_title_and_units() {
        let err = section(
            "Attitude error (current - commanded)",
            &["Error"],
            vec![row("Roll", "deg", vec![cell(Some(0.1), &[])])],
        );
        assert!(err.is_single() && err.is_centered());
        let sarj = section(
            "SARJ angle",
            &["Angle"],
            vec![
                row("Starboard", "deg", vec![cell(Some(124.0), &[])]),
                row("Port", "deg", vec![cell(Some(236.1), &[])]),
            ],
        );
        assert_eq!(
            sarj.dial_strip().unwrap(),
            vec![("Starboard".to_owned(), 124.0), ("Port".to_owned(), 236.1)]
        );
        assert_eq!(
            sarj.height(),
            2 + 1 + 4 + 2,
            "rows + heading + dial strip + border"
        );
        let bga = section(
            "Solar arrays",
            &["2A", "4A"],
            vec![row(
                "BGA position",
                "deg",
                vec![cell(Some(46.0), &[]), cell(Some(313.0), &[])],
            )],
        );
        assert_eq!(bga.dial_strip().unwrap().len(), 2);
        let plain = section(
            "Atmosphere",
            &["Lab"],
            vec![row("ppO2", "mmHg", vec![cell(Some(160.0), &[])])],
        );
        assert!(plain.dial_strip().is_none() && !plain.is_centered());
    }

    #[test]
    fn charts_group_single_column_rows_by_unit_and_matrix_rows_by_row() {
        let h: &[f32] = &[1.0, 2.0, 3.0];
        let short: &[f32] = &[1.0];
        let rate = section(
            "Inertial attitude rate",
            &["Rate"],
            vec![
                row("X axis", "deg/s", vec![cell(Some(0.0), h)]),
                row("Y axis", "deg/s", vec![cell(Some(0.0), h)]),
                row("Z axis", "deg/s", vec![cell(Some(0.0), short)]),
            ],
        );
        let temps = section(
            "CMG temperatures",
            &["CMG 1", "CMG 2"],
            vec![
                row(
                    "Spin bearing temp",
                    "degC",
                    vec![cell(Some(1.0), h), cell(Some(2.0), h)],
                ),
                row("State", "", vec![cell(Some(1.0), h), cell(Some(1.0), h)]),
                row(
                    "BGA position",
                    "deg",
                    vec![cell(Some(1.0), h), cell(Some(1.0), h)],
                ),
            ],
        );
        let c = charts(&[rate, temps]);
        assert_eq!(c.len(), 2, "unitless codes and angles are not charted");
        assert_eq!(c[0].title, "Inertial attitude rate (deg/s)");
        assert_eq!(c[0].series.len(), 2, "a series with one sample is dropped");
        assert_eq!(c[1].title, "Spin bearing temp (degC)");
        assert_eq!(c[1].series[1].0, "CMG 2");
    }

    #[test]
    fn a_section_wider_than_a_column_spans_the_region_and_nothing_overlaps() {
        let wide = section(
            "Solar arrays",
            &["a", "b", "c", "d", "e", "f", "g", "h"],
            vec![row(
                "BGA position",
                "deg",
                (0..8).map(|_| cell(Some(1.0), &[])).collect(),
            )],
        );
        let narrow = section(
            "Small",
            &["x"],
            vec![row("r", "V", vec![cell(Some(1.0), &[])])],
        );
        let region = CellRect::new(0, 1, 100, 30);
        let placed = pack(&[narrow.clone(), wide.clone(), narrow], region, 2);
        assert_eq!(placed.len(), 3);
        let wide_rect = placed.iter().find(|(i, _)| *i == 1).unwrap().1;
        assert_eq!(wide_rect.cols, 100, "the wide section spans the region");
        for (_, r) in placed.iter().filter(|(i, _)| *i != 1) {
            assert!(
                r.row >= wide_rect.row + wide_rect.rows,
                "narrow sections sit below it"
            );
        }
    }

    #[test]
    fn chart_boxes_fill_the_free_area_only_when_they_fit() {
        let mut g = Grid::new(120, 40);
        let spec = |n: &str| ChartSpec {
            title: n.into(),
            series: vec![("s".into(), vec![1.0f32, 2.0, 3.0].into())],
        };
        render_charts(&mut g, CellRect::new(0, 30, 120, 5), &[spec("a")]);
        assert!(g.take_widgets().is_empty(), "too short for a chart");
        render_charts(
            &mut g,
            CellRect::new(0, 10, 120, 12),
            &[spec("a"), spec("b"), spec("c"), spec("d")],
        );
        let w = g.take_widgets();
        assert_eq!(
            w.len(),
            3,
            "three across; a second row would leave each under 8 rows tall"
        );
        assert!(w.iter().all(|w| w.rect().rows >= MIN_CHART_ROWS));
    }

    fn item(
        id: &str,
        section: &str,
        row: &str,
        col: &str,
        units: &str,
        value: Option<f64>,
        age: f64,
    ) -> TelemetryItem {
        TelemetryItem {
            id: id.into(),
            group: "g".into(),
            description: id.into(),
            label: row.into(),
            section: section.into(),
            row: row.into(),
            col: col.into(),
            units: units.into(),
            value,
            text: value.map(|v| v.to_string()),
            rx: Utc(0.0),
            age_s: age,
            history: Default::default(),
        }
    }

    #[test]
    fn numbers_are_formatted_by_magnitude_and_codes_stay_integers() {
        assert_eq!(fmt_value(2.0, ""), "2", "a state code is not 2.0000");
        assert_eq!(fmt_value(6601.0, "rpm"), "6601.0");
        assert_eq!(fmt_value(19_522.612_7, "Nms"), "19523");
        assert_eq!(fmt_value(749.0583, "mmHg"), "749.1");
        assert_eq!(fmt_value(34.3012, "degC"), "34.30");
        assert_eq!(fmt_value(2.4351, "mmHg"), "2.435");
        assert_eq!(
            fmt_value(0.9971, ""),
            "0.9971",
            "quaternion components keep four digits"
        );
        assert_eq!(fmt_value(-0.0674, ""), "-0.0674");
        assert_eq!(fmt_value(f64::NAN, "x"), "--");
        assert_eq!(
            fmt_value(0.0, "A"),
            "0.00",
            "a true zero is not four digits of nothing"
        );
    }

    #[test]
    fn items_sharing_rows_and_columns_become_a_matrix_with_dashes_for_gaps() {
        let items = vec![
            item(
                "a",
                "Health",
                "Wheel speed",
                "CMG 1",
                "rpm",
                Some(6601.0),
                1.0,
            ),
            item(
                "b",
                "Health",
                "Wheel speed",
                "CMG 2",
                "rpm",
                Some(6600.0),
                1.0,
            ),
            item("c", "Health", "Vibration", "CMG 1", "g", Some(0.0107), 1.0),
            item("d", "Other", "Mode", "", "", Some(2.0), 1.0),
        ];
        let s = sections(&items, "g", &|i| i.age_s > 30.0);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].title, "Health");
        assert_eq!(s[0].cols, vec!["CMG 1", "CMG 2"]);
        assert_eq!(s[0].rows[0].label, "Wheel speed");
        assert_eq!(s[0].rows[0].cells[1].as_ref().unwrap().text, "6600.0");
        assert!(
            s[0].rows[1].cells[1].is_none(),
            "no Vibration item for CMG 2: a gap, not a zero"
        );
        assert!(s[1].is_list());
        assert_eq!(s[1].rows[0].cells[0].as_ref().unwrap().text, "2");
        assert_eq!(
            s[0].height(),
            2 + 1 + 2,
            "two rows, a heading row and the borders"
        );
        assert_eq!(s[1].height(), 1 + 2);
    }

    #[test]
    fn only_the_requested_group_and_only_received_items_appear() {
        let mut other = item("x", "S", "r", "", "", Some(1.0), 0.0);
        other.group = "other".into();
        assert!(sections(&[other], "g", &|i| i.age_s > 30.0).is_empty());
        assert!(sections(&[], "g", &|i| i.age_s > 30.0).is_empty());
    }

    #[test]
    fn stale_cells_are_flagged_by_age() {
        let s = sections(
            &[
                item("a", "S", "r", "", "u", Some(1.0), 31.0),
                item("b", "S", "q", "", "u", Some(1.0), 5.0),
            ],
            "g",
            &|i| i.age_s > 30.0,
        );
        assert!(s[0].rows[0].cells[0].as_ref().unwrap().stale);
        assert!(!s[0].rows[1].cells[0].as_ref().unwrap().stale);
    }

    #[test]
    fn packing_balances_columns_stays_inside_the_region_and_drops_what_does_not_fit() {
        let sec = |n: usize| Section {
            title: "t".into(),
            cols: vec![String::new()],
            rows: (0..n)
                .map(|i| Row {
                    label: format!("r{i}"),
                    units: String::new(),
                    cells: vec![None],
                })
                .collect(),
        };
        let secs = vec![sec(10), sec(2), sec(2), sec(30)];
        let region = CellRect::new(0, 1, 100, 20);
        let placed = pack(&secs, region, 2);
        let idx: Vec<usize> = placed.iter().map(|p| p.0).collect();
        assert_eq!(
            idx,
            vec![0, 1, 2],
            "the 30-row section cannot fit and is dropped, not clipped"
        );
        assert_eq!(placed[0].1.col, 0);
        assert_eq!(
            placed[1].1.col, 50,
            "second section goes to the emptier column"
        );
        assert_eq!(
            placed[2].1.col, 50,
            "and the third stacks there while it is still shorter"
        );
        assert!(
            placed
                .iter()
                .all(|(_, r)| r.row >= 1 && r.row + r.rows <= 21 && r.col + r.cols <= 100)
        );
    }

    #[test]
    fn rendering_a_matrix_aligns_headings_values_and_units() {
        let items = vec![
            item(
                "a",
                "Health",
                "Wheel speed",
                "CMG 1",
                "rpm",
                Some(6601.0),
                1.0,
            ),
            item(
                "b",
                "Health",
                "Wheel speed",
                "CMG 2",
                "rpm",
                Some(6600.0),
                1.0,
            ),
        ];
        let s = &sections(&items, "g", &|i| i.age_s > 30.0)[0];
        let mut g = Grid::new(60, 8);
        render(&mut g, CellRect::new(0, 0, width(s).min(60), s.height()), s);
        let t = g.text();
        assert!(t.contains("HEALTH"), "{t}");
        let heading = g.row_text(1);
        let row = g.row_text(2);
        assert!(
            heading.contains("CMG 1") && heading.contains("CMG 2"),
            "{heading:?}"
        );
        assert!(
            row.contains("Wheel speed")
                && row.contains("6601.0")
                && row.contains("6600.0")
                && row.contains("rpm"),
            "{row:?}"
        );
        // Right-aligned: each value ends where its heading ends.
        let end = |line: &str, needle: &str| line.find(needle).map(|p| p + needle.len());
        assert_eq!(
            end(&heading, "CMG 1"),
            end(&row, "6601.0"),
            "{heading:?} / {row:?}"
        );
    }
}
