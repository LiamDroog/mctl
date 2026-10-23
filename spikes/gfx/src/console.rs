//! Front-end-independent console state: themes, effect toggles, and per-frame UI preparation.
//!
//! The windowed app, the benchmark and the screenshot tool all drive this same type, so what is
//! measured and captured is exactly what is shown.

use anyhow::{Context as _, Result, bail};
use egui::{ClippedPrimitive, Context, LayerId, Pos2, RawInput, TexturesDelta};

use crate::coast::{self, Line};
use crate::globe::{self, GlobeScene};
use crate::grid::Grid;
use crate::render::EffectToggles;
use crate::theme::Theme;
use crate::ui::{self, Hud};

/// Embedded data (see `assets/LICENSES.md`).
pub const EARTH_JPEG: &[u8] = include_bytes!("../../../assets/earth/blue_marble_2048x1024.jpg");
pub const COAST_110M: &[u8] = include_bytes!("../../../assets/coastlines/ne_110m_coastline.bin");
pub const COAST_50M: &[u8] = include_bytes!("../../../assets/coastlines/ne_50m_coastline.bin");
/// Coastline mask width in texels (height is half). 4096 gives ~1 px lines at the globe's on-screen size.
pub const COAST_MASK_WIDTH: u32 = 4096;
/// Mip levels of the coastline mask.
pub const COAST_MASK_LEVELS: u32 = 6;

/// Everything the renderer needs for one frame, produced on the CPU.
pub struct Prepared {
    pub jobs: Vec<ClippedPrimitive>,
    pub textures_delta: TexturesDelta,
    pub pixels_per_point: f32,
    pub globe_rect_px: Option<[u32; 4]>,
    pub scene: GlobeScene,
}

pub struct Console {
    pub ctx: Context,
    pub themes: Vec<Theme>,
    pub theme_idx: usize,
    pub toggles: EffectToggles,
    coast_110m: Vec<Line>,
    adapter: String,
}

/// Per-frame dynamic values for the HUD.
pub struct FrameStats {
    pub elapsed_s: f64,
    pub frame_ms: f32,
    pub gpu_ms: Option<f32>,
}

impl Console {
    pub fn new(themes: Vec<Theme>, start_theme: Option<&str>, adapter: String) -> Result<Self> {
        if themes.is_empty() {
            bail!("no themes loaded");
        }
        let theme_idx = match start_theme {
            None => 0,
            Some(name) => themes
                .iter()
                .position(|t| t.name.eq_ignore_ascii_case(name))
                .with_context(|| {
                    format!(
                        "unknown theme {name:?}; available: {:?}",
                        themes.iter().map(|t| &t.name).collect::<Vec<_>>()
                    )
                })?,
        };
        let ctx = Context::default();
        ui::install_fonts(&ctx);
        ui::verify_cell_metrics(&ctx, &themes)?;
        let coast_110m = coast::parse(COAST_110M).context("parsing embedded 110m coastlines")?;
        let toggles = EffectToggles::from_theme(&themes[theme_idx].effects);
        Ok(Self {
            ctx,
            themes,
            theme_idx,
            toggles,
            coast_110m,
            adapter,
        })
    }

    pub fn theme(&self) -> &Theme {
        &self.themes[self.theme_idx]
    }

    pub fn set_theme(&mut self, idx: usize) {
        self.theme_idx = idx % self.themes.len();
        self.toggles = EffectToggles::from_theme(&self.theme().effects);
        tracing::info!(theme = %self.theme().name, "theme selected");
    }

    pub fn cycle_theme(&mut self) {
        self.set_theme(self.theme_idx + 1);
    }

    /// Run the UI for one frame and tessellate it.
    pub fn prepare(&mut self, raw: RawInput, size_px: [u32; 2], stats: &FrameStats) -> Prepared {
        let theme = self.themes[self.theme_idx].clone();
        let (cw, ch) = (theme.font.cell_w_px, theme.font.cell_h_px);
        let (cols, rows) = (
            (size_px[0] as f32 / cw) as usize,
            (size_px[1] as f32 / ch) as usize,
        );
        let mut grid = Grid::new(cols, rows);
        let hud = Hud {
            theme: &theme,
            toggles: self.toggles,
            elapsed_s: stats.elapsed_s,
            frame_ms: stats.frame_ms,
            gpu_ms: stats.gpu_ms,
            adapter: &self.adapter,
            size_px,
        };
        let layout = ui::build_page(&mut grid, &hud);
        let globe_px = ui::cell_rect_px(&theme, layout.globe);
        let ground_px = ui::cell_rect_px(&theme, layout.ground);
        let aspect = globe_px[2] as f32 / globe_px[3].max(1) as f32;
        let scene = globe::scene_at(stats.elapsed_s, aspect);
        let has_globe = globe_px[2] > 0 && globe_px[3] > 0;

        let coast = &self.coast_110m;
        let out = self.ctx.run_ui(raw, |ui| {
            let painter = ui.ctx().layer_painter(LayerId::background());
            grid.paint(&painter, &theme, Pos2::ZERO, stats.elapsed_s);
            ui::paint_ground_track(&painter, &theme, ground_px, coast, &scene);
            if has_globe {
                ui::paint_vehicle_label(&painter, &theme, globe_px, &scene);
            }
        });
        let jobs = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        Prepared {
            jobs,
            textures_delta: out.textures_delta,
            pixels_per_point: out.pixels_per_point,
            globe_rect_px: has_globe.then_some(globe_px),
            scene,
        }
    }
}

/// Build the raw egui input for a headless frame of the given size at 1 point = 1 pixel.
pub fn headless_input(size_px: [u32; 2], time_s: f64) -> RawInput {
    let mut raw = RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2(size_px[0] as f32, size_px[1] as f32),
        )),
        time: Some(time_s),
        ..Default::default()
    };
    raw.viewports.insert(
        raw.viewport_id,
        egui::ViewportInfo {
            native_pixels_per_point: Some(1.0),
            ..Default::default()
        },
    );
    raw
}
