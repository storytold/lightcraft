//! Per-container hostile-input table (completion plan 2.2). Every compressed container with a decoder of its
//! own (CR3 lossless and C-RAW, Sony ARW6, GoPro VC-5, Fujifilm compressed RAF in Bayer and X-Trans, Nikon High
//! Efficiency) gets a structurally valid seed from the crate's own procedural test encoders, then truncations of
//! it (every prefix of a seed up to 4 KiB, a spread of up to 4096 prefix lengths plus the last 64 bytes one by
//! one beyond that) and seeded mutations of it (`lcc_fuzzkit`) through the entry point a photo takes. Each row has two budgets:
//!
//! - **time:** the whole row (seeds, truncations, mutations) finishes inside a generous deadline, so a mutation
//!   that sends a decoder into a near-endless loop fails the test instead of hanging CI. A deadline, not a
//!   stopwatch: the numbers sit an order of magnitude above what a debug build needs.
//! - **allocation:** a successful decode of a mutated input never produces more samples than the row allows. The
//!   seeds are tiny, so an image above the cap means a header claim was trusted without the data to back it (the
//!   60000 × 60000-from-ten-bytes class). Peak-heap accounting of failing paths stays in the dhat binaries
//!   (`tests/sr2_heap.rs`), which need a process of their own.
//!
//! An input that panics is printed by `lcc_fuzzkit` (harness name, iteration, bytes); fold it into the row's
//! `regressions` so every run replays it. `LCC_FUZZ_ITERS` raises the mutation count for long runs
//! (`LCC_FUZZ_ITERS=N cargo test -p lcc-raw --lib container_robust`); the deadline scales with it.

use crate::{RawImage, Result};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// One container: how to build its seed, how to decode a candidate, and its budgets.
struct Row {
    name: &'static str,
    seed: fn() -> Vec<u8>,
    /// Decodes a candidate the way the container's entry point does; returns the decoded sample count.
    decode: fn(&[u8]) -> Result<usize>,
    /// Mutations per run without `LCC_FUZZ_ITERS`.
    mutations: usize,
    /// Most samples a decode of any mutation may produce.
    max_samples: usize,
    /// Deadline for the row at the default mutation count (scaled up with `LCC_FUZZ_ITERS`).
    deadline: Duration,
    /// Inputs that once failed (from a fuzz run), replayed as extra seeds.
    regressions: &'static [&'static [u8]],
}

/// Full-file containers: probe, the header-only description, the embedded preview, the decode and (for small
/// images) a develop.
fn whole_file(bytes: &[u8]) -> Result<usize> {
    let _ = crate::probe(bytes);
    let _ = crate::probe_info(bytes);
    let _ = crate::embedded_preview(bytes);
    let img: RawImage = crate::decode(bytes)?;
    let n = img.data.len();
    if n <= 1 << 16 {
        let _ = img.develop(crate::Method::Ahd);
    }
    Ok(n)
}

fn arw6_seed() -> Vec<u8> {
    use super::arw6_testenc::{Qis, arw6_file, one_tile_quantised};
    let (tile, _) = one_tile_quantised(64, 48, 0, Qis::REAL);
    arw6_file(&[(0, 0, 64, 48, tile)], 64, 48)
}

/// A GoPro GPR: a DNG whose one tile is a VC-5 stream (compression 9).
fn vc5_seed() -> Vec<u8> {
    use lcc_tiff::tags as t;
    use lcc_tiff::{IfdBuilder, ImageData, TiffWriter, Value};
    let mut ifd = IfdBuilder::new();
    ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
    ifd.set(t::IMAGE_WIDTH, Value::Long(vec![64]));
    ifd.set(t::IMAGE_LENGTH, Value::Long(vec![48]));
    ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16]));
    ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
    ifd.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::CFA]));
    ifd.set(t::COMPRESSION, Value::Short(vec![9]));
    ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
    ifd.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
    ifd.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
    ifd.set(t::WHITE_LEVEL, Value::Short(vec![16383]));
    let tile = crate::vc5::synthetic_stream(64, 48, [4000, 8192, 8192, 8192], 7);
    ifd.set_image(ImageData::Tiles { tile_width: 64, tile_height: 48, tiles: vec![tile] });
    TiffWriter::default().write(&[ifd]).unwrap()
}

/// A compressed Bayer RAF (14-bit, lossy quantiser groups) in its full container.
fn raf_seed() -> Vec<u8> {
    let stream = super::rafc::tests::compressed(14, false, Some(&[2, 0, 5]));
    super::raf::tests::raf(780, 18, 14, stream, None, &[])
}

/// X-Trans compressed stripes, decoded at the stream level (the container is the RAF row's).
fn xtrans_seed() -> Vec<u8> {
    super::rafc::tests::compressed(12, true, None)
}

fn xtrans_decode(bytes: &[u8]) -> Result<usize> {
    super::rafc::decode(bytes, 780, 18, 12, true, crate::Mode::Header)?;
    Ok(super::rafc::decode(bytes, 780, 18, 12, true, crate::Mode::Full)?.len())
}

/// Nikon High Efficiency: the JPEG XS-derived codestream a NEF carries, at the stream level.
fn nefhe_seed() -> Vec<u8> {
    super::nefhe::tests::fixture(70, 12)
}

fn nefhe_decode(bytes: &[u8]) -> Result<usize> {
    Ok(super::nefhe::decode(bytes, 70, 12, 14)?.0.len())
}

const ROWS: &[Row] = &[
    Row {
        name: "raw.cr3.lossless",
        seed: cr3::lossless_file,
        decode: whole_file,
        mutations: 600,
        max_samples: 1 << 20,
        deadline: Duration::from_secs(120),
        regressions: &[],
    },
    Row {
        name: "raw.cr3.craw",
        seed: cr3::craw_file,
        decode: whole_file,
        mutations: 600,
        max_samples: 1 << 20,
        deadline: Duration::from_secs(120),
        regressions: &[],
    },
    Row {
        name: "raw.arw6",
        seed: arw6_seed,
        decode: whole_file,
        mutations: 300,
        max_samples: 1 << 20,
        deadline: Duration::from_secs(180),
        regressions: &[],
    },
    Row {
        name: "raw.vc5",
        seed: vc5_seed,
        decode: whole_file,
        mutations: 300,
        max_samples: 1 << 20,
        deadline: Duration::from_secs(180),
        regressions: &[],
    },
    Row {
        name: "raw.raf.compressed",
        seed: raf_seed,
        decode: whole_file,
        mutations: 200,
        max_samples: 1 << 20,
        deadline: Duration::from_secs(180),
        regressions: &[],
    },
    Row {
        name: "raw.raf.xtrans",
        seed: xtrans_seed,
        decode: xtrans_decode,
        mutations: 200,
        max_samples: 1 << 20,
        deadline: Duration::from_secs(180),
        regressions: &[],
    },
    Row {
        name: "raw.nef.he",
        seed: nefhe_seed,
        decode: nefhe_decode,
        mutations: 300,
        max_samples: 1 << 20,
        deadline: Duration::from_secs(180),
        regressions: &[],
    },
];

/// Prefix lengths tried for a seed of `len` bytes: all of them up to 4 KiB, beyond that up to 4096 evenly
/// spread lengths plus each of the last 64.
fn truncation_lengths(len: usize) -> Vec<usize> {
    let step = len.div_ceil(4096).max(1);
    let mut v: Vec<usize> = (0..len).step_by(step).collect();
    v.extend(len.saturating_sub(64)..len);
    v.sort_unstable();
    v.dedup();
    v
}

#[test]
fn truncation_lengths_reach_the_tail_of_long_seeds() {
    let v = truncation_lengths(100_000);
    assert!(v.len() <= 4096 + 64 + 1);
    assert!(v.contains(&0) && v.contains(&99_999) && v.contains(&99_937));
    assert_eq!(truncation_lengths(10), (0..10).collect::<Vec<_>>());
}

/// Every seed decodes as built, so the mutations below start from live decode paths, not an early error.
#[test]
fn every_container_seed_decodes() {
    for row in ROWS {
        let n = (row.decode)(&(row.seed)()).unwrap_or_else(|e| panic!("{}: seed does not decode: {e}", row.name));
        assert!(n > 0 && n <= row.max_samples, "{}: seed decodes to {n} samples", row.name);
    }
}

/// Truncations of every seed (including its tail) and every mutation of every container decodes or errors within its budgets; never a panic.
#[test]
fn mutated_containers_stay_within_their_budgets() {
    let iters = lcc_fuzzkit::iterations(0);
    let start = Instant::now();
    let rows: Vec<_> = ROWS
        .iter()
        .map(|row| {
            let (tx, rx) = mpsc::channel();
            let (name, seed, decode, max, mutations, regressions) = (row.name, row.seed, row.decode, row.max_samples, row.mutations, row.regressions);
            std::thread::spawn(move || {
                let outcome = std::panic::catch_unwind(|| {
                    let seed = seed();
                    let mut seeds: Vec<&[u8]> = vec![&seed];
                    seeds.extend_from_slice(regressions);
                    let mut over = None;
                    let mut check = |bytes: &[u8]| {
                        if let Ok(n) = decode(bytes)
                            && n > max
                            && over.is_none()
                        {
                            over = Some((n, bytes.len()));
                        }
                    };
                    // `lcc_fuzzkit::run` only truncates the shortest seed and stops at 4 KiB; cover the whole
                    // seed here, tail included, so cuts near the end of a long container are tried too.
                    for n in truncation_lengths(seed.len()) {
                        check(seed.get(..n).unwrap_or_default());
                    }
                    lcc_fuzzkit::run(name, &seeds, mutations, check);
                    over
                });
                let _ = tx.send(outcome.map_err(|_| ()));
            });
            let scale = u32::try_from(iters / row.mutations + 1).unwrap_or(u32::MAX);
            (row.name, rx, row.deadline.saturating_mul(scale))
        })
        .collect();
    let mut failures = Vec::new();
    for (name, rx, deadline) in rows {
        match rx.recv_timeout(deadline.saturating_sub(start.elapsed())) {
            Ok(Ok(None)) => {}
            Ok(Ok(Some((n, len)))) => failures.push(format!("{name}: a {len}-byte candidate decoded to {n} samples, over the allocation budget")),
            Ok(Err(())) => failures.push(format!("{name}: a candidate panicked (input printed above)")),
            Err(_) => failures.push(format!("{name}: did not finish within {deadline:?} (time budget)")),
        }
    }
    assert!(
        failures.is_empty(),
        "{}",
        failures.join(
            "
"
        )
    );
}

/// Both CR3 fixtures decode as built, and the header-only description (used at import) agrees with the decode.
#[test]
fn cr3_fixtures_decode_and_describe() {
    let lossless = crate::decode(&cr3::lossless_file()).unwrap();
    assert_eq!((lossless.width, lossless.height, lossless.bits), (4, 4, 14));
    assert_eq!(lossless.data.get(0), 8193.0);
    let craw = crate::decode(&cr3::craw_file()).unwrap();
    assert_eq!((craw.width, craw.height, craw.bits), (16, 16, 14));
    assert_eq!(craw.data.get(0), 8192.0);
    assert_eq!(crate::probe_info(&cr3::craw_file()).unwrap(), craw.info());
}

/// CR3 containers (Canon CRX): a single CRAW track carrying a CMP1 descriptor plus raw samples.
mod cr3 {
    pub(crate) fn box_bytes(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn track(entry: &[u8], offset: u64, size: u32) -> Vec<u8> {
        let mut stsd = vec![0u8; 4];
        stsd.extend(1u32.to_be_bytes());
        stsd.extend_from_slice(entry);
        let mut stsz = vec![0u8; 4];
        stsz.extend(size.to_be_bytes());
        stsz.extend(1u32.to_be_bytes());
        let mut co64 = vec![0u8; 4];
        co64.extend(1u32.to_be_bytes());
        co64.extend(offset.to_be_bytes());
        let tables = [box_bytes(b"stsd", &stsd), box_bytes(b"stsz", &stsz), box_bytes(b"co64", &co64)].concat();
        box_bytes(b"trak", &box_bytes(b"mdia", &box_bytes(b"minf", &box_bytes(b"stbl", &tables))))
    }

    /// `CMP1` coding descriptor: 52 bytes, four planes, 14 bits, RGGB, `levels` wavelet levels.
    pub(crate) fn cmp1(width: u32, height: u32, tile_width: u32, tile_height: u32, levels: u8, header_size: u32) -> Vec<u8> {
        let mut coding = vec![0u8; 52];
        coding[0..2].copy_from_slice(&0xff00u16.to_be_bytes());
        coding[2..4].copy_from_slice(&48u16.to_be_bytes());
        coding[4..6].copy_from_slice(&0x100u16.to_be_bytes());
        coding[8..12].copy_from_slice(&width.to_be_bytes());
        coding[12..16].copy_from_slice(&height.to_be_bytes());
        coding[16..20].copy_from_slice(&tile_width.to_be_bytes());
        coding[20..24].copy_from_slice(&tile_height.to_be_bytes());
        coding[24] = 14;
        coding[25] = 0x40;
        coding[26] = levels;
        coding[28..32].copy_from_slice(&header_size.to_be_bytes());
        coding
    }

    pub(crate) fn file(coding: &[u8], width: u16, height: u16, sample: &[u8]) -> Vec<u8> {
        let mut craw = vec![0u8; 82];
        craw[24..26].copy_from_slice(&width.to_be_bytes());
        craw[26..28].copy_from_slice(&height.to_be_bytes());
        craw.extend(box_bytes(b"CMP1", coding));
        let entry = box_bytes(b"CRAW", &craw);
        let ftyp = box_bytes(b"ftyp", b"crx \0\0\0\x01crx isom");
        let track_len = track(&entry, 0, sample.len() as u32).len();
        let offset = (ftyp.len() + 8 + track_len + 7) & !7;
        let mut out = ftyp;
        out.extend(box_bytes(b"moov", &track(&entry, offset as u64, sample.len() as u32)));
        out.resize(offset, 0);
        out.extend_from_slice(sample);
        out
    }

    fn marker(out: &mut Vec<u8>, code: u16, size: u32, flags: u32) {
        out.extend(code.to_be_bytes());
        out.extend(8u16.to_be_bytes());
        out.extend(size.to_be_bytes());
        out.extend(flags.to_be_bytes());
    }

    /// A 4×4 lossless CRX stream: four 2×2 planes, each sample value 1 (sensor level 8193).
    pub(crate) fn lossless_sample() -> Vec<u8> {
        let mut sample = Vec::new();
        marker(&mut sample, 0xff01, 4, 0);
        for p in 0..4u32 {
            marker(&mut sample, 0xff02, 1, (p << 28) | 0x0800_0000);
            marker(&mut sample, 0xff03, 1, 4 << 19);
        }
        sample.extend([0x1e; 4]);
        sample
    }

    pub(crate) fn lossless_file() -> Vec<u8> {
        file(&cmp1(4, 4, 4, 4, 0, 108), 4, 4, &lossless_sample())
    }

    /// A 16×16 three-level C-RAW CRX stream of flat zero subbands (sensor level 8192).
    pub(crate) fn craw_sample() -> Vec<u8> {
        let mut sample = Vec::new();
        marker(&mut sample, 0xff01, 52, 0);
        for plane in 0..4u32 {
            marker(&mut sample, 0xff02, 13, (plane << 28) | 0x0800_0000);
            for band in 0..10u32 {
                marker(&mut sample, 0xff03, if band >= 7 { 2 } else { 1 }, (band << 28) | (4 << 19));
            }
        }
        for _ in 0..4 {
            sample.extend([0x80; 4]);
            sample.extend([0xf0; 3]);
            for _ in 0..3 {
                sample.extend([0xff, 0xf8]);
            }
        }
        sample
    }

    pub(crate) fn craw_file() -> Vec<u8> {
        file(&cmp1(16, 16, 16, 16, 3, 540), 16, 16, &craw_sample())
    }
}
