//! FlashRaw decoding backend for LightCraft (`crates/raw`).
//!
//! Provides high-performance, SIMD-accelerated raw image decoding
//! across camera RAW formats using the `flashraw` crate.

use crate::{
    BlackLevel, Cfa, ColorData, LensMetadata, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result,
};
use flashraw::{Backend, DecodeOptions, FlashRaw, OutputFormat};
use lightcraft_color::Mat3;
use lightcraft_geom::Orientation;

/// Decode a RAW image using the FlashRaw engine into a [`RawImage`].
pub fn decode(bytes: &[u8], format: RawFormat, mode: Mode) -> Result<RawImage> {
    let raw = FlashRaw::open(bytes).map_err(|e| RawError::Corrupt(format!("flashraw open failed: {e}")))?;
    let meta = raw.metadata();

    let width = meta.width as usize;
    let height = meta.height as usize;
    if width == 0 || height == 0 {
        return Err(RawError::Corrupt("flashraw reported zero dimensions".into()));
    }

    // Active image area (including borders if reported)
    let active_area = if let Some(a) = meta.active_area {
        Rect::new(a.x as usize, a.y as usize, a.width as usize, a.height as usize)
    } else {
        Rect::new(0, 0, width, height)
    };

    // Default crop relative to the active area
    let crop = if let Some(c) = meta.default_crop {
        Rect::new(c.x as usize, c.y as usize, c.width as usize, c.height as usize)
    } else {
        Rect::new(0, 0, active_area.width, active_area.height)
    };

    // CFA Pattern (Bayer / X-Trans)
    let cfa = meta.cfa.as_ref().and_then(|p| {
        if p.width == 0 || p.height == 0 {
            return None;
        }
        let pattern: Vec<u8> = (0..p.height)
            .flat_map(|y| {
                (0..p.width).map(move |x| match p.color_at(x, y) {
                    flashraw::CfaColor::Red => 0,
                    flashraw::CfaColor::Green => 1,
                    flashraw::CfaColor::Blue => 2,
                    _ => 1,
                })
            })
            .collect();
        Some(Cfa {
            width: p.width as usize,
            height: p.height as usize,
            pattern,
        })
    });

    // Black level
    let black = if let Some(ref bl) = meta.black_level {
        let values: Vec<f32> = bl.values().iter().map(|&v| v as f32).collect();
        BlackLevel {
            repeat_rows: bl.repeat_rows as usize,
            repeat_cols: bl.repeat_cols as usize,
            values: if values.is_empty() { vec![0.0] } else { values },
            delta_h: Vec::new(),
            delta_v: Vec::new(),
        }
    } else {
        BlackLevel::default()
    };

    // White level
    let white = vec![meta.white_level.unwrap_or(65535) as f32];

    // Color matrices & calibration
    let mut color = ColorData::default();
    if let Some(ref cm) = meta.color_matrix {
        let m = cm.matrix();
        color.color_matrix[0] = Some(Mat3(m));
    }

    let wb_multipliers = meta.white_balance.map(|wb| [wb.r as f32, wb.g as f32, wb.b as f32]);

    // Orientation
    let orientation = Orientation::from_exif(meta.orientation.to_exif());

    // Extract file metadata (EXIF/TIFF tags)
    let mut metadata = lightcraft_meta::extract(bytes);
    if metadata.width.is_none() {
        metadata.width = Some(crop.width as u32);
    }
    if metadata.height.is_none() {
        metadata.height = Some(crop.height as u32);
    }

    // Decode sample buffer
    let data = match mode {
        Mode::Header => RawData::U16(Vec::new()),
        Mode::Full => {
            let options = DecodeOptions {
                backend: Backend::Auto,
                output: OutputFormat::RawU16,
                ..Default::default()
            };
            let frame = raw.decode(options).map_err(|e| RawError::Corrupt(format!("flashraw decode failed: {e}")))?;
            let samples = frame.as_u16().ok_or_else(|| RawError::Corrupt("missing u16 samples from flashraw".into()))?;
            RawData::U16(samples.to_vec())
        }
    };

    let bits = meta.bits.unwrap_or(14) as u32;

    Ok(RawImage {
        format,
        width,
        height,
        cpp: 1,
        data,
        cfa,
        bits,
        black,
        white,
        active_area,
        crop,
        orientation,
        color,
        wb_multipliers,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata,
        lens_metadata: LensMetadata::default(),
    })
}
