//! HDR display (LR-VIEW-HDR-DISPLAY): paints an HDR edit's loupe render into the window when its
//! surface is HDR.
//!
//! The window asks egui-wgpu for an HDR surface (`SurfaceConfig::prefer_hdr`): `Rgba16Float` in
//! the extended linear sRGB color space (scRGB, where 1.0 is SDR white and larger values are
//! brighter). wgpu sets that up per platform (Vulkan, Metal EDR, DX12), so this crate has no
//! platform code and no `unsafe`. [`HdrDisplay`] implements ui-egui's
//! [`HdrPresenter`]: it uploads the render's HDR values as a half-float texture (only when the
//! render changes) and draws them with an egui-wgpu paint callback over the image's rectangle,
//! easing highlights above the display's [headroom](HdrDisplay::set_headroom) in instead of
//! letting the panel clip them. The UI around it stays at SDR white.
//!
//! See `docs/hdr-display.md`.
//!
//! Desktop only: on wasm32 (the web build draws with glow) the crate is empty.
#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::sync::{Arc, Mutex, PoisonError};

use egui_wgpu::{CallbackResources, CallbackTrait, RenderState, ScreenDescriptor, wgpu};
use lightcraft_ui_egui::hdr_view::{HdrPixels, HdrPresenter};

const SHADER: &str = r"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
struct Params {
    // x: the display's headroom (highest value it shows, SDR white = 1); 0 = unknown, no roll-off
    headroom: vec4<f32>,
    // the image's whole rectangle in framebuffer pixels: left, top, width, height
    rect: vec4<f32>,
};
@group(0) @binding(2) var<uniform> params: Params;

// two triangles over the viewport (egui-wgpu sets it to the callback's rectangle clamped to the
// window, so a zoomed photo's viewport is not its rectangle: texture coordinates come from the
// fragment's position in that rectangle instead)
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    return vec4<f32>(corners[i], 0.0, 1.0);
}

// scRGB is linear sRGB with 1.0 at SDR white: the HDR values as they are, with the brightest
// channel eased in towards the headroom from 80 % of it (hue kept) when the headroom is known
@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = (pos.xy - params.rect.xy) / max(params.rect.zw, vec2<f32>(1e-6));
    let c = max(textureSample(tex, samp, uv).rgb, vec3<f32>(0.0));
    let h = params.headroom.x;
    let m = max(c.r, max(c.g, c.b));
    let knee = 0.8 * h;
    if (h > 1.0 && m > knee) {
        let room = h - knee;
        let mapped = knee + room * (1.0 - exp(-(m - knee) / room));
        return vec4<f32>(c * (mapped / m), 1.0);
    }
    return vec4<f32>(c, 1.0);
}
";

/// The shader's roll-off on the CPU (for tests): the brightest channel of `c` eased in towards
/// `headroom` from 80 % of it; `headroom` ≤ 1 (or not finite) leaves `c` as it is.
pub fn roll_off(c: [f32; 3], headroom: f32) -> [f32; 3] {
    let c = c.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 });
    let m = c[0].max(c[1]).max(c[2]);
    let knee = 0.8 * headroom;
    if !(headroom.is_finite() && headroom > 1.0 && m > knee) {
        return c;
    }
    let room = headroom - knee;
    let mapped = knee + room * (1.0 - (-(m - knee) / room).exp());
    c.map(|v| v * mapped / m)
}

/// The shader's texture coordinate (for tests) at framebuffer pixel `px` (a fragment's position:
/// pixel centres at +0.5) for an image whose whole rectangle is `rect` (left, top, width, height
/// in pixels), however much of it the window clips.
pub fn uv_at(px: [f32; 2], rect: [f32; 4]) -> [f32; 2] {
    [(px[0] - rect[0]) / rect[2].max(1e-6), (px[1] - rect[1]) / rect[3].max(1e-6)]
}

/// The callback's uniform: the headroom, then the image's whole rectangle (`rect`, points) in
/// framebuffer pixels at `pixels_per_point`, as the shader's `Params` (little-endian `f32`s).
fn params_bytes(headroom: f32, rect: egui::Rect, pixels_per_point: f32) -> Vec<u8> {
    let h = if headroom.is_finite() { headroom.max(0.0) } else { 0.0 };
    let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 { pixels_per_point } else { 1.0 };
    let r = [rect.min.x * ppp, rect.min.y * ppp, rect.width() * ppp, rect.height() * ppp];
    [h, 0.0, 0.0, 0.0, r[0], r[1], r[2], r[3]].iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// The pipeline and the current HDR texture, kept in egui-wgpu's callback resources.
struct Resources {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    max_side: u32,
    /// The uploaded render's key, and its bind group.
    current: Option<(u64, wgpu::BindGroup)>,
}

impl Resources {
    fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let module =
            device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("hdr_display"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty, count: None };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hdr_display"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(1, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
                entry(2, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hdr_display"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hdr_display"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: target, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("hdr_display"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hdr_display"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, layout, sampler, params, max_side: device.limits().max_texture_dimension_2d, current: None }
    }

    /// Upload `px` unless it is the current texture. Too large for the device: nothing (the
    /// previous texture, or none, stays; `paint` then draws nothing for it).
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, px: &HdrPixels) {
        if self.current.as_ref().is_some_and(|(k, _)| *k == px.key) {
            return;
        }
        let (Ok(w), Ok(h)) = (u32::try_from(px.width), u32::try_from(px.height)) else { return };
        if w == 0 || h == 0 || w > self.max_side || h > self.max_side {
            return;
        }
        let Some(data) = rgba_f16_bytes(px) else { return };
        let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hdr_display"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            &data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: Some(h) },
            size,
        );
        let view = texture.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hdr_display"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: self.params.as_entire_binding() },
            ],
        });
        self.current = Some((px.key, bind));
    }
}

/// `px` as RGBA half floats (alpha 1), little-endian bytes. `None` if the buffer is short.
fn rgba_f16_bytes(px: &HdrPixels) -> Option<Vec<u8>> {
    let n = px.width.checked_mul(px.height)?;
    let rgb = px.rgb.get(..n.checked_mul(3)?)?;
    let one = f16_bits(1.0).to_le_bytes();
    let mut out = Vec::with_capacity(n.checked_mul(8)?);
    for c in rgb.as_chunks::<3>().0 {
        for v in c {
            out.extend_from_slice(&f16_bits(*v).to_le_bytes());
        }
        out.extend_from_slice(&one);
    }
    Some(out)
}

/// An `f32` as an IEEE half float (round to nearest even; NaN and negatives become 0, values
/// beyond the half range its largest finite value: HDR pixels are finite and non-negative).
fn f16_bits(v: f32) -> u16 {
    if v.is_nan() || v <= 0.0 {
        return 0;
    }
    if v >= 65504.0 {
        return 0x7bff;
    }
    let bits = v.to_bits();
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mant = bits & 0x7f_ffff;
    if exp <= 0 {
        // subnormal half (or zero)
        if exp < -10 {
            return 0;
        }
        let m = (mant | 0x80_0000) >> (1 - exp);
        let half = m >> 13;
        let rest = m & 0x1fff;
        let round = u32::from(rest > 0x1000 || (rest == 0x1000 && half & 1 == 1));
        return (half + round) as u16;
    }
    let half = ((exp as u32) << 10) | (mant >> 13);
    let rest = mant & 0x1fff;
    let round = u32::from(rest > 0x1000 || (rest == 0x1000 && half & 1 == 1));
    (half + round).min(0x7bff) as u16
}

struct Callback {
    pixels: Arc<HdrPixels>,
    headroom: f32,
    /// The image's whole rectangle (points; it may reach past the window).
    rect: egui::Rect,
}

impl CallbackTrait for Callback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(r) = resources.get_mut::<Resources>() {
            r.upload(device, queue, &self.pixels);
            queue.write_buffer(&r.params, 0, &params_bytes(self.headroom, self.rect, screen.pixels_per_point));
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(r) = resources.get::<Resources>() else { return };
        let Some((key, bind)) = &r.current else { return };
        if *key != self.pixels.key {
            return; // (couldn't upload this one)
        }
        pass.set_pipeline(&r.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// The window's HDR presenter (an [`HdrPresenter`] for ui-egui).
pub struct HdrDisplay {
    /// The display's headroom (highest value it can show, SDR white = 1), when known.
    headroom: Mutex<Option<f32>>,
}

impl HdrDisplay {
    /// The presenter for this window, if its surface is HDR (`Rgba16Float`, scRGB): sets up the
    /// pipeline in egui-wgpu's callback resources. `None` for SDR windows (and without wgpu).
    pub fn new(render_state: Option<&RenderState>) -> Option<HdrDisplay> {
        let rs = render_state?;
        if rs.target_format != wgpu::TextureFormat::Rgba16Float {
            return None;
        }
        rs.renderer.write().callback_resources.insert(Resources::new(&rs.device, rs.target_format));
        Some(HdrDisplay { headroom: Mutex::new(None) })
    }

    /// The display's headroom as last set (`None`: unknown, values are drawn as they are and the
    /// compositor or panel decides what happens above it).
    pub fn headroom(&self) -> Option<f32> {
        *self.headroom.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Set the display's headroom (highest value it can show, SDR white = 1; `None` = unknown).
    /// Where the platform reports it (macOS EDR, Windows) the host feeds wgpu's
    /// `DisplayHdrInfo::tone_map_headroom` here, re-queried when the display, the window's
    /// screen or the brightness changes; Linux reports nothing.
    pub fn set_headroom(&self, headroom: Option<f32>) {
        let h = headroom.filter(|h| h.is_finite() && *h >= 1.0);
        *self.headroom.lock().unwrap_or_else(PoisonError::into_inner) = h;
    }
}

impl HdrPresenter for HdrDisplay {
    fn paint(&self, painter: &egui::Painter, rect: egui::Rect, pixels: &Arc<HdrPixels>) {
        let headroom = self.headroom().unwrap_or(0.0);
        painter.add(egui_wgpu::Callback::new_paint_callback(rect, Callback { pixels: pixels.clone(), headroom, rect }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(SHADER).unwrap();
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty()).validate(&module).unwrap();
    }

    #[test]
    fn half_floats_round_trip_and_saturate() {
        let back = |h: u16| {
            let e = i32::from((h >> 10) & 0x1f);
            let m = f32::from(h & 0x3ff);
            if e == 0 { m * 2f32.powi(-24) } else { (1.0 + m / 1024.0) * 2f32.powi(e - 15) }
        };
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(16.0), 0x4c00);
        assert_eq!(f16_bits(0.5), 0x3800);
        for v in [1e-6f32, 0.001, 0.18, 0.7, 1.0, 2.5, 15.9, 1000.0] {
            // (half subnormals, below 6.1e-5, have a fixed step of 2^-24)
            let tol = (v * 1e-3).max(2f32.powi(-25));
            assert!((back(f16_bits(v)) - v).abs() <= tol, "{v} -> {}", back(f16_bits(v)));
        }
        assert_eq!(f16_bits(f32::NAN), 0);
        assert_eq!(f16_bits(-3.0), 0);
        assert_eq!(f16_bits(1e9), 0x7bff);
        assert_eq!(f16_bits(f32::INFINITY), 0x7bff);
        assert_eq!(f16_bits(1e-12), 0);
    }

    #[test]
    fn pixels_become_rgba_half_floats() {
        let px = HdrPixels { key: 1, width: 2, height: 1, rgb: vec![1.0, 2.0, 0.5, 16.0, 0.0, 1.0] };
        let b = rgba_f16_bytes(&px).unwrap();
        assert_eq!(b.len(), 2 * 8);
        let h = |i: usize| u16::from_le_bytes([b[i * 2], b[i * 2 + 1]]);
        assert_eq!([h(0), h(1), h(2), h(3)], [0x3c00, 0x4000, 0x3800, 0x3c00]);
        assert_eq!([h(4), h(7)], [0x4c00, 0x3c00]);
        assert!(rgba_f16_bytes(&HdrPixels { key: 1, width: 4, height: 4, rgb: vec![1.0; 3] }).is_none());
        assert!(rgba_f16_bytes(&HdrPixels { key: 1, width: usize::MAX, height: 2, rgb: vec![] }).is_none());
    }

    #[test]
    fn roll_off_eases_highlights_into_the_headroom_and_keeps_hue() {
        // unknown headroom: as they are
        assert_eq!(roll_off([3.0, 1.5, 0.5], 0.0), [3.0, 1.5, 0.5]);
        // below the knee (80 % of the headroom): untouched
        assert_eq!(roll_off([1.5, 1.0, 0.2], 2.0), [1.5, 1.0, 0.2]);
        // above: never past the headroom, monotone, hue kept
        let mut prev = 0.0;
        for i in 0..400 {
            let v = i as f32 * 0.05;
            let o = roll_off([v, v * 0.5, v * 0.25], 4.0);
            assert!(o[0] <= 4.0 && o[0] >= prev - 1e-5, "{v}: {o:?}");
            if v > 0.0 {
                assert!((o[1] / o[0] - 0.5).abs() < 1e-5 && (o[2] / o[0] - 0.25).abs() < 1e-5);
            }
            prev = o[0];
        }
        assert_eq!(roll_off([f32::NAN, f32::INFINITY, -1.0], 4.0), [0.0, 0.0, 0.0]);
        assert_eq!(roll_off([8.0; 3], f32::NAN), [8.0; 3]);
    }

    #[test]
    fn texture_coordinates_follow_the_whole_image_when_the_window_clips_it() {
        // a 4000 x 3000 photo at 50 % (2000 x 1500 px) in a 1400 x 1000 window, panned so its
        // centre is at the window's centre (pan 0.5) and 10 % further right (pan 0.6): egui-wgpu
        // clamps both viewports to the same 1400 x 1000, but the centre must sample u 0.5 and 0.6
        let centre = [700.0, 500.0];
        let at_half = [700.0 - 1000.0, 500.0 - 750.0, 2000.0, 1500.0];
        let at_six_tenths = [700.0 - 1200.0, 500.0 - 750.0, 2000.0, 1500.0];
        let close = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6;
        assert!(close(uv_at(centre, at_half), [0.5, 0.5]));
        assert!(close(uv_at(centre, at_six_tenths), [0.6, 0.5]));
        // the window's corner is inside the photo, not its corner
        assert!(close(uv_at([0.0, 0.0], at_half), [0.15, 1.0 / 6.0]));
        // a degenerate rectangle stays finite
        assert!(uv_at(centre, [0.0, 0.0, 0.0, 0.0]).iter().all(|v| v.is_finite()));
    }

    #[test]
    fn params_carry_the_headroom_and_the_rectangle_in_pixels() {
        let f = |b: &[u8], i: usize| f32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
        let rect = egui::Rect::from_min_size(egui::pos2(-150.0, -125.0), egui::vec2(1000.0, 750.0));
        let b = params_bytes(3.0, rect, 2.0);
        assert_eq!(b.len(), 32);
        assert_eq!((0..8).map(|i| f(&b, i)).collect::<Vec<_>>(), [3.0, 0.0, 0.0, 0.0, -300.0, -250.0, 2000.0, 1500.0]);
        let b = params_bytes(f32::NAN, rect, f32::NAN);
        assert_eq!([f(&b, 0), f(&b, 6)], [0.0, 1000.0]);
    }

    #[test]
    fn headroom_ignores_values_below_sdr_white_and_damaged_ones() {
        let d = HdrDisplay { headroom: Mutex::new(None) };
        assert_eq!(d.headroom(), None);
        d.set_headroom(Some(3.5));
        assert_eq!(d.headroom(), Some(3.5));
        for bad in [Some(0.5), Some(f32::NAN), Some(f32::INFINITY), None] {
            d.set_headroom(bad);
            assert_eq!(d.headroom(), None, "{bad:?}");
        }
    }
}
