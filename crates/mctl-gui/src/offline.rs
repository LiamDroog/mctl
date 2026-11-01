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
        assets::NIGHT_JPEG,
        gpu.info.device_type == wgpu::DeviceType::Cpu,
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
    /// The `[keys]` table from the configuration.
    pub keys: std::collections::BTreeMap<String, Vec<String>>,
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
        allow_software: bool,
    ) -> Result<Self> {
        let instance = gpu::new_instance();
        let gpu = Arc::new(gpu::create(&instance, None, low_power, allow_software)?);
        let console = Console::new(
            themes,
            start_theme,
            gpu.info.name.clone(),
            live.limits.clone(),
            live.vehicle_id.clone(),
        )?;
        let mut console = console;
        console.set_keys(crate::keys::KeyMap::from_config(&live.keys).map_err(anyhow::Error::msg)?);
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
    /// `page`, `page:sub` or `all`.
    pub pages: Vec<String>,
    pub low_power: bool,
    /// Render on a software (CPU) adapter instead of refusing to.
    pub allow_software: bool,
    /// Command lines run before the captures, e.g. `burn add apo vnb 1 0 0`.
    pub exec: Vec<String>,
    /// Capture each scene of the Pi/CRT display (with this mix of scenes) instead of console pages.
    pub kiosk: Option<crate::kiosk::Mix>,
}

/// Expand `--pages` into (page, sub-page index) pairs.
fn expand_pages(specs: &[String]) -> Result<Vec<(crate::pages::Page, usize)>> {
    use crate::pages::{ALL_PAGES, Page};
    let mut out = Vec::new();
    for s in specs {
        if s == "all" {
            for p in ALL_PAGES {
                out.extend((0..p.subpages().len()).map(|i| (p, i)));
            }
            continue;
        }
        let (name, sub) = s
            .split_once(':')
            .map_or((s.as_str(), None), |(a, b)| (a, Some(b)));
        let page = Page::from_name(name).with_context(|| format!("unknown page {name:?}"))?;
        match sub {
            None => out.push((page, 0)),
            Some(x) => out.push((
                page,
                page.resolve_sub(x)
                    .with_context(|| format!("{name} has no sub-page {x:?}"))?,
            )),
        }
    }
    Ok(out)
}

pub fn shots(live: &Live, themes: Vec<Theme>, opts: &ShotOptions) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(&opts.out_dir)
        .with_context(|| format!("creating {}", opts.out_dir.display()))?;
    let count = themes.len();
    let mut off = Offline::new(
        live,
        themes,
        None,
        opts.size,
        opts.low_power,
        opts.allow_software,
    )?;
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
    let pages = if opts.kiosk.is_some() {
        Vec::new()
    } else {
        expand_pages(&opts.pages)?
    };
    let mut written = Vec::new();
    for line in &opts.exec {
        let world = live.bus.latest();
        off.console
            .exec_line(line, &world)
            .map_err(|e| anyhow::anyhow!("--exec {line:?}: {e}"))?;
    }
    if !opts.exec.is_empty() {
        // Give the engine a moment to take the commands and publish the result.
        std::thread::sleep(std::time::Duration::from_secs(4));
    }
    if let Some(mix) = opts.kiosk {
        if let Some(&idx) = which.first() {
            off.select_theme(idx);
        }
        // Held, so nothing advances on its own while a scene settles.
        off.console.set_kiosk(crate::kiosk::KioskConfig {
            dwell_s: f64::MAX,
            mix,
            order: crate::kiosk::Order::Sequential,
            cycle_themes: false,
            vehicles: Vec::new(),
        });
        let count = crate::kiosk::scenes(&live.bus.latest(), mix).len();
        for n in 0..count {
            off.console.set_kiosk_scene(n);
            for f in 0..opts.warmup_frames {
                off.frame(f, 16.7)?;
            }
            let path = opts.out_dir.join(format!(
                "kiosk-{n:02}-{}x{}.png",
                opts.size[0], opts.size[1]
            ));
            off.capture()?
                .save(&path)
                .with_context(|| format!("writing {}", path.display()))?;
            tracing::info!(path = %path.display(), "screenshot written");
            written.push(path);
        }
        return Ok(written);
    }
    for idx in which {
        off.select_theme(idx);
        if opts.effects_off {
            off.console.toggles = EffectToggles::all(false);
        }
        for (page, sub) in &pages {
            off.console.page = *page;
            off.console.sub = *sub;
            for n in 0..opts.warmup_frames {
                off.frame(n, 16.7)?;
            }
            let img = off.capture()?;
            let name = off.console.theme().name.to_lowercase();
            let path = opts.out_dir.join(format!(
                "{name}-{}-{}-{}x{}{}.png",
                page.key(),
                sub + 1,
                opts.size[0],
                opts.size[1],
                if opts.effects_off { "-fx-off" } else { "" }
            ));
            img.save(&path)
                .with_context(|| format!("writing {}", path.display()))?;
            tracing::info!(path = %path.display(), "screenshot written");
            written.push(path);
        }
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
    /// Render on a software (CPU) adapter instead of refusing to.
    pub allow_software: bool,
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
        let mut off = Offline::new(
            live,
            themes.clone(),
            None,
            size,
            opts.low_power,
            opts.allow_software,
        )?;
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

    /// A world whose ISS telemetry is synthetic (every item a slow wave with 15 minutes of history),
    /// for looking at the telemetry visuals when the real feed is down. The numbers mean nothing.
    fn synthetic_iss_world() -> mctl_core::world::World {
        use mctl_core::time::Utc;
        use mctl_core::world::{
            HISTORY_LEN, Health, TelemetryItem, TelemetryView, VehicleView, World,
        };
        let cfg = mctl_sources::isslive::config::Config::parse(
            "embedded",
            mctl_sources::isslive::config::DEFAULT_CONFIG,
        )
        .unwrap();
        let mut items = Vec::new();
        let mut totals: Vec<(String, usize)> = Vec::new();
        for (k, i) in cfg.items.iter().enumerate() {
            match totals.iter_mut().find(|(g, _)| *g == i.group) {
                Some((_, n)) => *n += 1,
                None => totals.push((i.group.clone(), 1)),
            }
            let (base, amp) = match i.units.as_str() {
                "deg" => (120.0 + 30.0 * k as f64 % 200.0, 0.15),
                "deg/s" => (0.0, 0.05),
                "V" => (151.0, 0.3),
                "A" => (3.0, 1.0),
                "degC" => (22.0 + (k % 7) as f64, 0.6),
                "mmHg" => (150.0 - (k % 5) as f64 * 30.0, 2.0),
                "N\u{b7}m" => (0.0, 3.0),
                _ => (1.0 + (k % 4) as f64, 0.0),
            };
            let phase = k as f64 * 0.7;
            let hist: Vec<f32> = (0..HISTORY_LEN)
                .map(|n| (base + amp * (n as f64 * 0.15 + phase).sin()) as f32)
                .collect();
            let value = f64::from(*hist.last().unwrap());
            items.push(TelemetryItem {
                id: i.id.clone(),
                group: i.group.clone(),
                description: i.description.clone(),
                label: i.label.clone().unwrap_or_else(|| i.description.clone()),
                section: i.section.clone().unwrap_or_else(|| i.group.clone()),
                row: i.row.clone().unwrap_or_else(|| i.description.clone()),
                col: i.col.clone().unwrap_or_default(),
                units: i.units.clone(),
                value: Some(value),
                text: Some(value.to_string()),
                rx: Utc(0.0),
                age_s: 1.0,
                history: hist.into(),
            });
        }
        let mut w = World::empty(Utc::now().unwrap());
        w.telemetry = Some(TelemetryView {
            health: Health::Ok,
            subscribed: items.len(),
            items,
            group_totals: totals,
            feed_age_s: Some(1.0),
        });
        {
            use mctl_core::prox::{ProxPoint, ProxState, ProxView};
            // An invented approach along the V-bar from 3 km behind, for looking at the plot.
            let path = |from: f64, to: f64| -> Vec<ProxPoint> {
                (0..=40)
                    .map(|k| {
                        let f = f64::from(k) / 40.0;
                        let x = from + (to - from) * f;
                        ProxPoint {
                            t: Utc(f64::from(k) * 60.0),
                            lvlh_km: [x, 0.0, 0.25 * (f * 3.0).sin() + 0.4 * (1.0 - f)],
                        }
                    })
                    .collect()
            };
            let now = [-1.1, 0.0, 0.30];
            w.prox = vec![
                std::sync::Arc::new(ProxView {
                    chaser: "dragon-crew".into(),
                    target: "iss".into(),
                    state: ProxState::Docked,
                    rel_lvlh_km: [0.0; 3],
                    range_km: 0.0,
                    range_rate_km_s: 0.0,
                    source_note: "GP/SGP4 FOR BOTH VEHICLES".into(),
                    ephemeris_grade: false,
                    history: Default::default(),
                    predicted: Default::default(),
                    keep_out_km: 0.2,
                    ellipsoid_semi_km: [2.0, 1.0],
                }),
                std::sync::Arc::new(ProxView {
                    chaser: "dragon-cargo".into(),
                    target: "iss".into(),
                    state: ProxState::Near,
                    rel_lvlh_km: now,
                    range_km: 1.14,
                    range_rate_km_s: -0.0004,
                    source_note: "SYNTHETIC DATA FOR LOOKING AT THE PLOT".into(),
                    ephemeris_grade: false,
                    history: std::sync::Arc::new(path(-3.0, now[0])),
                    predicted: std::sync::Arc::new(path(now[0], 0.0)),
                    keep_out_km: 0.2,
                    ellipsoid_semi_km: [2.0, 1.0],
                }),
            ];
        }
        w.vehicles.push(VehicleView {
            id: "iss".into(),
            name: "ISS".into(),
            norad: 25544,
            resolved_name: None,
            visits: None,
            visitors: vec!["dragon-crew".into(), "dragon-cargo".into()],
            telemetry: Some("isslive".into()),
            regime: mctl_core::config::Regime::Earth,
            schedule: None,
            deep: None,
            reference_burns: vec![],
            fields: vec![],
            ops: None,
            ops_health: None,
            met_epoch: None,
            sources: vec![],
            solution: None,
            track: None,
            passes: vec![],
            beta_series: Default::default(),
        });
        w
    }

    /// Writes PNGs of the telemetry pages with synthetic data into `$MCTL_SYNTH_DIR`. Needs a GPU
    /// adapter: `MCTL_SYNTH_DIR=/tmp/x cargo test -p mctl-gui synthetic -- --ignored`.
    #[test]
    #[ignore = "needs a GPU adapter and an output directory"]
    fn render_synthetic_telemetry_pages() {
        let dir = std::env::var("MCTL_SYNTH_DIR").expect("set MCTL_SYNTH_DIR");
        let (tx, _rx) = std::sync::mpsc::channel();
        let cfg = mctl_core::config::Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None)
            .unwrap();
        let live = Live {
            bus: Bus::new(synthetic_iss_world()),
            limits: cfg.limits.clone(),
            vehicle_id: "iss".into(),
            commands: tx,
            settings: vec![],
            keys: cfg.keys.clone(),
        };
        let themes = crate::theme::embedded().unwrap();
        let opts = ShotOptions {
            size: [1920, 1080],
            theme: Some("phosphor".into()),
            out_dir: dir.into(),
            effects_off: false,
            warmup_frames: 40,
            pages: [
                "gnc:2", "gnc:3", "eps", "tcs", "tcs:2", "comm", "prox", "sys:5",
            ]
            .map(String::from)
            .to_vec(),
            low_power: false,
            allow_software: false,
            exec: vec![],
            kiosk: None,
        };
        shots(&live, themes, &opts).unwrap();
    }

    mod golden {
        use super::*;
        use std::path::{Path, PathBuf};

        const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
        /// A pixel differs when any channel is off by more than this: absorbs the last-bit differences
        /// between versions of a software driver, not a changed glyph or colour.
        const CHANNEL_TOLERANCE: u8 = 32;
        /// The share of pixels that may differ before an image counts as changed.
        const DIFFERENT_MAX: f64 = 0.001;
        /// Rows at the bottom of a console page that show frame timings, which change every run.
        const FOOTER_PX: u32 = 64;

        fn out_dir() -> PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/golden-actual")
        }

        /// Err with a description when `actual` differs from `golden` by more than the tolerance.
        fn compare(actual: &Path, golden: &Path, ignore_bottom: u32) -> Result<(), String> {
            let a = image::open(actual).map_err(|e| format!("{}: {e}", actual.display()))?;
            let g = image::open(golden).map_err(|e| {
                format!(
                    "{}: {e} (to create it: MCTL_BLESS=1 cargo test -p mctl-gui golden -- --ignored)",
                    golden.display()
                )
            })?;
            let (a, g) = (a.to_rgb8(), g.to_rgb8());
            if a.dimensions() != g.dimensions() {
                return Err(format!(
                    "{}: size {:?} but the golden is {:?}",
                    actual.display(),
                    a.dimensions(),
                    g.dimensions()
                ));
            }
            let rows = a.height().saturating_sub(ignore_bottom);
            let (mut different, mut total) = (0u64, 0u64);
            for y in 0..rows {
                for x in 0..a.width() {
                    total += 1;
                    let (p, q) = (a.get_pixel(x, y), g.get_pixel(x, y));
                    if p.0
                        .iter()
                        .zip(q.0)
                        .any(|(c, d)| c.abs_diff(d) > CHANNEL_TOLERANCE)
                    {
                        different += 1;
                    }
                }
            }
            let share = different as f64 / total.max(1) as f64;
            if share > DIFFERENT_MAX {
                return Err(format!(
                    "{} differs from its golden in {:.2}% of pixels (limit {:.2}%); the new image is left in {}",
                    actual.file_name().unwrap_or_default().to_string_lossy(),
                    share * 100.0,
                    DIFFERENT_MAX * 100.0,
                    out_dir().display()
                ));
            }
            Ok(())
        }

        fn live() -> Live {
            let (tx, _rx) = std::sync::mpsc::channel();
            let cfg =
                mctl_core::config::Config::from_layers(mctl_core::config::DEFAULT_CONFIG, None)
                    .unwrap();
            let mut world = crate::kiosk::test_world(1);
            world.vehicles[0].id = "iss".into();
            world.vehicles[0].name = "ISS".into();
            Live {
                bus: Bus::new(world),
                limits: cfg.limits.clone(),
                vehicle_id: "iss".into(),
                commands: tx,
                settings: vec![],
                keys: cfg.keys.clone(),
            }
        }

        fn options(size: [u32; 2], theme: Option<&str>, pages: &[&str], dir: &Path) -> ShotOptions {
            ShotOptions {
                size,
                theme: theme.map(String::from),
                out_dir: dir.into(),
                effects_off: false,
                warmup_frames: 40,
                pages: pages.iter().map(|p| (*p).to_owned()).collect(),
                low_power: false,
                allow_software: true,
                exec: vec![],
                kiosk: None,
            }
        }

        /// Renders fixed pages from a fixed world and compares them with `tests/golden`. Needs a GPU adapter,
        /// and a software one is fine: `cargo test -p mctl-gui golden -- --ignored`. After an intended change
        /// to how things look, `MCTL_BLESS=1` rewrites the golden images; review them in the diff.
        #[test]
        #[ignore = "needs a GPU adapter (a software one is fine)"]
        fn rendered_pages_match_the_golden_images() {
            let dir = out_dir();
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let live = live();
            let embedded = crate::theme::embedded().unwrap();
            let mut checks: Vec<(PathBuf, u32)> = Vec::new();

            // Console pages: one with the CRT effects on, one without.
            let console = options([1280, 720], Some("phosphor"), &["sys:7"], &dir);
            checks.extend(
                shots(&live, embedded.clone(), &console)
                    .unwrap()
                    .into_iter()
                    .map(|p| (p, FOOTER_PX)),
            );
            let plain = options([1280, 720], Some("modern"), &["sys:8"], &dir);
            checks.extend(
                shots(&live, embedded, &plain)
                    .unwrap()
                    .into_iter()
                    .map(|p| (p, FOOTER_PX)),
            );
            // The Pi display: every picture scene of the (single-vehicle) cycle, at its own size.
            let mut kiosk = options([640, 480], None, &[], &dir);
            kiosk.kiosk = Some(crate::kiosk::Mix::Pretty);
            checks.extend(
                shots(
                    &live,
                    vec![crate::theme::crt_themes().unwrap().remove(0)],
                    &kiosk,
                )
                .unwrap()
                .into_iter()
                .map(|p| (p, 0)),
            );
            assert!(checks.len() >= 5, "only {} images rendered", checks.len());

            let golden = Path::new(GOLDEN_DIR);
            if std::env::var_os("MCTL_BLESS").is_some() {
                std::fs::create_dir_all(golden).unwrap();
                for (p, _) in &checks {
                    std::fs::copy(p, golden.join(p.file_name().unwrap())).unwrap();
                }
                return;
            }
            let failures: Vec<String> = checks
                .iter()
                .filter_map(|(p, ignore_bottom)| {
                    compare(p, &golden.join(p.file_name().unwrap()), *ignore_bottom).err()
                })
                .collect();
            assert!(failures.is_empty(), "{}", failures.join("\n"));
        }
    }
}
