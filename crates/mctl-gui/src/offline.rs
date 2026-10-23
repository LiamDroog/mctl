//! Headless rendering: frame-time benchmark and screenshot capture.
//!
//! Renders the complete pipeline (UI, globe, post chain) into an offscreen texture of any size,
//! so 1440p can be measured on a 1080p panel and results are not clamped to the display refresh.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

use crate::coast;
use mctl_core::bus::Bus;
use mctl_core::config::Limits;

use crate::assets;
use crate::console::{Console, FrameStats, headless_input};
use crate::globe::GlobePass;
use crate::gpu::{self, Gpu};
use crate::render::{EffectToggles, FrameRequest, Renderer, SCENE_FORMAT};
use crate::theme::Theme;

/// Simulated frame interval for headless runs, so animation advances as it would at 60 fps.
const SIM_FRAME_S: f64 = 1.0 / 60.0;
/// Bytes per pixel of the capture format.
const CAPTURE_BPP: u32 = 4;

pub fn build_globe(gpu: &Gpu, theme: &Theme) -> Result<GlobePass> {
    let lines = coast::parse(assets::COAST_50M).context("parsing embedded 50m coastlines")?;
    let mask = coast::rasterise(&lines, assets::COAST_MASK_WIDTH, assets::COAST_MASK_LEVELS);
    GlobePass::new(
        &gpu.device,
        &gpu.queue,
        SCENE_FORMAT,
        assets::EARTH_JPEG,
        &mask,
        theme.globe.clone(),
    )
}

pub fn parse_size(s: &str) -> Result<[u32; 2]> {
    let (w, h) = s
        .split_once('x')
        .with_context(|| format!("size {s:?} must look like 1920x1080"))?;
    let size = [
        w.parse::<u32>().context("width")?,
        h.parse::<u32>().context("height")?,
    ];
    ensure!(
        size.iter().all(|&d| (64..=8192).contains(&d)),
        "size {s:?} out of range 64..=8192"
    );
    Ok(size)
}

/// The live engine a headless run reads its snapshots from.
#[derive(Clone)]
pub struct Live {
    pub bus: Arc<Bus>,
    pub limits: Limits,
    pub vehicle_id: String,
    pub commands: std::sync::mpsc::Sender<mctl_core::command::Command>,
    pub settings: Vec<(String, String)>,
}

struct Offline {
    live: Live,
    gpu: Arc<Gpu>,
    renderer: Renderer,
    console: Console,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    size: [u32; 2],
}

impl Offline {
    fn new(
        live: &Live,
        themes: Vec<Theme>,
        start_theme: Option<&str>,
        size: [u32; 2],
        low_power: bool,
    ) -> Result<Self> {
        let instance = gpu::new_instance();
        let gpu = Arc::new(gpu::create(&instance, None, low_power)?);
        let console = Console::new(
            themes,
            start_theme,
            gpu.info.name.clone(),
            live.limits.clone(),
            live.vehicle_id.clone(),
        )?;
        let mut console = console;
        console.connect(Some(live.commands.clone()), live.settings.clone());
        let globe = build_globe(&gpu, console.theme())?;
        let renderer = Renderer::new(gpu.clone(), SCENE_FORMAT, size, globe);
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offline-target"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCENE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self {
            live: live.clone(),
            gpu,
            renderer,
            console,
            target,
            target_view,
            size,
        })
    }

    fn select_theme(&mut self, idx: usize) {
        self.console.set_theme(idx);
        let style = self.console.theme().globe.clone();
        self.renderer.globe_mut().set_style(style);
    }

    /// Render frame number `n`; returns CPU time spent preparing the UI and recording commands, ms.
    fn frame(&mut self, n: u64, frame_ms: f32) -> Result<f64> {
        let t0 = Instant::now();
        let elapsed = n as f64 * SIM_FRAME_S;
        let stats = FrameStats {
            elapsed_s: elapsed,
            frame_ms,
            gpu_ms: self.renderer.gpu_ms(),
        };
        let world = self.live.bus.latest();
        let mut prepared = self.console.prepare(
            headless_input(self.size, elapsed),
            self.size,
            &world,
            &stats,
        );
        self.renderer.render(&mut FrameRequest {
            target: &self.target_view,
            size_px: self.size,
            pixels_per_point: prepared.pixels_per_point,
            paint_jobs: &prepared.jobs,
            textures_delta: &mut prepared.textures_delta,
            globe_rect_px: prepared.globe_rect_px,
            scene: prepared.scene.as_ref(),
            orbit: prepared.orbit.as_ref().map(|(k, v)| (*k, v.as_slice())),
            theme: self.console.theme(),
            toggles: self.console.toggles,
        })?;
        Ok(t0.elapsed().as_secs_f64() * 1000.0)
    }

    fn capture(&mut self) -> Result<image::RgbaImage> {
        self.renderer.wait_idle()?;
        let [w, h] = self.size;
        let padded = (w * CAPTURE_BPP).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("capture"),
            });
        enc.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.gpu.queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .context("waiting for capture")?;
        rx.recv()
            .context("capture map callback dropped")?
            .context("mapping capture buffer")?;
        let mapped = buf
            .slice(..)
            .get_mapped_range()
            .context("reading capture buffer")?;
        let mut pixels = Vec::with_capacity((w * h * CAPTURE_BPP) as usize);
        for row in mapped.chunks_exact(padded as usize) {
            pixels.extend_from_slice(&row[..(w * CAPTURE_BPP) as usize]);
        }
        drop(mapped);
        buf.unmap();
        image::RgbaImage::from_raw(w, h, pixels).context("capture buffer size mismatch")
    }
}

// ---- screenshots ---------------------------------------------------------------------------------

pub struct ShotOptions {
    pub size: [u32; 2],
    pub theme: Option<String>,
    pub out_dir: PathBuf,
    pub effects_off: bool,
    pub warmup_frames: u64,
    pub low_power: bool,
}

pub fn shots(live: &Live, themes: Vec<Theme>, opts: &ShotOptions) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(&opts.out_dir)
        .with_context(|| format!("creating {}", opts.out_dir.display()))?;
    let count = themes.len();
    let mut off = Offline::new(live, themes, None, opts.size, opts.low_power)?;
    let which: Vec<usize> = match &opts.theme {
        None => (0..count).collect(),
        Some(name) => vec![
            off.console
                .themes
                .iter()
                .position(|t| t.name.eq_ignore_ascii_case(name))
                .with_context(|| format!("unknown theme {name:?}"))?,
        ],
    };
    let mut written = Vec::new();
    for idx in which {
        off.select_theme(idx);
        if opts.effects_off {
            off.console.toggles = EffectToggles::all(false);
        }
        for n in 0..opts.warmup_frames {
            off.frame(n, 16.7)?;
        }
        let img = off.capture()?;
        let name = off.console.theme().name.to_lowercase();
        let path = opts.out_dir.join(format!(
            "{name}-{}x{}{}.png",
            opts.size[0],
            opts.size[1],
            if opts.effects_off { "-fx-off" } else { "" }
        ));
        img.save(&path)
            .with_context(|| format!("writing {}", path.display()))?;
        tracing::info!(path = %path.display(), "screenshot written");
        written.push(path);
    }
    Ok(written)
}

// ---- benchmark -----------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct Stat {
    median: f64,
    p95: f64,
    max: f64,
}

fn stat(mut v: Vec<f64>) -> Stat {
    v.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
    Stat {
        median: at(0.5),
        p95: at(0.95),
        max: at(1.0),
    }
}

#[derive(Debug, Serialize)]
struct BenchRow {
    theme: String,
    size: [u32; 2],
    effects: &'static str,
    frames: usize,
    /// CPU time to run the UI and record/submit commands.
    cpu_ms: Stat,
    /// Submit-to-idle wall time, i.e. GPU execution as the CPU sees it.
    gpu_wall_ms: Stat,
    /// GPU timestamp query across the whole frame; absent if unsupported.
    gpu_ts_ms: Option<Stat>,
}

#[derive(Debug, Serialize)]
struct BenchReport {
    adapter: String,
    backend: String,
    device_type: String,
    driver: String,
    driver_info: String,
    timestamps: bool,
    warmup_frames: u64,
    rows: Vec<BenchRow>,
}

pub struct BenchOptions {
    pub sizes: Vec<[u32; 2]>,
    pub frames: u64,
    pub warmup: u64,
    pub theme: Option<String>,
    pub out: Option<PathBuf>,
    pub low_power: bool,
    /// Also measure each effect alone, to attribute cost.
    pub breakdown: bool,
}

/// Effect combinations to measure. "All on" forces every effect on regardless of the theme's
/// own defaults so the comparison is identical across themes.
fn variants(breakdown: bool) -> Vec<(&'static str, EffectToggles)> {
    let none = EffectToggles::all(false);
    let mut v = vec![("all-on", EffectToggles::all(true)), ("all-off", none)];
    if breakdown {
        v.extend([
            (
                "scanlines-only",
                EffectToggles {
                    scanlines: true,
                    ..none
                },
            ),
            (
                "bloom-only",
                EffectToggles {
                    bloom: true,
                    ..none
                },
            ),
            (
                "persistence-only",
                EffectToggles {
                    persistence: true,
                    ..none
                },
            ),
            (
                "curvature-only",
                EffectToggles {
                    curvature: true,
                    ..none
                },
            ),
        ]);
    }
    v
}

pub fn bench(live: &Live, themes: Vec<Theme>, opts: &BenchOptions) -> Result<()> {
    ensure!(opts.frames >= 10, "need at least 10 measured frames");
    let count = themes.len();
    let mut rows = Vec::new();
    let mut header: Option<BenchReport> = None;
    for &size in &opts.sizes {
        // One device per size keeps targets exactly sized; the adapter and driver are identical.
        let mut off = Offline::new(live, themes.clone(), None, size, opts.low_power)?;
        let which: Vec<usize> = match &opts.theme {
            None => (0..count).collect(),
            Some(n) => vec![
                off.console
                    .themes
                    .iter()
                    .position(|t| t.name.eq_ignore_ascii_case(n))
                    .with_context(|| format!("unknown theme {n:?}"))?,
            ],
        };
        header.get_or_insert_with(|| {
            let i = &off.gpu.info;
            BenchReport {
                adapter: i.name.clone(),
                backend: format!("{:?}", i.backend),
                device_type: format!("{:?}", i.device_type),
                driver: i.driver.clone(),
                driver_info: i.driver_info.clone(),
                timestamps: off.gpu.timestamps,
                warmup_frames: opts.warmup,
                rows: Vec::new(),
            }
        });
        for idx in which {
            for (label, toggles) in variants(opts.breakdown) {
                off.select_theme(idx);
                off.console.toggles = toggles;
                let (mut cpu, mut wall, mut ts) = (Vec::new(), Vec::new(), Vec::new());
                for n in 0..opts.warmup + opts.frames {
                    let cpu_ms = off.frame(n, 16.7)?;
                    let t = Instant::now();
                    off.renderer.wait_idle()?;
                    let wall_ms = t.elapsed().as_secs_f64() * 1000.0;
                    if n >= opts.warmup {
                        cpu.push(cpu_ms);
                        wall.push(wall_ms);
                        if let Some(g) = off.renderer.gpu_ms() {
                            ts.push(f64::from(g));
                        }
                    }
                }
                let row = BenchRow {
                    theme: off.console.theme().name.clone(),
                    size,
                    effects: label,
                    frames: cpu.len(),
                    cpu_ms: stat(cpu),
                    gpu_wall_ms: stat(wall),
                    gpu_ts_ms: (!ts.is_empty()).then(|| stat(ts)),
                };
                tracing::info!(
                    theme = %row.theme, w = size[0], h = size[1], effects = row.effects,
                    cpu_med = format!("{:.2}", row.cpu_ms.median),
                    gpu_wall_med = format!("{:.2}", row.gpu_wall_ms.median),
                    gpu_ts_med = row.gpu_ts_ms.as_ref().map(|s| format!("{:.2}", s.median)).unwrap_or_else(|| "n/a".into()),
                    gpu_ts_p95 = row.gpu_ts_ms.as_ref().map(|s| format!("{:.2}", s.p95)).unwrap_or_else(|| "n/a".into()),
                    "bench"
                );
                rows.push(row);
            }
        }
    }
    let Some(mut report) = header else {
        bail!("no sizes given")
    };
    report.rows = rows;
    if let Some(out) = &opts.out {
        write_json(out, &report)?;
    }
    Ok(())
}

fn write_json(path: &Path, report: &BenchReport) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(report).context("serialising report")?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    tracing::info!(path = %path.display(), "benchmark report written");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_parsing() {
        assert_eq!(parse_size("1920x1080").unwrap(), [1920, 1080]);
        assert!(parse_size("1920").is_err());
        assert!(parse_size("10x10").is_err());
        assert!(parse_size("axb").is_err());
    }

    #[test]
    fn stat_percentiles() {
        let s = stat((1..=100).map(f64::from).collect());
        assert_eq!(s.max, 100.0);
        assert!((s.median - 50.0).abs() <= 1.0);
        assert!((s.p95 - 95.0).abs() <= 1.0);
    }
}
