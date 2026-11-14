use crate::convert::{from_craft, to_craft};
use crate::{Metadata, Orientation};
use craft_tiff::{ParseOptions, Tiff, TiffError, tags as t};

pub fn strip_exif_header(b: &[u8]) -> &[u8] {
    craft_metadata::strip_exif_header(b)
}

pub fn read_exif(bytes: &[u8]) -> Metadata {
    from_craft(craft_metadata::read_exif(bytes))
}

pub fn try_read_exif(bytes: &[u8]) -> Result<Metadata, TiffError> {
    craft_metadata::try_read_exif(bytes).map(from_craft)
}

pub fn try_read_exif_with(bytes: &[u8], opts: &ParseOptions) -> Result<Metadata, TiffError> {
    craft_metadata::try_read_exif_with(bytes, opts).map(from_craft)
}

pub fn from_tiff(tiff: &Tiff) -> Metadata {
    from_craft(craft_metadata::from_tiff(tiff))
}

pub fn write_exif(meta: &Metadata) -> Vec<u8> {
    craft_metadata::write_exif(&to_craft(meta))
}

pub(crate) fn ifd0_orientation(bytes: &[u8]) -> Option<Orientation> {
    let tiff = Tiff::parse(strip_exif_header(bytes)).ok()?;
    tiff.ifds.first()?.u16(t::ORIENTATION).map(Orientation::from_exif)
}
