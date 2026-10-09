//! HDR display (LR-VIEW-HDR-DISPLAY): paints an HDR edit's loupe into the window when its
//! surface is scRGB (`Rgba16Float`, extended linear sRGB: 1.0 = SDR white, brighter above; the
//! patched egui-wgpu in `vendor/` creates it when asked and offered). The HDR values are uploaded
//! as a half-float texture (only when the render changes) and drawn by an egui-wgpu paint
//! callback over the image's rectangle; the UI around it stays at SDR white.

use std::sync::Arc;

use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, RenderState, ScreenDescriptor, wgpu};
use lightcraft_ui_egui::hdr_view::{HdrPixels, HdrPresenter};

const SHADER: &str = r"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// two triangles over the viewport (egui-wgpu sets it to the callback's rectangle)
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    let xy = corners[i];
    var out: VOut;
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>((xy.x + 1.0) * 0.5, (1.0 - xy.y) * 0.5);
    return out;
}

// the HDR values as they are: scRGB is linear sRGB with 1.0 at SDR white
@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    return vec4<f32>(c.rgb, 1.0);
}
";

/// The pipeline and the current HDR texture, kept in egui-wgpu's callback resources.
struct HdrResources {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    max_side: u32,
    /// The uploaded render's key and its bind group.
    current: Option<(u64, wgpu::BindGroup)>,
}

impl HdrResources {
    fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Self {
        let module =
            device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("hdr_loupe"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hdr_loupe"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hdr_loupe"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hdr_loupe"),
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
            label: Some("hdr_loupe"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self { pipeline, layout, sampler, max_side: device.limits().max_texture_dimension_2d, current: None }
    }

    /// Upload `px` unless it is the current texture. Too large for the device: nothing (the
    /// previous texture, or none, stays).
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
            label: Some("hdr_loupe"),
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
            label: Some("hdr_loupe"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
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

struct HdrCallback {
    pixels: Arc<HdrPixels>,
}

impl CallbackTrait for HdrCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(r) = resources.get_mut::<HdrResources>() {
            r.upload(device, queue, &self.pixels);
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(r) = resources.get::<HdrResources>() else { return };
        let Some((key, bind)) = &r.current else { return };
        if *key != self.pixels.key {
            return; // (couldn't upload this one)
        }
        pass.set_pipeline(&r.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// The desktop app's [`HdrPresenter`]: egui-wgpu paint callbacks.
pub struct WgpuHdrPresenter;

impl HdrPresenter for WgpuHdrPresenter {
    fn paint(&self, painter: &egui::Painter, rect: egui::Rect, pixels: &Arc<HdrPixels>) {
        painter.add(egui_wgpu::Callback::new_paint_callback(rect, HdrCallback { pixels: pixels.clone() }));
    }
}

/// The presenter for this window, if its surface is HDR (scRGB): sets up the pipeline.
pub fn presenter(render_state: Option<&RenderState>) -> Option<Box<dyn HdrPresenter>> {
    let rs = render_state?;
    if rs.target_format != wgpu::TextureFormat::Rgba16Float {
        return None;
    }
    rs.renderer.write().callback_resources.insert(HdrResources::new(&rs.device, rs.target_format));
    Some(Box::new(WgpuHdrPresenter))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
