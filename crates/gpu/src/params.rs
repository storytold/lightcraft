//! Parameter blocks: the per-pixel stage's [`FinishParams`] packed into the 32-bit words the
//! `finish` kernel reads (`F_*` constants, generated from [`FIELDS`]), plus its auxiliary tables.

use lcc_color::spline::Lut1;
use lcc_develop::VignetteStyle;
use lcc_pipeline::colorops::{HSL_WORDS, MIXER_WORDS, POINT_WORDS};
use lcc_pipeline::finish::{CURVE_LUT_N, FinishParams, MASK_TERMS, srgb_lut};

use crate::ctx::{FailKind, fail};

/// Entries per tone-curve table (`lcc_pipeline::finish` builds them at this size).
pub const CURVE_N: u32 = CURVE_LUT_N as u32;

/// Words per mask in the local colour records: the number of Point Color samples, whether the mask
/// has a Color Mixer, the mixer's [`MIXER_WORDS`], then up to 8 samples of [`POINT_WORDS`].
pub const LOCAL_POINT_WORDS: usize = 2 + MIXER_WORDS + 8 * POINT_WORDS;
/// Offset of the first Point Color sample in a local colour record.
pub const LOCAL_POINT_SAMPLES: usize = 2 + MIXER_WORDS;

/// Field name and number of 32-bit words.
const FIELDS: &[(&str, usize)] = &[
    ("W", 1),
    ("H", 1),
    ("NMASK", 1),
    ("MASK_OFF", 1),
    ("SRGB_OFF", 1),
    ("CURVE_OFF", 1),
    ("CURVES", 1),
    // the masks' local Curves: a slot per mask (its table set, or −1), then the table sets
    ("LCURVE_OFF", 1),
    // the masks' local Point Color: one record of `LOCAL_POINT_WORDS` per mask in `aux`
    ("LPOINTS", 1),
    ("LPOINT_OFF", 1),
    ("LCURVES", 1),
    ("GAIN", 1),
    ("EV", 1),
    ("AIR", 1),
    ("AIR_PRE", 1),
    ("HL", 1),
    ("SH", 1),
    ("CLAR", 1),
    ("TEX", 1),
    ("DEHAZE", 1),
    ("DEHAZE_V3", 1),
    ("SHARPEN_V4", 1),
    // process V5: over-range colours keep hue and saturation (`lcc_pipeline::finish::hue_clip`)
    ("HUE_CLIP", 1),
    // process V8: 8-bit output is ordered-dithered (`lcc_pipeline::finish::bayer_dither_offset`)
    ("DITHER", 1),
    ("SHARPEN", 1),
    ("SHARPEN_MASK", 1),
    ("SHARPEN_LIMIT", 1),
    ("SHARPEN_FADE", 1),
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
    // the colourist primaries (`PrimariesK`): the Color wheels then the Log wheels (rgb + master
    // each), the Log wheels' range boundaries
    ("PRIMARIES", 1),
    ("PRIM_LIFT", 4),
    ("PRIM_GAMMA", 4),
    ("PRIM_GAIN", 4),
    ("PRIM_OFFSET", 4),
    ("PRIM_SHADOW", 4),
    ("PRIM_MIDTONE", 4),
    ("PRIM_HIGHLIGHT", 4),
    ("PRIM_LOG_OFFSET", 4),
    ("PRIM_LOW", 1),
    ("PRIM_HIGH", 1),
    // the HSL curves (`HslK`): the hue, saturation and luminance LUTs over the hue circle
    ("HSL", 1),
    ("HSL_LUT", HSL_WORDS),
    // Levels (`LevelsK`) on the encoded values before the curves: the master stage, then red, green, blue,
    // each 8 words (active, black, 1/range, out black, span, slope at 0, slope at 1, has a midtone table)
    ("LEVELS", 1),
    ("LEVELS_M", 8),
    ("LEVELS_R", 8),
    ("LEVELS_G", 8),
    ("LEVELS_B", 8),
    // where each stage's midtone table starts in `aux` (`MIDTONE_SAMPLES` floats; unused without a midtone)
    ("LEVELS_OFF_M", 1),
    ("LEVELS_OFF_R", 1),
    ("LEVELS_OFF_G", 1),
    ("LEVELS_OFF_B", 1),
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
    // 0 = value noise (processes V1–V6: GRAIN_AMT, GRAIN_SC, GRAIN_ROUGH), 1 = particles (V7)
    ("GRAIN_MODEL", 1),
    ("GRAIN_AMT", 1),
    ("GRAIN_AMP", 1),
    ("GRAIN_INV_R", 1),
    ("GRAIN_S", 1),
    ("GRAIN_INV_S", 1),
    ("GRAIN_SPREAD", 1),
    ("GRAIN_ASPECT", 1),
    ("GRAIN_LUMP", 1),
    ("GRAIN_CLUMP_E", 1),
    ("GRAIN_CLUMP_D", 1),
    ("GRAIN_AREA", 1),
    ("GRAIN_SUPPORT", 1),
    ("GRAIN_SUB_REACH", 1),
    ("GRAIN_CELL", 1),
    ("GRAIN_KAPPA", 1),
    ("GRAIN_K_HH", 1),
    ("GRAIN_K_AH", 1),
    ("GRAIN_AREA_REF", 1),
    ("GRAIN_KAPPA_REF", 1),
    ("GRAIN_K_HH_REF", 1),
    ("GRAIN_K_AH_REF", 1),
    ("GRAIN_HALO", 1),
    ("GRAIN_HALO_IN", 1),
    ("GRAIN_HALO_OUT", 1),
    ("GRAIN_HALO_MEAN", 1),
    ("GRAIN_TILT", 1),
    ("GRAIN_WF", 1),
    ("GRAIN_WN", 1),
    ("GRAIN_NOISE_E", 1),
    ("GRAIN_NOISE_N", 1),
    ("GRAIN_COLOR", 1),
    ("GRAIN_GAMMA", 1),
    ("GRAIN_PPL", 1),
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
    // the HDR render's peak (SDR white = 1) and whether the kernel writes linear floats (`DEEP`)
    ("PEAK", 1),
    ("DEEP", 1),
    ("DEEP_OFF", 1),
    // soft proofing (matrix proofs; `ProofParams`): working → proof RGB, its luma, proof → output, warnings
    ("PROOF", 1),
    ("PROOF_M", 9),
    ("PROOF_Y", 3),
    ("PROOF_OUT", 9),
    ("PROOF_DEST_WARN", 1),
    ("PROOF_DISP_WARN", 1),
    // a LUT profile (`finish`'s `lut`): its table in `aux` at LUT_OFF (size³ rgb, red fastest), amount, domain
    ("LUT", 1),
    ("LUT_OFF", 1),
    ("LUT_N", 1),
    ("LUT_AMT", 1),
    ("LUT_MIN", 3),
    ("LUT_MAX", 3),
    // per-channel RGB Contrast/Blacks LUT (`FinishParams::rgb_contrast`), `RGB_CONTRAST_N` floats in `aux`
    ("RGB_CONTRAST", 1),
    ("RGB_CONTRAST_OFF", 1),
    ("RGB_CONTRAST_ZERO", 1),
    // first row of a band dispatch (the kernel runs over rows Y0.., see `render`)
    ("Y0", 1),
    // the mask planes' stride (0: the whole image's, `w × h`) and the band's first pixel in them
    // (planes too big for one buffer reach the kernel band by band, see `render`)
    ("MASK_N", 1),
    ("MASK_I0", 1),
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
/// mask terms | local-curve slots | local-curve LUTs) for `fp` with `masks` (their local adjustments' terms) and
/// `curves` (their local Curves, [`lcc_pipeline::finish::local_curve_luts`], in the same order).
///
/// A curve whose length is not [`CURVE_N`] fails the render ([`FailKind::Fatal`]) and returns an
/// empty parameter block with a truncated aux table, which the caller discards (CPU fallback).
pub fn finish_block(fp: &FinishParams, masks: &[[f32; MASK_TERMS]], curves: &[Option<[Lut1; 3]>], present: &Present) -> (Vec<u32>, Vec<f32>) {
    let mut aux: Vec<f32> = fp.tone.lut().to_vec();
    aux.extend_from_slice(fp.tone.chroma_lut());
    let srgb_off = aux.len();
    aux.extend_from_slice(&srgb_lut()[..]);
    let curve_off = aux.len();
    if let Some(c) = &fp.curves {
        for l in c {
            if l.v.len() != CURVE_N as usize {
                // a wrong-length curve would shift every later aux offset and make the kernel read
                // the wrong data. `fail()` records the failure on this thread so the render falls
                // back to the CPU; the empty block returned here is never bound to a pipeline.
                fail(FailKind::Fatal, format!("curve table has {} entries, expected {}", l.v.len(), CURVE_N));
                return (Vec::new(), aux);
            }
            aux.extend_from_slice(&l.v);
        }
    }
    let mask_off = aux.len();
    for m in masks {
        aux.extend_from_slice(m);
    }
    // one slot per mask (the kernel reads `NMASK` of them), then each local Curve's three tables
    let lcurve_off = aux.len();
    let mut next = 0.0f32;
    for k in 0..masks.len() {
        match curves.get(k) {
            Some(Some(_)) => {
                aux.push(next);
                next += 1.0;
            }
            _ => aux.push(-1.0),
        }
    }
    for l in curves.iter().take(masks.len()).flatten().flatten() {
        aux.extend_from_slice(&l.v);
    }
    // the LUT profile's table, when it is well formed (size ≥ 2, size³ entries)
    let lut_off = aux.len();
    let lut = fp.lut.as_ref().filter(|(l, _)| l.size >= 2 && l.size.checked_pow(3) == Some(l.data.len()));
    if let Some((l, _)) = lut {
        aux.extend(l.data.iter().flatten());
    }
    let rgb_contrast_off = aux.len();
    if let Some(rc) = &fp.rgb_contrast {
        if rc.lut().len() != lcc_pipeline::tone::RGB_CONTRAST_N {
            fail(FailKind::Fatal, format!("rgb_contrast table has {} entries, expected {}", rc.lut().len(), lcc_pipeline::tone::RGB_CONTRAST_N));
            return (Vec::new(), aux);
        }
        aux.extend_from_slice(rc.lut());
    }
    // local Color Mixer and Point Color: one record per mask (each list used only when it lines up
    // with the masks, as on the CPU)
    let points_ok = fp.local_points.len() == masks.len();
    let mixers_ok = fp.local_mixers.len() == masks.len();
    let lpoints = !masks.is_empty() && ((points_ok && !fp.local_points.is_empty()) || (mixers_ok && !fp.local_mixers.is_empty()));
    let lpoint_off = aux.len();
    if lpoints {
        for m in 0..masks.len() {
            let mut rec = [0.0f32; LOCAL_POINT_WORDS];
            let points: &[lcc_pipeline::colorops::PointK] = if points_ok { fp.local_points.get(m).map_or(&[], Vec::as_slice) } else { &[] };
            rec[0] = points.len().min(8) as f32;
            if let Some(mk) = fp.local_mixers.get(m).copied().flatten().filter(|_| mixers_ok) {
                rec[1] = 1.0;
                for (slot, v) in rec.iter_mut().skip(2).zip(mk.words()) {
                    *slot = v;
                }
            }
            for (slot, v) in rec.iter_mut().skip(LOCAL_POINT_SAMPLES).zip(points.iter().take(8).flat_map(|k| k.words())) {
                *slot = v;
            }
            aux.extend_from_slice(&rec);
        }
    }

    // Levels: the midtone tables of the stages that have one (`MIDTONE_SAMPLES` floats each), after everything else
    let mut levels_off = [0u32; 4];
    if let Some(lv) = &fp.levels {
        for (slot, st) in [&lv.master, &lv.channels[0], &lv.channels[1], &lv.channels[2]].into_iter().enumerate() {
            if let Some(mid) = st.as_ref().and_then(|k| k.mid.as_ref()) {
                if let Some(o) = levels_off.get_mut(slot) {
                    *o = aux.len() as u32;
                }
                aux.extend_from_slice(&mid.y);
            }
        }
    }

    let mut p = Block::new();
    p.u("W", fp.w as u32);
    p.u("H", fp.h as u32);
    p.u("NMASK", masks.len() as u32);
    p.u("MASK_OFF", mask_off as u32);
    p.u("SRGB_OFF", srgb_off as u32);
    p.u("CURVE_OFF", curve_off as u32);
    p.b("CURVES", fp.curves.is_some());
    p.u("LCURVE_OFF", lcurve_off as u32);
    p.b("LCURVES", next > 0.0);
    p.b("LPOINTS", lpoints);
    p.u("LPOINT_OFF", lpoint_off as u32);
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
    p.b("DEHAZE_V3", fp.dehaze_v3);
    p.b("SHARPEN_V4", fp.sharpen_v4);
    p.b("HUE_CLIP", fp.hue_clip);
    p.b("DITHER", fp.dither);
    p.f("SHARPEN", fp.sharpen);
    p.f("SHARPEN_MASK", fp.sharpen_mask);
    p.f("SHARPEN_LIMIT", fp.sharpen_limit);
    p.f("SHARPEN_FADE", fp.sharpen_fade);
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
    if let Some(pr) = &ops.primaries {
        p.b("PRIMARIES", true);
        for (name, w) in [
            ("PRIM_LIFT", &pr.lift),
            ("PRIM_GAMMA", &pr.gamma),
            ("PRIM_GAIN", &pr.gain),
            ("PRIM_OFFSET", &pr.offset),
            ("PRIM_SHADOW", &pr.shadow),
            ("PRIM_MIDTONE", &pr.midtone),
            ("PRIM_HIGHLIGHT", &pr.highlight),
            ("PRIM_LOG_OFFSET", &pr.log_offset),
        ] {
            p.fs(name, &[w.rgb[0], w.rgb[1], w.rgb[2], w.master]);
        }
        p.f("PRIM_LOW", pr.low);
        p.f("PRIM_HIGH", pr.high);
    }
    if let Some(lv) = &fp.levels {
        p.b("LEVELS", true);
        let stages = [
            ("LEVELS_M", "LEVELS_OFF_M", &lv.master),
            ("LEVELS_R", "LEVELS_OFF_R", &lv.channels[0]),
            ("LEVELS_G", "LEVELS_OFF_G", &lv.channels[1]),
            ("LEVELS_B", "LEVELS_OFF_B", &lv.channels[2]),
        ];
        for (slot, (name, off, st)) in stages.into_iter().enumerate() {
            if let Some(k) = st {
                let (s0, s1, has) = k.mid.as_ref().map_or((1.0, 1.0, 0.0), |m| (m.s0, m.s1, 1.0));
                p.fs(name, &[1.0, k.black, k.inv_range, k.out_black, k.span, s0, s1, has]);
                p.u(off, levels_off.get(slot).copied().unwrap_or(0));
            }
        }
    }
    if let Some(hsl) = &ops.hsl {
        p.b("HSL", true);
        p.fs("HSL_LUT", &hsl.words());
    }
    p.f("SKIN", ops.skin);
    p.fs("BANDH", lcc_pipeline::colorops::band_hues());

    // also the dither's place in the whole output
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
    p.f("PEAK", if fp.peak.is_finite() { fp.peak.max(1.0) } else { 1.0 });
    p.f("OUT_GAMMA", gamma);
    if let Some(pp) = fp.proof.as_ref().filter(|pp| pp.table.is_none()) {
        p.b("PROOF", true);
        p.fs("PROOF_M", pp.to_proof.as_flattened());
        p.fs("PROOF_Y", &pp.luma);
        p.fs("PROOF_OUT", pp.proof_to_out.as_flattened());
        p.b("PROOF_DEST_WARN", pp.dest_warning);
        p.b("PROOF_DISP_WARN", pp.display_warning);
    }
    if let Some((l, amount)) = lut {
        p.b("LUT", true);
        p.u("LUT_OFF", lut_off as u32);
        p.u("LUT_N", l.size as u32);
        p.f("LUT_AMT", *amount);
        p.fs("LUT_MIN", &l.domain_min);
        p.fs("LUT_MAX", &l.domain_max);
    }
    if let Some(rc) = &fp.rgb_contrast {
        p.b("RGB_CONTRAST", true);
        p.u("RGB_CONTRAST_OFF", rgb_contrast_off as u32);
        p.f("RGB_CONTRAST_ZERO", rc.zero());
    }
    let grain_on = fp.grain.is_some() || fp.particles.is_some();
    if let Some((amt, cell, rough, seed)) = fp.grain {
        p.f("GRAIN_AMT", amt);
        p.f("GRAIN_SC", fp.px_per_long as f32 / cell);
        p.f("GRAIN_ROUGH", rough);
        p.u("GRAIN_SEED", seed);
    }
    if let Some(g) = &fp.particles {
        p.u("GRAIN_MODEL", 1);
        p.f("GRAIN_AMP", g.amp);
        p.f("GRAIN_INV_R", g.inv_r);
        p.f("GRAIN_S", g.s);
        p.f("GRAIN_INV_S", g.inv_s);
        p.f("GRAIN_SPREAD", g.spread);
        p.f("GRAIN_ASPECT", g.aspect);
        p.f("GRAIN_LUMP", g.lump);
        p.f("GRAIN_CLUMP_E", g.clump_extra);
        p.f("GRAIN_CLUMP_D", g.clump_d);
        p.f("GRAIN_AREA", g.area);
        p.f("GRAIN_SUPPORT", g.support);
        p.f("GRAIN_SUB_REACH", g.sub_reach);
        p.f("GRAIN_CELL", g.cell);
        p.f("GRAIN_KAPPA", g.kappa);
        p.f("GRAIN_K_HH", g.k_hh);
        p.f("GRAIN_K_AH", g.k_ah);
        p.f("GRAIN_AREA_REF", g.area_ref);
        p.f("GRAIN_KAPPA_REF", g.kappa_ref);
        p.f("GRAIN_K_HH_REF", g.k_hh_ref);
        p.f("GRAIN_K_AH_REF", g.k_ah_ref);
        p.f("GRAIN_HALO", g.halo);
        p.f("GRAIN_HALO_IN", g.halo_in);
        p.f("GRAIN_HALO_OUT", g.halo_out);
        p.f("GRAIN_HALO_MEAN", g.halo_mean);
        p.f("GRAIN_TILT", g.tilt);
        p.f("GRAIN_WF", g.w_field);
        p.f("GRAIN_WN", g.w_noise);
        p.f("GRAIN_NOISE_E", g.noise_e);
        p.f("GRAIN_NOISE_N", g.noise_n);
        p.f("GRAIN_COLOR", g.color);
        p.f("GRAIN_GAMMA", g.gamma);
        p.f("GRAIN_PPL", g.px_per_long);
        p.u("GRAIN_SEED", g.seed);
    }
    if grain_on {
        p.b("GRAIN", true);
        let long = fp.ow.max(fp.oh);
        let [a, b, c, d, e, f] = fp.out_to_norm.0;
        let (kx, ky) = (fp.ow / long, fp.oh / long);
        p.fs("GRAIN_AFF", &[(a * kx) as f32, (b * ky) as f32, (c * kx) as f32, (d * ky) as f32, (e * kx) as f32, (f * ky) as f32]);
    }
    (p.0, aux)
}

// the kernel's `MIDTONE_N` (finish.wgsl) is the number of samples of a midtone table
const _: () = assert!(lcc_develop::levels_curve::MIDTONE_SAMPLES == 257);
