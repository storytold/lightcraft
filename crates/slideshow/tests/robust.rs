//! Never-crash (P6.2): slideshow settings, templates and saved slideshows come from files, users
//! and agents; music files from anywhere. Damaged ones are errors or clamped values, never a
//! panic, a hang or an unbounded allocation.

use std::path::Path;

use dac_slideshow::compose::geometry;
use dac_slideshow::music;
use dac_slideshow::settings::{SavedSlideshow, Settings, Template, builtin_templates};
use dac_slideshow::timeline::{Plan, pan_zoom};

#[test]
fn hostile_settings_never_panic() {
    let seeds: Vec<String> = builtin_templates().iter().map(|t| serde_json::to_string(t).unwrap()).collect();
    let refs: Vec<&str> = seeds.iter().map(String::as_str).collect();
    dac_fuzzkit::run_json("slideshow.settings", &refs, 1500, |s| {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else { return };
        let _ = serde_json::from_value::<SavedSlideshow>(v.clone());
        let t: Template = serde_json::from_value(v.clone()).unwrap_or_default();
        let st = Settings::from_json(&serde_json::to_value(&t.settings).unwrap_or_default()).unwrap_or_default();
        let _ = Settings::default().patched(&v);
        let _ = st.patched(v.get("settings").unwrap_or(&v));
        for (w, h, pw, ph) in [(1920, 1080, 6000, 4000), (1, 1, 1, 1), (0, 0, 0, 0), (100, 4000, 1, 60000)] {
            let _ = geometry(w, h, pw, ph, &st);
        }
        for n in [0, 1, 7] {
            for music in [None, Some(0.0), Some(f64::NAN), Some(1e12), Some(3.5)] {
                let plan = Plan::new(n, &st, 42, music);
                let d = plan.duration();
                for t in [0.0, d / 2.0, d, d * 3.0, -1.0, f64::NAN, f64::INFINITY] {
                    let _ = plan.frame_at(t);
                }
            }
        }
        let _ = pan_zoom(usize::MAX, f32::NAN, 1e30);
    });
}

fn wav(channels: u16, bits: u16, n: usize) -> Vec<u8> {
    let spec = hound::WavSpec { channels, sample_rate: 8000, bits_per_sample: bits, sample_format: hound::SampleFormat::Int };
    let mut c = std::io::Cursor::new(Vec::new());
    let mut w = hound::WavWriter::new(&mut c, spec).unwrap();
    for i in 0..n * usize::from(channels) {
        w.write_sample(((i as f32 * 0.05).sin() * 1000.0) as i32).unwrap();
    }
    w.finalize().unwrap();
    c.into_inner()
}

/// `fLaC` + a STREAMINFO block (last) for 8 kHz stereo 16-bit, no frames.
fn flac() -> Vec<u8> {
    let mut v = b"fLaC".to_vec();
    v.extend([0x80, 0, 0, 34]);
    v.extend([0x10, 0x00, 0x10, 0x00]); // block sizes 4096
    v.extend([0, 0, 0, 0, 0, 0]); // frame sizes unknown
    // 20 bits rate (8000), 3 bits channels-1 (1), 5 bits bps-1 (15), 36 bits total samples (0)
    let packed: u64 = (8000u64 << 44) | (1 << 41) | (15 << 36);
    v.extend(packed.to_be_bytes());
    v.extend([0u8; 16]); // MD5
    v
}

/// One Ogg page holding a Vorbis identification header (the stream then lacks its other headers).
fn ogg() -> Vec<u8> {
    let mut id = b"\x01vorbis".to_vec();
    id.extend(0u32.to_le_bytes());
    id.push(2);
    id.extend(8000u32.to_le_bytes());
    id.extend([0u8; 12]);
    id.push(0xB8);
    id.push(1);
    let mut page = b"OggS\0\x02".to_vec();
    page.extend([0u8; 8]); // granule
    page.extend(1u32.to_le_bytes()); // serial
    page.extend(0u32.to_le_bytes()); // sequence
    page.extend([0u8; 4]); // crc, filled below
    page.push(1);
    page.push(id.len() as u8);
    page.extend(&id);
    let crc = ogg_crc(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

fn ogg_crc(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04c1_1db7 } else { crc << 1 };
        }
    }
    crc
}

#[test]
fn damaged_music_files_never_panic() {
    let dir = std::env::temp_dir().join(format!("dac-slideshow-robust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cases: [(&str, Vec<u8>); 4] = [("a.wav", wav(2, 16, 800)), ("b.wav", wav(1, 24, 300)), ("c.flac", flac()), ("d.ogg", ogg())];
    for (name, seed) in &cases {
        let p = dir.join(name);
        // every seed is at least a valid header for its decoder
        std::fs::write(&p, seed).unwrap();
        let _ = music::decode(&p);
        dac_fuzzkit::run(&format!("slideshow.{name}"), &[seed], 600, |b| {
            std::fs::write(&p, b).unwrap();
            check(&p);
        });
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn check(p: &Path) {
    if let Ok(mut a) = music::decode(p) {
        assert!(a.seconds().is_finite());
        music::mix(&mut a.samples, 0.5, -0.3);
    }
    let _ = music::duration(p);
    let _ = music::total_duration(&[p.display().to_string()]);
}
