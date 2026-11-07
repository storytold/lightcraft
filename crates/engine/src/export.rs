//! Export: encode a rendered image to JPEG / PNG / TIFF / WebP / AVIF / JPEG XL (or copy the original / write a
//! DNG, each with the edits in XMP) in sRGB, Display P3, Adobe RGB
//! (1998) compatible, ProPhoto RGB or Rec. 2020 with an embedded ICC profile we generate from the
//! published primaries and curves, optional output sharpening and a JPEG file-size limit. Pure (bytes in, bytes out) so the desktop
//! app, CLI, MCP and the web build share it; writing the file is the caller's job.

use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, JxlColour, JxlOptions, NamedSpace, Samples, encode, icc};
pub use lightcraft_codecs::{JxlEffort, TiffCompression};
use lightcraft_meta::{DateTime, Gps, Metadata};
pub use lightcraft_pipeline::{DeepImage, DeepSamples, OutputDepth, OutputSpace};
use lightcraft_raster::Rgba8;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::text::{TEXT_FONT as WATERMARK_FONT, cell_glyphs as watermark_cell_glyphs, faces as watermark_fonts};

mod batch;
mod encoding;
mod options;
mod params;
mod place;
mod prepare;
mod watermark;

#[cfg(test)]
use batch::run_batch_with;
pub use batch::{export_parallelism, run_batch};
pub use encoding::{
    encode_deep, encode_gain_map_jpeg, encode_image, encode_rendered, encode_with_metadata, export_metadata, named_space, output_sharpen,
    output_sharpen_deep, removes_location, srgb8_in,
};
pub use options::{
    Conflict, DestinationMode, ExportFormat, ExportPreset, HdrCurve, MAX_ENLARGED_PIXELS, MetadataPolicy, Resize, ResizeMode, SharpenAmount,
    SharpenFor, builtin_presets,
};
pub use params::{
    OPTION_PARAMS, TARGET_PARAMS, WATERMARK_IMAGE_WIDTH_RANGE, WATERMARK_INSET_RANGE, WATERMARK_PARAMS, WATERMARK_SIZE_RANGE,
    apple_photos_unsupported,
};
#[cfg(test)]
use place::Placer;
pub use place::{write_file, write_file_durable, write_file_new};
pub use prepare::{
    AfterExport, Destination, DngCompression, Exported, PreparedExport, export_batch, export_photo, output_size, prepare_batch, prepare_export,
};
#[cfg(test)]
use prepare::{render_weight, too_big_for_memory};
#[cfg(test)]
use watermark::watermark_coverage;
pub(crate) use watermark::watermark_logo;
pub use watermark::{Anchor, Watermark, draw_watermark, draw_watermark_deep, draw_watermark_in, draw_watermark_preview, read_graphic};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExportOptions {
    pub format: ExportFormat,
    /// 1–100 (JPEG, AVIF, lossy JPEG XL).
    pub quality: u8,
    /// Output size (`None` = full size).
    pub resize: Option<Resize>,
    /// Print resolution written into the file (pixels per inch).
    pub ppi: u16,
    /// JPEG only: largest quality whose file fits in this many KB.
    pub limit_kb: Option<u32>,
    pub sharpen: SharpenFor,
    pub sharpen_amount: SharpenAmount,
    /// File name template with the batch-rename tokens ([`crate::rename::expand_tokens`]): `{name}`
    /// (original stem), `{seq}` (zero-padded to 3, counting from `start_number`; `{seq:N}` for N
    /// digits), `{date}` (capture date, YYYYMMDD; `{date:%Y-%m-%d}`…), `{num}`, `{folder}`,
    /// `{camera}`, `{lens}`, `{iso}`, `{rating}`, `{title}`, `{creator}`, `{ext}`; the output
    /// extension is appended.
    pub naming: String,
    /// First `{seq}` value.
    pub start_number: u32,
    /// A specific folder for the whole batch, or each photo's own folder.
    pub destination_mode: DestinationMode,
    /// Created inside the destination folder (empty = none).
    pub subfolder: String,
    /// When a file of the same name exists in the destination folder.
    pub conflict: Conflict,
    /// TIFF compression.
    pub tiff_compression: TiffCompression,
    /// DNG export: how the raw data is stored.
    pub dng_compression: DngCompression,
    pub metadata: MetadataPolicy,
    /// Strip GPS / location even when the policy would include it.
    pub remove_location: bool,
    /// Text watermark (none when absent or the text is empty).
    pub watermark: Option<Watermark>,
    /// Output colour space (AVIF is always sRGB: its muxer cannot embed a profile; lossy JPEG XL
    /// writes Adobe RGB and ProPhoto as Rec. 2020, see [`ExportOptions::effective_space`]).
    pub color_space: OutputSpace,
    /// Bits per channel: 8, 16 (PNG, TIFF, JPEG XL), 32 (TIFF: float, linear) or 10 (AVIF); `None` = the
    /// format's default (TIFF 16, everything else 8). See [`ExportOptions::effective_depth`].
    pub bit_depth: Option<u8>,
    /// HDR output for photos edited in HDR: JPEG as an ISO 21496-1 gain map JPEG (the SDR
    /// rendition plus a gain map, so the file looks right everywhere; `limit_kb` includes both
    /// images and their metadata), AVIF as 10-bit Rec. 2020 PQ, PNG as 16-bit Rec. 2020 PQ (`cICP`; viewers
    /// without cICP support show it as sRGB, washed out), JPEG XL as 16-bit Rec. 2020 PQ, 32-bit float
    /// TIFF with the highlights above SDR white kept. Other formats, and photos without an HDR
    /// edit, are SDR.
    pub hdr: bool,
    /// JPEG XL: mathematically lossless instead of lossy (`quality` is then unused).
    pub jxl_lossless: bool,
    /// JPEG XL: encoder effort (time spent for a smaller file).
    pub jxl_effort: JxlEffort,
    /// JPEG XL: progressive file (a coarse image first, refined as it loads).
    pub jxl_progressive: bool,
    /// After the export, add the files written to Apple Photos (macOS only; see [`AfterExport`]).
    pub add_to_photos: bool,
    /// The Photos album they go into, made when missing (empty = none).
    pub photos_album: String,
    /// The curve of an HDR AVIF: PQ (default) or HLG.
    pub hdr_transfer: HdrCurve,
}

#[cfg(test)]
mod tests;
