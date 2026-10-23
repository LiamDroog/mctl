//! The sample console page: header, bordered panels with placeholder data, footer.
//!
//! Every number here is fixture data for the spike and is labelled `SAMPLE` on screen, per the
//! honesty rule (DESIGN.md §1.3): nothing on screen may pass for real telemetry.

use std::sync::Arc;

use anyhow::{Result, bail};
use egui::{Context, FontData, FontDefinitions, FontFamily, FontId, Pos2, Stroke, Vec2};

use crate::coast::Line;
use crate::globe::{ALTITUDE_EXAGGERATION, DEMO_TIME_WARP, GlobeScene};
use crate::grid::{CellRect, Grid, Limit, Role, Style};
use crate::render::EffectToggles;
use crate::theme::{FontFamily as ThemeFont, Theme};

const FONT_JETBRAINS: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");
const FONT_UNSCII: &[u8] = include_bytes!("../../../assets/fonts/unscii-16.ttf");

/// Largest allowed difference between the font's real glyph advance and the theme's `cell_w_px`.
const CELL_WIDTH_TOLERANCE_PX: f32 = 0.01;
/// Fraction of body width given to the left (graphical) column.
const LEFT_COLUMN_FRACTION: f32 = 0.55;
/// Fraction of the left column's height given to the orbit map.
const ORBIT_PANEL_FRACTION: f32 = 0.62;

/// Placeholder state-vector rows (label, value, unit). Sample values only.
const SAMPLE_STATE_VECTOR: &[(&str, &str, &str)] = &[
    ("X", "  -2891.402", "km"),
    ("Y", "   5203.117", "km"),
    ("Z", "   3341.668", "km"),
    ("VX", "    -5.0213", "km/s"),
    ("VY", "    -3.9941", "km/s"),
    ("VZ", "     3.3022", "km/s"),
];
const SAMPLE_ELEMENTS: &[(&str, &str, &str)] = &[
    ("SMA", "   6791.020", "km"),
    ("ECC", "  0.000520", ""),
    ("INC", "     51.642", "deg"),
    ("RAAN", "   212.884", "deg"),
    ("HA", "      417.9", "km"),
    ("HP", "      410.8", "km"),
];

pub fn install_fonts(ctx: &Context) {
    let mut defs = FontDefinitions::empty();
    defs.font_data.insert(
        "jetbrains".into(),
        Arc::new(FontData::from_static(FONT_JETBRAINS)),
    );
    defs.font_data.insert(
        "unscii".into(),
        Arc::new(FontData::from_static(FONT_UNSCII)),
    );
    // egui insists that the built-in families exist, even though the console only uses named ones.
    defs.families
        .insert(FontFamily::Proportional, vec!["jetbrains".into()]);
    defs.families
        .insert(FontFamily::Monospace, vec!["jetbrains".into()]);
    defs.families.insert(
        FontFamily::Name("jetbrains".into()),
        vec!["jetbrains".into()],
    );
    defs.families
        .insert(FontFamily::Name("unscii".into()), vec!["unscii".into()]);
    ctx.set_fonts(defs);
}

pub fn font_id(theme: &Theme) -> FontId {
    let family = match theme.font.family {
        ThemeFont::Unscii => "unscii",
        ThemeFont::Jetbrains => "jetbrains",
    };
    FontId::new(theme.font.size_px, FontFamily::Name(family.into()))
}

/// Check that every theme's configured cell width equals the font's real advance.
///
/// A mismatch would silently misalign every column and break box-drawing joins, so it is an error.
pub fn verify_cell_metrics(ctx: &Context, themes: &[Theme]) -> Result<()> {
    let mut measured = Vec::new();
    // Fonts only become available inside a pass.
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        for t in themes {
            let w = ui.ctx().fonts_mut(|f| f.glyph_width(&font_id(t), 'M'));
            measured.push((t.name.clone(), t.font.cell_w_px, w));
        }
    });
    // Nothing is rendered from this probe pass; the font texture is re-sent on the first real frame.
    out.textures_delta.clear();
    for (name, want, got) in measured {
        if (want - got).abs() > CELL_WIDTH_TOLERANCE_PX {
            bail!(
                "theme {name}: cell_w_px = {want} but the font advance at its size_px is {got}; fix the theme"
            );
        }
    }
    Ok(())
}

/// Live values shown on screen.
pub struct Hud<'a> {
    pub theme: &'a Theme,
    pub toggles: EffectToggles,
    pub elapsed_s: f64,
    /// Wall-clock frame interval, ms.
    pub frame_ms: f32,
    pub gpu_ms: Option<f32>,
    pub adapter: &'a str,
    pub size_px: [u32; 2],
}

/// Where the graphical panels sit, in cells.
pub struct Layout {
    pub globe: CellRect,
    pub ground: CellRect,
}

fn fmt_gmt(elapsed_s: f64) -> String {
    // Demo clock only: seconds since launch, shown in the DDD:HH:MM:SS.ss form from §6.3.
    let total = elapsed_s * DEMO_TIME_WARP;
    let (d, rem) = ((total / 86_400.0) as u32, total % 86_400.0);
    let (h, rem) = ((rem / 3600.0) as u32, rem % 3600.0);
    let (m, s) = ((rem / 60.0) as u32, rem % 60.0);
    format!("{:03}:{h:02}:{m:02}:{s:05.2}", d + 1)
}

fn on_off(b: bool) -> &'static str {
    if b { "ON " } else { "OFF" }
}

/// Fill the grid with the sample MASTER page and return where the graphical panels went.
pub fn build_page(grid: &mut Grid, hud: &Hud<'_>) -> Layout {
    let (cols, rows) = (grid.cols, grid.rows);
    let fg = Style::new(Role::Fg);
    let dim = Style::new(Role::Dim);
    let bright = Style::new(Role::Bright);
    let border = Style::new(Role::Border);

    // Header.
    let header = format!(
        " ISS (25544) \u{2502} MASTER \u{2502} GMT {} \u{2502} SIM {:.0}x \u{2502} SRC SAMPLE \u{2502} {}",
        fmt_gmt(hud.elapsed_s),
        DEMO_TIME_WARP,
        hud.theme.name.to_uppercase()
    );
    let annunciator = " SAMPLE DATA ";
    // Keep the annunciator visible when the window is too narrow for the whole header.
    let header_cols = cols.saturating_sub(annunciator.len() + 1);
    let header: String = header.chars().take(header_cols).collect();
    grid.put(0, 0, &header, bright);
    grid.put(
        header_cols + 1,
        0,
        annunciator,
        Style::limit(Limit::Caution),
    );

    // Body geometry: header row 0; two footer rows (stats, then key legend).
    let body_rows = rows.saturating_sub(3);
    let left_w =
        ((cols as f32 * LEFT_COLUMN_FRACTION) as usize).clamp(2, cols.saturating_sub(2).max(2));
    let right_w = cols.saturating_sub(left_w);
    let orbit_h = ((body_rows as f32 * ORBIT_PANEL_FRACTION) as usize)
        .max(2)
        .min(body_rows);
    let orbit = CellRect::new(0, 1, left_w, orbit_h);
    let ground = CellRect::new(0, 1 + orbit_h, left_w, body_rows.saturating_sub(orbit_h));

    let panel_h = (body_rows / 3).max(2);
    let sv = CellRect::new(left_w, 1, right_w, panel_h);
    let el = CellRect::new(left_w, 1 + panel_h, right_w, panel_h);
    let cw = CellRect::new(
        left_w,
        1 + 2 * panel_h,
        right_w,
        body_rows.saturating_sub(2 * panel_h),
    );

    let title = |s: &str| -> String { s.to_owned() };
    grid.draw_box(
        orbit,
        &title(&format!(
            "ORBIT MAP  ALT x{ALTITUDE_EXAGGERATION:.0}  SAMPLE"
        )),
        border,
        bright,
    );
    grid.draw_box(ground, "GROUND TRACK  SAMPLE", border, bright);
    grid.draw_box(sv, "STATE VECTOR (ECI)  SAMPLE", border, bright);
    grid.draw_box(el, "ELEMENTS (OSC)  SAMPLE", border, bright);
    grid.draw_box(cw, "CAUTION & WARNING", border, bright);

    for (rect, data) in [(sv, SAMPLE_STATE_VECTOR), (el, SAMPLE_ELEMENTS)] {
        for (i, (label, value, unit)) in data.iter().enumerate() {
            let row = rect.row + 1 + i;
            if row < rect.row + rect.rows.saturating_sub(1) {
                grid.put(rect.col + 2, row, &format!("{label:<5}"), dim);
                grid.put(rect.col + 8, row, value, fg);
                grid.put(rect.col + 8 + value.len() + 1, row, unit, dim);
            }
        }
    }

    // Limit-state samples: each theme must make these distinguishable without relying on hue.
    let cw_rows: [(&str, Style); 5] = [
        (
            " NOM   EPS BUS VOLTAGE          SAMPLE",
            Style::new(Role::Nominal),
        ),
        (
            " CAUT  TLE AGE > 24 H           SAMPLE",
            Style::limit(Limit::Caution),
        ),
        (
            " WARN  LIVE FEED LOST           SAMPLE",
            Style::limit(Limit::Warning),
        ),
        (
            " STALE OEM AGE                  SAMPLE",
            Style::limit(Limit::Stale),
        ),
        (" ----- CMG MOMENTUM       NO DATA", dim),
    ];
    for (i, (text, style)) in cw_rows.iter().enumerate() {
        let row = cw.row + 1 + i;
        if row < cw.row + cw.rows.saturating_sub(1) {
            grid.put(cw.col + 1, row, text, *style);
        }
    }

    // Footer.
    let t = hud.toggles;
    let legend = format!(
        " F1 MASTER  T THEME  1 SCAN {}  2 BLOOM {}  3 PERSIST {}  4 CRT {}  E ALL  Q QUIT",
        on_off(t.scanlines),
        on_off(t.bloom),
        on_off(t.persistence),
        on_off(t.curvature)
    );
    let legend: String = legend.chars().take(cols).collect();
    grid.put(0, rows.saturating_sub(1), &legend, dim);
    let gpu = hud
        .gpu_ms
        .map_or_else(|| "GPU n/a".to_owned(), |g| format!("GPU {g:5.2}ms"));
    let stats = format!(
        "{}x{} FRAME {:5.2}ms {:5.1}fps {gpu} ",
        hud.size_px[0],
        hud.size_px[1],
        hud.frame_ms,
        if hud.frame_ms > 0.0 {
            1000.0 / hud.frame_ms
        } else {
            0.0
        }
    );
    let stats_row = rows.saturating_sub(2);
    grid.put(
        cols.saturating_sub(stats.len()),
        stats_row,
        &stats,
        Style::new(Role::Info),
    );
    let adapter: String = hud
        .adapter
        .chars()
        .take(cols.saturating_sub(stats.len() + 2))
        .collect();
    grid.put(1, stats_row, &adapter, dim);

    Layout {
        globe: orbit.inner(),
        ground: ground.inner(),
    }
}

/// Pixel rectangle `[x, y, w, h]` of a cell rectangle.
pub fn cell_rect_px(theme: &Theme, r: CellRect) -> [u32; 4] {
    let (cw, ch) = (theme.font.cell_w_px, theme.font.cell_h_px);
    [
        (r.col as f32 * cw) as u32,
        (r.row as f32 * ch) as u32,
        (r.cols as f32 * cw) as u32,
        (r.rows as f32 * ch) as u32,
    ]
}

/// Draw the ground-track panel body (equirectangular coastlines plus sub-vehicle point) with egui.
pub fn paint_ground_track(
    painter: &egui::Painter,
    theme: &Theme,
    rect_px: [u32; 4],
    coast: &[Line],
    scene: &GlobeScene,
) {
    let [x, y, w, h] = rect_px;
    if w < 4 || h < 4 {
        return;
    }
    let to_screen = |lon: f32, lat: f32| {
        Pos2::new(
            x as f32 + (lon + 180.0) / 360.0 * w as f32,
            y as f32 + (90.0 - lat) / 180.0 * h as f32,
        )
    };
    let stroke = Stroke::new(1.0, theme.palette.dim.to_egui());
    for line in coast {
        let mut run: Vec<Pos2> = Vec::new();
        for pair in line.windows(2) {
            // Break polylines that cross the antimeridian instead of striking across the map.
            if (pair[0].0 - pair[1].0).abs() > 180.0 {
                if run.len() > 1 {
                    painter.add(egui::Shape::line(std::mem::take(&mut run), stroke));
                }
                run.clear();
                continue;
            }
            if run.is_empty() {
                run.push(to_screen(pair[0].0, pair[0].1));
            }
            run.push(to_screen(pair[1].0, pair[1].1));
        }
        if run.len() > 1 {
            painter.add(egui::Shape::line(run, stroke));
        }
    }
    // Sub-vehicle point: inertial -> Earth-fixed by undoing the Earth rotation angle.
    let (s, c) = (-scene.era_rad).sin_cos();
    let p = scene.vehicle_pos;
    let e = glam::Vec3::new(c * p.x - s * p.y, s * p.x + c * p.y, p.z).normalize();
    let (lon, lat) = (e.y.atan2(e.x).to_degrees(), e.z.asin().to_degrees());
    let at = to_screen(lon, lat);
    painter.circle_stroke(at, 6.0, Stroke::new(2.0, theme.globe.vehicle.to_egui()));
}

/// Label the vehicle marker over the globe, flagging it when Earth hides it.
pub fn paint_vehicle_label(
    painter: &egui::Painter,
    theme: &Theme,
    globe_px: [u32; 4],
    scene: &GlobeScene,
) {
    let [x, y, w, h] = globe_px;
    let Some(p) = scene.project(scene.vehicle_pos, [w as f32, h as f32]) else {
        return;
    };
    let text = if scene.vehicle_occulted {
        "ISS OCCULTED"
    } else {
        "ISS"
    };
    let at = Pos2::new(x as f32 + p[0], y as f32 + p[1]) + Vec2::new(12.0, -8.0);
    painter.text(
        at,
        egui::Align2::LEFT_BOTTOM,
        text,
        font_id(theme),
        theme.palette.bright.to_egui(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    fn hud(theme: &Theme) -> Hud<'_> {
        Hud {
            theme,
            toggles: EffectToggles::all(true),
            elapsed_s: 1.0,
            frame_ms: 16.6,
            gpu_ms: Some(2.0),
            adapter: "test",
            size_px: [1920, 1080],
        }
    }

    #[test]
    fn page_builds_at_reference_and_tiny_sizes_without_panicking() {
        let themes = theme::embedded().unwrap();
        for (cols, rows) in [
            (240, 67),
            (120, 33),
            (160, 45),
            (10, 4),
            (1, 1),
            (0, 0),
            (80, 24),
        ] {
            let mut g = Grid::new(cols, rows);
            let _ = build_page(&mut g, &hud(&themes[0]));
        }
    }

    #[test]
    fn globe_rect_is_inside_the_grid() {
        let themes = theme::embedded().unwrap();
        let mut g = Grid::new(120, 33);
        let l = build_page(&mut g, &hud(&themes[0]));
        assert!(l.globe.col + l.globe.cols <= 120 && l.globe.row + l.globe.rows <= 33);
        assert!(l.globe.cols > 10 && l.globe.rows > 5);
    }

    #[test]
    fn gmt_formats_as_ddd_hh_mm_ss() {
        assert_eq!(fmt_gmt(0.0), "001:00:00:00.00");
    }
}
