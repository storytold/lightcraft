//! The text detector's mask decoder on the GPU against the candle decoder (the reference), on one photo: the per-query
//! mask logits of all 200 queries, and the merged Describe mask, for a few prompts. Both models get the same backbone
//! output, necks and DETR encoder memory (one GPU accelerator, shared), so only the mask decoder differs. For the first
//! prompt, the instance features also against the decoder transcribed with candle's tensor ops in f64 (and in f32, to
//! show how far candle's own f32 decoder is from it).
//!
//! Needs the checkpoint: `LIGHTCRAFT_SAM3_DIR=<model dir> cargo test --release -p lightcraft-gpu --features sam
//! --test it sam3_mask_decoder:: -- --nocapture`. The photo is a procedural one, or the input of a reference file made by
//! `tools/sam3_reference.py` (`LIGHTCRAFT_SAM3_REF=<ref.safetensors>`). `LIGHTCRAFT_SAM3_PROMPTS` lists the prompts,
//! comma-separated (default "person,sky,the red car"). Skipped without a model or a GPU.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use candle_core::{Device, Tensor};
use lightcraft_gpu::sam3::{self, Precision, Sam3Gpu};
use lightcraft_segment::{BackboneAccel, DetectorOutput, INPUT, NeckKind, Sam3, WeightReader};

fn values(t: &Tensor) -> Vec<f32> {
    t.to_device(&Device::Cpu).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap()
}

/// Max |a − b|, that relative to max |b|, and mean |a − b|.
fn errors(name: &str, a: &[f32], b: &[f32]) -> (f32, f32, f64) {
    assert_eq!(a.len(), b.len(), "{name}: length");
    let scale = b.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let max = a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
    let mean = a.iter().zip(b).map(|(x, y)| f64::from((x - y).abs())).sum::<f64>() / a.len().max(1) as f64;
    eprintln!("  {name}: max abs {max:.3e}, max rel {:.3e} (scale {scale:.2}), mean abs {mean:.3e}", max / scale);
    (max, max / scale, mean)
}

/// Intersection over union of two masks (logits > 0).
fn iou(a: &[f32], b: &[f32]) -> f32 {
    // masks of different sizes are no match (`zip` would compare only the shorter's pixels)
    assert_eq!(a.len(), b.len(), "mask sizes");
    let (mut both, mut either) = (0usize, 0usize);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x > 0.0, *y > 0.0);
        both += usize::from(x && y);
        either += usize::from(x || y);
    }
    if either == 0 { 1.0 } else { both as f32 / either as f32 }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// The Describe mask of `out` as `Sam3::segment_text` merges it at 0.5 (the per-pixel maximum over the instances
/// scoring above it), the instances, and the best-scoring query.
fn merged(out: &DetectorOutput, masks: &[f32]) -> (Vec<f32>, Vec<usize>, usize) {
    let side = lightcraft_segment::MASK_SIDE * lightcraft_segment::MASK_SIDE;
    let mut merged = vec![f32::MIN; side];
    let keep: Vec<usize> = (0..out.logits.len()).filter(|&i| sigmoid(out.logits[i]) * sigmoid(out.presence) > 0.5).collect();
    for i in &keep {
        for (m, v) in merged.iter_mut().zip(&masks[i * side..(i + 1) * side]) {
            *m = m.max(*v);
        }
    }
    let best = (0..out.logits.len()).max_by(|a, b| out.logits[*a].total_cmp(&out.logits[*b])).unwrap();
    (merged, keep, best)
}

/// The photo as preprocessed pixels `[1, 3, 1008, 1008]`.
fn photo() -> Tensor {
    if let Some(path) = std::env::var_os("LIGHTCRAFT_SAM3_REF").map(PathBuf::from) {
        let mut r = candle_core::safetensors::load(&path, &Device::Cpu).unwrap();
        return r.remove("pixel_values").unwrap();
    }
    let img = lightcraft_scenes::demo_library()[0].render_fit(1600).to_srgb8();
    let rgb: Vec<u8> = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let px = lightcraft_segment::mask::preprocess(&rgb, img.width, img.height, INPUT).unwrap();
    Tensor::from_vec(px, (1, 3, INPUT, INPUT), &Device::Cpu).unwrap()
}

/// The mask decoder transcribed with candle's tensor ops in `dtype` (f64: the arbiter between the two f32 decoders):
/// the instance features `[256, 288 · 288]` for the encoder memory `[5184, 256]`, the prompt `[32, 256]` (`valid`: its
/// real tokens) and the neck levels `[256, 144, 144]`, `[256, 288, 288]`.
fn decoder_ref(w: &WeightReader, memory: &Tensor, (text, valid): (&Tensor, &[bool]), [l1, l0]: [&Tensor; 2], dtype: candle_core::DType) -> Tensor {
    let get = |name: &str| {
        let (shape, data) = w.read(&format!("detector_model.mask_decoder.{name}")).unwrap();
        Tensor::from_vec(data, shape, &Device::Cpu).unwrap().to_dtype(dtype).unwrap()
    };
    let lin = |x: &Tensor, name: &str| {
        x.matmul(&get(&format!("{name}.weight")).t().unwrap()).unwrap().broadcast_add(&get(&format!("{name}.bias"))).unwrap()
    };
    let (memory, text) = (memory.to_dtype(dtype).unwrap(), text.to_dtype(dtype).unwrap());
    // prompt attention: LN (eps 1e-5) → 8 heads of 32 over the prompt (padding masked) → output projection, added
    let mean = memory.mean_keepdim(1).unwrap();
    let centred = memory.broadcast_sub(&mean).unwrap();
    let var = centred.sqr().unwrap().mean_keepdim(1).unwrap();
    let h = centred.broadcast_div(&(var + 1e-5).unwrap().sqrt().unwrap()).unwrap();
    let h = h.broadcast_mul(&get("prompt_cross_attn_norm.weight")).unwrap().broadcast_add(&get("prompt_cross_attn_norm.bias")).unwrap();
    let heads = |x: Tensor| x.reshape((x.dim(0).unwrap(), 8, 32)).unwrap().transpose(0, 1).unwrap().contiguous().unwrap();
    let q = heads(lin(&h, "prompt_cross_attn.q_proj"));
    let k = heads(lin(&text, "prompt_cross_attn.k_proj"));
    let v = heads(lin(&text, "prompt_cross_attn.v_proj"));
    let bias: Vec<f32> = valid.iter().map(|&ok| if ok { 0.0 } else { f32::MIN }).collect();
    let bias = Tensor::from_vec(bias, (1, 1, 32), &Device::Cpu).unwrap().to_dtype(dtype).unwrap();
    let s = (q.matmul(&k.t().unwrap()).unwrap() * (32f64).powf(-0.5)).unwrap().broadcast_add(&bias).unwrap();
    let e = s.broadcast_sub(&s.max_keepdim(2).unwrap()).unwrap().exp().unwrap();
    let p = e.broadcast_div(&e.sum_keepdim(2).unwrap()).unwrap();
    let o = p.matmul(&v).unwrap().transpose(0, 1).unwrap().reshape((5184, 256)).unwrap();
    let mem = (memory + lin(&o, "prompt_cross_attn.o_proj")).unwrap();
    // the pixel decoder
    let mut prev = mem.t().unwrap().reshape((1, 256, 72, 72)).unwrap();
    for (i, skip) in [l1, l0].into_iter().enumerate() {
        let side = skip.dim(1).unwrap();
        prev = (prev.upsample_nearest2d(side, side).unwrap() + skip.to_dtype(dtype).unwrap().unsqueeze(0).unwrap()).unwrap();
        let c = format!("pixel_decoder.conv_layers.{i}");
        prev = prev
            .conv2d(&get(&format!("{c}.weight")), 1, 1, 1, 1)
            .unwrap()
            .broadcast_add(&get(&format!("{c}.bias")).reshape((1, 256, 1, 1)).unwrap())
            .unwrap();
        let g = prev.reshape((8, 32 * side * side)).unwrap();
        let centred = g.broadcast_sub(&g.mean_keepdim(1).unwrap()).unwrap();
        let var = centred.sqr().unwrap().mean_keepdim(1).unwrap();
        let normed = centred.broadcast_div(&(var + 1e-5).unwrap().sqrt().unwrap()).unwrap().reshape((1, 256, side, side)).unwrap();
        let n = format!("pixel_decoder.norms.{i}");
        let (gamma, beta) =
            (get(&format!("{n}.weight")).reshape((1, 256, 1, 1)).unwrap(), get(&format!("{n}.bias")).reshape((1, 256, 1, 1)).unwrap());
        prev = normed.broadcast_mul(&gamma).unwrap().broadcast_add(&beta).unwrap().relu().unwrap();
    }
    let inst = prev.conv2d(&get("instance_projection.weight"), 0, 1, 1, 1).unwrap();
    inst.broadcast_add(&get("instance_projection.bias").reshape((1, 256, 1, 1)).unwrap()).unwrap().reshape((256, 288 * 288)).unwrap()
}

/// One GPU accelerator for both models; the mask decoder only for one of them, the other gets a result of the wrong
/// size (and then runs the candle decoder; `asked` counts the calls).
struct Shared {
    gpu: Arc<Sam3Gpu>,
    masks: bool,
    asked: Arc<AtomicUsize>,
}

impl BackboneAccel for Shared {
    fn forward(&self, pixels: &[f32]) -> Result<Vec<f32>, String> {
        self.gpu.forward(pixels)
    }

    fn device(&self) -> String {
        self.gpu.device()
    }

    fn runs_necks(&self) -> bool {
        true
    }

    fn runs_detr_encoder(&self) -> bool {
        true
    }

    fn runs_mask_decoder(&self) -> bool {
        true
    }

    fn neck(&self, which: NeckKind, backbone: &[f32], n: usize) -> Result<Vec<Vec<f32>>, String> {
        self.gpu.neck(which, backbone, n)
    }

    fn detr_encoder(&self, image: &[f32], pos: &[f32], text: &[f32], valid: &[bool]) -> Result<Vec<f32>, String> {
        self.gpu.detr_encoder(image, pos, text, valid)
    }

    fn mask_decoder(
        &self,
        memory: &[f32],
        text: &[f32],
        valid: &[bool],
        levels: [&[f32]; 2],
        rows: &[f32],
        bias: &[f32],
    ) -> Result<Vec<f32>, String> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        if self.masks { self.gpu.mask_decoder(memory, text, valid, levels, rows, bias) } else { Ok(vec![0.0; 10]) }
    }
}

#[test]
fn the_gpu_mask_decoder_matches_the_candle_decoder() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_SAM3_DIR").map(PathBuf::from).filter(|d| lightcraft_segment::is_model_dir(d)) else {
        lightcraft_testkit::skip!(Model, "set LIGHTCRAFT_SAM3_DIR to the SAM 3 model directory");
        return;
    };
    let adapter = match sam3::adapter() {
        Ok(a) => a,
        // a kernel that doesn't compile is a failure, not a missing GPU
        Err(e) if e.contains("did not compile") => panic!("{e}"),
        Err(e) => {
            lightcraft_testkit::skip!(Gpu, "no GPU for the SAM 3 kernels ({e})");
            return;
        }
    };
    let gpu = Arc::new(sam3::load(&WeightReader::open(&dir).unwrap(), Precision::F32).unwrap());
    let (asked_cpu, asked_gpu) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let with = |masks: bool, asked: &Arc<AtomicUsize>| {
        let (gpu, asked) = (gpu.clone(), asked.clone());
        move |_: &WeightReader| -> Result<Box<dyn BackboneAccel>, String> { Ok(Box::new(Shared { gpu, masks, asked })) }
    };
    let mut cpu = Sam3::load_accelerated(&dir, Device::Cpu, with(false, &asked_cpu)).unwrap();
    let mut acc = Sam3::load_accelerated(&dir, Device::Cpu, with(true, &asked_gpu)).unwrap();
    let pixels = photo();
    let mut enc_cpu = cpu.encode_pixels(&pixels).unwrap();
    let mut enc_acc = acc.encode_pixels(&pixels).unwrap();

    let prompts = std::env::var("LIGHTCRAFT_SAM3_PROMPTS").unwrap_or_else(|_| "person,sky,the red car".into());
    let prompts: Vec<&str> = prompts.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    for (n, prompt) in prompts.iter().copied().enumerate() {
        let t = Instant::now();
        let want = cpu.detect(&mut enc_cpu, prompt).unwrap();
        let cpu_time = t.elapsed();
        let t = Instant::now();
        let got = acc.detect(&mut enc_acc, prompt).unwrap();
        let gpu_time = t.elapsed();
        let first = if n == 0 { " (first prompt: with the detector neck and loading)" } else { "" };
        eprintln!("{prompt:?}: detector with the candle mask decoder {cpu_time:?}, with the GPU one {gpu_time:?}{first}");
        assert_eq!(acc.mask_fallback(), None, "the GPU mask decoder ran");
        assert!(cpu.mask_fallback().is_some_and(|w| w.contains("returned 10 values")));
        assert_eq!(acc.heads_device(), "CPU; the necks, the DETR encoder and the mask decoder on the GPU");
        assert_eq!(cpu.heads_device(), "CPU; the necks and the DETR encoder on the GPU");

        if n == 0 {
            // the instance features (the instance projection as the rows) against the f64 transcription, and how far
            // the same transcription in f32 is from it
            let reader = WeightReader::open(&dir).unwrap();
            let (_, valid) = lightcraft_segment::tokenizer::Tokenizer::load(&dir).unwrap().encode(prompt);
            let levels = acc.neck_levels(NeckKind::Detector, &enc_acc).unwrap();
            let (l1, l0) = (levels[1].squeeze(0).unwrap(), levels[0].squeeze(0).unwrap());
            let (memory, text) = (got.memory.squeeze(0).unwrap(), got.text.squeeze(0).unwrap());
            let (_, rows) = reader.read("detector_model.mask_decoder.instance_projection.weight").unwrap();
            let (_, bias) = reader.read("detector_model.mask_decoder.instance_projection.bias").unwrap();
            let t = Instant::now();
            let ours = gpu.mask_decoder(&values(&memory), &values(&text), &valid, [&values(&l1), &values(&l0)], &rows, &bias).unwrap();
            eprintln!("  GPU instance features {:?}", t.elapsed());
            let t = Instant::now();
            let f64_ref: Vec<f32> = values(
                &decoder_ref(&reader, &memory, (&text, &valid), [&l1, &l0], candle_core::DType::F64).to_dtype(candle_core::DType::F32).unwrap(),
            );
            let f32_ref = values(&decoder_ref(&reader, &memory, (&text, &valid), [&l1, &l0], candle_core::DType::F32));
            eprintln!("  references {:?}", t.elapsed());
            let (_, rel, mean) = errors("instance features, GPU vs f64", &ours, &f64_ref);
            errors("instance features, candle f32 vs f64", &f32_ref, &f64_ref);
            assert!(rel < 1e-4 && mean < 1e-5, "the GPU instance features are off");
        }

        // the same memory and decoder for both: only the masks may differ, by candle's own error (its f32 decoder is
        // ~400 × further from the f64 one than the GPU's, above)
        let (_, logits, _) = errors("query logits", &got.logits, &want.logits);
        assert!(logits < 1e-4, "the query logits moved");
        let (mg, mc) = (values(&got.masks), values(&want.masks));
        let (_, rel, mean) = errors("mask logits (200 queries), GPU vs candle", &mg, &mc);
        assert!(rel < 1e-2 && mean < 0.1, "the GPU mask logits are off");
        let ((mask_g, kept_g, best_g), (mask_c, kept_c, best_c)) = (merged(&got, &mg), merged(&want, &mc));
        let side = lightcraft_segment::MASK_SIDE * lightcraft_segment::MASK_SIDE;
        let best = iou(&mg[best_g * side..(best_g + 1) * side], &mc[best_c * side..(best_c + 1) * side]);
        let i = iou(&mask_g, &mask_c);
        eprintln!("  instances {} vs {}, Describe mask IoU {i:.5}, best query's mask IoU {best:.5}", kept_g.len(), kept_c.len());
        assert_eq!(kept_g, kept_c, "the same instances");
        assert!(i > 0.99 && best > 0.99, "describe mask IoU {i}, best query {best}");
        // the semantic mask (one more row of the GPU's mask product)
        let (_, rel, mean) = errors("semantic mask logits, GPU vs candle", &got.semantic, &want.semantic);
        let semantic = iou(&got.semantic, &want.semantic);
        eprintln!("  semantic mask IoU {semantic:.5}");
        assert!(rel < 1e-2 && mean < 0.1 && semantic > 0.99, "the GPU semantic mask is off");
    }
    // an accelerator whose decoder failed once isn't asked again
    assert_eq!(asked_cpu.load(Ordering::Relaxed), 1);
    assert_eq!(asked_gpu.load(Ordering::Relaxed), prompts.len());

    // the decoder alone (with the uploads and the read-back)
    let (memory, text) = (vec![0.1f32; 5184 * 256], vec![0.3f32; 32 * 256]);
    let valid: Vec<bool> = (0..32).map(|i| i < 4).collect();
    let (l1, l0) = (vec![0.2f32; 256 * 144 * 144], vec![0.4f32; 256 * 288 * 288]);
    let (rows, bias) = (vec![0.01f32; 200 * 256], vec![0.5f32; 200]);
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        gpu.mask_decoder(&memory, &text, &valid, [&l1, &l0], &rows, &bias).unwrap();
        best = best.min(t.elapsed().as_secs_f64());
    }
    let (w, b) = gpu.mask_decoder_bytes().unwrap();
    eprintln!("GPU mask decoder alone: {:.1} ms on {adapter}; {:.0} MB weights, {:.0} MB work buffers", best * 1e3, w as f64 / 1e6, b as f64 / 1e6);
}
