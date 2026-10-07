//! TEMPORARY analysis helper (not committed): decode every tile of a subsampled lossless ARW into full planes.
//! Output: per plane `u32 w, u32 h` + `u16` samples, planes concatenated.
use lightcraft_tiff::Tiff;
use lightcraft_tiff::image::chunk_bytes;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&a[0]).unwrap();
    let tiff = Tiff::parse(&bytes).unwrap();
    let raw = tiff.all_ifds().into_iter().max_by_key(|i| i.u64(256).unwrap_or(0) * i.u64(257).unwrap_or(0)).unwrap();
    let info = raw.image().unwrap();
    let (w, h) = (info.width as usize, info.height as usize);
    let chunks = info.chunks(bytes.len() as u64);
    let mut planes: Vec<(usize, usize, Vec<u16>)> = Vec::new();
    for c in &chunks {
        let src = chunk_bytes(&bytes, c).unwrap();
        let f = lightcraft_raw::ljpeg::decode_planar(src, 1 << 24).unwrap();
        if planes.is_empty() {
            for p in &f.planes {
                let (sx, sy) = (f.width / p.width, f.height / p.height);
                planes.push((w / sx, h / sy, vec![0; (w / sx) * (h / sy)]));
            }
        }
        for (k, p) in f.planes.iter().enumerate() {
            let (sx, sy) = (f.width / p.width, f.height / p.height);
            let (pw, ph, ref mut d) = planes[k];
            let (x0, y0) = (c.x as usize / sx, c.y as usize / sy);
            for y in 0..p.height {
                for x in 0..p.width {
                    if x0 + x < pw && y0 + y < ph {
                        d[(y0 + y) * pw + x0 + x] = p.data[y * p.width + x];
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for (pw, ph, d) in &planes {
        out.extend_from_slice(&(*pw as u32).to_le_bytes());
        out.extend_from_slice(&(*ph as u32).to_le_bytes());
        for v in d {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    std::fs::write(&a[1], out).unwrap();
}
