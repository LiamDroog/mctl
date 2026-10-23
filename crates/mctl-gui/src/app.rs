//! Windowed front end: winit event loop, wgpu surface, egui-winit input.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::console::{Console, FrameStats, KeyInput, Outcome};
use crate::gpu::{self, Gpu};
use crate::render::{FrameRequest, Renderer};
use crate::theme::Theme;

/// Zoom factor per wheel notch; mouse drag gain.
const ZOOM_STEP: f32 = 1.15;
const DRAG_DEG_PER_PX: f32 = 0.3;

/// Frames after which the GPU is assumed to have left its low-clock idle state (see the gfx report).
const SLOW_FRAME_WARMUP: usize = 150;

#[derive(Debug, Clone, Copy)]
pub enum Present {
    /// Vsync (always available).
    Fifo,
    /// No vsync; tearing possible. Used to measure real frame time rather than the refresh interval.
    Immediate,
    Mailbox,
}

pub struct Options {
    pub size: [u32; 2],
    pub fullscreen: bool,
    pub present: Present,
    pub low_power: bool,
    pub exit_after_frames: Option<u64>,
    pub start_theme: Option<String>,
    /// The running engine this window displays.
    pub live: crate::offline::Live,
}

struct Running {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    console: Console,
    egui_state: egui_winit::State,
    gpu: Arc<Gpu>,
}

struct App {
    opts: Options,
    themes: Option<Vec<Theme>>,
    running: Option<Running>,
    started: Instant,
    last_frame: Instant,
    frame_ms: f32,
    frames: u64,
    /// Per-frame (interval, surface-acquire wait, UI+record+submit) in ms.
    frame_log: Vec<[f32; 3]>,
    error: Option<anyhow::Error>,
    /// A drag that began inside the orbit map; continues until release wherever the cursor goes.
    dragging: bool,
    last_cursor: Option<(f32, f32)>,
    /// Orbit-map rectangle of the last frame, `[x, y, w, h]` px.
    interactive: Option<[u32; 4]>,
    shift: bool,
}

fn fkey(n: &NamedKey) -> Option<u8> {
    use NamedKey::*;
    [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10]
        .iter()
        .position(|k| k == n)
        .map(|i| i as u8 + 1)
}

fn inside(rect: Option<[u32; 4]>, p: Option<(f32, f32)>) -> bool {
    match (rect, p) {
        (Some([x, y, w, h]), Some((px, py))) => {
            px >= x as f32 && py >= y as f32 && px < (x + w) as f32 && py < (y + h) as f32
        }
        _ => false,
    }
}

fn pick_format(caps: &wgpu::SurfaceCapabilities) -> Result<wgpu::TextureFormat> {
    // The post pass writes gamma-space values, so an sRGB surface would double-encode them.
    caps.formats
        .iter()
        .copied()
        .find(|f| !f.is_srgb())
        .ok_or_else(|| {
            anyhow!(
                "surface offers only sRGB formats {:?}; need a linear one",
                caps.formats
            )
        })
}

fn pick_present(caps: &wgpu::SurfaceCapabilities, want: Present) -> Result<wgpu::PresentMode> {
    let mode = match want {
        Present::Fifo => wgpu::PresentMode::Fifo,
        Present::Immediate => wgpu::PresentMode::Immediate,
        Present::Mailbox => wgpu::PresentMode::Mailbox,
    };
    if !caps.present_modes.contains(&mode) {
        bail!(
            "present mode {mode:?} not supported; surface offers {:?}",
            caps.present_modes
        );
    }
    Ok(mode)
}

impl App {
    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let mut attrs = Window::default_attributes()
            .with_title("mctl gfx spike")
            .with_inner_size(PhysicalSize::new(self.opts.size[0], self.opts.size[1]));
        if self.opts.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = Arc::new(event_loop.create_window(attrs).context("creating window")?);
        let instance = gpu::new_instance();
        let surface = instance
            .create_surface(window.clone())
            .context("creating surface")?;
        let gpu = Arc::new(gpu::create(&instance, Some(&surface), self.opts.low_power)?);

        let caps = surface.get_capabilities(&gpu.adapter);
        let format = pick_format(&caps)?;
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: pick_present(&caps, self.opts.present)?,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &config);
        tracing::info!(?format, present = ?config.present_mode, w = config.width, h = config.height, "surface configured");

        let themes = self.themes.take().expect("themes consumed once");
        let console = Console::new(
            themes,
            self.opts.start_theme.as_deref(),
            gpu.info.name.clone(),
            self.opts.live.limits.clone(),
            self.opts.live.vehicle_id.clone(),
        )?;
        let mut console = console;
        console.set_keys(
            crate::keys::KeyMap::from_config(&self.opts.live.keys).map_err(anyhow::Error::msg)?,
        );
        console.connect(
            Some(self.opts.live.commands.clone()),
            self.opts.live.settings.clone(),
        );
        let globe = crate::offline::build_globe(&gpu, console.theme())?;
        let renderer = Renderer::new(gpu.clone(), format, [config.width, config.height], globe);
        let egui_state = egui_winit::State::new(
            console.ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(1.0),
            None,
            None,
        );
        self.running = Some(Running {
            window,
            surface,
            config,
            renderer,
            console,
            egui_state,
            gpu,
        });
        Ok(())
    }

    fn redraw(&mut self) -> Result<bool> {
        let now = Instant::now();
        self.frame_ms = now.duration_since(self.last_frame).as_secs_f32() * 1000.0;
        self.last_frame = now;
        // The first interval includes startup; do not let it pollute the log.

        let Some(r) = self.running.as_mut() else {
            return Ok(true);
        };
        let elapsed_s = self.started.elapsed().as_secs_f64();

        let size = [r.config.width, r.config.height];
        let mut raw = r.egui_state.take_egui_input(&r.window);
        // The grid is addressed in physical pixels (theme cell sizes are pixels), so egui is run at
        // 1 point = 1 pixel regardless of the display scale factor. HiDPI policy is an M2 decision.
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size[0] as f32, size[1] as f32),
        ));
        raw.viewports
            .entry(raw.viewport_id)
            .or_default()
            .native_pixels_per_point = Some(1.0);
        let stats = FrameStats {
            elapsed_s,
            frame_ms: self.frame_ms,
            gpu_ms: r.renderer.gpu_ms(),
        };
        let world = self.opts.live.bus.latest();
        let mut prepared = r.console.prepare(raw, size, &world, &stats);
        self.interactive = prepared.interactive_rect_px;

        let acquire_start = Instant::now();
        let frame = match r.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                r.surface.configure(&r.gpu.device, &r.config);
                return Ok(true);
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(true);
            }
            other => bail!("surface error: {other:?}"),
        };
        let acquire_ms = acquire_start.elapsed().as_secs_f32() * 1000.0;
        let work_start = Instant::now();
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        r.renderer.render(&mut FrameRequest {
            target: &view,
            size_px: size,
            pixels_per_point: prepared.pixels_per_point,
            paint_jobs: &prepared.jobs,
            textures_delta: &mut prepared.textures_delta,
            globe_rect_px: prepared.globe_rect_px,
            scene: prepared.scene.as_ref(),
            orbit: prepared.orbit.as_ref().map(|(k, v)| (*k, v.as_slice())),
            theme: r.console.theme(),
            toggles: r.console.toggles,
        })?;
        let work_ms = work_start.elapsed().as_secs_f32() * 1000.0;
        // The first frames include startup and shader compilation; keep them out of the statistics.
        if self.frames > 5 {
            self.frame_log.push([self.frame_ms, acquire_ms, work_ms]);
        }
        r.window.pre_present_notify();
        r.gpu.queue.present(frame);
        self.frames += 1;
        Ok(self.opts.exit_after_frames.is_none_or(|n| self.frames < n))
    }

    fn handle_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        key: &Key,
        text: Option<&str>,
        shift: bool,
    ) {
        let world = self.opts.live.bus.latest();
        let Some(r) = self.running.as_mut() else {
            return;
        };
        let input = match key {
            Key::Named(NamedKey::Escape) => Some(KeyInput::Escape),
            Key::Named(NamedKey::Enter) => Some(KeyInput::Enter),
            Key::Named(NamedKey::Backspace) => Some(KeyInput::Backspace),
            Key::Named(NamedKey::Tab) if shift => Some(KeyInput::BackTab),
            Key::Named(NamedKey::Tab) => Some(KeyInput::Tab),
            Key::Named(NamedKey::Space) => Some(KeyInput::Char(' ')),
            Key::Named(NamedKey::ArrowLeft) => Some(KeyInput::Left),
            Key::Named(NamedKey::ArrowRight) => Some(KeyInput::Right),
            Key::Named(NamedKey::ArrowUp) => Some(KeyInput::Up),
            Key::Named(NamedKey::ArrowDown) => Some(KeyInput::Down),
            Key::Named(n) => fkey(n).map(KeyInput::F),
            // `text` carries the shifted character (`?`, `:`) that logical_key may not.
            Key::Character(c) => text.unwrap_or(c).chars().next().map(KeyInput::Char),
            _ => None,
        };
        if let Some(k) = input
            && r.console.handle_key(k, &world) == Outcome::Quit
        {
            event_loop.exit();
        }
        let style = r.console.theme().globe.clone();
        r.renderer.globe_mut().set_style(style);
    }

    fn summarise(&self) {
        if self.frame_log.is_empty() {
            return;
        }
        let col = |i: usize| {
            let mut v: Vec<f32> = self.frame_log.iter().map(|r| r[i]).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            v
        };
        let pct = |v: &[f32], p: f32| v[((v.len() - 1) as f32 * p) as usize];
        let (interval, acquire, work) = (col(0), col(1), col(2));
        // A "slow" frame missed at least one refresh: more than 1.5x the median interval.
        let slow_at: Vec<usize> = self
            .frame_log
            .iter()
            .enumerate()
            .filter(|(_, r)| r[0] > pct(&interval, 0.5) * 1.5)
            .map(|(i, _)| i)
            .collect();
        tracing::info!(
            frames = interval.len(),
            interval_median_ms = pct(&interval, 0.5),
            interval_p95_ms = pct(&interval, 0.95),
            interval_max_ms = pct(&interval, 1.0),
            acquire_median_ms = pct(&acquire, 0.5),
            work_median_ms = pct(&work, 0.5),
            work_max_ms = pct(&work, 1.0),
            slow_frames = slow_at.len(),
            slow_after_warmup = slow_at.iter().filter(|&&i| i >= SLOW_FRAME_WARMUP).count(),
            first_slow = ?&slow_at[..slow_at.len().min(20)],
            "window frame statistics"
        );
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.running.is_some() {
            return;
        }
        if let Err(e) = self.init(event_loop) {
            self.error = Some(e);
            event_loop.exit();
            return;
        }
        if let Some(r) = &self.running {
            r.window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let Some(r) = self.running.as_mut() {
            let _ = r.egui_state.on_window_event(&r.window, &event);
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.running.as_mut() {
                    r.config.width = size.width.max(1);
                    r.config.height = size.height.max(1);
                    r.surface.configure(&r.gpu.device, &r.config);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                self.dragging =
                    state == ElementState::Pressed && inside(self.interactive, self.last_cursor);
                self.last_cursor = self.last_cursor.filter(|_| self.dragging);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let now = (position.x as f32, position.y as f32);
                if let (true, Some(prev), Some(r)) =
                    (self.dragging, self.last_cursor, self.running.as_mut())
                {
                    r.console.camera.orbit_by(
                        -(now.0 - prev.0) * DRAG_DEG_PER_PX,
                        (now.1 - prev.1) * DRAG_DEG_PER_PX,
                    );
                }
                self.last_cursor = Some(now);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
                if let (true, Some(r)) = (
                    inside(self.interactive, self.last_cursor),
                    self.running.as_mut(),
                ) {
                    r.console.camera.zoom_by(ZOOM_STEP.powf(-notches));
                }
            }
            WindowEvent::ModifiersChanged(m) => self.shift = m.state().shift_key(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                self.handle_key(
                    event_loop,
                    &event.logical_key,
                    event.text.as_deref(),
                    self.shift,
                );
            }
            WindowEvent::RedrawRequested => match self.redraw() {
                Ok(true) => {
                    if let Some(r) = &self.running {
                        r.window.request_redraw();
                    }
                }
                Ok(false) => event_loop.exit(),
                Err(e) => {
                    self.error = Some(e);
                    event_loop.exit();
                }
            },
            _ => {}
        }
    }
}

pub fn run(themes: Vec<Theme>, opts: Options) -> Result<()> {
    let event_loop = EventLoop::new().context("creating event loop (no display server?)")?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        opts,
        themes: Some(themes),
        running: None,
        started: Instant::now(),
        last_frame: Instant::now(),
        frame_ms: 0.0,
        frames: 0,
        frame_log: Vec::new(),
        error: None,
        dragging: false,
        last_cursor: None,
        interactive: None,
        shift: false,
    };
    event_loop.run_app(&mut app).context("event loop")?;
    app.summarise();
    if let Some(e) = app.error {
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::inside;

    #[test]
    fn a_point_is_inside_only_within_the_orbit_map_rectangle() {
        let rect = Some([100, 50, 200, 100]);
        assert!(
            inside(rect, Some((100.0, 50.0))),
            "top-left corner is inside"
        );
        assert!(inside(rect, Some((299.9, 149.9))));
        assert!(!inside(rect, Some((300.0, 100.0))), "right edge is outside");
        assert!(
            !inside(rect, Some((150.0, 150.0))),
            "bottom edge is outside"
        );
        assert!(!inside(rect, Some((99.0, 100.0))));
        assert!(!inside(rect, Some((150.0, 49.0))));
    }

    #[test]
    fn without_a_rectangle_or_a_cursor_nothing_is_inside() {
        assert!(
            !inside(None, Some((10.0, 10.0))),
            "pages with no orbit map cannot rotate it"
        );
        assert!(
            !inside(Some([0, 0, 100, 100]), None),
            "unknown cursor position"
        );
    }
}
