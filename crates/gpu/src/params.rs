//! Parameter blocks: the per-pixel stage's [`FinishParams`] packed into the 32-bit words the
//! `finish` kernel reads (`F_*` constants, generated from [`FIELDS`]), plus its auxiliary tables.

use lightcraft_develop::VignetteStyle;
use lightcraft_pipeline::colorops::POINT_WORDS;
use lightcraft_pipeline::finish::{FinishParams, GrainModel, MASK_TERMS, srgb_lut};

use crate::ctx::{FailKind, fail};

/// Entries per tone-curve table (`lightcraft_pipeline::finish` builds them at this size).
pub const CURVE_N: u32 = 1024;

/// Set on every word of a float render's output: the values are at least 0, so the sign bit is
/// free, and it tells a written 0 from a word the kernel never reached.
pub const DEEP_WRITTEN: u32 = 1 << 31;

/// Field name and number of 32-bit words.
const FIELDS: &[(&str, usize)] = &[
    ("W", 1),
    ("H", 1),
    ("NMASK", 1),
    ("MASK_OFF", 1),
    ("SRGB_OFF", 1),
    ("CURVE_OFF", 1),
    ("CURVES", 1),
    ("GAIN", 1),
    ("EV", 1),
    ("AIR", 1),
    ("AIR_PRE", 1),
    ("HL", 1),
    ("SH", 1),
    ("CLAR", 1),
    ("TEX", 1),
    ("DEHAZE", 1),
    ("SHARPEN", 1),
    ("SHARPEN_MASK", 1),
    ("SHARPEN_LIMIT", 1),
    ("SHARPEN_GAIN", 1),
    ("SHARPEN_V3", 1),
    ("SHP_OFF", 1),
    ("HAS_SHP", 1),
    ("HAS_CLAR", 1),
    ("HAS_TEX", 1),
    ("HAS_DARK", 1),
    ("HAS_CHROMA", 1),
    ("OPS_IDENTITY", 1),
    ("VIBRANCE", 1),
    ("SATURATION", 1),
    ("MIXER", 1),
    ("MIX_HUE", 8),
    ("MIX_SAT", 8),
    ("MIX_LUM", 8),
    ("NPC", 1),
    ("PC", 8 * POINT_WORDS),
    ("BW", 1),
    ("BW_MIX", 8),
    ("GRADING", 1),
    ("WHEELS", 12),
    ("BLENDING", 1),
    ("BALANCE", 1),
    ("SKIN", 1),
    ("BANDH", 8),
    ("VIG", 1),
    ("VIG_AMOUNT", 1),
    ("VIG_START", 1),
    ("VIG_WIDTH", 1),
    ("VIG_ASPECT_MIX", 1),
    ("VIG_POWER", 1),
    ("VIG_HL", 1),
    ("VIG_STYLE", 1),
    // the window's offset in the whole output and the whole output's size (pixels)
    ("VIG_VIEW", 4),
    ("GRAIN", 1),
    // 0 = value noise (processes V1 to V6: GRAIN_AMT, GRAIN_SC, GRAIN_ROUGH), 1 = crystal layers (V7)
    ("GRAIN_MODEL", 1),
    // crystal grain: amplitude, footprint (tile px per output px), the tori's tables in `aux`
    ("GRAIN_K", 1),
    ("GRAIN_FOOT", 1),
    ("GRAIN_SAT", 2),
    ("GRAIN_AMT", 1),
    ("GRAIN_SC", 1),
    ("GRAIN_ROUGH", 1),
    ("GRAIN_SEED", 1),
    ("GRAIN_AFF", 6),
    ("REFINE_SAT", 1),
    ("CALIB", 1),
    ("CALIB_M", 9),
    ("SHADOW_TINT", 1),
    ("OUT_M", 9),
    ("OUT_Y", 3),
    ("OUT_TRC", 1),
    ("OUT_GAMMA", 1),
    // a LUT profile: its table in `aux` at LUT_OFF (size³ rgb, red fastest), amount and domain
    ("LUT", 1),
    ("LUT_OFF", 1),
    ("LUT_N", 1),
    ("LUT_AMT", 1),
    ("LUT_MIN", 3),
    ("LUT_MAX", 3),
    // soft proofing (matrix proofs; `ProofParams`): working → proof RGB, its luma, proof → output, warnings
    ("PROOF", 1),
    ("PROOF_M", 9),
    ("PROOF_Y", 3),
    ("PROOF_OUT", 9),
    ("PROOF_DEST_WARN", 1),
    ("PROOF_DISP_WARN", 1),
    // first row of a band dispatch (the kernel runs over rows Y0.., see `render`)
    ("Y0", 1),
    // process V4's calibrated Contrast, Blacks (`FinishParams::light`): the number of active
    // operators (0 = none), each one's mode (`light_v3::Mode::code`) and mix weight, and the offset
    // of their tables in `aux` (LIGHT_N entries each, in order)
    ("LIGHT_OPS", 1),
    ("LIGHT_MODE", 2),
    ("LIGHT_W", 2),
    ("LIGHT_OFF", 1),
    // the render's peak over SDR white (`FinishParams::peak`; above 1: an HDR render)
    ("PEAK", 1),
    // a float render (`finish_deep`): the exact sRGB curve, and three words per pixel in `out`, each
    // a linear value with `DEEP_WRITTEN` set
    ("DEEP", 1),
];

/// `(name, index)` of every field (for the WGSL constants).
pub fn finish_fields() -> Vec<(&'static str, usize)> {
    let mut i = 0;
    FIELDS
        .iter()
        .map(|(n, len)| {
            let r = (*n, i);
            i += len;
            r
        })
        .collect()
}

/// Offset of field `name`; `None` (and a debug assertion) for a name not in [`FIELDS`].
pub fn index(name: &str) -> Option<usize> {
    let mut i = 0;
    for (n, len) in FIELDS {
        if *n == name {
            return Some(i);
        }
        i += len;
    }
    debug_assert!(false, "unknown finish field {name}");
    None
}

/// A parameter block being filled by name.
pub struct Block(pub Vec<u32>);

impl Block {
    fn new() -> Block {
        Block(vec![0; FIELDS.iter().map(|f| f.1).sum()])
    }
    fn f(&mut self, name: &str, v: f32) {
        self.u(name, v.to_bits());
    }
    fn u(&mut self, name: &str, v: u32) {
        if let Some(slot) = index(name).and_then(|i| self.0.get_mut(i)) {
            *slot = v;
        }
    }
    fn b(&mut self, name: &str, v: bool) {
        self.u(name, v as u32);
    }
    fn fs(&mut self, name: &str, v: &[f32]) {
        let Some(i) = index(name) else { return };
        for (slot, x) in self.0.iter_mut().skip(i).zip(v) {
            *slot = x.to_bits();
        }
    }
}

/// Which optional planes are bound.
pub struct Present {
    pub clarity: bool,
    pub texture: bool,
    /// The sharpening blur plane (after the texture plane in `tex` when both are present).
    pub sharpen: bool,
    pub dark: bool,
    /// The blurred chromaticity follows the mask planes in the `masks` buffer.
    pub chroma: bool,
}

/// The `finish` kernel's parameter block and auxiliary table (tone LUT | chroma curve | sRGB LUT | curve LUTs |
/// mask terms | light operator tables) for `fp` with `masks` (their local adjustments' terms). The crystal
/// grain's tables go after `aux` in the kernel's buffer ([`crate::render`] copies them there).
///
/// `deep`: the kernel writes float samples, as the pipeline's `finish_deep` does.
///
/// A curve whose length is not [`CURVE_N`] fails the render ([`FailKind::Fatal`]) and returns an
/// empty parameter block with a truncated aux table, which the caller discards (CPU fallback).
pub fn finish_block(fp: &FinishParams, masks: &[[f32; MASK_TERMS]], present: &Present, deep: bool) -> (Vec<u32>, Vec<f32>) {
    let mut aux: Vec<f32> = fp.tone.lut().to_vec();
    aux.extend_from_slice(fp.tone.chroma_lut());
    let srgb_off = aux.len();
    aux.extend_from_slice(&srgb_lut()[..]);
    let curve_off = aux.len();
    if let Some(c) = &fp.curves {
        for l in c {
            if l.v.len() != CURVE_N as usize {
                // a wrong-length curve would shift every later aux offset (CURVE_OFF, MASK_OFF and
                // the mask terms that follow) and make the kernel read the wrong data. `fail()`
                // records the failure on this thread so the render falls back to the CPU; the
                // partial `(p, aux)` returned here is never bound to a pipeline.
                fail(FailKind::Fatal, format!("curve table has {} entries, expected {}", l.v.len(), CURVE_N));
                // `p` is only assembled below the curve loop, so an empty block is as good as any:
                // the caller (render.rs) gates on `failed()` right after this returns and bails to
                // the CPU fallback before indexing `p` or uploading `aux`.
                return (Vec::new(), aux);
            }
            aux.extend_from_slice(&l.v);
        }
    }
    let mask_off = aux.len();
    for m in masks {
        aux.extend_from_slice(m);
    }
    let light_off = aux.len();
    let light = fp.light.as_ref().map_or(&[][..], |l| &l.ops[..]);
    let light = &light[..light.len().min(2)];
    for op in light {
        aux.extend_from_slice(&op.lut[..]);
    }
    let lut_off = aux.len();
    if let Some((l, _)) = &fp.lut {
        aux.extend(l.data.iter().flatten());
    }
    // the crystal grain's tables follow `aux` on the GPU without being part of it: they stay
    // resident between renders ([`crate::Gpu::grain_tables`]) and the render copies them in
    let mut sat_off = [0u32; 2];
    if let Some(GrainModel::Crystal(g)) = &fp.grain {
        let mut off = aux.len();
        for (slot, sat) in sat_off.iter_mut().zip(&g.tile.sats) {
            *slot = off as u32;
            off += sat.len();
        }
    }

    let mut p = Block::new();
    p.u("LIGHT_OPS", light.len() as u32);
    p.u("LIGHT_OFF", light_off as u32);
    if let Some(i) = index("LIGHT_MODE") {
        for (k, op) in light.iter().enumerate() {
            if let Some(slot) = p.0.get_mut(i + k) {
                *slot = op.mode.code();
            }
        }
    }
    p.fs("LIGHT_W", &light.iter().map(|op| op.weight).collect::<Vec<f32>>());
    p.f("PEAK", fp.peak);
    p.b("DEEP", deep);
    p.u("W", fp.w as u32);
    p.u("H", fp.h as u32);
    p.u("NMASK", masks.len() as u32);
    p.u("MASK_OFF", mask_off as u32);
    p.u("SRGB_OFF", srgb_off as u32);
    p.u("CURVE_OFF", curve_off as u32);
    p.b("CURVES", fp.curves.is_some());
    p.f("REFINE_SAT", fp.refine_sat);
    p.f("GAIN", fp.gain);
    p.f("EV", fp.ev);
    p.f("AIR", fp.air);
    p.f("AIR_PRE", fp.air_pre);
    p.f("HL", fp.hl);
    p.f("SH", fp.sh);
    p.f("CLAR", fp.clar);
    p.f("TEX", fp.tex);
    p.f("DEHAZE", fp.dehaze);
    p.f("SHARPEN", fp.sharpen);
    p.f("SHARPEN_MASK", fp.sharpen_mask);
    p.f("SHARPEN_LIMIT", fp.sharpen_limit);
    p.f("SHARPEN_GAIN", fp.sharpen_gain);
    p.b("SHARPEN_V3", fp.pixel_sharpen);
    p.u("SHP_OFF", if present.texture { (fp.w * fp.h) as u32 } else { 0 });
    p.b("HAS_SHP", present.sharpen);
    p.b("HAS_CLAR", present.clarity);
    p.b("HAS_TEX", present.texture);
    p.b("HAS_DARK", present.dark);
    p.b("HAS_CHROMA", present.chroma);

    let ops = &fp.ops;
    p.b("OPS_IDENTITY", ops.is_identity());
    p.f("VIBRANCE", ops.vibrance);
    p.f("SATURATION", ops.saturation);
    p.b("MIXER", ops.mixer);
    p.fs("MIX_HUE", &ops.hue);
    p.fs("MIX_SAT", &ops.sat);
    p.fs("MIX_LUM", &ops.lum);
    p.u("NPC", ops.points.len().min(8) as u32);
    let pc: Vec<f32> = ops.points.iter().take(8).flat_map(|k| k.words()).collect();
    p.fs("PC", &pc);
    p.b("BW", ops.bw.is_some());
    p.fs("BW_MIX", &ops.bw.unwrap_or([0.0; 8]));
    if let Some((wheels, blending, balance)) = &ops.grading {
        p.b("GRADING", true);
        let w: Vec<f32> = wheels.iter().flat_map(|k| [k.a, k.b, k.lum]).collect();
        p.fs("WHEELS", &w);
        p.f("BLENDING", *blending);
        p.f("BALANCE", *balance);
    }
    p.f("SKIN", ops.skin);
    p.fs("BANDH", lightcraft_pipeline::colorops::band_hues());
    p.fs("VIG_VIEW", &fp.view);

    if let Some(v) = &fp.vig {
        p.b("VIG", true);
        p.f("VIG_AMOUNT", v.amount);
        p.f("VIG_START", v.start);
        p.f("VIG_WIDTH", v.width);
        p.f("VIG_ASPECT_MIX", v.aspect_mix);
        p.f("VIG_POWER", v.power);
        p.f("VIG_HL", v.highlights);
        p.u(
            "VIG_STYLE",
            match v.style {
                VignetteStyle::HighlightPriority => 1,
                VignetteStyle::PaintOverlay => 2,
                VignetteStyle::ColorPriority => 0,
            },
        );
    }
    if let Some(m) = &fp.calib {
        p.b("CALIB", true);
        p.fs("CALIB_M", m.as_flattened());
    }
    p.f("SHADOW_TINT", fp.shadow_tint);
    p.fs("OUT_M", fp.to_out.as_flattened());
    p.fs("OUT_Y", &fp.out_luma);
    let (trc, gamma) = fp.out_trc.code();
    p.u("OUT_TRC", trc);
    p.f("OUT_GAMMA", gamma);
    if let Some((l, amount)) = &fp.lut {
        p.b("LUT", true);
        p.u("LUT_OFF", lut_off as u32);
        p.u("LUT_N", l.size as u32);
        p.f("LUT_AMT", *amount);
        p.fs("LUT_MIN", &l.domain_min);
        p.fs("LUT_MAX", &l.domain_max);
    }
    if let Some(pp) = fp.proof.as_ref().filter(|pp| pp.table.is_none()) {
        p.b("PROOF", true);
        p.fs("PROOF_M", pp.to_proof.as_flattened());
        p.fs("PROOF_Y", &pp.luma);
        p.fs("PROOF_OUT", pp.proof_to_out.as_flattened());
        p.b("PROOF_DEST_WARN", pp.dest_warning);
        p.b("PROOF_DISP_WARN", pp.display_warning);
    }
    if let Some(model) = &fp.grain {
        p.b("GRAIN", true);
        match model {
            GrainModel::Value(g) => {
                p.u("GRAIN_MODEL", 0);
                p.f("GRAIN_AMT", g.amt);
                p.f("GRAIN_SC", fp.px_per_long as f32 / g.cell);
                p.f("GRAIN_ROUGH", g.rough);
                p.u("GRAIN_SEED", g.seed);
            }
            GrainModel::Crystal(g) => {
                p.u("GRAIN_MODEL", 1);
                p.f("GRAIN_K", g.k);
                p.f("GRAIN_FOOT", g.foot);
                if let Some(i) = index("GRAIN_SAT") {
                    for (slot, off) in p.0.iter_mut().skip(i).zip(sat_off) {
                        *slot = off;
                    }
                }
            }
        }
        let long = fp.ow.max(fp.oh);
        let [a, b, c, d, e, f] = fp.out_to_norm.0;
        let (kx, ky) = (fp.ow / long, fp.oh / long);
        p.fs("GRAIN_AFF", &[(a * kx) as f32, (b * ky) as f32, (c * kx) as f32, (d * ky) as f32, (e * kx) as f32, (f * ky) as f32]);
    }
    (p.0, aux)
}
