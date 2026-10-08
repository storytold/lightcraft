//! Parameter blocks: the per-pixel stage's [`FinishParams`] packed into the 32-bit words the
//! `finish` kernel reads (`F_*` constants, generated from [`FIELDS`]), plus its auxiliary tables.

use lightcraft_develop::VignetteStyle;
use lightcraft_pipeline::colorops::POINT_WORDS;
use lightcraft_pipeline::finish::{FinishParams, MASK_TERMS, srgb_lut};

/// Entries per tone-curve table (`lightcraft_pipeline::finish` builds them at this size).
pub const CURVE_N: u32 = 1024;

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
    // sharpening on the finished image (`finish::Sharpen`): the pass (0 none, 1 write the finished
    // luminance, 2 apply), the amount, per unit of local Sharpness, Masking, output px per source
    // px, the amount knots and k / L at them
    ("SH_PASS", 1),
    ("SH_AMT", 1),
    ("SH_LOCAL", 1),
    ("SH_MASK", 1),
    ("SH_PXSRC", 1),
    ("SH_KNOTS", 8),
    ("SH_K", 8),
    ("SH_L", 8),
    // offset of the sharpening blur in the `tex` buffer (after the texture plane when both exist)
    ("SHARP_OFF", 1),
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
    ("MIX_DESAT", 8),
    ("MIX_LUM", 8),
    ("MIX_C", 8),
    ("NPC", 1),
    ("PC", 8 * POINT_WORDS),
    ("BW", 1),
    ("BW_MIX", 8),
    ("GRADING", 1),
    ("GRADE_D", 12),
    ("GRADE_MU", 4),
    ("GRADE_SG", 4),
    ("BANDH", 8),
    ("VIG", 1),
    ("VIG_AMOUNT", 1),
    ("VIG_STRENGTH", 1),
    ("VIG_START", 1),
    ("VIG_WIDTH", 1),
    ("VIG_ASPECT_MIX", 1),
    ("VIG_POWER", 1),
    ("VIG_HL", 1),
    ("VIG_STYLE", 1),
    ("GRAIN", 1),
    ("GRAIN_AMT", 1),
    ("GRAIN_SC", 1),
    ("GRAIN_ROUGH", 1),
    ("GRAIN_SEED", 1),
    ("GRAIN_AFF", 6),
    ("REFINE_SAT", 1),
    ("CURVE_M", 9),
    ("CURVE_MI", 9),
    ("CURVE_Y", 3),
    ("CALIB", 1),
    ("CALIB_M", 9),
    ("SHADOW_TINT", 1),
    // a DNG profile tone curve applied per channel (`ToneMap::apply_rgb`)
    ("TONE_RGB", 1),
    ("TONE_TO", 9),
    ("TONE_FROM", 9),
    // Lightroom's tone in stages (`tone::ToneStages`: Whites / Blacks set on a DNG profile curve):
    // on, the stage tables' offsets in `aux`, and the Blacks / Whites luminance-gain share
    ("TONE_STAGED", 1),
    ("TONE_PRE_OFF", 1),
    ("TONE_BK", 1),
    ("TONE_BK_OFF", 1),
    ("TONE_KB", 1),
    ("TONE_WH", 1),
    ("TONE_WH_OFF", 1),
    ("TONE_KW", 1),
    ("TONE_POST_OFF", 1),
    // Contrast as its own stage after Highlights / Shadows (`ToneMap::apply_contrast_rgb`): on,
    // and its table's offset in `aux`
    ("TONE_CON", 1),
    ("TONE_CON_OFF", 1),
    // Highlights / Shadows inside the staged map (`ToneMap::apply_rgb_hs`): on, the profile curve,
    // its inverse and the neighbourhood (base operator + curve) tables' offsets in `aux`
    ("TONE_HS_IN", 1),
    ("TONE_PROF_OFF", 1),
    ("TONE_PROFINV_OFF", 1),
    ("TONE_CTX_OFF", 1),
    // Highlights / Shadows after the tone map (`tone::LrHs`): on, then per slider its amount
    // (relative to the reference), pixel / neighbourhood blend and table
    ("LR_HS", 1),
    ("LR_HL", 1),
    ("LR_HL_ALPHA", 1),
    ("LR_HL_TAB", lightcraft_pipeline::tone::LR_KNOTS),
    ("LR_SH", 1),
    ("LR_SH_ALPHA", 1),
    ("LR_SH_TAB", lightcraft_pipeline::tone::LR_KNOTS),
    ("OUT_M", 9),
    ("OUT_Y", 3),
    ("OUT_TRC", 1),
    ("OUT_GAMMA", 1),
    // first row of a band dispatch (the kernel runs over rows Y0.., see `render`)
    ("Y0", 1),
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
    /// Offset (words) of the sharpening blur in the `tex` buffer.
    pub sharpen_off: usize,
    pub dark: bool,
    /// The blurred chromaticity follows the mask planes in the `masks` buffer.
    pub chroma: bool,
}

/// The `finish` kernel's parameter block and auxiliary table (tone LUT | chroma curve | sRGB LUT |
/// curve LUTs | tone stage LUTs | mask terms) for `fp` with `masks` (their local adjustments' terms).
pub fn finish_block(fp: &FinishParams, masks: &[[f32; MASK_TERMS]], present: &Present) -> (Vec<u32>, Vec<f32>) {
    let mut aux: Vec<f32> = fp.tone.lut().to_vec();
    aux.extend_from_slice(fp.tone.chroma_lut());
    let srgb_off = aux.len();
    aux.extend_from_slice(&srgb_lut()[..]);
    let curve_off = aux.len();
    if let Some(c) = &fp.curves {
        for l in c {
            assert_eq!(l.v.len(), CURVE_N as usize);
            aux.extend_from_slice(&l.v);
        }
    }
    let mut push = |t: &[f32]| {
        let off = aux.len() as u32;
        aux.extend_from_slice(t);
        off
    };
    let stages = fp
        .tone
        .stages()
        .map(|s| (push(&s.pre), s.blacks.as_ref().map(|(t, k)| (push(t), *k)), s.whites.as_ref().map(|(t, k)| (push(t), *k)), push(&s.post)));
    let contrast_off = fp.tone.contrast_table().map(&mut push);
    let hs_in = fp.tone.stages().filter(|_| fp.tone.hs_inside()).map(|s| (push(&s.profile), push(&s.profile_inv), push(&s.context)));
    let mask_off = aux.len();
    for m in masks {
        aux.extend_from_slice(m);
    }

    let mut p = Block::new();
    p.u("W", fp.w as u32);
    p.u("H", fp.h as u32);
    p.u("NMASK", masks.len() as u32);
    p.u("MASK_OFF", mask_off as u32);
    p.u("SRGB_OFF", srgb_off as u32);
    p.u("CURVE_OFF", curve_off as u32);
    p.b("CURVES", fp.curves.is_some());
    p.f("REFINE_SAT", fp.refine_sat);
    p.fs("CURVE_M", fp.curve_in.as_flattened());
    p.fs("CURVE_MI", fp.curve_out.as_flattened());
    p.fs("CURVE_Y", &fp.curve_luma);
    p.f("GAIN", fp.gain);
    p.f("EV", fp.ev);
    p.f("AIR", fp.air);
    p.f("AIR_PRE", fp.air_pre);
    p.f("HL", fp.hl);
    p.f("SH", fp.sh);
    p.f("CLAR", fp.clar);
    p.f("TEX", fp.tex);
    p.f("DEHAZE", fp.dehaze);
    if let Some(sh) = &fp.sharpen {
        p.f("SH_AMT", sh.amount);
        p.f("SH_LOCAL", sh.local);
        p.f("SH_MASK", sh.mask);
        p.f("SH_PXSRC", sh.px_per_src);
        p.fs("SH_KNOTS", &lightcraft_pipeline::finish::SHARPEN_AMT);
        p.fs("SH_K", &sh.k);
        p.fs("SH_L", &sh.limit);
    }
    p.u("SHARP_OFF", present.sharpen_off as u32);
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
    p.fs("MIX_DESAT", &ops.desat);
    p.fs("MIX_LUM", &ops.lum);
    p.fs("MIX_C", &lightcraft_pipeline::colorops::MIX_CENTERS);
    p.u("NPC", ops.points.len().min(8) as u32);
    let pc: Vec<f32> = ops.points.iter().take(8).flat_map(|k| k.words()).collect();
    p.fs("PC", &pc);
    p.b("BW", ops.bw.is_some());
    p.fs("BW_MIX", &ops.bw.unwrap_or([0.0; 8]));
    if let Some(g) = &ops.grading {
        p.b("GRADING", true);
        p.fs("GRADE_D", g.d.as_flattened());
        p.fs("GRADE_MU", &g.mu);
        p.fs("GRADE_SG", &g.sg);
    }
    p.fs("BANDH", lightcraft_pipeline::colorops::band_hues());

    if let Some(v) = &fp.vig {
        p.b("VIG", true);
        p.f("VIG_AMOUNT", v.amount);
        p.f("VIG_STRENGTH", v.strength);
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
    if fp.tone.per_channel() {
        let (to, from) = lightcraft_pipeline::tone::prophoto_matrices();
        p.b("TONE_RGB", true);
        p.fs("TONE_TO", to.as_flattened());
        p.fs("TONE_FROM", from.as_flattened());
    }
    if let Some(off) = contrast_off {
        p.b("TONE_CON", true);
        p.u("TONE_CON_OFF", off);
    }
    if let Some((prof, inv, ctx)) = hs_in {
        p.b("TONE_HS_IN", true);
        p.u("TONE_PROF_OFF", prof);
        p.u("TONE_PROFINV_OFF", inv);
        p.u("TONE_CTX_OFF", ctx);
    }
    if let Some((pre, blacks, whites, post)) = stages {
        p.b("TONE_STAGED", true);
        p.u("TONE_PRE_OFF", pre);
        p.u("TONE_POST_OFF", post);
        if let Some((off, k)) = blacks {
            p.b("TONE_BK", true);
            p.u("TONE_BK_OFF", off);
            p.f("TONE_KB", k);
        }
        if let Some((off, k)) = whites {
            p.b("TONE_WH", true);
            p.u("TONE_WH_OFF", off);
            p.f("TONE_KW", k);
        }
    }
    if let Some(lr) = &fp.lr_hs {
        p.b("LR_HS", true);
        p.f("LR_HL", lr.hl.1);
        p.f("LR_HL_ALPHA", lr.hl.0.alpha);
        p.fs("LR_HL_TAB", &lr.hl.0.tab);
        p.f("LR_SH", lr.sh.1);
        p.f("LR_SH_ALPHA", lr.sh.0.alpha);
        p.fs("LR_SH_TAB", &lr.sh.0.tab);
    }
    p.fs("OUT_M", fp.to_out.as_flattened());
    p.fs("OUT_Y", &fp.out_luma);
    let (trc, gamma) = fp.out_trc.code();
    p.u("OUT_TRC", trc);
    p.f("OUT_GAMMA", gamma);
    if let Some((amt, cell, rough, seed)) = fp.grain {
        p.b("GRAIN", true);
        p.f("GRAIN_AMT", amt);
        p.f("GRAIN_SC", fp.px_per_long as f32 / cell);
        p.f("GRAIN_ROUGH", rough);
        p.u("GRAIN_SEED", seed);
        let long = fp.ow.max(fp.oh);
        let [a, b, c, d, e, f] = fp.out_to_norm.0;
        let (kx, ky) = (fp.ow / long, fp.oh / long);
        p.fs("GRAIN_AFF", &[(a * kx) as f32, (b * ky) as f32, (c * kx) as f32, (d * ky) as f32, (e * kx) as f32, (f * ky) as f32]);
    }
    (p.0, aux)
}
