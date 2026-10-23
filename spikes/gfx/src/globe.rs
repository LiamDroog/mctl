//! The Earth/orbit graphical panel: its own wgpu pass drawn into a grid rectangle.
//!
//! The scene is a demo, not telemetry: a fixed Sun, an ISS-like orbit and a time-warped clock.

use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::coast::MaskLevel;
use crate::theme::{GlobeMode, GlobeStyle};

/// Earth equatorial radius, km (WGS-84). Orbit units are Earth radii.
const EARTH_RADIUS_KM: f64 = 6378.137;
/// Earth's gravitational parameter, km³/s² (EGM-96, as used by SGP4).
const EARTH_MU_KM3_S2: f64 = 398_600.441_8;
/// Earth rotation rate, rad/s (IERS; sidereal).
const EARTH_ROTATION_RAD_S: f64 = 7.292_115_0e-5;

/// Demo orbit, ISS-like: semi-major axis km, eccentricity, inclination/RAAN/arg-perigee degrees.
const DEMO_SMA_KM: f64 = 6_791.0;
const DEMO_ECC: f64 = 0.0005;
const DEMO_INC_DEG: f64 = 51.64;
const DEMO_RAAN_DEG: f64 = 40.0;
const DEMO_ARGP_DEG: f64 = 0.0;
/// Simulated seconds per wall second for the demo (one ISS revolution ≈ 9 s).
pub const DEMO_TIME_WARP: f64 = 600.0;
/// Radial exaggeration applied to the orbit so it separates from the surface; labelled in the panel header.
pub const ALTITUDE_EXAGGERATION: f64 = 3.0;
/// Demo Sun: right ascension and declination of the direction toward the Sun, degrees.
const DEMO_SUN_RA_DEG: f32 = 115.0;
const DEMO_SUN_DEC_DEG: f32 = -12.0;

/// Camera: distance in Earth radii, azimuth/elevation degrees, vertical field of view degrees.
const CAMERA_DISTANCE_RE: f32 = 4.6;
const CAMERA_AZIMUTH_DEG: f32 = 35.0;
const CAMERA_ELEVATION_DEG: f32 = 24.0;
const CAMERA_FOV_DEG: f32 = 32.0;
const CAMERA_NEAR_RE: f32 = 0.1;
const CAMERA_FAR_RE: f32 = 30.0;

const ORBIT_SEGMENTS: usize = 256;
const ORBIT_DASHES_PER_REV: f32 = 72.0;
const ORBIT_DASH_DUTY: f32 = 0.55;
const ORBIT_WIDTH_PX: f32 = 2.5;
const MARKER_SIZE_PX: f32 = 10.0;
const GRATICULE_ALPHA: f32 = 0.55;
const COAST_ALPHA: f32 = 0.9;
const ORBIT_ALPHA: f32 = 1.0;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Per-frame state of the demo scene.
#[derive(Debug, Clone, Copy)]
pub struct GlobeScene {
    pub era_rad: f32,
    pub sun_dir: Vec3,
    pub vehicle_pos: Vec3,
    pub view_proj: Mat4,
    pub cam_pos: Vec3,
    pub vehicle_occulted: bool,
}

/// Solve Kepler's equation for the eccentric anomaly (Newton, converges in a few steps for e ≪ 1).
fn eccentric_anomaly(mean: f64, ecc: f64) -> f64 {
    let mut e = mean;
    for _ in 0..8 {
        e -= (e - ecc * e.sin() - mean) / (1.0 - ecc * e.cos());
    }
    e
}

/// Position in the inertial frame (Earth radii, altitude exaggerated) at true-anomaly-from-mean-anomaly `mean`.
fn orbit_position(mean: f64) -> Vec3 {
    let e = eccentric_anomaly(mean, DEMO_ECC);
    let a = DEMO_SMA_KM / EARTH_RADIUS_KM;
    let (x_pf, y_pf) = (
        a * (e.cos() - DEMO_ECC),
        a * (1.0 - DEMO_ECC * DEMO_ECC).sqrt() * e.sin(),
    );
    let r_pf = (x_pf * x_pf + y_pf * y_pf).sqrt();
    let nu = y_pf.atan2(x_pf);
    let (raan, inc, argp) = (
        DEMO_RAAN_DEG.to_radians(),
        DEMO_INC_DEG.to_radians(),
        DEMO_ARGP_DEG.to_radians(),
    );
    let u = argp + nu;
    let r = 1.0 + (r_pf - 1.0) * ALTITUDE_EXAGGERATION;
    let (su, cu) = u.sin_cos();
    let (sr, cr) = raan.sin_cos();
    let (si, ci) = inc.sin_cos();
    Vec3::new(
        (r * (cr * cu - sr * su * ci)) as f32,
        (r * (sr * cu + cr * su * ci)) as f32,
        (r * (su * si)) as f32,
    )
}

/// Orbit polyline: xyz plus phase (0..=1, mean anomaly / 2π) in w, closed (last == first).
pub fn orbit_points() -> Vec<[f32; 4]> {
    (0..=ORBIT_SEGMENTS)
        .map(|i| {
            let phase = i as f64 / ORBIT_SEGMENTS as f64;
            let p = orbit_position(phase * std::f64::consts::TAU);
            [p.x, p.y, p.z, phase as f32]
        })
        .collect()
}

/// Demo scene at wall-clock time `t_s` seconds for a viewport of the given aspect ratio.
pub fn scene_at(t_s: f64, aspect: f32) -> GlobeScene {
    let sim_s = t_s * DEMO_TIME_WARP;
    let period_s = std::f64::consts::TAU * (DEMO_SMA_KM.powi(3) / EARTH_MU_KM3_S2).sqrt();
    let vehicle_pos = orbit_position(sim_s / period_s * std::f64::consts::TAU);
    let (ra, dec) = (DEMO_SUN_RA_DEG.to_radians(), DEMO_SUN_DEC_DEG.to_radians());
    let sun_dir = Vec3::new(dec.cos() * ra.cos(), dec.cos() * ra.sin(), dec.sin());
    let (az, el) = (
        CAMERA_AZIMUTH_DEG.to_radians(),
        CAMERA_ELEVATION_DEG.to_radians(),
    );
    let cam_pos =
        Vec3::new(el.cos() * az.cos(), el.cos() * az.sin(), el.sin()) * CAMERA_DISTANCE_RE;
    let view = glam::camera::rh::view::look_at_mat4(cam_pos, Vec3::ZERO, Vec3::Z);
    let proj = glam::camera::rh::proj::directx::perspective(
        CAMERA_FOV_DEG.to_radians(),
        aspect.max(0.01),
        CAMERA_NEAR_RE,
        CAMERA_FAR_RE,
    );
    GlobeScene {
        era_rad: ((sim_s * EARTH_ROTATION_RAD_S) % std::f64::consts::TAU) as f32,
        sun_dir,
        vehicle_pos,
        view_proj: proj * view,
        cam_pos,
        vehicle_occulted: segment_hits_unit_sphere(cam_pos, vehicle_pos),
    }
}

/// True if the segment from `a` to `b` passes through the unit sphere (the Earth).
fn segment_hits_unit_sphere(a: Vec3, b: Vec3) -> bool {
    let d = b - a;
    let (qa, qb, qc) = (d.dot(d), 2.0 * a.dot(d), a.dot(a) - 1.0);
    let disc = qb * qb - 4.0 * qa * qc;
    if disc < 0.0 {
        return false;
    }
    let t = (-qb - disc.sqrt()) / (2.0 * qa);
    (0.0..1.0).contains(&t)
}

impl GlobeScene {
    /// Project a world point to viewport-local pixels (origin top-left), or `None` if behind the camera.
    pub fn project(&self, world: Vec3, viewport_px: [f32; 2]) -> Option<[f32; 2]> {
        let c = self.view_proj * world.extend(1.0);
        if c.w <= 0.0 {
            return None;
        }
        let ndc = (c.x / c.w, c.y / c.w);
        Some([
            (ndc.0 * 0.5 + 0.5) * viewport_px[0],
            (0.5 - ndc.1 * 0.5) * viewport_px[1],
        ])
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    inv_view_proj: [[f32; 4]; 4],
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

/// CPU box-filter mip chain for RGBA8, down to 1×1 on the short side.
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
    bind_group: wgpu::BindGroup,
    style: GlobeStyle,
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

        let orbit = orbit_points();
        let orbit_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("orbit-points"),
            contents: bytemuck::cast_slice(&orbit),
            usage: wgpu::BufferUsages::STORAGE,
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
            bind_group,
            style,
        })
    }

    pub fn set_style(&mut self, style: GlobeStyle) {
        self.style = style;
    }

    /// Upload this frame's uniforms for a viewport of `viewport_px` pixels.
    pub fn update(&self, queue: &wgpu::Queue, scene: &GlobeScene, viewport_px: [f32; 2]) {
        let s = &self.style;
        let u = Uniforms {
            view_proj: scene.view_proj.to_cols_array_2d(),
            inv_view_proj: scene.view_proj.inverse().to_cols_array_2d(),
            cam_pos: scene.cam_pos.extend(1.0).to_array(),
            sun_dir: scene.sun_dir.extend(0.0).to_array(),
            day_tint: rgba(s.day_tint, 1.0),
            night_tint: rgba(s.night_tint, 1.0),
            graticule: rgba(s.graticule, GRATICULE_ALPHA),
            coast: rgba(s.coast, COAST_ALPHA),
            orbit: rgba(s.orbit, ORBIT_ALPHA),
            vehicle: rgba(s.vehicle, 1.0),
            vehicle_pos: scene
                .vehicle_pos
                .extend(if scene.vehicle_occulted { 1.0 } else { 0.0 })
                .to_array(),
            viewport: [
                viewport_px[0],
                viewport_px[1],
                scene.era_rad,
                if s.mode == GlobeMode::Mono { 1.0 } else { 0.0 },
            ],
            shade: [
                s.ambient,
                s.terminator_softness,
                ORBIT_WIDTH_PX,
                MARKER_SIZE_PX,
            ],
            orbit_dash: [ORBIT_DASHES_PER_REV, ORBIT_DASH_DUTY, 0.0, 0.0],
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
    }

    /// Record the globe into `pass`, restricted to `rect_px` = [x, y, w, h].
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, rect_px: [u32; 4]) {
        let [x, y, w, h] = rect_px;
        pass.set_viewport(x as f32, y as f32, w as f32, h as f32, 0.0, 1.0);
        pass.set_scissor_rect(x, y, w, h);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_pipeline(&self.sphere);
        pass.draw(0..3, 0..1);
        pass.set_pipeline(&self.orbit);
        pass.draw(0..(ORBIT_SEGMENTS * 6) as u32, 0..1);
        pass.set_pipeline(&self.marker);
        pass.draw(0..6, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orbit_is_closed_and_above_the_surface() {
        let pts = orbit_points();
        assert_eq!(pts.len(), ORBIT_SEGMENTS + 1);
        let (a, b) = (
            Vec3::from_slice(&pts[0][..3]),
            Vec3::from_slice(&pts[ORBIT_SEGMENTS][..3]),
        );
        assert!((a - b).length() < 1e-4, "orbit must close");
        assert!(pts.iter().all(|p| Vec3::from_slice(&p[..3]).length() > 1.0));
    }

    #[test]
    fn orbit_inclination_bounds_z() {
        let max_z = orbit_points().iter().map(|p| p[2]).fold(0.0f32, f32::max);
        let r = orbit_points()
            .iter()
            .map(|p| Vec3::from_slice(&p[..3]).length())
            .fold(0.0f32, f32::max);
        assert!((max_z / r - DEMO_INC_DEG.to_radians().sin() as f32).abs() < 0.01);
    }

    #[test]
    fn occlusion_test_matches_geometry() {
        let cam = Vec3::new(3.0, 0.0, 0.0);
        assert!(
            segment_hits_unit_sphere(cam, Vec3::new(-1.5, 0.0, 0.0)),
            "behind Earth"
        );
        assert!(
            !segment_hits_unit_sphere(cam, Vec3::new(1.5, 0.0, 0.0)),
            "in front"
        );
        assert!(
            !segment_hits_unit_sphere(cam, Vec3::new(0.0, 3.0, 0.0)),
            "off to the side"
        );
    }

    #[test]
    fn kepler_solver_satisfies_equation() {
        for mean in [0.1, 1.0, 3.0, 5.5] {
            let e = eccentric_anomaly(mean, 0.3);
            assert!((e - 0.3 * e.sin() - mean).abs() < 1e-10);
        }
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
