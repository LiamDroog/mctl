//! The orbit-map graphical panel: its own wgpu pass drawn into a grid rectangle.
//!
//! Everything drawn comes from the engine's [`Solution`] and [`Track`]: real Earth orientation, real
//! Sun direction, the real vehicle position and the real past/predicted orbit. With no solution the
//! panel draws nothing and the page says `NO DATA`.

use anyhow::{Context, Result, ensure};
use glam::{Mat3, Mat4, Vec3};
use mctl_core::world::{Solution, Track};
use wgpu::util::DeviceExt;

use crate::coast::MaskLevel;
use crate::theme::{GlobeMode, GlobeStyle};

/// Earth equatorial radius, km (WGS-84): the scene's unit of length.
const EARTH_RADIUS_KM: f32 = 6378.137;

/// Radial exaggeration steps for the orbit; always labelled in the panel title (DESIGN.md §9.6).
pub const EXAGGERATION_STEPS: [f32; 4] = [1.0, 2.0, 5.0, 10.0];

/// Camera: distances in Earth radii, vertical field of view degrees.
const DEFAULT_AZIMUTH_DEG: f32 = 35.0;
const DEFAULT_ELEVATION_DEG: f32 = 24.0;
const DEFAULT_DISTANCE_EARTH_RE: f32 = 3.6;
const DEFAULT_DISTANCE_VEHICLE_RE: f32 = 0.35;
const DISTANCE_LIMITS_EARTH_RE: (f32, f32) = (1.25, 14.0);
const DISTANCE_LIMITS_VEHICLE_RE: (f32, f32) = (0.02, 3.0);
const ELEVATION_LIMIT_DEG: f32 = 89.0;
const CAMERA_FOV_DEG: f32 = 32.0;
const NEAR_EARTH_RE: f32 = 0.05;
const NEAR_VEHICLE_RE: f32 = 0.004;
const FAR_RE: f32 = 60.0;

/// Orbit ribbon: dashes along the predicted part, width and marker size in pixels, alphas.
const ORBIT_DASHES: f32 = 90.0;
const ORBIT_DASH_DUTY: f32 = 0.55;
const ORBIT_WIDTH_PX: f32 = 2.5;
const MARKER_SIZE_PX: f32 = 11.0;
const GRATICULE_ALPHA: f32 = 0.55;
const COAST_ALPHA: f32 = 0.9;
const ORBIT_ALPHA: f32 = 1.0;
/// Capacity of the orbit vertex buffer (points).
pub const MAX_ORBIT_POINTS: usize = 2048;
/// Phase value marking already-flown track points (negative = drawn solid).
const PAST_PHASE: f32 = -1.0;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Interactive camera state (DESIGN.md §9.6: orbit, zoom, inertial or Earth-fixed, focus Earth or vehicle).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    pub dist_earth_re: f32,
    pub dist_vehicle_re: f32,
    /// Camera rides with the Earth instead of staying fixed in the inertial frame.
    pub earth_fixed: bool,
    pub focus_vehicle: bool,
    pub exaggeration_idx: usize,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            azimuth_deg: DEFAULT_AZIMUTH_DEG,
            elevation_deg: DEFAULT_ELEVATION_DEG,
            dist_earth_re: DEFAULT_DISTANCE_EARTH_RE,
            dist_vehicle_re: DEFAULT_DISTANCE_VEHICLE_RE,
            earth_fixed: false,
            focus_vehicle: false,
            exaggeration_idx: 0,
        }
    }
}

impl Camera {
    pub fn exaggeration(&self) -> f32 {
        EXAGGERATION_STEPS[self.exaggeration_idx % EXAGGERATION_STEPS.len()]
    }

    pub fn orbit_by(&mut self, d_az_deg: f32, d_el_deg: f32) {
        self.azimuth_deg = (self.azimuth_deg + d_az_deg).rem_euclid(360.0);
        self.elevation_deg =
            (self.elevation_deg + d_el_deg).clamp(-ELEVATION_LIMIT_DEG, ELEVATION_LIMIT_DEG);
    }

    /// Multiply the distance of the active focus by `factor` (clamped).
    pub fn zoom_by(&mut self, factor: f32) {
        if self.focus_vehicle {
            self.dist_vehicle_re = (self.dist_vehicle_re * factor)
                .clamp(DISTANCE_LIMITS_VEHICLE_RE.0, DISTANCE_LIMITS_VEHICLE_RE.1);
        } else {
            self.dist_earth_re = (self.dist_earth_re * factor)
                .clamp(DISTANCE_LIMITS_EARTH_RE.0, DISTANCE_LIMITS_EARTH_RE.1);
        }
    }

    pub fn cycle_exaggeration(&mut self) {
        self.exaggeration_idx = (self.exaggeration_idx + 1) % EXAGGERATION_STEPS.len();
    }
}

/// Everything the globe pass needs for one frame.
#[derive(Debug, Clone, Copy)]
pub struct GlobeScene {
    pub view_proj: Mat4,
    pub cam_pos: Vec3,
    pub sun_dir: Vec3,
    /// GCRF -> ITRF rotation.
    pub ecef: Mat3,
    /// Vehicle position in Earth radii, radially exaggerated.
    pub vehicle: Vec3,
    pub vehicle_occulted: bool,
}

/// Stretch a position (Earth radii) radially so altitude above the unit sphere is multiplied by `k`.
fn exaggerate(p: Vec3, k: f32) -> Vec3 {
    let r = p.length();
    if r <= 1.0 {
        p
    } else {
        p / r * (1.0 + (r - 1.0) * k)
    }
}

fn to_vec3(km: [f64; 3]) -> Vec3 {
    Vec3::new(km[0] as f32, km[1] as f32, km[2] as f32) / EARTH_RADIUS_KM
}

/// True if the segment from `a` to `b` passes through the unit sphere (the Earth).
fn segment_hits_unit_sphere(a: Vec3, b: Vec3) -> bool {
    let d = b - a;
    let (qa, qb, qc) = (d.dot(d), 2.0 * a.dot(d), a.dot(a) - 1.0);
    let disc = qb * qb - 4.0 * qa * qc;
    if disc < 0.0 || qa == 0.0 {
        return false;
    }
    let t = (-qb - disc.sqrt()) / (2.0 * qa);
    (0.0..1.0).contains(&t)
}

/// Scene for the camera and solution at aspect ratio `aspect`.
pub fn build_scene(camera: &Camera, sol: &Solution, aspect: f32) -> GlobeScene {
    let k = camera.exaggeration();
    let vehicle = exaggerate(to_vec3(sol.state_gcrf.r.into()), k);
    let (az, el) = (
        (camera.azimuth_deg
            + if camera.earth_fixed {
                sol.gmst_rad.to_degrees() as f32
            } else {
                0.0
            })
        .to_radians(),
        camera.elevation_deg.to_radians(),
    );
    let dir = Vec3::new(el.cos() * az.cos(), el.cos() * az.sin(), el.sin());
    let (target, dist, near) = if camera.focus_vehicle {
        (vehicle, camera.dist_vehicle_re, NEAR_VEHICLE_RE)
    } else {
        (Vec3::ZERO, camera.dist_earth_re, NEAR_EARTH_RE)
    };
    let cam_pos = target + dir * dist;
    let view = glam::camera::rh::view::look_at_mat4(cam_pos, target, Vec3::Z);
    let proj = glam::camera::rh::proj::directx::perspective(
        CAMERA_FOV_DEG.to_radians(),
        aspect.max(0.01),
        near,
        FAR_RE,
    );
    let m = sol.ecef_from_gcrf;
    let ecef = Mat3::from_cols(
        Vec3::new(m[0][0] as f32, m[1][0] as f32, m[2][0] as f32),
        Vec3::new(m[0][1] as f32, m[1][1] as f32, m[2][1] as f32),
        Vec3::new(m[0][2] as f32, m[1][2] as f32, m[2][2] as f32),
    );
    let sun = to_vec3(sol.sun_gcrf_km).normalize_or_zero();
    GlobeScene {
        view_proj: proj * view,
        cam_pos,
        sun_dir: sun,
        ecef,
        vehicle,
        vehicle_occulted: segment_hits_unit_sphere(cam_pos, vehicle),
    }
}

/// Orbit ribbon vertices: xyz in Earth radii (exaggerated) and phase in w. Flown points carry a
/// negative phase (solid); predicted points carry 0..1 across the predicted part (dashed).
pub fn orbit_vertices(track: &Track, exaggeration: f32) -> Vec<[f32; 4]> {
    let n = track.points.len();
    let future = (n - track.now_index.min(n)).max(1) as f32;
    track
        .points
        .iter()
        .enumerate()
        .take(MAX_ORBIT_POINTS)
        .map(|(i, p)| {
            let v = exaggerate(to_vec3(p.r_gcrf_km), exaggeration);
            let phase = if i < track.now_index {
                PAST_PHASE
            } else {
                (i - track.now_index) as f32 / future
            };
            [v.x, v.y, v.z, phase]
        })
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    inv_view_proj: [[f32; 4]; 4],
    ecef: [[f32; 4]; 4],
    cam_pos: [f32; 4],
    sun_dir: [f32; 4],
    day_tint: [f32; 4],
    night_tint: [f32; 4],
    graticule: [f32; 4],
    coast: [f32; 4],
    orbit: [f32; 4],
    vehicle: [f32; 4],
    vehicle_pos: [f32; 4],
    viewport: [f32; 4],
    shade: [f32; 4],
    orbit_dash: [f32; 4],
}

fn rgba(c: crate::theme::Rgb, a: f32) -> [f32; 4] {
    [c.0[0], c.0[1], c.0[2], a]
}

/// CPU box-filter mip chain for RGBA8, down to 1x1 on the short side.
fn rgba_mips(mut w: u32, mut h: u32, base: Vec<u8>) -> Vec<(u32, u32, Vec<u8>)> {
    let mut out = vec![(w, h, base)];
    while w > 1 && h > 1 {
        let (nw, nh) = (w / 2, h / 2);
        let prev = &out.last().expect("non-empty").2;
        let mut next = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for ch in 0..4 {
                    let at = |dx: u32, dy: u32| {
                        u32::from(prev[(((y * 2 + dy) * w + x * 2 + dx) * 4 + ch) as usize])
                    };
                    next[((y * nw + x) * 4 + ch) as usize] =
                        ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4) as u8;
                }
            }
        }
        out.push((nw, nh, next));
        w = nw;
        h = nh;
    }
    out
}

fn upload_mips(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    format: wgpu::TextureFormat,
    bpp: u32,
    mips: &[(u32, u32, Vec<u8>)],
) -> wgpu::TextureView {
    let (w, h, _) = mips[0];
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: mips.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, (mw, mh, data)) in mips.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(mw * bpp),
                rows_per_image: Some(*mh),
            },
            wgpu::Extent3d {
                width: *mw,
                height: *mh,
                depth_or_array_layers: 1,
            },
        );
    }
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

pub struct GlobePass {
    sphere: wgpu::RenderPipeline,
    orbit: wgpu::RenderPipeline,
    marker: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    orbit_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    style: GlobeStyle,
    orbit_segments: u32,
    orbit_key: Option<u64>,
    draw_marker: bool,
}

impl GlobePass {
    /// `earth_jpeg` is the colour texture; `mask` is the coastline mip chain.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        earth_jpeg: &[u8],
        mask: &[MaskLevel],
        style: GlobeStyle,
    ) -> Result<Self> {
        let img = image::load_from_memory_with_format(earth_jpeg, image::ImageFormat::Jpeg)
            .context("decoding embedded Earth texture")?
            .to_rgba8();
        let (iw, ih) = img.dimensions();
        ensure!(
            iw >= 2 && ih >= 2,
            "Earth texture is degenerate ({iw}x{ih})"
        );
        let earth_view = upload_mips(
            device,
            queue,
            "earth",
            wgpu::TextureFormat::Rgba8Unorm,
            4,
            &rgba_mips(iw, ih, img.into_raw()),
        );
        let mask_mips: Vec<_> = mask
            .iter()
            .map(|m| (m.width, m.height, m.data.clone()))
            .collect();
        ensure!(!mask_mips.is_empty(), "coastline mask has no levels");
        let mask_view = upload_mips(
            device,
            queue,
            "coast-mask",
            wgpu::TextureFormat::R8Unorm,
            1,
            &mask_mips,
        );

        let orbit_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("orbit-points"),
            contents: bytemuck::cast_slice(&vec![[0.0f32; 4]; MAX_ORBIT_POINTS]),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globe-uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("globe-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let vis = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
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
            label: Some("globe-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: vis,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(2),
                tex_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globe-bind-group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: orbit_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&earth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("globe-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("globe"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/globe.wgsl").into()),
        });
        let make = |label: &str,
                    vs: &str,
                    fs: &str,
                    depth_write: bool,
                    depth_compare: wgpu::CompareFunction| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(depth_compare),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Ok(Self {
            sphere: make(
                "globe-sphere",
                "vs_full",
                "fs_sphere",
                true,
                wgpu::CompareFunction::Less,
            ),
            orbit: make(
                "globe-orbit",
                "vs_orbit",
                "fs_orbit",
                false,
                wgpu::CompareFunction::Less,
            ),
            marker: make(
                "globe-marker",
                "vs_marker",
                "fs_marker",
                false,
                wgpu::CompareFunction::Always,
            ),
            uniforms,
            orbit_buf,
            bind_group,
            style,
            orbit_segments: 0,
            orbit_key: None,
            draw_marker: false,
        })
    }

    pub fn set_style(&mut self, style: GlobeStyle) {
        self.style = style;
    }

    /// Upload orbit vertices if `key` differs from the last upload (track revision and exaggeration).
    pub fn set_orbit(&mut self, queue: &wgpu::Queue, key: u64, points: &[[f32; 4]]) {
        if self.orbit_key == Some(key) {
            return;
        }
        let n = points.len().min(MAX_ORBIT_POINTS);
        queue.write_buffer(&self.orbit_buf, 0, bytemuck::cast_slice(&points[..n]));
        self.orbit_segments = n.saturating_sub(1) as u32;
        self.orbit_key = Some(key);
    }

    /// Draw no orbit (no solution this frame).
    pub fn clear_orbit(&mut self) {
        self.orbit_segments = 0;
        self.orbit_key = None;
    }

    /// Upload this frame's uniforms for a viewport of `viewport_px` pixels.
    pub fn update(&mut self, queue: &wgpu::Queue, scene: &GlobeScene, viewport_px: [f32; 2]) {
        let s = &self.style;
        let e = scene.ecef;
        let ecef4 = Mat4::from_cols(
            e.x_axis.extend(0.0),
            e.y_axis.extend(0.0),
            e.z_axis.extend(0.0),
            glam::Vec4::W,
        );
        let u = Uniforms {
            view_proj: scene.view_proj.to_cols_array_2d(),
            inv_view_proj: scene.view_proj.inverse().to_cols_array_2d(),
            ecef: ecef4.to_cols_array_2d(),
            cam_pos: scene.cam_pos.extend(1.0).to_array(),
            sun_dir: scene.sun_dir.extend(0.0).to_array(),
            day_tint: rgba(s.day_tint, 1.0),
            night_tint: rgba(s.night_tint, 1.0),
            graticule: rgba(s.graticule, GRATICULE_ALPHA),
            coast: rgba(s.coast, COAST_ALPHA),
            orbit: rgba(s.orbit, ORBIT_ALPHA),
            vehicle: rgba(s.vehicle, 1.0),
            vehicle_pos: scene
                .vehicle
                .extend(if scene.vehicle_occulted { 1.0 } else { 0.0 })
                .to_array(),
            viewport: [
                viewport_px[0],
                viewport_px[1],
                0.0,
                if s.mode == GlobeMode::Mono { 1.0 } else { 0.0 },
            ],
            shade: [
                s.ambient,
                s.terminator_softness,
                ORBIT_WIDTH_PX,
                MARKER_SIZE_PX,
            ],
            orbit_dash: [ORBIT_DASHES, ORBIT_DASH_DUTY, 0.0, 0.0],
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
        self.draw_marker = true;
    }

    /// Record the globe into `pass`, restricted to `rect_px` = [x, y, w, h].
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, rect_px: [u32; 4]) {
        let [x, y, w, h] = rect_px;
        pass.set_viewport(x as f32, y as f32, w as f32, h as f32, 0.0, 1.0);
        pass.set_scissor_rect(x, y, w, h);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_pipeline(&self.sphere);
        pass.draw(0..3, 0..1);
        if self.orbit_segments > 0 {
            pass.set_pipeline(&self.orbit);
            pass.draw(0..self.orbit_segments * 6, 0..1);
        }
        if self.draw_marker {
            pass.set_pipeline(&self.marker);
            pass.draw(0..6, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use mctl_core::state::{Elements, Geodetic, Quality, SourceKind, State};
    use mctl_core::time::Utc;
    use mctl_core::world::{Solution, TrackPoint};

    use super::*;

    fn solution(r: [f64; 3]) -> Solution {
        Solution {
            source: SourceKind::Gp,
            quality: Quality::Pred,
            state_gcrf: State::new(r, [0.0, 7.6, 0.0]),
            state_itrf: State::new(r, [0.0, 0.0, 0.0]),
            geodetic: Geodetic {
                lat_deg: 0.0,
                lon_deg: 0.0,
                alt_km: 400.0,
            },
            elements: Elements {
                sma_km: 6778.0,
                ecc: 0.0,
                inc_deg: 51.6,
                raan_deg: 0.0,
                argp_deg: 0.0,
                true_anom_deg: 0.0,
                mean_anom_deg: 0.0,
                period_s: 5560.0,
                apo_alt_km: 400.0,
                peri_alt_km: 400.0,
            },
            sun_gcrf_km: [1.5e8, 0.0, 0.0],
            beta_deg: 0.0,
            illumination: 1.0,
            gmst_rad: 0.0,
            ecef_from_gcrf: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    #[test]
    fn exaggeration_scales_altitude_not_direction() {
        let p = Vec3::new(1.0627, 0.0, 0.0); // 400 km altitude
        let q = exaggerate(p, 5.0);
        assert!((q.length() - (1.0 + 0.0627 * 5.0)).abs() < 1e-5);
        assert!(q.normalize().abs_diff_eq(p.normalize(), 1e-6));
        assert_eq!(
            exaggerate(Vec3::new(0.5, 0.0, 0.0), 5.0),
            Vec3::new(0.5, 0.0, 0.0),
            "inside the Earth is untouched"
        );
    }

    #[test]
    fn vehicle_behind_the_earth_is_occulted_and_in_front_is_not() {
        let cam = Camera::default();
        let sc = build_scene(&cam, &solution([7000.0, 0.0, 0.0]), 1.5);
        let far_side = {
            let eye_dir = sc.cam_pos.normalize();
            let r = (6778.0 / EARTH_RADIUS_KM) as f64;
            [
                -(eye_dir.x as f64) * r * EARTH_RADIUS_KM as f64,
                -(eye_dir.y as f64) * r * EARTH_RADIUS_KM as f64,
                -(eye_dir.z as f64) * r * EARTH_RADIUS_KM as f64,
            ]
        };
        assert!(build_scene(&cam, &solution(far_side), 1.5).vehicle_occulted);
        let near_side = [
            sc.cam_pos.normalize().x as f64 * 6778.0,
            sc.cam_pos.normalize().y as f64 * 6778.0,
            sc.cam_pos.normalize().z as f64 * 6778.0,
        ];
        assert!(!build_scene(&cam, &solution(near_side), 1.5).vehicle_occulted);
    }

    #[test]
    fn earth_fixed_camera_rotates_with_gmst() {
        let mut sol = solution([7000.0, 0.0, 0.0]);
        sol.gmst_rad = 1.0;
        let inertial = build_scene(&Camera::default(), &sol, 1.5).cam_pos;
        let fixed = build_scene(
            &Camera {
                earth_fixed: true,
                ..Camera::default()
            },
            &sol,
            1.5,
        )
        .cam_pos;
        let angle = (inertial.x * fixed.y - inertial.y * fixed.x)
            .atan2(inertial.x * fixed.x + inertial.y * fixed.y);
        assert!(
            (angle.abs() - 1.0).abs() < 1e-4,
            "camera should have rotated by gmst, got {angle}"
        );
    }

    #[test]
    fn orbit_vertices_mark_the_past_solid_and_the_future_dashed() {
        let pts: Vec<TrackPoint> = (0..10)
            .map(|i| TrackPoint {
                t: Utc(i as f64),
                r_gcrf_km: [7000.0, i as f64, 0.0],
                lat_deg: 0.0,
                lon_deg: 0.0,
                alt_km: 400.0,
                illumination: 1.0,
            })
            .collect();
        let track = Track {
            revision: 1,
            source: SourceKind::Gp,
            points: pts,
            now_index: 4,
        };
        let v = orbit_vertices(&track, 1.0);
        assert!(v[..4].iter().all(|p| p[3] < 0.0));
        assert!(v[4][3] == 0.0 && v[9][3] > 0.8 && v[9][3] < 1.0);
        assert!(v.iter().skip(4).all(|p| p[3] >= 0.0));
    }

    #[test]
    fn camera_limits_hold() {
        let mut c = Camera::default();
        for _ in 0..200 {
            c.orbit_by(7.0, 9.0);
            c.zoom_by(0.8);
        }
        assert!(c.elevation_deg <= 89.0 && c.dist_earth_re >= DISTANCE_LIMITS_EARTH_RE.0);
        c.focus_vehicle = true;
        for _ in 0..200 {
            c.zoom_by(1.3);
        }
        assert!(c.dist_vehicle_re <= DISTANCE_LIMITS_VEHICLE_RE.1);
        c.cycle_exaggeration();
        assert_eq!(c.exaggeration(), 2.0);
    }

    #[test]
    fn mips_halve_until_one() {
        let m = rgba_mips(8, 4, vec![255; 8 * 4 * 4]);
        assert_eq!(
            m.iter().map(|(w, h, _)| (*w, *h)).collect::<Vec<_>>(),
            [(8, 4), (4, 2), (2, 1)]
        );
    }
}
