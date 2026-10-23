// Whole-frame CRT post-processing. Each effect is a separate entry point so that a disabled
// effect costs nothing (the CPU skips the pass), which is what the frame-time comparison measures.

struct P {
    out_size: vec4<f32>,   // xy: output size px; zw: 1/size
    scan: vec4<f32>,       // x: enabled; y: strength; z: period px
    bloom: vec4<f32>,      // x: enabled; y: threshold; z: intensity; w: radius px (output pixels)
    persist: vec4<f32>,    // x: enabled; y: decay (per frame)
    curve: vec4<f32>,      // x: enabled; y: amount; z: vignette
    blur_dir: vec4<f32>,   // xy: unit step direction in source texels
    bloom_div: vec4<f32>,  // x: output pixels per bloom texel
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var tex_a: texture_2d<f32>;
@group(0) @binding(3) var tex_b: texture_2d<f32>;

struct V {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> V {
    let t = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: V;
    o.pos = vec4<f32>(t * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(t.x, 1.0 - t.y);
    return o;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.299, 0.587, 0.114));
}

// Bright-pass into a quarter-resolution target. Four bilinear taps cover the 4x4 source block.
@fragment
fn fs_extract(in: V) -> @location(0) vec4<f32> {
    let px = p.out_size.zw;
    var sum = vec3<f32>(0.0);
    for (var i = 0; i < 4; i++) {
        let o = vec2<f32>(f32(i & 1) * 2.0 - 1.0, f32(i >> 1) * 2.0 - 1.0) * px;
        sum += textureSample(tex_a, samp, in.uv + o).rgb;
    }
    let c = sum * 0.25;
    let thr = p.bloom.y;
    let k = max(luma(c) - thr, 0.0) / max(1.0 - thr, 1e-3);
    return vec4<f32>(c * k, 1.0);
}

// Separable 9-tap Gaussian. Step size converts the configured radius in output pixels to texels.
@fragment
fn fs_blur(in: V) -> @location(0) vec4<f32> {
    var w = array<f32, 5>(0.2270270, 0.1945946, 0.1216216, 0.0540541, 0.0162162);
    let texel = 1.0 / vec2<f32>(textureDimensions(tex_a));
    let step_texels = max(p.bloom.w / p.bloom_div.x / 4.0, 0.25);
    let d = p.blur_dir.xy * texel * step_texels;
    var acc = textureSample(tex_a, samp, in.uv).rgb * w[0];
    for (var i = 1; i < 5; i++) {
        let o = d * f32(i);
        acc += textureSample(tex_a, samp, in.uv + o).rgb * w[i];
        acc += textureSample(tex_a, samp, in.uv - o).rgb * w[i];
    }
    return vec4<f32>(acc, 1.0);
}

// Phosphor persistence: the screen keeps the brighter of the new frame and the decayed old one.
@fragment
fn fs_persist(in: V) -> @location(0) vec4<f32> {
    let now = textureSample(tex_a, samp, in.uv).rgb;
    // The one-LSB subtraction guarantees ghosts reach zero in 8-bit storage: 0.82 * 1/255 would
    // otherwise round straight back to 1/255 and stay forever.
    let old = max(textureSample(tex_b, samp, in.uv).rgb * p.persist.y - vec3<f32>(1.0 / 255.0), vec3<f32>(0.0));
    return vec4<f32>(max(now, old), 1.0);
}

fn barrel(uv: vec2<f32>, amount: f32) -> vec2<f32> {
    let c = uv * 2.0 - 1.0;
    let r2 = dot(c, c);
    return (c * (1.0 + amount * r2)) * 0.5 + 0.5;
}

@fragment
fn fs_final(in: V) -> @location(0) vec4<f32> {
    var uv = in.uv;
    if p.curve.x > 0.5 {
        uv = barrel(uv, p.curve.y);
    }
    var col = textureSample(tex_a, samp, uv).rgb;
    if p.bloom.x > 0.5 {
        col += textureSample(tex_b, samp, uv).rgb * p.bloom.z;
    }
    if p.scan.x > 0.5 {
        let line = 0.5 + 0.5 * cos(6.2831853 * in.uv.y * p.out_size.y / p.scan.z);
        col *= 1.0 - p.scan.y * (1.0 - line);
    }
    if p.curve.x > 0.5 {
        let c = in.uv * 2.0 - 1.0;
        col *= 1.0 - p.curve.z * smoothstep(0.45, 1.45, length(c));
        // Outside the bent screen is the bezel.
        let inside = step(0.0, uv.x) * step(uv.x, 1.0) * step(0.0, uv.y) * step(uv.y, 1.0);
        col *= inside;
    }
    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
