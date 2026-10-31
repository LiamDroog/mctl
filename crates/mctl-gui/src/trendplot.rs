//! Time-series plots for the TRAJ trends page.

use egui::{Align2, Painter, Pos2, Shape, Stroke};
use mctl_core::time::Utc;
use mctl_core::world::{TrendPlot, TrendSet};

use crate::theme::Theme;

/// Selectable windows, 6 h to 90 d.
pub const WINDOWS: [(&str, f64); 5] = [
    ("6h", 6.0 * 3600.0),
    ("24h", 86_400.0),
    ("7d", 7.0 * 86_400.0),
    ("30d", 30.0 * 86_400.0),
    ("90d", 90.0 * 86_400.0),
];

/// `6h`, `24h`, `7d`, ... or any `<number>h` / `<number>d` between 6 h and 90 d.
pub fn parse_window(s: &str) -> Result<f64, String> {
    let s = s.trim().to_ascii_lowercase();
    let (num, mult) = if let Some(n) = s.strip_suffix('h') {
        (n, 3600.0)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86_400.0)
    } else {
        return Err(format!(
            "trend window {s:?}: use a number of hours or days such as 6h, 24h, 7d, 90d"
        ));
    };
    let v: f64 = num
        .parse()
        .map_err(|_| format!("trend window {s:?}: {num:?} is not a number"))?;
    let secs = v * mult;
    if !(6.0 * 3600.0..=90.0 * 86_400.0).contains(&secs) {
        return Err(format!("trend window {s:?} is outside 6h .. 90d"));
    }
    Ok(secs)
}

pub fn window_label(secs: f64) -> String {
    if let Some((l, _)) = WINDOWS.iter().find(|(_, w)| (w - secs).abs() < 1.0) {
        return (*l).to_owned();
    }
    if secs >= 86_400.0 {
        format!("{:.1}d", secs / 86_400.0)
    } else {
        format!("{:.0}h", secs / 3600.0)
    }
}

/// Axis range covering the data with a margin; a flat series gets a visible span instead of dividing by zero.
pub fn axis_range(min: f32, max: f32) -> (f32, f32) {
    if !(min.is_finite() && max.is_finite()) {
        return (0.0, 1.0);
    }
    let span = max - min;
    if span < 1.0e-6_f32.max(max.abs() * 1.0e-7) {
        let pad = (max.abs() * 1.0e-3).max(0.5);
        return (min - pad, max + pad);
    }
    (min - span * 0.08, max + span * 0.08)
}

/// Compact value label for an axis end.
pub fn axis_label(v: f32) -> String {
    match v.abs() {
        a if a >= 1000.0 => format!("{v:.0}"),
        a if a >= 100.0 => format!("{v:.1}"),
        a if a >= 10.0 => format!("{v:.2}"),
        a if a >= 1.0 => format!("{v:.3}"),
        _ => format!("{v:.5}"),
    }
}

/// Whether the plot has anything to draw.
pub fn has_data(p: &TrendPlot) -> bool {
    p.series.iter().any(|s| s.points.len() >= 2)
}

/// Draw one plot into `rect_px` (`[x, y, w, h]`). Element-set epochs are marked on the plots where a
/// reboost would show as a step.
pub fn paint(
    painter: &Painter,
    theme: &Theme,
    rect_px: [u32; 4],
    plot: &TrendPlot,
    set: &TrendSet,
    marks: bool,
) {
    let [x, y, w, h] = rect_px.map(|v| v as f32);
    if w < 60.0 || h < 40.0 {
        return;
    }
    let p = &theme.palette;
    let (lo, hi) = {
        let vals = plot
            .series
            .iter()
            .flat_map(|s| s.points.iter().map(|q| q.1));
        let (mn, mx) = vals.fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), v| {
            (a.min(v), b.max(v))
        });
        axis_range(mn, mx)
    };
    // Start at the first recorded point when the recording is younger than the window.
    let first = plot
        .series
        .iter()
        .flat_map(|s| s.points.first().map(|q| q.0))
        .fold(f64::INFINITY, f64::min);
    let t0 = set
        .from
        .0
        .max(if first.is_finite() { first } else { set.from.0 });
    let t1 = set.to.0.max(t0 + 1.0);
    let (px0, py0, pw, ph) = (x + 6.0, y + 4.0, w - 12.0, h - 8.0);
    let to = |t: f64, v: f32| {
        Pos2::new(
            px0 + pw * ((t - t0) / (t1 - t0)).clamp(0.0, 1.0) as f32,
            py0 + ph * (1.0 - (v - lo) / (hi - lo)),
        )
    };
    painter.rect_stroke(
        egui::Rect::from_min_size(Pos2::new(px0, py0), egui::vec2(pw, ph)),
        0.0,
        Stroke::new(1.0, p.border.to_egui()),
        egui::StrokeKind::Inside,
    );
    if marks {
        for m in &set.element_marks {
            if m.0 >= t0 && m.0 <= t1 {
                let xm = to(m.0, lo).x;
                painter.add(Shape::dashed_line(
                    &[Pos2::new(xm, py0), Pos2::new(xm, py0 + ph)],
                    Stroke::new(1.0, p.dim.to_egui()),
                    3.0,
                    3.0,
                ));
            }
        }
    }
    let colours = [p.bright.to_egui(), p.info.to_egui(), p.foreground.to_egui()];
    for (i, s) in plot.series.iter().enumerate() {
        if s.points.len() < 2 {
            continue;
        }
        let pts: Vec<Pos2> = s.points.iter().map(|(t, v)| to(*t, *v)).collect();
        painter.add(Shape::line(
            pts,
            Stroke::new(1.5, colours[i % colours.len()]),
        ));
    }
    let font = crate::fonts::font_id(theme);
    painter.text(
        Pos2::new(px0 + 3.0, py0 + 1.0),
        Align2::LEFT_TOP,
        axis_label(hi),
        font.clone(),
        p.dim.to_egui(),
    );
    painter.text(
        Pos2::new(px0 + 3.0, py0 + ph - 1.0),
        Align2::LEFT_BOTTOM,
        axis_label(lo),
        font.clone(),
        p.dim.to_egui(),
    );
    let legend: Vec<String> = plot
        .series
        .iter()
        .filter(|s| s.points.len() >= 2)
        .map(|s| s.label.clone())
        .collect();
    if plot.series.len() > 1 {
        painter.text(
            Pos2::new(px0 + pw - 3.0, py0 + 1.0),
            Align2::RIGHT_TOP,
            legend.join(" / "),
            font,
            p.bright.to_egui(),
        );
    }
    let _ = Utc(0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_parse_in_hours_and_days_within_the_documented_range() {
        assert_eq!(parse_window("6h"), Ok(21_600.0));
        assert_eq!(parse_window("24H"), Ok(86_400.0));
        assert_eq!(parse_window("7d"), Ok(604_800.0));
        assert_eq!(parse_window("1.5d"), Ok(129_600.0));
        assert!(parse_window("5h").unwrap_err().contains("outside"));
        assert!(parse_window("91d").unwrap_err().contains("outside"));
        assert!(parse_window("week").is_err());
        assert!(parse_window("xh").unwrap_err().contains("not a number"));
    }

    #[test]
    fn labels_round_trip_the_standard_windows() {
        for (l, s) in WINDOWS {
            assert_eq!(window_label(s), l);
        }
        assert_eq!(window_label(129_600.0), "1.5d");
    }

    #[test]
    fn axis_ranges_pad_data_and_never_collapse() {
        let (lo, hi) = axis_range(10.0, 20.0);
        assert!(lo < 10.0 && hi > 20.0 && (hi - lo) < 13.0);
        let (lo, hi) = axis_range(6798.0, 6798.0);
        assert!(
            hi - lo >= 1.0 && lo < 6798.0 && hi > 6798.0,
            "a flat line still has a span"
        );
        assert_eq!(axis_range(f32::NAN, 1.0), (0.0, 1.0));
        let (lo, hi) = axis_range(0.0, 0.0);
        assert!(hi > lo);
    }

    #[test]
    fn axis_labels_keep_useful_digits() {
        assert_eq!(axis_label(6798.4), "6798");
        assert_eq!(axis_label(51.6412), "51.64");
        assert_eq!(axis_label(0.000612), "0.00061");
    }
}
