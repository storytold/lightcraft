// Playback grade of a video frame (M14.4): linear RGB in, the clip's baked 33^3 grade LUT
// (lcc_pipeline::grade::GradeLut) looked up with tetrahedral interpolation, display-encoded
// sRGB 8-bit out. The GPU twin of GradeLut::lookup. Sources: own design; tetrahedral
// interpolation after Kasson, Nin, Plouffe & Hafner (J. Electronic Imaging, 1995).

struct Params { w: u32, h: u32, n: u32, pad: u32 }

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read> lut: array<vec4<f32>>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba8unorm, write>;

fn encode(v: f32) -> f32 {
    if v <= 0.0031308 { return 12.92 * v; }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn at(i: vec3<u32>) -> vec3<f32> {
    return lut[i.x + p.n * (i.y + p.n * i.z)].xyz;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= p.w || gid.y >= p.h { return; }
    let o = (gid.y * p.w + gid.x) * 3u;
    var c = vec3<f32>(src[o], src[o + 1u], src[o + 2u]);
    // NaN → 0 (as the CPU lookup); then the lattice coordinate
    c = select(vec3<f32>(0.0), c, c == c);
    c = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let top = f32(p.n - 1u);
    let x = vec3<f32>(encode(c.x), encode(c.y), encode(c.z)) * top;
    let lo = min(vec3<u32>(floor(x)), vec3<u32>(p.n - 2u));
    let f = x - vec3<f32>(lo);
    let fr = f.x; let fg = f.y; let fb = f.z;
    var a: vec3<u32>; var b: vec3<u32>; var w: vec4<f32>;
    if fr >= fg {
        if fg >= fb { a = vec3(1u,0u,0u); b = vec3(1u,1u,0u); w = vec4(1.0-fr, fr-fg, fg-fb, fb); }
        else if fr >= fb { a = vec3(1u,0u,0u); b = vec3(1u,0u,1u); w = vec4(1.0-fr, fr-fb, fb-fg, fg); }
        else { a = vec3(0u,0u,1u); b = vec3(1u,0u,1u); w = vec4(1.0-fb, fb-fr, fr-fg, fg); }
    } else if fb >= fg { a = vec3(0u,0u,1u); b = vec3(0u,1u,1u); w = vec4(1.0-fb, fb-fg, fg-fr, fr); }
    else if fb >= fr { a = vec3(0u,1u,0u); b = vec3(0u,1u,1u); w = vec4(1.0-fg, fg-fb, fb-fr, fr); }
    else { a = vec3(0u,1u,0u); b = vec3(1u,1u,0u); w = vec4(1.0-fg, fg-fr, fr-fb, fb); }
    let out = w.x * at(lo) + w.y * at(lo + a) + w.z * at(lo + b) + w.w * at(lo + vec3(1u));
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(clamp(out, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0));
}
