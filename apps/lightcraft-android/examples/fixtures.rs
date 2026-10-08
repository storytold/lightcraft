//! Generate original synthetic fixtures, including a 24 MP sensor mosaic.
use lightcraft_raw::{BlackLevel, Cfa, ColorData, DngCompression, DngWriteOptions, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).ok_or("usage: fixtures OUTPUT_DIRECTORY")?;
    std::fs::create_dir_all(&dir)?;
    let (w, h) = (6000, 4000);
    let cfa = Cfa::bayer("RGGB").ok_or("invalid literal CFA")?;
    let data = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let color = cfa.color_at(x, y);
            let base = match color {
                0 => 1300,
                1 => 1800,
                _ => 900,
            };
            (base + x * 5000 / w + y * 3000 / h) as u16
        })
        .collect();
    let raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(cfa),
        bits: 14,
        black: BlackLevel::uniform(256.0),
        white: vec![16383.0],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::default(),
        color: ColorData::default(),
        wb_multipliers: None,
        linearized: false,
        opcodes: OpcodeLists::default(),
        metadata: Default::default(),
    };
    let bytes = lightcraft_raw::write_dng(&raw, &DngWriteOptions { compression: DngCompression::Uncompressed, ..Default::default() })?;
    std::fs::write(std::path::Path::new(&dir).join("synthetic-24mp.dng"), bytes)?;
    let mut img = lightcraft_raster::Rgba8::new(1200, 800);
    for (i, p) in img.data.iter_mut().enumerate() {
        *p = [(i % 1200 * 255 / 1200) as u8, (i / 1200 * 255 / 800) as u8, 120, 255];
    }
    let bytes = lightcraft_engine::export::encode_image(&img, &Default::default())?;
    std::fs::write(std::path::Path::new(&dir).join("synthetic-color.jpg"), bytes)?;
    Ok(())
}
