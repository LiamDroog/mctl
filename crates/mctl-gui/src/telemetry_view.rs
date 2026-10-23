//! Telemetry laid out for reading: titled sections, matrices where items share rows and columns
//! (CMG 1-4 by metric, solar arrays by metric), and numbers formatted by magnitude instead of a flat
//! list of IDs. Pure data shaping lives here so it is unit-tested without a window.

use mctl_core::world::TelemetryItem;

use crate::grid::{CellRect, Grid, Limit, Role, Style};
use crate::page::Lines;

/// Widest row label shown before it is truncated, characters.
const MAX_LABEL: usize = 30;
/// Narrowest value column, characters.
const MIN_CELL: usize = 9;

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub text: String,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: String,
    pub units: String,
    pub cells: Vec<Option<Cell>>,
}

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

    /// Rows the box needs: content, a heading row for matrices, and the two border rows.
    pub fn height(&self) -> usize {
        self.rows.len() + usize::from(!self.is_list()) + 2
    }
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
pub fn sections(items: &[TelemetryItem], group: &str, stale_after_s: f64) -> Vec<Section> {
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
                                stale: i.age_s > stale_after_s,
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
    for r in &sec.rows {
        l.put(1, &truncate(&r.label, label_w), fg);
        for (k, c) in r.cells.iter().enumerate() {
            let x = 1 + label_w + 2 + cell_w * k;
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
        l.row += 1;
    }
}

/// Natural width of a section box, characters (borders included).
#[cfg(test)]
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
/// Returns (section index, rectangle); sections that do not fit vertically are left out.
pub fn pack(secs: &[Section], region: CellRect, columns: usize) -> Vec<(usize, CellRect)> {
    let columns = columns.max(1);
    let col_w = region.cols / columns;
    let mut heights = vec![0usize; columns];
    let mut out = Vec::new();
    for (idx, s) in secs.iter().enumerate() {
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

#[cfg(test)]
mod tests {
    use mctl_core::time::Utc;

    use super::*;

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
        let s = sections(&items, "g", 30.0);
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
        assert!(sections(&[other], "g", 30.0).is_empty());
        assert!(sections(&[], "g", 30.0).is_empty());
    }

    #[test]
    fn stale_cells_are_flagged_by_age() {
        let s = sections(
            &[
                item("a", "S", "r", "", "u", Some(1.0), 31.0),
                item("b", "S", "q", "", "u", Some(1.0), 5.0),
            ],
            "g",
            30.0,
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
        let s = &sections(&items, "g", 30.0)[0];
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
