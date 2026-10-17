//! DSRNet's VGG19 pyramid and dual-stream MuGI blocks. The training-only LRM residue
//! head is deliberately omitted: it does not feed the transmission/reflection outputs.
use crate::{MAX_EDGE, Manifest};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use std::collections::HashMap;
type R<T> = candle_core::Result<T>;
pub struct Net {
    vb: VarBuilder<'static>,
    device: Device,
    width: usize,
    shared: bool,
    enc: Vec<usize>,
    middle: usize,
}
impl Net {
    pub fn load(dir: &std::path::Path, cpu: bool) -> Result<Self, String> {
        let (manifest, bytes) = crate::verified_weights(dir)?;
        let device = Self::device(cpu);
        Self::from_bytes(manifest, bytes, device).map_err(|e| e.to_string())
    }
    fn device(cpu: bool) -> Device {
        #[cfg(target_os = "macos")]
        if !cpu && let Ok(d) = Device::new_metal(0) {
            return d;
        }
        let _ = cpu;
        Device::Cpu
    }
    fn from_bytes(m: Manifest, bytes: Vec<u8>, device: Device) -> R<Self> {
        let small = m.architecture == "dsrnet-s";
        let tensors = candle_core::safetensors::load_buffer(&bytes, &device)?;
        let vb = VarBuilder::from_tensors(tensors, DType::F32, &device);
        Ok(Self {
            vb,
            device,
            width: if small { 32 } else { 64 },
            shared: !small,
            enc: if small { vec![2, 2, 2] } else { vec![2, 2, 4, 8] },
            middle: if small { 4 } else { 12 },
        })
    }
    fn conv(&self, x: &Tensor, p: &str, cin: usize, cout: usize, k: usize, stride: usize, pad: usize, bias: bool) -> R<Tensor> {
        let vb = self.vb.pp(p);
        let w = vb.get((cout, cin, k, k), "weight")?;
        let mut y = conv2d(x, &w, pad, stride)?;
        if bias {
            y = y.broadcast_add(&vb.get(cout, "bias")?.reshape((1, cout, 1, 1))?)?;
        }
        Ok(y)
    }
    fn norm(&self, x: &Tensor, p: &str, c: usize) -> R<Tensor> {
        let mean = x.mean_keepdim(1)?;
        let centered = x.broadcast_sub(&mean)?;
        let std = (centered.sqr()?.mean_keepdim(1)? + 1e-6)?.sqrt()?;
        let vb = self.vb.pp(p);
        centered
            .broadcast_div(&std)?
            .broadcast_mul(&vb.get(c, "weight")?.reshape((1, c, 1, 1))?)?
            .broadcast_add(&vb.get(c, "bias")?.reshape((1, c, 1, 1))?)
    }
    /// Nine shifted elementwise products avoid candle's per-channel grouped-convolution dispatch.
    fn depthwise(&self, x: &Tensor, p: &str, c: usize) -> R<Tensor> {
        let (_, _, h, w) = x.dims4()?;
        let vb = self.vb.pp(p);
        let kernel = vb.get((c, 1, 3, 3), "weight")?;
        let padded = x.pad_with_zeros(2, 1, 1)?.pad_with_zeros(3, 1, 1)?;
        let mut y = Tensor::zeros(x.shape(), DType::F32, &self.device)?;
        for dy in 0..3 {
            for dx in 0..3 {
                let weight = kernel.narrow(2, dy, 1)?.narrow(3, dx, 1)?.reshape((1, c, 1, 1))?;
                y = (y + padded.narrow(2, dy, h)?.narrow(3, dx, w)?.broadcast_mul(&weight)?)?;
            }
        }
        y.broadcast_add(&vb.get(c, "bias")?.reshape((1, c, 1, 1))?)
    }
    fn gate(&self, x: &Tensor, y: &Tensor, c: usize) -> R<(Tensor, Tensor)> {
        Ok((x.narrow(1, 0, c)?.mul(&y.narrow(1, c, c)?)?, y.narrow(1, 0, c)?.mul(&x.narrow(1, c, c)?)?))
    }
    fn first(&self, x: &Tensor, p: &str, c: usize) -> R<Tensor> {
        let x = self.norm(x, &format!("{p}.block1.0.seq.0"), c)?;
        let x = self.conv(&x, &format!("{p}.block1.0.seq.1"), c, c * 2, 1, 1, 0, true)?;
        self.depthwise(&x, &format!("{p}.block1.0.seq.2"), c * 2)
    }
    fn ca(&self, x: &Tensor, p: &str, c: usize) -> R<Tensor> {
        let avg = x.mean_keepdim((2, 3))?;
        x.broadcast_mul(&self.conv(&avg, &format!("{p}.block1.2.seq.0.ca.1"), c, c, 1, 1, 0, true)?)
    }
    fn second(&self, x: &Tensor, p: &str, c: usize) -> R<Tensor> {
        let x = self.norm(x, &format!("{p}.block2.0.seq.0"), c)?;
        self.conv(&x, &format!("{p}.block2.0.seq.1"), c, c * 2, 1, 1, 0, true)
    }
    fn mugi(&self, a: &Tensor, b: &Tensor, p: &str, c: usize) -> R<(Tensor, Tensor)> {
        let (x, y) = self.gate(&self.first(a, p, c)?, &self.first(b, p, c)?, c)?;
        let x = self.conv(&self.ca(&x, p, c)?, &format!("{p}.block1.3.seq.0"), c, c, 1, 1, 0, true)?;
        let y = self.conv(&self.ca(&y, p, c)?, &format!("{p}.block1.3.seq.0"), c, c, 1, 1, 0, true)?;
        let vb = self.vb.pp(p);
        let a = (a + x.broadcast_mul(&vb.get((1, c, 1, 1), "a_l")?)?)?;
        let b = (b + y.broadcast_mul(&vb.get((1, c, 1, 1), "a_r")?)?)?;
        let (x, y) = self.gate(&self.second(&a, p, c)?, &self.second(&b, p, c)?, c)?;
        let x = self.conv(&x, &format!("{p}.block2.2.seq.0"), c, c, 1, 1, 0, true)?;
        let y = self.conv(&y, &format!("{p}.block2.2.seq.0"), c, c, 1, 1, 0, true)?;
        Ok((
            (a + x.broadcast_mul(&vb.get((1, c, 1, 1), if self.shared { "b" } else { "b_l" })?)?)?,
            (b + y.broadcast_mul(&vb.get((1, c, 1, 1), if self.shared { "b" } else { "b_r" })?)?)?,
        ))
    }
    fn blocks(&self, mut x: Tensor, mut y: Tensor, p: &str, c: usize, n: usize) -> R<(Tensor, Tensor)> {
        for i in 0..n {
            (x, y) = self.mugi(&x, &y, &format!("{p}.{i}"), c)?;
        }
        Ok((x, y))
    }
    fn pyramid(&self, input: &Tensor) -> R<(Tensor, Tensor)> {
        let mut f = input.clone();
        let mut cin = 3;
        let mut features = HashMap::new();
        for i in 0..30 {
            if [4, 9, 18, 27].contains(&i) {
                f = f.max_pool2d(2)?;
            } else if [0, 2, 5, 7, 10, 12, 14, 16, 19, 21, 23, 25, 28].contains(&i) {
                let cout = if i < 4 {
                    64
                } else if i < 9 {
                    128
                } else if i < 18 {
                    256
                } else {
                    512
                };
                f = self.conv(&f, &format!("vgg.{i}"), cin, cout, 3, 1, 1, true)?;
                cin = cout;
            } else {
                f = f.relu()?;
            }
            if [1, 6, 11, 20, 29].contains(&i) {
                features.insert(i, f.clone());
            }
        }
        let get = |i| features.get(&i).ok_or_else(|| candle_core::Error::Msg("missing VGG feature".into()));
        let (x, y) = self.mugi(get(29)?, get(29)?, "intro.block5.0", 512)?;
        let (_, _, h, w) = x.dims4()?;
        let (mut x, mut y) = (x.upsample_bilinear2d(h * 2, w * 2, true)?, y.upsample_bilinear2d(h * 2, w * 2, true)?);
        for (level, index, c) in [(4, 20, 512), (3, 11, 256), (2, 6, 128)] {
            let (a, b) = self.mugi(get(index)?, get(index)?, &format!("intro.block{level}.0"), c)?;
            x = self.conv(&Tensor::cat(&[&x, &a], 1)?, &format!("intro.ch_map{level}.0.seq.0"), c * 2, c * 2, 1, 1, 0, true)?;
            y = self.conv(&Tensor::cat(&[&y, &b], 1)?, &format!("intro.ch_map{level}.0.seq.0"), c * 2, c * 2, 1, 1, 0, true)?;
            (x, y) =
                self.mugi(&candle_nn::ops::pixel_shuffle(&x, 2)?, &candle_nn::ops::pixel_shuffle(&y, 2)?, &format!("intro.ch_map{level}.1"), c / 2)?;
        }
        let (a, b) = self.mugi(get(1)?, get(1)?, "intro.block1.0", 64)?;
        x = self.conv(&Tensor::cat(&[&x, &a], 1)?, "intro.ch_map1.0.seq.0", 128, 128, 1, 1, 0, true)?;
        y = self.conv(&Tensor::cat(&[&y, &b], 1)?, "intro.ch_map1.0.seq.0", 128, 128, 1, 1, 0, true)?;
        (x, y) = self.mugi(&x, &y, "intro.ch_map1.1", 128)?;
        x = self.conv(&x, "intro.ch_map1.2.seq.0", 128, 32, 1, 1, 0, true)?;
        y = self.conv(&y, "intro.ch_map1.2.seq.0", 128, 32, 1, 1, 0, true)?;
        (x, y) = self.mugi(&x, &y, "intro.ch_map1.3", 32)?;
        let a = self.conv(input, "intro.block_intro.0.seq.0", 3, 32, 3, 1, 1, true)?;
        let (a, b) = self.mugi(&a, &a, "intro.block_intro.1", 32)?;
        Ok((
            self.conv(&Tensor::cat(&[&x, &a], 1)?, "intro.ch_map0.seq.0", 64, self.width, 1, 1, 0, true)?,
            self.conv(&Tensor::cat(&[&y, &b], 1)?, "intro.ch_map0.seq.0", 64, self.width, 1, 1, 0, true)?,
        ))
    }
    pub fn infer(&self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<[f32; 3]>, String> {
        if w == 0 || h == 0 || w > MAX_EDGE || h > MAX_EDGE || rgb.len() != w * h * 3 {
            return Err("invalid reflection input dimensions".into());
        }
        self.run(rgb, w, h).map_err(|e| e.to_string())
    }
    fn run(&self, rgb: &[u8], w: usize, h: usize) -> R<Vec<[f32; 3]>> {
        let data: Vec<f32> = rgb.iter().map(|x| *x as f32 / 255.0).collect();
        let mut input = Tensor::from_vec(data, (1, h, w, 3), &self.device)?.permute((0, 3, 1, 2))?.contiguous()?;
        if !w.is_multiple_of(32) {
            let n = 32 - w % 32;
            let last = input.narrow(3, w - 1, 1)?.repeat((1, 1, 1, n))?;
            input = Tensor::cat(&[&input, &last], 3)?;
        }
        if !h.is_multiple_of(32) {
            let n = 32 - h % 32;
            let last = input.narrow(2, h - 1, 1)?.repeat((1, 1, n, 1))?;
            input = Tensor::cat(&[&input, &last], 2)?;
        }
        let (mut x, mut y) = self.pyramid(&input)?;
        let mut skips = Vec::new();
        let mut c = self.width;
        for (i, n) in self.enc.iter().enumerate() {
            (x, y) = self.blocks(x, y, &format!("encoders.{i}"), c, *n)?;
            skips.push((x.clone(), y.clone()));
            x = self.conv(&x, &format!("downs.{i}.seq.0"), c, c * 2, 2, 2, 0, true)?;
            y = self.conv(&y, &format!("downs.{i}.seq.0"), c, c * 2, 2, 2, 0, true)?;
            c *= 2;
        }
        (x, y) = self.blocks(x, y, "middle_blks", c, self.middle)?;
        for (i, (a, b)) in skips.into_iter().rev().enumerate() {
            x = candle_nn::ops::pixel_shuffle(&self.conv(&x, &format!("ups.{i}.seq.0"), c, c * 2, 1, 1, 0, false)?, 2)?;
            y = candle_nn::ops::pixel_shuffle(&self.conv(&y, &format!("ups.{i}.seq.0"), c, c * 2, 1, 1, 0, false)?, 2)?;
            c /= 2;
            (x, y) = self.blocks((x + a)?, (y + b)?, &format!("decoders.{i}"), c, 2)?;
        }
        let out = self
            .conv(&x, "ending.seq.0", self.width, 3, 3, 1, 1, true)?
            .narrow(2, 0, h)?
            .narrow(3, 0, w)?
            .permute((0, 2, 3, 1))?
            .flatten_all()?
            .to_vec1::<f32>()?;
        let mut pixels = Vec::with_capacity(w * h);
        for p in out.as_chunks::<3>().0 {
            let a = *p;
            if !a.iter().all(|x| x.is_finite()) {
                candle_core::bail!("non-finite model output");
            }
            pixels.push(a.map(|x| x.clamp(0.0, 1.0)));
        }
        Ok(pixels)
    }
}

/// Candle 0.9.2's CPU tiled convolution can mistake equal channel/spatial
/// strides for channels-last storage. A padded view prevents that layout alias
/// without changing pixels; Metal and ordinary CPU layouts use the normal path.
fn conv2d(x: &Tensor, kernel: &Tensor, padding: usize, stride: usize) -> R<Tensor> {
    let (_, channels, h, w) = x.dims4()?;
    let aliases =
        matches!(x.device(), Device::Cpu) && kernel.dim(2)? > 1 && channels > 1 && x.stride() == [channels * h * w, channels * w, channels, 1];
    let input = if aliases { x.pad_with_zeros(3, 0, 1)?.narrow(3, 0, w)? } else { x.clone() };
    input.conv2d(kernel, padding, stride, 1, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_convolution_keeps_channel_order_when_spatial_dimensions_match() {
        for (channels, h, w) in [(3, 3, 3), (4, 4, 4), (3, 4, 5)] {
            let data: Vec<f32> = (0..channels * h * w).map(|v| v as f32).collect();
            let x = Tensor::from_vec(data.clone(), (1, channels, h, w), &Device::Cpu).unwrap();
            let mut weights = vec![0.0; channels * 9];
            for c in 0..channels {
                weights[c * 9 + 4] = (1 << c) as f32;
            }
            let kernel = Tensor::from_vec(weights, (1, channels, 3, 3), &Device::Cpu).unwrap();
            let expected: Vec<f32> = (0..h * w).map(|i| (0..channels).map(|c| data[c * h * w + i] * (1 << c) as f32).sum()).collect();
            assert_eq!(conv2d(&x, &kernel, 1, 1).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap(), expected);
        }
    }
}
