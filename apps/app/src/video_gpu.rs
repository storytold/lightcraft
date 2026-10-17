//! The window GPU's grade of playing video frames (M14.4): `lcc_gpu::video_lut` on the
//! egui-wgpu device, its output registered with the window's renderer as a native texture, so a
//! frame goes from the decoder to the screen with no CPU grade, conversion or texture copy.
//! Without a wgpu renderer (glow) the loupe keeps the CPU LUT.

use eframe::egui_wgpu::RenderState;
use eframe::wgpu;
use lcc_gpu::video_lut::{LutTarget, VideoLut};

pub struct WindowVideoGpu {
    state: RenderState,
    lut: Option<VideoLut>,
    /// The frame texture and its id in the window's renderer.
    target: Option<(LutTarget, egui::TextureId)>,
}

impl WindowVideoGpu {
    pub fn new(state: &RenderState) -> WindowVideoGpu {
        WindowVideoGpu { state: state.clone(), lut: None, target: None }
    }
}

impl lcc_ui_egui::VideoGpu for WindowVideoGpu {
    fn grade(&mut self, img: &lcc_raster::Rgb32f, key: u64, lut: &lcc_pipeline::grade::GradeLut) -> Result<(egui::TextureId, [usize; 2]), String> {
        let (device, queue) = (&self.state.device, &self.state.queue);
        if self.lut.is_none() {
            self.lut = Some(VideoLut::new(device)?);
        }
        let size = [img.width, img.height];
        if self.target.as_ref().is_none_or(|(t, _)| t.size() != size) {
            let t = LutTarget::new(device, img.width, img.height)?;
            let mut r = self.state.renderer.write();
            let id = match self.target.take() {
                Some((_, id)) => {
                    r.update_egui_texture_from_wgpu_texture(device, t.view(), wgpu::FilterMode::Linear, id);
                    id
                }
                None => r.register_native_texture(device, t.view(), wgpu::FilterMode::Linear),
            };
            self.target = Some((t, id));
        }
        let (Some(g), Some((t, id))) = (self.lut.as_mut(), self.target.as_ref()) else { return Err("no GPU frame texture".into()) };
        g.grade(device, queue, img, key, lut, t)?;
        Ok((*id, size))
    }
}

impl Drop for WindowVideoGpu {
    fn drop(&mut self) {
        if let Some((_, id)) = self.target.take() {
            self.state.renderer.write().free_texture(&id);
        }
    }
}
