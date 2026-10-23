//! Frame renderer: globe pass + egui into an offscreen scene texture, then the CRT post chain
//! into whatever target the caller supplies (swapchain image or an offscreen texture).
//!
//! The scene is gamma-space throughout (egui's native output), so scene/bloom use plain
//! `Rgba8Unorm`, including the persistence history. With 8 bits a decayed value can round back up
//! and a ghost would never fade, so the persist shader subtracts one LSB per frame as well.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::{Context, Result};

use crate::globe::{DEPTH_FORMAT, GlobePass, GlobeScene};
use crate::gpu::Gpu;
use crate::theme::{Effects, Theme};

pub const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const HISTORY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Bloom works at 1/N of the scene resolution per axis: wide glow for a fraction of the fill cost (R5).
const BLOOM_DIVISOR: u32 = 4;

/// Runtime on/off state of each effect, initialised from the theme and flipped by hotkeys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectToggles {
    pub scanlines: bool,
    pub bloom: bool,
    pub persistence: bool,
    pub curvature: bool,
}

impl EffectToggles {
    pub fn from_theme(e: &Effects) -> Self {
        Self {
            scanlines: e.scanlines.enabled,
            bloom: e.bloom.enabled,
            persistence: e.persistence.enabled,
            curvature: e.curvature.enabled,
        }
    }
    pub fn all(on: bool) -> Self {
        Self {
            scanlines: on,
            bloom: on,
            persistence: on,
            curvature: on,
        }
    }
    pub fn any(self) -> bool {
        self.scanlines || self.bloom || self.persistence || self.curvature
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PostUniforms {
    out_size: [f32; 4],
    scan: [f32; 4],
    bloom: [f32; 4],
    persist: [f32; 4],
    curve: [f32; 4],
    blur_dir: [f32; 4],
    bloom_div: [f32; 4],
}

fn post_uniforms(
    size: [u32; 2],
    e: &Effects,
    t: EffectToggles,
    blur_dir: [f32; 2],
) -> PostUniforms {
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    PostUniforms {
        out_size: [
            size[0] as f32,
            size[1] as f32,
            1.0 / size[0] as f32,
            1.0 / size[1] as f32,
        ],
        scan: [
            flag(t.scanlines),
            e.scanlines.strength,
            e.scanlines.period_px,
            0.0,
        ],
        bloom: [
            flag(t.bloom),
            e.bloom.threshold,
            e.bloom.intensity,
            e.bloom.radius_px,
        ],
        persist: [flag(t.persistence), e.persistence.decay, 0.0, 0.0],
        curve: [
            flag(t.curvature),
            e.curvature.amount,
            e.curvature.vignette,
            0.0,
        ],
        blur_dir: [blur_dir[0], blur_dir[1], 0.0, 0.0],
        bloom_div: [BLOOM_DIVISOR as f32, 0.0, 0.0, 0.0],
    }
}

struct Tex {
    view: wgpu::TextureView,
}

fn make_tex(
    device: &wgpu::Device,
    label: &str,
    size: [u32; 2],
    format: wgpu::TextureFormat,
    extra: wgpu::TextureUsages,
) -> Tex {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size[0].max(1),
            height: size[1].max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | extra,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Tex { view }
}

/// Size-dependent render targets and the bind groups that reference them.
struct Targets {
    size: [u32; 2],
    scene: Tex,
    depth: Tex,
    bloom_a: Tex,
    bloom_b: Tex,
    hist: [Tex; 2],
    bg_extract: wgpu::BindGroup,
    bg_blur_h: wgpu::BindGroup,
    bg_blur_v: wgpu::BindGroup,
    bg_persist: [wgpu::BindGroup; 2],
    bg_final_scene: wgpu::BindGroup,
    bg_final_hist: [wgpu::BindGroup; 2],
}

struct PostResources {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uni_main: wgpu::Buffer,
    uni_h: wgpu::Buffer,
    uni_v: wgpu::Buffer,
    extract: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    persist: wgpu::RenderPipeline,
    final_: wgpu::RenderPipeline,
}

impl PostResources {
    fn new(device: &wgpu::Device, final_format: wgpu::TextureFormat) -> Self {
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex(2),
                tex(3),
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uni = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size_of::<PostUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("post"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/post.wgsl").into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |label: &str, fs: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_full"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            extract: pipeline("post-extract", "fs_extract", SCENE_FORMAT),
            blur: pipeline("post-blur", "fs_blur", SCENE_FORMAT),
            persist: pipeline("post-persist", "fs_persist", HISTORY_FORMAT),
            final_: pipeline("post-final", "fs_final", final_format),
            uni_main: uni("post-uniforms-main"),
            uni_h: uni("post-uniforms-blur-h"),
            uni_v: uni("post-uniforms-blur-v"),
            layout,
            sampler,
        }
    }

    fn bind(
        &self,
        device: &wgpu::Device,
        uni: &wgpu::Buffer,
        a: &wgpu::TextureView,
        b: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post-bind-group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uni.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(a),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(b),
                },
            ],
        })
    }
}

impl Targets {
    fn new(device: &wgpu::Device, post: &PostResources, size: [u32; 2]) -> Self {
        let bloom_size = [
            size[0].div_ceil(BLOOM_DIVISOR),
            size[1].div_ceil(BLOOM_DIVISOR),
        ];
        let scene = make_tex(
            device,
            "scene",
            size,
            SCENE_FORMAT,
            wgpu::TextureUsages::empty(),
        );
        let depth = make_tex(
            device,
            "depth",
            size,
            DEPTH_FORMAT,
            wgpu::TextureUsages::empty(),
        );
        let bloom_a = make_tex(
            device,
            "bloom-a",
            bloom_size,
            SCENE_FORMAT,
            wgpu::TextureUsages::empty(),
        );
        let bloom_b = make_tex(
            device,
            "bloom-b",
            bloom_size,
            SCENE_FORMAT,
            wgpu::TextureUsages::empty(),
        );
        let hist = [
            make_tex(
                device,
                "history-0",
                size,
                HISTORY_FORMAT,
                wgpu::TextureUsages::empty(),
            ),
            make_tex(
                device,
                "history-1",
                size,
                HISTORY_FORMAT,
                wgpu::TextureUsages::empty(),
            ),
        ];
        Self {
            size,
            bg_extract: post.bind(device, &post.uni_main, &scene.view, &scene.view),
            bg_blur_h: post.bind(device, &post.uni_h, &bloom_a.view, &bloom_a.view),
            bg_blur_v: post.bind(device, &post.uni_v, &bloom_b.view, &bloom_b.view),
            bg_persist: [
                post.bind(device, &post.uni_main, &scene.view, &hist[1].view),
                post.bind(device, &post.uni_main, &scene.view, &hist[0].view),
            ],
            bg_final_scene: post.bind(device, &post.uni_main, &scene.view, &bloom_a.view),
            bg_final_hist: [
                post.bind(device, &post.uni_main, &hist[0].view, &bloom_a.view),
                post.bind(device, &post.uni_main, &hist[1].view, &bloom_a.view),
            ],
            scene,
            depth,
            bloom_a,
            bloom_b,
            hist,
        }
    }
}

const MAP_PENDING: u8 = 0;
const MAP_READY: u8 = 1;
const MAP_FAILED: u8 = 2;

/// GPU timestamp readback with one frame in flight.
struct GpuTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    period_ns: f32,
    /// `MAP_PENDING` until the readback callback runs, then `MAP_READY` or `MAP_FAILED`.
    mapped: Arc<AtomicU8>,
    in_flight: bool,
    last_ms: Option<f32>,
}

impl GpuTimer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let buf = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 16,
                usage,
                mapped_at_creation: false,
            })
        };
        Self {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("frame-timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            }),
            resolve: buf(
                "ts-resolve",
                wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            ),
            read: buf(
                "ts-read",
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            ),
            period_ns: queue.get_timestamp_period(),
            mapped: Arc::new(AtomicU8::new(MAP_PENDING)),
            in_flight: false,
            last_ms: None,
        }
    }

    /// Pick up a finished readback, if any. A failed map is an error, not something to hide.
    fn collect(&mut self) -> Result<()> {
        if !self.in_flight {
            return Ok(());
        }
        match self.mapped.load(Ordering::Acquire) {
            MAP_PENDING => return Ok(()),
            MAP_FAILED => anyhow::bail!("GPU timestamp readback failed"),
            _ => {}
        }
        let ticks: [u64; 2] = {
            let view = self
                .read
                .slice(..)
                .get_mapped_range()
                .context("reading mapped timestamps")?;
            let words: &[u64] = bytemuck::cast_slice(&view);
            [words[0], words[1]]
        };
        self.read.unmap();
        self.mapped.store(MAP_PENDING, Ordering::Release);
        self.in_flight = false;
        // Timestamps wrap or reorder across some power transitions; drop those samples.
        if ticks[1] > ticks[0] {
            self.last_ms = Some((ticks[1] - ticks[0]) as f32 * self.period_ns / 1.0e6);
        }
        Ok(())
    }
}

/// Everything needed to draw one frame.
/// Takes the egui texture delta mutably: egui panics if one is dropped unapplied, so render consumes it.
pub struct FrameRequest<'a> {
    pub target: &'a wgpu::TextureView,
    pub size_px: [u32; 2],
    pub pixels_per_point: f32,
    pub paint_jobs: &'a [egui::ClippedPrimitive],
    pub textures_delta: &'a mut egui::TexturesDelta,
    /// Globe viewport in target pixels: [x, y, w, h].
    pub globe_rect_px: Option<[u32; 4]>,
    pub scene: &'a GlobeScene,
    pub theme: &'a Theme,
    pub toggles: EffectToggles,
}

pub struct Renderer {
    gpu: Arc<Gpu>,
    egui: egui_wgpu::Renderer,
    globe: GlobePass,
    post: PostResources,
    targets: Targets,
    timer: Option<GpuTimer>,
    hist_cur: usize,
    prev_persistence: bool,
}

impl Renderer {
    pub fn new(
        gpu: Arc<Gpu>,
        final_format: wgpu::TextureFormat,
        size_px: [u32; 2],
        globe: GlobePass,
    ) -> Self {
        let egui = egui_wgpu::Renderer::new(
            &gpu.device,
            SCENE_FORMAT,
            egui_wgpu::RendererOptions::default(),
        );
        let post = PostResources::new(&gpu.device, final_format);
        let targets = Targets::new(&gpu.device, &post, size_px);
        let timer = gpu
            .timestamps
            .then(|| GpuTimer::new(&gpu.device, &gpu.queue));
        Self {
            gpu,
            egui,
            globe,
            post,
            targets,
            timer,
            hist_cur: 0,
            prev_persistence: false,
        }
    }

    pub fn globe_mut(&mut self) -> &mut GlobePass {
        &mut self.globe
    }

    /// GPU time of the most recently completed frame, if timestamps are supported.
    pub fn gpu_ms(&self) -> Option<f32> {
        self.timer.as_ref().and_then(|t| t.last_ms)
    }

    /// Block until the GPU has finished all submitted work and the timestamp is available.
    pub fn wait_idle(&mut self) -> Result<()> {
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .context("waiting for GPU")?;
        if let Some(t) = &mut self.timer {
            t.collect()?;
        }
        Ok(())
    }

    pub fn render(&mut self, req: &mut FrameRequest<'_>) -> Result<()> {
        let device = &self.gpu.device;
        let queue = &self.gpu.queue;
        // Progress outstanding buffer maps without blocking.
        device
            .poll(wgpu::PollType::Poll)
            .context("polling device")?;
        if let Some(t) = &mut self.timer {
            t.collect()?;
        }
        if req.size_px != self.targets.size {
            self.targets = Targets::new(device, &self.post, req.size_px);
        }
        let size = req.size_px;
        let effects = &req.theme.effects;
        let toggles = req.toggles;

        for (id, delta) in &req.textures_delta.set {
            for d in delta {
                self.egui.update_texture(device, queue, *id, d);
            }
        }
        queue.write_buffer(
            &self.post.uni_main,
            0,
            bytemuck::bytes_of(&post_uniforms(size, effects, toggles, [1.0, 0.0])),
        );
        queue.write_buffer(
            &self.post.uni_h,
            0,
            bytemuck::bytes_of(&post_uniforms(size, effects, toggles, [1.0, 0.0])),
        );
        queue.write_buffer(
            &self.post.uni_v,
            0,
            bytemuck::bytes_of(&post_uniforms(size, effects, toggles, [0.0, 1.0])),
        );
        if let Some(r) = req.globe_rect_px {
            self.globe
                .update(queue, req.scene, [r[2] as f32, r[3] as f32]);
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size,
            pixels_per_point: req.pixels_per_point,
        };
        let extra_cmds =
            self.egui
                .update_buffers(device, queue, &mut encoder, req.paint_jobs, &screen);

        // Persistence history is meaningless once effects were off for a while: restart from black.
        if toggles.persistence && !self.prev_persistence {
            for h in &self.targets.hist {
                // A load-clear pass; `clear_texture` would need an optional device feature.
                drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("clear-history"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &h.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                }));
            }
        }
        self.prev_persistence = toggles.persistence;

        let bg = req.theme.palette.background.0;
        let ts_begin = self
            .timer
            .as_ref()
            .map(|t| wgpu::RenderPassTimestampWrites {
                query_set: &t.queries,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: None,
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("globe"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets.scene.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.targets.depth.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: ts_begin,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(r) = req.globe_rect_px {
                self.globe.draw(&mut pass, r);
            }
        }
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.targets.scene.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.egui.render(&mut pass, req.paint_jobs, &screen);
        }

        let fullscreen =
            |encoder: &mut wgpu::CommandEncoder,
             label: &str,
             view: &wgpu::TextureView,
             pipeline: &wgpu::RenderPipeline,
             bg: &wgpu::BindGroup,
             ts_end: Option<wgpu::RenderPassTimestampWrites<'_>>| {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some(label),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        // Every pixel is overwritten by the fullscreen triangle.
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: ts_end,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..3, 0..1);
            };

        let t = &self.targets;
        if toggles.bloom {
            fullscreen(
                &mut encoder,
                "bloom-extract",
                &t.bloom_a.view,
                &self.post.extract,
                &t.bg_extract,
                None,
            );
            fullscreen(
                &mut encoder,
                "bloom-blur-h",
                &t.bloom_b.view,
                &self.post.blur,
                &t.bg_blur_h,
                None,
            );
            fullscreen(
                &mut encoder,
                "bloom-blur-v",
                &t.bloom_a.view,
                &self.post.blur,
                &t.bg_blur_v,
                None,
            );
        }
        let final_bg = if toggles.persistence {
            let cur = self.hist_cur;
            fullscreen(
                &mut encoder,
                "persistence",
                &t.hist[cur].view,
                &self.post.persist,
                &t.bg_persist[cur],
                None,
            );
            self.hist_cur = 1 - cur;
            &t.bg_final_hist[cur]
        } else {
            &t.bg_final_scene
        };
        let ts_end = self
            .timer
            .as_ref()
            .map(|tm| wgpu::RenderPassTimestampWrites {
                query_set: &tm.queries,
                beginning_of_pass_write_index: None,
                end_of_pass_write_index: Some(1),
            });
        fullscreen(
            &mut encoder,
            "final",
            req.target,
            &self.post.final_,
            final_bg,
            ts_end,
        );

        let read_back = self.timer.as_ref().is_some_and(|t| !t.in_flight);
        if read_back {
            let tm = self.timer.as_ref().expect("checked above");
            encoder.resolve_query_set(&tm.queries, 0..2, &tm.resolve, 0);
            encoder.copy_buffer_to_buffer(&tm.resolve, 0, &tm.read, 0, 16);
        }
        queue.submit(extra_cmds.into_iter().chain([encoder.finish()]));
        if read_back {
            let tm = self.timer.as_mut().expect("checked above");
            let flag = tm.mapped.clone();
            tm.read.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                // Errors are reported by `collect` on the render thread, not by panicking here:
                // the callback also fires with an error when the device is torn down at exit.
                flag.store(
                    if r.is_ok() { MAP_READY } else { MAP_FAILED },
                    Ordering::Release,
                );
            });
            tm.in_flight = true;
        }
        for id in &req.textures_delta.free {
            self.egui.free_texture(id);
        }
        req.textures_delta.clear();
        Ok(())
    }
}
