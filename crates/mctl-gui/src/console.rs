//! Front-end-independent console state: themes, effect toggles, camera, selected vehicle, and the
//! per-frame preparation of the page from the engine's snapshot. The windowed app, the benchmark and
//! the screenshot tool all drive this same type, so what is measured and captured is what is shown.

use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use egui::{ClippedPrimitive, Context, LayerId, Pos2, RawInput, TexturesDelta};
use mctl_core::config::Limits;
use mctl_core::world::World;

use crate::assets::COAST_110M;
use crate::coast::{self, Line};
use crate::fonts;
use crate::globe::{Camera, GlobeScene, build_scene, orbit_vertices};
use crate::grid::Grid;
use crate::page::{self, PageInput};
use crate::render::EffectToggles;
use crate::theme::Theme;

/// Everything the renderer needs for one frame, produced on the CPU.
pub struct Prepared {
    pub jobs: Vec<ClippedPrimitive>,
    pub textures_delta: TexturesDelta,
    pub pixels_per_point: f32,
    pub globe_rect_px: Option<[u32; 4]>,
    pub scene: Option<GlobeScene>,
    pub orbit: Option<(u64, Arc<Vec<[f32; 4]>>)>,
}

/// Per-frame dynamic values for the footer.
pub struct FrameStats {
    pub elapsed_s: f64,
    pub frame_ms: f32,
    pub gpu_ms: Option<f32>,
}

pub struct Console {
    pub ctx: Context,
    pub themes: Vec<Theme>,
    pub theme_idx: usize,
    pub toggles: EffectToggles,
    pub camera: Camera,
    pub vehicle_id: String,
    pub show_ecef: bool,
    coast_110m: Vec<Line>,
    adapter: String,
    limits: Limits,
    orbit_cache: Option<(u64, Arc<Vec<[f32; 4]>>)>,
    /// Texture uploads from start-up, delivered with the first frame.
    pending_textures: TexturesDelta,
}

/// Pixel rectangle `[x, y, w, h]` of a cell rectangle.
pub fn cell_rect_px(theme: &Theme, r: crate::grid::CellRect) -> [u32; 4] {
    let (cw, ch) = (theme.font.cell_w_px, theme.font.cell_h_px);
    [
        (r.col as f32 * cw) as u32,
        (r.row as f32 * ch) as u32,
        (r.cols as f32 * cw) as u32,
        (r.rows as f32 * ch) as u32,
    ]
}

impl Console {
    pub fn new(
        themes: Vec<Theme>,
        start_theme: Option<&str>,
        adapter: String,
        limits: Limits,
        vehicle_id: String,
    ) -> Result<Self> {
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
        fonts::install_fonts(&ctx);
        let pending_textures = fonts::verify_cell_metrics(&ctx, &themes)?;
        let coast_110m = coast::parse(COAST_110M).context("parsing embedded 110m coastlines")?;
        let toggles = EffectToggles::from_theme(&themes[theme_idx].effects);
        Ok(Self {
            ctx,
            themes,
            theme_idx,
            toggles,
            camera: Camera::default(),
            vehicle_id,
            show_ecef: false,
            coast_110m,
            adapter,
            limits,
            orbit_cache: None,
            pending_textures,
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

    /// Select the next vehicle in the world's order.
    pub fn next_vehicle(&mut self, world: &World) {
        if let Some(i) = world.vehicles.iter().position(|v| v.id == self.vehicle_id) {
            self.vehicle_id = world.vehicles[(i + 1) % world.vehicles.len()].id.clone();
        } else if let Some(v) = world.vehicles.first() {
            self.vehicle_id = v.id.clone();
        }
    }

    /// Run the UI for one frame against `world` and tessellate it.
    pub fn prepare(
        &mut self,
        raw: RawInput,
        size_px: [u32; 2],
        world: &World,
        stats: &FrameStats,
    ) -> Prepared {
        let theme = self.themes[self.theme_idx].clone();
        let (cw, ch) = (theme.font.cell_w_px, theme.font.cell_h_px);
        let (cols, rows) = (
            (size_px[0] as f32 / cw) as usize,
            (size_px[1] as f32 / ch) as usize,
        );
        let vehicle_idx = world.vehicles.iter().position(|v| v.id == self.vehicle_id);
        let vehicle = vehicle_idx.map(|i| &world.vehicles[i]);
        let mut grid = Grid::new(cols, rows);
        let layout = page::build_master(
            &mut grid,
            &PageInput {
                world,
                vehicle,
                vehicle_idx: vehicle_idx.unwrap_or(0),
                vehicle_count: world.vehicles.len(),
                theme: &theme,
                toggles: self.toggles,
                camera: &self.camera,
                show_ecef: self.show_ecef,
                limits: &self.limits,
                frame_ms: stats.frame_ms,
                gpu_ms: stats.gpu_ms,
                adapter: &self.adapter,
                size_px,
            },
        );
        let globe_px = cell_rect_px(&theme, layout.orbit);
        let ground_px = cell_rect_px(&theme, layout.ground);
        let eclipse_px = cell_rect_px(&theme, layout.eclipse);
        let has_area = globe_px[2] > 0 && globe_px[3] > 0;
        let aspect = globe_px[2] as f32 / globe_px[3].max(1) as f32;
        let sol = vehicle.and_then(|v| v.solution.as_ref());
        let scene = sol
            .filter(|_| has_area)
            .map(|s| build_scene(&self.camera, s, aspect));
        let orbit = match (scene.as_ref(), vehicle.and_then(|v| v.track.as_ref())) {
            (Some(_), Some(track)) => {
                let key = track.revision * 8 + self.camera.exaggeration_idx as u64;
                if self.orbit_cache.as_ref().is_none_or(|(k, _)| *k != key) {
                    self.orbit_cache = Some((
                        key,
                        Arc::new(orbit_vertices(track, self.camera.exaggeration())),
                    ));
                }
                self.orbit_cache.clone()
            }
            _ => None,
        };

        let coast = &self.coast_110m;
        let font = fonts::font_id(&theme);
        let now = world.t;
        let mut out = self.ctx.run_ui(raw, |ui| {
            let painter = ui.ctx().layer_painter(LayerId::background());
            grid.paint(&painter, &theme, Pos2::ZERO, stats.elapsed_s);
            if let Some(v) = vehicle {
                page::paint_ground_track(&painter, &theme, ground_px, coast, v);
                page::paint_eclipse_ring(&painter, &theme, eclipse_px, v, now);
            }
            if let (Some(sc), true) = (scene.as_ref(), has_area) {
                // Label the vehicle over the globe, flagging it when the Earth hides it.
                let clip = sc.view_proj * sc.vehicle.extend(1.0);
                if clip.w > 0.0 {
                    let (nx, ny) = (clip.x / clip.w, clip.y / clip.w);
                    let at = Pos2::new(
                        globe_px[0] as f32 + (nx * 0.5 + 0.5) * globe_px[2] as f32 + 14.0,
                        globe_px[1] as f32 + (0.5 - ny * 0.5) * globe_px[3] as f32 - 10.0,
                    );
                    let name = vehicle.map_or("", |v| v.name.as_str());
                    let text = if sc.vehicle_occulted {
                        format!("{name} OCCULTED")
                    } else {
                        name.to_owned()
                    };
                    painter.text(
                        at,
                        egui::Align2::LEFT_BOTTOM,
                        text,
                        font.clone(),
                        theme.palette.bright.to_egui(),
                    );
                }
            }
        });
        let jobs = self
            .ctx
            .tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
        let mut textures_delta = std::mem::take(&mut self.pending_textures);
        textures_delta.append(std::mem::take(&mut out.textures_delta));
        Prepared {
            jobs,
            textures_delta,
            pixels_per_point: out.pixels_per_point,
            globe_rect_px: has_area.then_some(globe_px),
            scene,
            orbit,
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
