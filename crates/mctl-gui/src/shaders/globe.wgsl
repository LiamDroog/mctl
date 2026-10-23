// Globe pass: analytic ray-cast Earth sphere, orbit ribbon and vehicle marker.
//
// World frame: inertial (GCRF), Earth radius = 1. The Earth-fixed frame is reached through the full
// GCRF -> ITRF rotation matrix supplied by the engine. Colours are gamma-space throughout,
// matching egui's output, so no sRGB conversions happen anywhere in this pipeline.

struct U {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    ecef: mat4x4<f32>,       // rotation of inertial vectors into the Earth-fixed frame (upper-left 3x3)
    cam_pos: vec4<f32>,
    sun_dir: vec4<f32>,      // xyz: unit vector toward the Sun, inertial
    day_tint: vec4<f32>,
    night_tint: vec4<f32>,
    graticule: vec4<f32>,
    coast: vec4<f32>,
    orbit: vec4<f32>,
    vehicle: vec4<f32>,
    vehicle_pos: vec4<f32>,  // xyz inertial; w = 1 when occulted by Earth
    viewport: vec4<f32>,     // xy: viewport size px; z: ERA rad; w: 1 = mono mode
    shade: vec4<f32>,        // x: ambient; y: terminator half-width (N.L); z: orbit width px; w: marker size px
    orbit_dash: vec4<f32>,   // x: dashes per revolution; y: duty (0..1)
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> orbit_pts: array<vec4<f32>>; // xyz position, w = orbit phase 0..1
@group(0) @binding(2) var earth_tex: texture_2d<f32>;
@group(0) @binding(3) var coast_tex: texture_2d<f32>;
@group(0) @binding(4) var samp: sampler;

const PI: f32 = 3.14159265358979;
/// Graticule spacing in degrees of latitude and longitude.
const GRATICULE_STEP_DEG: f32 = 15.0;
/// Graticule line half-width in screen pixels.
const GRATICULE_HALF_WIDTH_PX: f32 = 0.7;
/// Thickness of the atmosphere glow in Earth radii.
const ATMOSPHERE_THICKNESS: f32 = 0.06;

struct FsOut {
    @location(0) color: vec4<f32>,   // premultiplied alpha
    @builtin(frag_depth) depth: f32,
}

struct FullOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> FullOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: FullOut;
    o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.ndc = p * 2.0 - 1.0;
    return o;
}

fn rot_z(p: vec3<f32>, a: f32) -> vec3<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec3<f32>(c * p.x - s * p.y, s * p.x + c * p.y, p.z);
}

// Screen-space derivative of a longitude-like coordinate with the 0/1 wrap removed, so the
// antimeridian does not select the smallest mip or blow up the graticule width.
fn wrapped(d: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(d.x - round(d.x), d.y);
}

@fragment
fn fs_sphere(in: FullOut) -> FsOut {
    let n0 = u.inv_view_proj * vec4<f32>(in.ndc, 0.0, 1.0);
    let n1 = u.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let o = u.cam_pos.xyz;
    let dir = normalize(n1.xyz / n1.w - n0.xyz / n0.w);

    let b = dot(o, dir);
    let c = dot(o, o) - 1.0;
    let disc = b * b - c;
    let hit = disc >= 0.0;
    let t = -b - sqrt(max(disc, 0.0));
    let p = o + dir * t;

    // Earth-fixed direction -> lon/lat -> texture coordinates. Evaluated for every pixel so the
    // derivatives below are taken in uniform control flow.
    let e = (u.ecef * vec4<f32>(select(normalize(o + dir * max(-b, 0.0)), p, hit), 0.0)).xyz;
    let lon = atan2(e.y, e.x);
    let lat = asin(clamp(e.z, -1.0, 1.0));
    let uv = vec2<f32>(lon / (2.0 * PI) + 0.5, 0.5 - lat / PI);
    let dx = wrapped(dpdx(uv));
    let dy = wrapped(dpdy(uv));
    let albedo = textureSampleGrad(earth_tex, samp, uv, dx, dy).rgb;
    let coast = textureSampleGrad(coast_tex, samp, uv, dx, dy).r;

    // Graticule: distance to the nearest line, in degrees, divided by the pixel footprint.
    let deg = vec2<f32>(lon, lat) * (180.0 / PI);
    let deg_px = max(abs(dx) * vec2<f32>(360.0, 180.0) + abs(dy) * vec2<f32>(360.0, 180.0), vec2<f32>(1e-4));
    let to_line = abs(fract(deg / GRATICULE_STEP_DEG + 0.5) - 0.5) * GRATICULE_STEP_DEG;
    let line_cov = 1.0 - smoothstep(vec2<f32>(0.0), deg_px * GRATICULE_HALF_WIDTH_PX * 2.0, to_line);
    // Longitude lines converge at the poles; fade them out where they would merge into a blob.
    let grat = max(line_cov.x * (1.0 - smoothstep(0.85, 0.98, abs(sin(lat)))), line_cov.y);

    // Lambert shading with a soft terminator.
    let n = p;
    let ndl = dot(n, u.sun_dir.xyz);
    let soft = u.shade.y;
    let day = smoothstep(-soft, soft, ndl);
    let lam = mix(1.0, clamp(ndl, 0.0, 1.0), 0.5);
    let luma = dot(albedo, vec3<f32>(0.299, 0.587, 0.114));
    let mono = u.viewport.w > 0.5;
    let day_col = select(albedo * u.day_tint.rgb, u.day_tint.rgb * luma, mono) * lam;
    let night_col = u.night_tint.rgb * (0.35 + 0.65 * luma);
    var col = mix(night_col, day_col, day);
    col = max(col, u.night_tint.rgb * u.shade.x);
    col = mix(col, u.coast.rgb, coast * u.coast.a);
    col = mix(col, u.graticule.rgb, grat * u.graticule.a);
    // Limb darkening/glow so the sphere edge reads against the background.
    let fres = pow(1.0 - clamp(dot(n, -dir), 0.0, 1.0), 3.0);
    col = col + u.night_tint.rgb * fres * 0.6;

    // Atmosphere: glow outside the limb based on the ray's closest approach to the centre.
    let r_min = length(o + dir * max(-b, 0.0));
    let glow = (1.0 - smoothstep(1.0, 1.0 + ATMOSPHERE_THICKNESS, r_min)) * select(1.0, 0.0, hit);
    let atm = mix(u.night_tint.rgb, u.day_tint.rgb, 0.5) * glow * 0.8;

    var out: FsOut;
    if hit {
        out.color = vec4<f32>(col, 1.0);
        let clip = u.view_proj * vec4<f32>(p, 1.0);
        out.depth = clip.z / clip.w;
    } else {
        out.color = vec4<f32>(atm, glow * 0.8);
        out.depth = 1.0;
    }
    return out;
}

// ---- Orbit ribbon: one instance per segment, six vertices per segment, expanded in screen space.

struct RibbonOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) phase: f32,
    @location(1) side: f32,
}

@vertex
fn vs_orbit(@builtin(vertex_index) vi: u32) -> RibbonOut {
    let seg = vi / 6u;
    let corner = vi % 6u;
    // Two triangles: (a0,a1,b0) and (b0,a1,b1) with a = segment start, b = end, 0/1 = side.
    var which_end = array<u32, 6>(0u, 0u, 1u, 1u, 0u, 1u);
    var which_side = array<f32, 6>(-1.0, 1.0, -1.0, -1.0, 1.0, 1.0);
    let p0 = orbit_pts[seg];
    let p1 = orbit_pts[seg + 1u];
    let c0 = u.view_proj * vec4<f32>(p0.xyz, 1.0);
    let c1 = u.view_proj * vec4<f32>(p1.xyz, 1.0);
    let half_vp = u.viewport.xy * 0.5;
    let s0 = c0.xy / c0.w * half_vp;
    let s1 = c1.xy / c1.w * half_vp;
    var d = s1 - s0;
    let len = max(length(d), 1e-4);
    d = d / len;
    let nrm = vec2<f32>(-d.y, d.x);
    let use_end = which_end[corner] == 1u;
    let c = select(c0, c1, use_end);
    let side = which_side[corner];
    let off_px = nrm * side * u.shade.z * 0.5;
    var o: RibbonOut;
    o.pos = vec4<f32>(c.xy + off_px / half_vp * c.w, c.z, c.w);
    o.phase = select(p0.w, p1.w, use_end);
    o.side = side;
    return o;
}

@fragment
fn fs_orbit(in: RibbonOut) -> @location(0) vec4<f32> {
    // Dashes are laid out in orbit phase, not arc length: uniform enough for the low-eccentricity
    // orbits this view is for, and it keeps the dash pattern stable as the camera moves.
    // Negative phase marks the already-flown track (solid); the predicted track is dashed.
    let dash = fract(in.phase * u.orbit_dash.x);
    let on = in.phase < 0.0 || dash < u.orbit_dash.y;
    let edge = 1.0 - smoothstep(0.6, 1.0, abs(in.side));
    let a = select(0.0, edge * u.orbit.a, on);
    return vec4<f32>(u.orbit.rgb * a, a);
}

// ---- Vehicle marker: screen-aligned quad; filled diamond when visible, hollow ring when occulted.

struct MarkerOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) q: vec2<f32>,
}

@vertex
fn vs_marker(@builtin(vertex_index) vi: u32) -> MarkerOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0));
    let q = corners[vi];
    let c = u.view_proj * vec4<f32>(u.vehicle_pos.xyz, 1.0);
    let half_vp = u.viewport.xy * 0.5;
    var o: MarkerOut;
    o.pos = vec4<f32>(c.xy + q * u.shade.w / half_vp * c.w, 0.0, c.w);
    o.q = q;
    return o;
}

@fragment
fn fs_marker(in: MarkerOut) -> @location(0) vec4<f32> {
    let occulted = u.vehicle_pos.w > 0.5;
    let diamond = abs(in.q.x) + abs(in.q.y);
    var a: f32;
    if occulted {
        a = smoothstep(0.55, 0.7, diamond) * (1.0 - smoothstep(0.9, 1.0, diamond));
    } else {
        a = 1.0 - smoothstep(0.85, 1.0, diamond);
    }
    return vec4<f32>(u.vehicle.rgb * a, a);
}
