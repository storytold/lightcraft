//! The playback grade of a video frame on the GPU (M14.4): the twin of
//! [`GradeLut::apply`](lcc_pipeline::grade::GradeLut::apply). A decoded linear frame is uploaded
//! as-is (`f32` RGB, no CPU conversion), looked up in the clip's baked [`LUT_SIZE`]³ grade with
//! tetrahedral interpolation by one small compute shader, and written straight into an RGBA8
//! texture the window can draw ([`LutTarget::view`], an `Rgba8UnormSrgb` view, the format
//! egui's own textures use). Takes a borrowed device and queue so the window's renderer
//! (egui-wgpu) can draw the result without a copy or a readback.
//!
//! Errors (validation, out of memory, a lost device) come back as `Err`: the caller falls back to
//! the CPU LUT. Sources: own design; tetrahedral interpolation after Kasson, Nin, Plouffe &
//! Hafner, "Performing color space conversions with three-dimensional linear interpolation"
//! (J. Electronic Imaging, 1995).

use lcc_pipeline::grade::{GradeLut, LUT_SIZE};
use lcc_raster::{Rgb32f, Rgba8};

const SHADER: &str = include_str!("wgsl/video_lut.wgsl");
/// Largest frame side graded here (the window's texture limit is checked by wgpu as well).
const MAX_SIDE: usize = 8192;

/// The RGBA8 texture a graded frame lands in.
pub struct LutTarget {
    texture: wgpu::Texture,
    store: wgpu::TextureView,
    view: wgpu::TextureView,
    size: [usize; 2],
}

impl LutTarget {
    /// A `w × h` target (`Rgba8Unorm` storage, drawn through an `Rgba8UnormSrgb` view).
    pub fn new(device: &wgpu::Device, w: usize, h: usize) -> Result<LutTarget, String> {
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Err(format!("video frame of {w}×{h} can't be graded on the GPU"));
        }
        let scopes = Scopes::push(device);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video-lut-frame"),
            size: wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        let store = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            ..Default::default()
        });
        scopes.pop()?;
        Ok(LutTarget { texture, store, view, size: [w, h] })
    }

    /// The view to draw (sRGB-decoding, as egui expects of its textures).
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// `[width, height]`.
    pub fn size(&self) -> [usize; 2] {
        self.size
    }

    /// Read the frame back (tests and benchmarks; playback never reads back).
    pub fn read(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Rgba8, String> {
        let [w, h] = self.size;
        let row = (w * 4).div_ceil(256) * 256;
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video-lut-readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("video-lut-readback") });
        enc.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row as u32), rows_per_image: Some(h as u32) },
            },
            wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) })
            .map_err(|e| format!("GPU readback: {e}"))?;
        rx.recv_timeout(std::time::Duration::from_secs(10)).map_err(|e| format!("GPU readback: {e}"))?.map_err(|e| format!("GPU readback: {e}"))?;
        let data = slice.get_mapped_range().map_err(|e| format!("GPU readback: {e:?}"))?;
        let mut px = Vec::with_capacity(w * h);
        for y in 0..h {
            let line = data.get(y * row..y * row + w * 4).ok_or("GPU readback: short buffer")?;
            px.extend(line.as_chunks::<4>().0.iter().copied());
        }
        drop(data);
        staging.unmap();
        Ok(Rgba8 { width: w, height: h, data: px })
    }
}

/// The compute pipeline and its buffers (one per window; reused frame to frame).
pub struct VideoLut {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    lut: wgpu::Buffer,
    /// The key of the LUT in [`Self::lut`].
    lut_key: Option<u64>,
    /// The frame upload buffer and its size in bytes.
    src: Option<(wgpu::Buffer, u64)>,
}

impl VideoLut {
    /// Build the shader on `device`.
    pub fn new(device: &wgpu::Device) -> Result<VideoLut, String> {
        let scopes = Scopes::push(device);
        let module =
            device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("video_lut"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let buf = |binding, ty| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty, count: None };
        let storage = |binding| {
            buf(
                binding,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            )
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video_lut"),
            entries: &[
                buf(0, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }),
                storage(1),
                storage(2),
                buf(
                    3,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video_lut"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("video_lut"),
            layout: Some(&pl),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video_lut-params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let lut = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video_lut-table"),
            size: (LUT_SIZE * LUT_SIZE * LUT_SIZE * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        scopes.pop()?;
        Ok(VideoLut { pipeline, layout, params, lut, lut_key: None, src: None })
    }

    /// Grade `img` through `lut` (identified by `lut_key`: re-uploaded only when it changes)
    /// into `target`, which must be the frame's size. Submits; doesn't wait.
    pub fn grade(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        img: &Rgb32f,
        lut_key: u64,
        lut: &GradeLut,
        target: &LutTarget,
    ) -> Result<(), String> {
        let [w, h] = target.size;
        if [img.width, img.height] != target.size || img.data.len() != w.saturating_mul(h) {
            return Err(format!("video frame of {}×{} for a {w}×{h} target", img.width, img.height));
        }
        let scopes = Scopes::push(device);
        if self.lut_key != Some(lut_key) {
            let table: Vec<[f32; 4]> = lut.table().iter().map(|c| [c[0], c[1], c[2], 1.0]).collect();
            if table.len() != LUT_SIZE * LUT_SIZE * LUT_SIZE {
                return Err("grade LUT of the wrong size".into());
            }
            queue.write_buffer(&self.lut, 0, bytemuck::cast_slice(&table));
            self.lut_key = Some(lut_key);
        }
        let bytes: &[u8] = bytemuck::cast_slice(&img.data);
        let need = (bytes.len() as u64).max(16);
        if self.src.as_ref().is_none_or(|(_, len)| *len < need) {
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("video_lut-frame"),
                size: need,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.src = Some((b, need));
        }
        let Some((src, _)) = self.src.as_ref() else { return Err("no frame buffer".into()) };
        queue.write_buffer(src, 0, bytes);
        let p: [u32; 4] = [w as u32, h as u32, LUT_SIZE as u32, 0];
        queue.write_buffer(&self.params, 0, bytemuck::cast_slice(&p));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video_lut"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.lut.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&target.store) },
            ],
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("video_lut") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("video_lut"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(w.div_ceil(16) as u32, h.div_ceil(16) as u32, 1);
        }
        queue.submit([enc.finish()]);
        let r = scopes.pop();
        if r.is_err() {
            // the upload may not have happened: upload the table again next time
            self.lut_key = None;
        }
        r
    }
}

/// Validation / out-of-memory / internal error scopes around a few calls.
struct Scopes([wgpu::ErrorScopeGuard; 3]);

impl Scopes {
    fn push(d: &wgpu::Device) -> Scopes {
        Scopes([
            d.push_error_scope(wgpu::ErrorFilter::OutOfMemory),
            d.push_error_scope(wgpu::ErrorFilter::Internal),
            d.push_error_scope(wgpu::ErrorFilter::Validation),
        ])
    }

    fn pop(self) -> Result<(), String> {
        let [oom, internal, validation] = self.0;
        let errors = [pollster::block_on(validation.pop()), pollster::block_on(internal.pop()), pollster::block_on(oom.pop())];
        match errors.into_iter().flatten().next() {
            Some(e) => Err(format!("GPU video grade: {e}")),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_pipeline::Quality;

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        // the sRGB view of the frame needs view formats, which downlevel (GL) adapters lack;
        // the app grades on the CPU there
        if !adapter.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::VIEW_FORMATS) {
            return None;
        }
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    #[test]
    fn gpu_lut_matches_cpu_lut() {
        let Some((device, queue)) = device() else {
            eprintln!("no GPU adapter with view formats: skipped");
            return;
        };
        let mut s = lcc_develop::DevelopSettings::default();
        s.light.exposure = 0.4;
        s.light.contrast = 25.0;
        s.light.highlights = -30.0;
        s.light.shadows = 20.0;
        s.wb.temp = 7000.0;
        s.color.vibrance = 20.0;
        s.grading.shadows.hue = 220.0;
        s.grading.shadows.sat = 30.0;
        let lut = GradeLut::bake(&s).unwrap();
        let mut img = lcc_scenes::demo_library()[0].render(333, 187);
        // out-of-range, NaN and the lattice's edges too
        img.data[0] = [f32::NAN, 2.0, -1.0];
        img.data[1] = [1.0, 1.0, 1.0];
        img.data[2] = [0.0, 0.0, 0.0];
        let cpu = lut.apply(&img, Quality::Full);
        let mut g = VideoLut::new(&device).unwrap();
        let target = LutTarget::new(&device, img.width, img.height).unwrap();
        g.grade(&device, &queue, &img, 1, &lut, &target).unwrap();
        let gpu = target.read(&device, &queue).unwrap();
        assert_eq!((gpu.width, gpu.height), (cpu.width, cpu.height));
        let worst = cpu.data.iter().zip(&gpu.data).flat_map(|(a, b)| (0..4).map(move |k| a[k].abs_diff(b[k]))).max().unwrap();
        assert!(worst <= 2, "GPU LUT differs from the CPU LUT by {worst} codes");
        // a new key uploads the new table
        let lut2 = GradeLut::bake(&lcc_develop::DevelopSettings::default()).unwrap();
        g.grade(&device, &queue, &img, 2, &lut2, &target).unwrap();
        let gpu2 = target.read(&device, &queue).unwrap();
        let cpu2 = lut2.apply(&img, Quality::Full);
        let worst2 = cpu2.data.iter().zip(&gpu2.data).flat_map(|(a, b)| (0..4).map(move |k| a[k].abs_diff(b[k]))).max().unwrap();
        assert!(worst2 <= 2, "after a LUT change: {worst2} codes");
        // a frame of the wrong size is an error, not a panic
        assert!(g.grade(&device, &queue, &lcc_scenes::demo_library()[0].render(10, 10), 2, &lut2, &target).is_err());
        assert!(LutTarget::new(&device, 0, 10).is_err());
    }
}
