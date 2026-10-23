//! Front-end-independent console state: themes, effect toggles, camera, selected vehicle, and the
//! per-frame preparation of the page from the engine's snapshot. The windowed app, the benchmark and
//! the screenshot tool all drive this same type, so what is measured and captured is what is shown.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use anyhow::{Context as _, Result, bail};
use egui::{ClippedPrimitive, Context, LayerId, Pos2, RawInput, TexturesDelta};
use mctl_core::command::Command;
use mctl_core::config::Limits;
use mctl_core::world::World;

use crate::assets::COAST_110M;
use crate::coast::{self, Line};
use crate::commands::{self, Action, Effect, Names, UiAction};
use crate::fonts;
use crate::globe::{Camera, GlobeScene, build_scene, orbit_vertices};
use crate::grid::Grid;
use crate::page::{self, CommandView, PageInput, StatusMsg};
use crate::pages::Page;
use crate::render::EffectToggles;
use crate::screens;
use crate::theme::Theme;

/// Everything the renderer needs for one frame, produced on the CPU.
pub struct Prepared {
    pub jobs: Vec<ClippedPrimitive>,
    pub textures_delta: TexturesDelta,
    pub pixels_per_point: f32,
    pub globe_rect_px: Option<[u32; 4]>,
    /// Where mouse drag and wheel act (the orbit map); none on pages without one.
    pub interactive_rect_px: Option<[u32; 4]>,
    pub scene: Option<GlobeScene>,
    pub orbit: Option<(u64, Arc<Vec<[f32; 4]>>)>,
}

/// Per-frame dynamic values for the footer.
pub struct FrameStats {
    pub elapsed_s: f64,
    pub frame_ms: f32,
    pub gpu_ms: Option<f32>,
}

/// A key press, independent of the windowing system so the whole key map is unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyInput {
    Char(char),
    F(u8),
    Escape,
    Enter,
    Backspace,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
}

const ARROW_STEP_DEG: f32 = 5.0;
pub const ZOOM_STEP: f32 = 1.15;
/// Seconds a status message stays in the footer.
const STATUS_SECONDS: f64 = 6.0;
const HISTORY_MAX: usize = 50;
/// Clock rate ladder for `[` / `]` (powers of two times this base are clamped to the limits).
const WARP_BASE: f64 = 1.0;

struct CommandLine {
    text: String,
    candidates: Vec<String>,
    history_pos: Option<usize>,
}

pub struct Console {
    pub ctx: Context,
    pub themes: Vec<Theme>,
    pub theme_idx: usize,
    pub toggles: EffectToggles,
    pub camera: Camera,
    pub vehicle_id: String,
    pub show_ecef: bool,
    pub page: Page,
    pub sub: usize,
    pub help: bool,
    cmd: Option<CommandLine>,
    history: Vec<String>,
    status: Option<(StatusMsg, f64)>,
    now_s: f64,
    commands: Option<Sender<Command>>,
    settings: Vec<(String, String)>,
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
            page: Page::Master,
            sub: 0,
            help: false,
            cmd: None,
            history: Vec::new(),
            status: None,
            now_s: 0.0,
            commands: None,
            settings: Vec::new(),
            coast_110m,
            adapter,
            limits,
            orbit_cache: None,
            pending_textures,
        })
    }

    /// Connect the clock-command channel and the read-only settings rows for the SYS page.
    pub fn connect(&mut self, commands: Option<Sender<Command>>, settings: Vec<(String, String)>) {
        self.commands = commands;
        self.settings = settings;
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some((
            StatusMsg {
                text: text.into(),
                error,
            },
            self.now_s + STATUS_SECONDS,
        ));
    }

    fn send(&mut self, c: Command) {
        match &self.commands {
            Some(tx) => {
                if tx.send(c).is_err() {
                    self.say("engine is not running; clock command lost", true);
                }
            }
            None => self.say("no engine connected", true),
        }
    }

    fn names(&self, world: &World) -> (Vec<String>, Vec<String>) {
        (
            self.themes.iter().map(|t| t.name.clone()).collect(),
            world.vehicles.iter().map(|v| v.id.clone()).collect(),
        )
    }

    fn set_effect(&mut self, which: Option<Effect>, on: Option<bool>) {
        let t = &mut self.toggles;
        match which {
            None => *t = EffectToggles::all(on.unwrap_or(!t.any())),
            Some(e) => {
                let slot = match e {
                    Effect::Scanlines => &mut t.scanlines,
                    Effect::Bloom => &mut t.bloom,
                    Effect::Persistence => &mut t.persistence,
                    Effect::Curvature => &mut t.curvature,
                };
                *slot = on.unwrap_or(!*slot);
            }
        }
    }

    fn set_page(&mut self, page: Page, sub: usize) {
        self.page = page;
        self.sub = sub.min(page.subpages().len() - 1);
    }

    fn cycle_sub(&mut self, delta: isize) {
        let n = self.page.subpages().len() as isize;
        self.sub = (self.sub as isize + delta).rem_euclid(n) as usize;
    }

    fn run_ui(&mut self, a: UiAction) -> Outcome {
        match a {
            UiAction::Quit => return Outcome::Quit,
            UiAction::ToggleHelp => self.help = !self.help,
            UiAction::Theme(n) => {
                if let Some(i) = self.themes.iter().position(|t| t.name == n) {
                    self.set_theme(i);
                }
            }
            UiAction::Vehicle(id) => self.vehicle_id = id,
            UiAction::GoPage { page, sub } => self.set_page(page, sub.unwrap_or(0)),
            UiAction::Fx { which, on } => self.set_effect(which, on),
            UiAction::Altitude(i) => self.camera.exaggeration_idx = i,
            UiAction::CameraEarthFixed(b) => self.camera.earth_fixed = b,
            UiAction::FocusVehicle(b) => self.camera.focus_vehicle = b,
            UiAction::StateEcef(b) => self.show_ecef = b,
        }
        Outcome::Continue
    }

    fn execute(&mut self, line: &str, world: &World) -> Outcome {
        let (themes, vehicles) = self.names(world);
        match commands::parse(
            line,
            &Names {
                themes: &themes,
                vehicles: &vehicles,
            },
        ) {
            Err(e) => {
                self.say(e, true);
                Outcome::Continue
            }
            Ok(Action::Engine(c)) => {
                self.send(c);
                Outcome::Continue
            }
            Ok(Action::Ui(a)) => self.run_ui(a),
        }
    }

    /// Apply one key press. All key bindings live here.
    pub fn handle_key(&mut self, key: KeyInput, world: &World) -> Outcome {
        if let Some(cmd) = self.cmd.as_mut() {
            match key {
                KeyInput::Escape => self.cmd = None,
                KeyInput::Enter => {
                    let line = std::mem::take(&mut cmd.text);
                    self.cmd = None;
                    if !line.trim().is_empty() {
                        self.history.push(line.clone());
                        if self.history.len() > HISTORY_MAX {
                            self.history.remove(0);
                        }
                        return self.execute(&line, world);
                    }
                }
                KeyInput::Backspace => {
                    cmd.text.pop();
                    cmd.candidates.clear();
                }
                KeyInput::Tab => {
                    let (themes, vehicles) = (
                        self.themes
                            .iter()
                            .map(|t| t.name.clone())
                            .collect::<Vec<_>>(),
                        world
                            .vehicles
                            .iter()
                            .map(|v| v.id.clone())
                            .collect::<Vec<_>>(),
                    );
                    let c = commands::complete(
                        &cmd.text,
                        &Names {
                            themes: &themes,
                            vehicles: &vehicles,
                        },
                    );
                    cmd.text = c.line;
                    cmd.candidates = c.candidates;
                }
                KeyInput::Up | KeyInput::Down => {
                    let n = self.history.len();
                    if n > 0 {
                        let pos = match (key, cmd.history_pos) {
                            (KeyInput::Up, None) => n - 1,
                            (KeyInput::Up, Some(p)) => p.saturating_sub(1),
                            (_, Some(p)) if p + 1 < n => p + 1,
                            _ => n,
                        };
                        cmd.history_pos = (pos < n).then_some(pos);
                        cmd.text = self.history.get(pos).cloned().unwrap_or_default();
                        cmd.candidates.clear();
                    }
                }
                KeyInput::Char(c) if !c.is_control() => {
                    cmd.text.push(c);
                    cmd.candidates.clear();
                }
                _ => {}
            }
            return Outcome::Continue;
        }
        if self.help {
            if matches!(key, KeyInput::Escape | KeyInput::Char('?')) {
                self.help = false;
            }
            if key != KeyInput::Char('q') {
                return Outcome::Continue;
            }
        }
        match key {
            KeyInput::Escape => return Outcome::Quit,
            KeyInput::F(n) => {
                if let Some(p) = Page::from_fkey(n) {
                    self.set_page(p, 0);
                }
            }
            KeyInput::Tab => self.cycle_sub(1),
            KeyInput::BackTab => self.cycle_sub(-1),
            KeyInput::Left => self.camera.orbit_by(-ARROW_STEP_DEG, 0.0),
            KeyInput::Right => self.camera.orbit_by(ARROW_STEP_DEG, 0.0),
            KeyInput::Up => self.camera.orbit_by(0.0, ARROW_STEP_DEG),
            KeyInput::Down => self.camera.orbit_by(0.0, -ARROW_STEP_DEG),
            KeyInput::Char(c) => return self.handle_char(c.to_ascii_lowercase(), world),
            _ => {}
        }
        Outcome::Continue
    }

    fn handle_char(&mut self, c: char, world: &World) -> Outcome {
        match c {
            ':' | ';' => {
                self.cmd = Some(CommandLine {
                    text: String::new(),
                    candidates: Vec::new(),
                    history_pos: None,
                })
            }
            '?' => self.help = !self.help,
            'q' => return Outcome::Quit,
            't' => self.cycle_theme(),
            'v' => self.next_vehicle(world),
            'x' => self.show_ecef = !self.show_ecef,
            'z' => self.camera.cycle_exaggeration(),
            'f' => self.camera.earth_fixed = !self.camera.earth_fixed,
            'c' => self.camera.focus_vehicle = !self.camera.focus_vehicle,
            '+' | '=' => self.camera.zoom_by(1.0 / ZOOM_STEP),
            '-' | '_' => self.camera.zoom_by(ZOOM_STEP),
            '1' => self.toggles.scanlines = !self.toggles.scanlines,
            '2' => self.toggles.bloom = !self.toggles.bloom,
            '3' => self.toggles.persistence = !self.toggles.persistence,
            '4' => self.toggles.curvature = !self.toggles.curvature,
            'e' => self.toggles = EffectToggles::all(!self.toggles.any()),
            '0' => self.send(Command::Live),
            ' ' => {
                let c = if world.clock.paused {
                    Command::Resume
                } else {
                    Command::Pause
                };
                // A LIVE clock has nothing to resume; Space on LIVE pauses it.
                self.send(c);
            }
            '[' => self.send(Command::SetRate(warp_step(world, false))),
            ']' => self.send(Command::SetRate(warp_step(world, true))),
            _ => {}
        }
        Outcome::Continue
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
        let cmd_view = self.cmd.as_ref().map(|c| CommandView {
            text: c.text.clone(),
            candidates: c.candidates.clone(),
        });
        self.now_s = stats.elapsed_s;
        if self
            .status
            .as_ref()
            .is_some_and(|(_, until)| *until < stats.elapsed_s)
        {
            self.status = None;
        }
        let layout = screens::render(
            &mut grid,
            &PageInput {
                world,
                vehicle,
                theme: &theme,
                toggles: self.toggles,
                camera: &self.camera,
                show_ecef: self.show_ecef,
                limits: &self.limits,
                frame_ms: stats.frame_ms,
                gpu_ms: stats.gpu_ms,
                adapter: &self.adapter,
                size_px,
                page: self.page,
                sub: self.sub,
                command: cmd_view.as_ref(),
                status: self.status.as_ref().map(|(m, _)| m),
                help: self.help,
                settings: &self.settings,
            },
        );
        let globe_px = layout.globe.map_or([0; 4], |r| cell_rect_px(&theme, r));
        let ground_px = layout.ground.map(|r| cell_rect_px(&theme, r));
        let eclipse_px = layout.eclipse.map(|r| cell_rect_px(&theme, r));
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
            if let Some(v) = vehicle {
                if let Some(px) = ground_px {
                    page::paint_ground_track(&painter, &theme, px, coast, v);
                }
                if let Some(px) = eclipse_px {
                    page::paint_eclipse_ring(&painter, &theme, px, v, now);
                }
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
            grid.paint(&painter, &theme, Pos2::ZERO, stats.elapsed_s);
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
            interactive_rect_px: has_area.then_some(globe_px),
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

/// Next rate for `[` (halve) / `]` (double) from the current clock; from LIVE or a pause start at 1x.
fn warp_step(world: &World, up: bool) -> f64 {
    use mctl_core::command::{MAX_RATE, MIN_RATE};
    let c = world.clock;
    let cur = if c.is_live() || c.rate == 0.0 {
        WARP_BASE
    } else {
        c.rate
    };
    // Doubling moves the magnitude away from zero for `]` regardless of direction of travel.
    let mag = if up { cur.abs() * 2.0 } else { cur.abs() / 2.0 };
    mag.clamp(MIN_RATE, MAX_RATE).copysign(cur)
}
