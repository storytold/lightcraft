//! Creative profiles saved as XMP "looks" (`crs:PresetType="Look"` with a `crs:RGBTable`): the look
//! is a 3D colour table stored in the packet as `crs:Table_<id>`, encoded as base-85 text over a
//! zlib stream. Decoded here into a [`Lut3d`] on LightCraft's LUT-profile domain (sRGB-encoded
//! output colour) so it imports and renders like a `.cube` profile.
//!
//! Layout of the decoded stream (all little-endian), as observed in the files: `u32` length of the
//! inflated data, then zlib; inflated: `u32` kind (1 = RGB table), `u32` version, `u32` dimensions
//! (3), `u32` divisions `n`, then `n³` entries of three `u16` (red slowest, blue fastest), each the
//! difference from the identity table, then optional `u32` primaries, gamma and gamut codes.
//! Personal-fork feature; nothing here ships a look.

use lightcraft_color::transfer::{linear_to_srgb, srgb_to_linear};
use lightcraft_color::{ADOBE_RGB, DISPLAY_P3, Mat3, PROPHOTO, REC2020, RgbSpace, SRGB};
use lightcraft_pipeline::lut::Lut3d;

/// The 85 characters of the table encoding, by value.
const ALPHABET: &[u8; 85] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-:+=^!/*?`'|()[]{}@%$#";
/// Largest encoded table read (a 64³ table is ~2 MB of text).
const MAX_TEXT: usize = 16 << 20;
/// Largest inflated table (64³ entries of 6 bytes plus the header and trailer).
const MAX_INFLATED: usize = 64 * 64 * 64 * 6 + 1024;
/// Grid size of the LUT written for LightCraft.
const OUT_SIZE: usize = 33;

/// An XMP look read from a file.
#[derive(Clone, Debug)]
pub struct XmpLook {
    pub name: String,
    pub group: Option<String>,
    pub lut: Lut3d,
}

/// The decoded table.
#[derive(Clone, Debug, PartialEq)]
pub struct RgbTable {
    pub divisions: usize,
    /// `divisions³` outputs (0..=65535), red index slowest, blue fastest.
    pub data: Vec<[u16; 3]>,
    pub primaries: u32,
    pub gamma: u32,
    pub gamut: u32,
}

/// Whether `text` looks like an XMP look with a colour table (cheap check, no parsing).
pub fn is_look(text: &str) -> bool {
    text.contains("crs:RGBTable") && text.contains("crs:Table_")
}

/// Whether the file at `path` is an XMP look with a colour table (reads at most a few MB).
pub fn is_look_file(path: &std::path::Path) -> bool {
    if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("xmp")) {
        return false;
    }
    let small = std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_TEXT as u64);
    small && std::fs::read(path).is_ok_and(|b| is_look(&String::from_utf8_lossy(&b)))
}

/// `name="value"` of an attribute in `text` (attribute form, XML entities decoded), else
/// `<name>value</name>` (element form).
fn raw_attr(text: &str, name: &str) -> Option<String> {
    let attr = format!("{name}=\"");
    if let Some(at) = text.find(&attr) {
        let rest = text.get(at + attr.len()..)?;
        let end = rest.find('"')?;
        return Some(unescape(rest.get(..end)?));
    }
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let at = text.find(&open)?;
    let rest = text.get(at + open.len()..)?;
    let end = rest.find(&close)?;
    Some(unescape(rest.get(..end)?.trim()))
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// The first value of an `rdf:Alt` / plain property, from the parsed packet.
fn prop(text: &str, name: &str) -> Option<String> {
    let parsed = lightcraft_meta::parse_xmp(text).ok().and_then(|d| d.properties.get(name).and_then(|v| v.first().cloned()));
    parsed.or_else(|| raw_attr(text, name)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Read an XMP look. `fallback_name` names it when the packet doesn't.
pub fn parse(text: &str, fallback_name: &str) -> Result<XmpLook, String> {
    if text.len() > MAX_TEXT {
        return Err("file too large for a look".into());
    }
    let key = raw_attr(text, "crs:RGBTable")
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .ok_or("not a look with a colour table (no crs:RGBTable)")?;
    if !key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("bad table id `{key}`"));
    }
    let encoded = raw_attr(text, &format!("crs:Table_{key}")).ok_or_else(|| format!("the colour table crs:Table_{key} is missing"))?;
    let table = decode_table(&encoded)?;
    let name = prop(text, "crs:Name").unwrap_or_else(|| fallback_name.to_string());
    let group = prop(text, "crs:Group");
    let mut lut = to_lut(&table, OUT_SIZE)?;
    lut.title = Some(name.clone());
    Ok(XmpLook { name, group, lut })
}

/// Base-85 text → bytes: five characters (least significant first) per four little-endian bytes;
/// a short last group of `k` characters gives `k − 1` bytes. Other characters (line breaks) are
/// skipped.
fn base85(text: &str) -> Result<Vec<u8>, String> {
    let mut value = [0u8; 256];
    let mut valid = [false; 256];
    for (i, c) in ALPHABET.iter().enumerate() {
        value[*c as usize] = i as u8;
        valid[*c as usize] = true;
    }
    let digits: Vec<u8> = text.bytes().filter(|b| valid[*b as usize]).map(|b| value[b as usize]).collect();
    let mut out = Vec::with_capacity(digits.len() / 5 * 4 + 4);
    for group in digits.chunks(5) {
        let mut v: u64 = 0;
        let mut m: u64 = 1;
        for d in group {
            v += u64::from(*d) * m;
            m *= 85;
        }
        let bytes = (v as u32).to_le_bytes();
        let keep = if group.len() == 5 { 4 } else { group.len().saturating_sub(1) };
        out.extend_from_slice(bytes.get(..keep).unwrap_or(&[]));
    }
    Ok(out)
}

/// Decode a `crs:Table_…` value.
pub fn decode_table(encoded: &str) -> Result<RgbTable, String> {
    let bytes = base85(encoded)?;
    let head = bytes.get(..4).ok_or("colour table too short")?;
    let declared = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if declared > MAX_INFLATED {
        return Err(format!("colour table too large ({declared} bytes)"));
    }
    let inflated = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(bytes.get(4..).unwrap_or(&[]), MAX_INFLATED)
        .map_err(|e| format!("damaged colour table ({e:?})"))?;
    let u32_at = |i: usize| inflated.get(i * 4..i * 4 + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let (kind, version, dims, n) = (u32_at(0), u32_at(1), u32_at(2), u32_at(3));
    if kind != Some(1) {
        return Err(format!("not an RGB table (kind {kind:?})"));
    }
    if !version.is_some_and(|v| (1..=3).contains(&v)) {
        return Err(format!("unsupported colour table version {version:?}"));
    }
    if dims != Some(3) {
        return Err(format!("only 3D colour tables are supported (dimensions {dims:?})"));
    }
    let n = n.map(|n| n as usize).filter(|n| (2..=64).contains(n)).ok_or("colour table size out of range")?;
    let count = n * n * n;
    let start = 16;
    let end = start + count * 6;
    let body = inflated.get(start..end).ok_or("colour table cut short")?;
    let mut data = Vec::with_capacity(count);
    for (k, e) in body.chunks_exact(6).enumerate() {
        let (r, g, b) = (k / (n * n), (k / n) % n, k % n);
        let d = |o: usize| u16::from_le_bytes([e[o], e[o + 1]]);
        data.push([d(0).wrapping_add(identity(r, n)), d(2).wrapping_add(identity(g, n)), d(4).wrapping_add(identity(b, n))]);
    }
    let trail = |i: usize| inflated.get(end + i * 4..end + i * 4 + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    Ok(RgbTable { divisions: n, data, primaries: trail(0).unwrap_or(0), gamma: trail(1).unwrap_or(1), gamut: trail(2).unwrap_or(0) })
}

/// The identity table's value at grid index `i` of `n`: `(i · 65535 + n/2) / (n − 1)`, the rounding
/// the files use (with `(n − 1)/2` instead, index 15 of a 32-point table comes out one low, and an
/// output of 0 there wraps around to 65535: a Cobalt black-and-white look then had coloured specks).
fn identity(i: usize, n: usize) -> u16 {
    if i == 0 || n < 2 {
        return 0;
    }
    ((i as u64 * 65535 + n as u64 / 2) / (n as u64 - 1)).min(65535) as u16
}

/// The colour space a table's codes name.
fn space_of(primaries: u32) -> Result<RgbSpace, String> {
    Ok(match primaries {
        0 => SRGB,
        1 => ADOBE_RGB,
        2 => PROPHOTO,
        3 => DISPLAY_P3,
        4 => REC2020,
        p => return Err(format!("unknown colour table primaries {p}")),
    })
}

/// Linear → encoded and back for a table's gamma code.
fn gamma_of(gamma: u32) -> Result<(fn(f32) -> f32, fn(f32) -> f32), String> {
    fn lin(v: f32) -> f32 {
        v
    }
    fn g18(v: f32) -> f32 {
        v.max(0.0).powf(1.0 / 1.8)
    }
    fn g18_inv(v: f32) -> f32 {
        v.max(0.0).powf(1.8)
    }
    fn g22(v: f32) -> f32 {
        v.max(0.0).powf(1.0 / 2.2)
    }
    fn g22_inv(v: f32) -> f32 {
        v.max(0.0).powf(2.2)
    }
    const A: f32 = 1.099_296_8;
    const B: f32 = 0.018_053_97;
    fn r2020(v: f32) -> f32 {
        let v = v.max(0.0);
        if v < B { 4.5 * v } else { A * v.powf(0.45) - (A - 1.0) }
    }
    fn r2020_inv(v: f32) -> f32 {
        let v = v.max(0.0);
        if v < 4.5 * B { v / 4.5 } else { ((v + A - 1.0) / A).powf(1.0 / 0.45) }
    }
    type F = fn(f32) -> f32;
    let pair: (F, F) = match gamma {
        0 => (lin as F, lin as F),
        1 => (linear_to_srgb as F, srgb_to_linear as F),
        2 => (g18 as F, g18_inv as F),
        3 => (g22 as F, g22_inv as F),
        4 => (r2020 as F, r2020_inv as F),
        g => return Err(format!("unknown colour table gamma {g}")),
    };
    Ok(pair)
}

fn mul(m: &Mat3, v: [f32; 3]) -> [f32; 3] {
    let r = m.apply([f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]);
    [r[0] as f32, r[1] as f32, r[2] as f32]
}

impl RgbTable {
    /// Trilinear lookup of encoded `c` (0..1 per channel) → encoded output (0..1).
    fn lookup(&self, c: [f32; 3]) -> [f32; 3] {
        let n = self.divisions;
        let s = (n - 1) as f32;
        let mut i = [0usize; 3];
        let mut f = [0f32; 3];
        for k in 0..3 {
            let x = c[k].clamp(0.0, 1.0) * s;
            let ik = (x.floor() as usize).min(n - 2);
            i[k] = ik;
            f[k] = x - ik as f32;
        }
        let at =
            |r: usize, g: usize, b: usize| -> [f32; 3] { self.data.get((r * n + g) * n + b).map_or([0.0; 3], |v| v.map(|x| f32::from(x) / 65535.0)) };
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
        let (r, g, b) = (i[0], i[1], i[2]);
        let c00 = lerp(at(r, g, b), at(r, g, b + 1), f[2]);
        let c01 = lerp(at(r, g + 1, b), at(r, g + 1, b + 1), f[2]);
        let c10 = lerp(at(r + 1, g, b), at(r + 1, g, b + 1), f[2]);
        let c11 = lerp(at(r + 1, g + 1, b), at(r + 1, g + 1, b + 1), f[2]);
        lerp(lerp(c00, c01, f[1]), lerp(c10, c11, f[1]), f[0])
    }
}

/// Resample `table` onto LightCraft's LUT-profile domain: sRGB-encoded colour in, out.
pub fn to_lut(table: &RgbTable, size: usize) -> Result<Lut3d, String> {
    let space = space_of(table.primaries)?;
    let (encode, decode) = gamma_of(table.gamma)?;
    let to_table = SRGB.to_space(&space);
    let from_table = space.to_space(&SRGB);
    let s = (size - 1) as f32;
    let mut data = Vec::with_capacity(size * size * size);
    // `.cube` order: red fastest
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let lin = [r, g, b].map(|i| srgb_to_linear(i as f32 / s));
                let t = mul(&to_table, lin).map(|v| encode(v.clamp(0.0, 1.0)));
                let out = mul(&from_table, table.lookup(t).map(decode));
                data.push(out.map(|v| linear_to_srgb(v.clamp(0.0, 1.0))));
            }
        }
    }
    Ok(Lut3d { size, data, domain_min: [0.0; 3], domain_max: [1.0; 3], title: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode bytes the way the format stores them (the inverse of [`base85`]).
    fn encode85(bytes: &[u8]) -> String {
        let mut s = String::new();
        for chunk in bytes.chunks(4) {
            let mut b = [0u8; 4];
            b[..chunk.len()].copy_from_slice(chunk);
            let mut v = u32::from_le_bytes(b) as u64;
            let digits = if chunk.len() == 4 { 5 } else { chunk.len() + 1 };
            for _ in 0..digits {
                s.push(ALPHABET[(v % 85) as usize] as char);
                v /= 85;
            }
        }
        s
    }

    /// A table packet: `f` maps an encoded input to an encoded output.
    fn table_text(n: usize, primaries: u32, gamma: u32, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
        let mut raw = Vec::new();
        for v in [1u32, 1, 3, n as u32] {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        let id = |i: usize| identity(i, n);
        for r in 0..n {
            for g in 0..n {
                for b in 0..n {
                    let o = f([r, g, b].map(|i| i as f32 / (n - 1) as f32)).map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16);
                    for (k, i) in [r, g, b].into_iter().enumerate() {
                        raw.extend_from_slice(&o[k].wrapping_sub(id(i)).to_le_bytes());
                    }
                }
            }
        }
        for v in [primaries, gamma, 0] {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        let mut packed = (raw.len() as u32).to_le_bytes().to_vec();
        packed.extend(miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6));
        let enc = encode85(&packed).replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;");
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:PresetType="Look"
 crs:RGBTable="ABCDEF0123" crs:Table_ABCDEF0123="{enc}">
<crs:Name><rdf:Alt><rdf:li xml:lang="x-default">Test Look</rdf:li></rdf:Alt></crs:Name>
<crs:Group><rdf:Alt><rdf:li xml:lang="x-default">Test Group</rdf:li></rdf:Alt></crs:Group>
</rdf:Description></rdf:RDF></x:xmpmeta>"#
        )
    }

    #[test]
    fn identity_table_gives_an_identity_lut() {
        for (p, g) in [(0, 1), (2, 2), (4, 4), (1, 3), (3, 0)] {
            let look = parse(&table_text(9, p, g, |c| c), "x").unwrap();
            assert_eq!((look.name.as_str(), look.group.as_deref()), ("Test Look", Some("Test Group")));
            for c in [[0.2f32, 0.5, 0.8], [0.9, 0.1, 0.4], [0.5, 0.5, 0.5]] {
                let o = look.lut.apply(c);
                assert!((0..3).all(|k| (o[k] - c[k]).abs() < 0.02), "{p}/{g}: {c:?} → {o:?}");
            }
        }
    }

    #[test]
    fn identity_matches_the_files_rounding() {
        // the identity of a 32-point table, as read from a real look (output 0 along the blue axis)
        let seen = [0u16, 2114, 4228, 6342, 8456, 10570, 12684, 14798, 16912, 19026, 21140, 23254, 25368, 27482, 29596, 31711];
        for (i, v) in seen.iter().enumerate() {
            assert_eq!(identity(i, 32), *v, "index {i}");
        }
        assert_eq!((identity(31, 32), identity(1, 2), identity(0, 2)), (65535, 65535, 0));
    }

    #[test]
    fn channel_order_is_red_green_blue() {
        // red → output red only: a pure red stays red, blue goes black
        let look = parse(&table_text(5, 0, 1, |c| [c[0], 0.0, 0.0]), "x").unwrap();
        let o = look.lut.apply([1.0, 0.0, 0.0]);
        assert!(o[0] > 0.99 && o[1] < 0.01 && o[2] < 0.01, "{o:?}");
        let o = look.lut.apply([0.0, 0.0, 1.0]);
        assert!(o.iter().all(|v| *v < 0.01), "{o:?}");
    }

    #[test]
    fn damaged_tables_are_errors_not_panics() {
        let good = table_text(4, 2, 2, |c| c);
        let at = good.find("crs:Table_ABCDEF0123=\"").unwrap() + 22;
        let end = good[at..].find('"').unwrap() + at;
        for cut in [0usize, 1, 5, 9, (end - at) / 2, end - at - 6] {
            let mut bad = good.clone();
            let keep = at + cut;
            bad.replace_range(keep..end, "");
            assert!(parse(&bad, "x").is_err(), "cut {cut}");
        }
        assert!(parse("<x/>", "x").is_err());
        assert!(parse(&good.replace("crs:Table_ABCDEF0123", "crs:Table_OTHER"), "x").is_err());
        assert!(decode_table("").is_err());
        assert!(decode_table("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err());
    }
}
