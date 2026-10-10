//! Slideshow music: decode WAV (hound), FLAC (claxon) and Ogg Vorbis (lewton), all pure Rust and
//! permissive, to interleaved stereo `f32`; measure track lengths (for "fit to music"); apply volume
//! and balance; and, with the `audio-out` feature, play through the sound card with cpal.
//!
//! MP3 and AAC are not decoded yet: the pure-Rust decoder that covers them (symphonia) is MPL-2.0,
//! which the licence policy rejects.

use std::path::Path;

/// Longest track decoded (in seconds), so a hostile or huge file can't exhaust memory.
pub const MAX_SECONDS: f64 = 30.0 * 60.0;

/// Decoded audio: interleaved stereo samples in −1..1.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Audio {
    pub rate: u32,
    /// L, R, L, R …
    pub samples: Vec<f32>,
}

impl Audio {
    pub fn seconds(&self) -> f64 {
        if self.rate == 0 { 0.0 } else { self.samples.len() as f64 / 2.0 / f64::from(self.rate) }
    }
}

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase()
}

/// The formats [`decode`] reads.
pub const EXTENSIONS: &[&str] = &["wav", "flac", "ogg", "oga"];

fn cap(rate: u32, channels: usize) -> usize {
    (f64::from(rate) * MAX_SECONDS) as usize * channels.max(1)
}

/// Push one frame of `ch` channel samples as stereo.
fn push_frame(out: &mut Vec<f32>, frame: &[f32]) {
    match frame {
        [] => {}
        [m] => {
            out.push(*m);
            out.push(*m);
        }
        [l, r, ..] => {
            out.push(*l);
            out.push(*r);
        }
    }
}

fn decode_wav(path: &Path) -> Result<Audio, String> {
    let mut r = hound::WavReader::open(path).map_err(|e| format!("WAV: {e}"))?;
    let spec = r.spec();
    let ch = usize::from(spec.channels).max(1);
    if spec.sample_rate == 0 {
        return Err("WAV: sample rate 0".into());
    }
    let limit = cap(spec.sample_rate, ch);
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().take(limit).collect::<Result<_, _>>().map_err(|e| format!("WAV: {e}"))?,
        hound::SampleFormat::Int => {
            let bits = u32::from(spec.bits_per_sample).clamp(1, 32);
            let scale = 1.0 / (1u64 << (bits - 1)) as f32;
            r.samples::<i32>().take(limit).map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>().map_err(|e| format!("WAV: {e}"))?
        }
    };
    let mut samples = Vec::with_capacity(raw.len() / ch * 2);
    for f in raw.chunks(ch) {
        push_frame(&mut samples, f);
    }
    Ok(Audio { rate: spec.sample_rate, samples })
}

fn decode_flac(path: &Path) -> Result<Audio, String> {
    let mut r = claxon::FlacReader::open(path).map_err(|e| format!("FLAC: {e}"))?;
    let info = r.streaminfo();
    let ch = info.channels as usize;
    if info.sample_rate == 0 || ch == 0 {
        return Err("FLAC: bad stream info".into());
    }
    let bits = info.bits_per_sample.clamp(1, 32);
    let scale = 1.0 / (1u64 << (bits - 1)) as f32;
    let limit = cap(info.sample_rate, ch);
    let mut raw = Vec::new();
    for s in r.samples() {
        if raw.len() >= limit {
            break;
        }
        raw.push(s.map_err(|e| format!("FLAC: {e}"))? as f32 * scale);
    }
    let mut samples = Vec::with_capacity(raw.len() / ch * 2);
    for f in raw.chunks(ch) {
        push_frame(&mut samples, f);
    }
    Ok(Audio { rate: info.sample_rate, samples })
}

fn decode_vorbis(path: &Path) -> Result<Audio, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("Ogg: {e}"))?;
    let mut r = lewton::inside_ogg::OggStreamReader::new(std::io::BufReader::new(f)).map_err(|e| format!("Ogg Vorbis: {e}"))?;
    let rate = r.ident_hdr.audio_sample_rate;
    let ch = usize::from(r.ident_hdr.audio_channels);
    if rate == 0 || ch == 0 {
        return Err("Ogg Vorbis: bad header".into());
    }
    let limit = cap(rate, 2);
    let mut samples = Vec::new();
    while let Some(pck) = r.read_dec_packet_itl().map_err(|e| format!("Ogg Vorbis: {e}"))? {
        for f in pck.chunks(ch) {
            let frame: Vec<f32> = f.iter().map(|v| f32::from(*v) / 32768.0).collect();
            push_frame(&mut samples, &frame);
        }
        if samples.len() >= limit {
            samples.truncate(limit);
            break;
        }
    }
    Ok(Audio { rate, samples })
}

/// Decode a music file (by extension).
pub fn decode(path: &Path) -> Result<Audio, String> {
    match ext(path).as_str() {
        "wav" | "wave" => decode_wav(path),
        "flac" => decode_flac(path),
        "ogg" | "oga" => decode_vorbis(path),
        "mp3" | "m4a" | "aac" => Err(format!("{}: MP3 and AAC music is not supported yet (use WAV, FLAC or Ogg Vorbis)", path.display())),
        other => Err(format!("{}: unsupported music format “{other}”", path.display())),
    }
}

/// A track's length in seconds without keeping the samples.
pub fn duration(path: &Path) -> Result<f64, String> {
    match ext(path).as_str() {
        "wav" | "wave" => {
            let r = hound::WavReader::open(path).map_err(|e| format!("WAV: {e}"))?;
            let rate = r.spec().sample_rate;
            if rate == 0 {
                return Err("WAV: sample rate 0".into());
            }
            Ok(f64::from(r.duration()) / f64::from(rate))
        }
        "flac" => {
            let r = claxon::FlacReader::open(path).map_err(|e| format!("FLAC: {e}"))?;
            let i = r.streaminfo();
            match (i.samples, i.sample_rate) {
                (Some(n), rate) if rate > 0 => Ok(n as f64 / f64::from(rate)),
                _ => decode(path).map(|a| a.seconds()),
            }
        }
        _ => decode(path).map(|a| a.seconds()),
    }
}

/// The total length of the tracks that decode; the problems with the others.
pub fn total_duration(tracks: &[String]) -> (f64, Vec<String>) {
    let mut total = 0.0;
    let mut problems = Vec::new();
    for t in tracks {
        match duration(Path::new(t)) {
            Ok(d) if d.is_finite() => total += d.clamp(0.0, MAX_SECONDS),
            Ok(_) => problems.push(format!("{t}: bad length")),
            Err(e) => problems.push(e),
        }
    }
    (total, problems)
}

/// Apply volume (0..1) and balance (−1 left … 1 right) to stereo samples in place.
pub fn mix(samples: &mut [f32], volume: f32, balance: f32) {
    let v = if volume.is_finite() { volume.clamp(0.0, 1.0) } else { 0.0 };
    let b = if balance.is_finite() { balance.clamp(-1.0, 1.0) } else { 0.0 };
    let (gl, gr) = (v * (1.0 - b.max(0.0)), v * (1.0 + b.min(0.0)));
    for f in samples.chunks_mut(2) {
        if let [l, r] = f {
            *l *= gl;
            *r *= gr;
        }
    }
}

/// Whether this build can play sound.
pub const CAN_PLAY: bool = cfg!(feature = "audio-out");

/// Music playing on the default output device; dropping it stops the sound.
pub struct Player {
    #[cfg(feature = "audio-out")]
    _stream: cpal::Stream,
}

impl Player {
    /// Start playing `audio` (already mixed), looping when `repeat`.
    #[cfg(feature = "audio-out")]
    pub fn play(audio: Audio, repeat: bool) -> Result<Player, String> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no audio output device")?;
        let config = cpal::StreamConfig { channels: 2, sample_rate: cpal::SampleRate(audio.rate), buffer_size: cpal::BufferSize::Default };
        let mut pos = 0usize;
        let data = audio.samples;
        let stream = device
            .build_output_stream(
                &config,
                move |out: &mut [f32], _| {
                    for s in out.iter_mut() {
                        if pos >= data.len() && repeat {
                            pos = 0;
                        }
                        *s = data.get(pos).copied().unwrap_or(0.0);
                        pos = pos.saturating_add(1);
                    }
                },
                |e| log::warn!("slideshow audio: {e}"),
                None,
            )
            .map_err(|e| format!("audio output: {e}"))?;
        stream.play().map_err(|e| format!("audio output: {e}"))?;
        Ok(Player { _stream: stream })
    }

    /// Without the `audio-out` feature: an "unsupported" error (the show plays silently).
    #[cfg(not(feature = "audio-out"))]
    pub fn play(_audio: Audio, _repeat: bool) -> Result<Player, String> {
        Err("this build plays slideshows without sound (built without the audio-out feature)".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, secs: f32, channels: u16) {
        let spec = hound::WavSpec { channels, sample_rate: 8000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..(8000.0 * secs) as usize * channels as usize {
            w.write_sample(((i as f32 * 0.05).sin() * 10000.0) as i16).unwrap();
        }
        w.finalize().unwrap();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dac-slideshow-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn wav_decodes_to_stereo_and_has_a_length() {
        let d = tmp("wav");
        let p = d.join("a.wav");
        write_wav(&p, 1.5, 1);
        let a = decode(&p).unwrap();
        assert_eq!(a.rate, 8000);
        assert!((a.seconds() - 1.5).abs() < 1e-6);
        assert!((duration(&p).unwrap() - 1.5).abs() < 1e-6);
        let (total, problems) = total_duration(&[p.display().to_string(), d.join("missing.flac").display().to_string()]);
        assert!((total - 1.5).abs() < 1e-6);
        assert_eq!(problems.len(), 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn garbage_files_are_errors() {
        let d = tmp("garbage");
        for name in ["x.wav", "x.flac", "x.ogg", "x.mp3", "x.txt"] {
            let p = d.join(name);
            std::fs::write(&p, b"RIFF\x00\x00garbage-not-audio").unwrap();
            assert!(decode(&p).is_err(), "{name}");
            assert!(duration(&p).is_err(), "{name}");
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn balance_and_volume() {
        let mut s = vec![1.0, 1.0, 1.0, 1.0];
        mix(&mut s, 0.5, 1.0);
        assert_eq!(s, vec![0.0, 0.5, 0.0, 0.5]);
        let mut s = vec![1.0, 1.0, 1.0];
        mix(&mut s, f32::NAN, f32::NAN);
        assert_eq!(s, vec![0.0, 0.0, 1.0]);
    }
}
