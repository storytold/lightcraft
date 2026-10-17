//! Clip audio for export: decode the trimmed range of the source's first audio track with
//! `lcc_video::AudioReader` (AAC, Opus, AC-3 / E-AC-3 in MP4/MOV, Matroska or MPEG-TS), fold it to
//! mono or stereo, and encode it once per clip for the muxer: AAC-LC for MP4 (resampled to 48 kHz
//! when the source rate is not an AAC rate), 16-bit little-endian PCM (`sowt`) for ProRes MOV.
//!
//! Sync: the reader's block times are seconds from the clip's first video frame, so samples are
//! placed at `(block.time - start) * rate` relative to the first exported frame; a gap becomes
//! silence and an overlap is skipped. The AAC encoder delay is hidden with an edit list
//! (`media_start` = priming samples); PCM needs none.
//!
//! Sources: own design over FilmCraft's public crate APIs; the stereo fold-down uses the usual
//! −3 dB centre/surround coefficients (ITU-R BS.775).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use filmcraft_isobmff::{PcmConfig, SampleEntry, TrackConfig};
use lcc_video::{AudioReader, MAX_RANGE_SECONDS};

use crate::native::{ClipFormat, ExportClipError};

/// AAC bitrate per channel.
const AAC_BPS_PER_CHANNEL: u32 = 96_000;
/// Output rate when the source rate cannot be carried as is.
const FALLBACK_RATE: u32 = 48_000;
/// PCM frames per written sample chunk.
const PCM_CHUNK: usize = 4096;
/// −3 dB.
const MINUS_3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Audio output codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodecOut {
    /// AAC-LC (MP4).
    Aac,
    /// Signed 16-bit little-endian PCM (QuickTime `sowt`).
    PcmS16,
}

/// One encoded audio packet: `duration` in the track timescale (the sample rate).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioPacket {
    pub data: Vec<u8>,
    pub duration: u32,
}

/// The encoded audio track of a trimmed clip, ready to mux.
#[derive(Clone, Debug)]
pub struct EncodedAudioTrack {
    pub codec: AudioCodecOut,
    pub sample_rate: u32,
    pub channels: usize,
    /// Leading samples the edit list hides (AAC priming); 0 for PCM.
    pub priming: u32,
    /// AAC AudioSpecificConfig (empty for PCM).
    pub asc: Vec<u8>,
    pub packets: Vec<AudioPacket>,
}

impl EncodedAudioTrack {
    /// The muxer track for these packets.
    pub fn track_config(&self) -> TrackConfig {
        match self.codec {
            AudioCodecOut::Aac => {
                let entry = SampleEntry::aac(self.asc.clone(), self.channels as u32, self.sample_rate);
                let mut t = TrackConfig::new(entry, self.sample_rate);
                t.media_start = Some(i64::from(self.priming));
                t
            }
            AudioCodecOut::PcmS16 => {
                let cfg = PcmConfig {
                    bits: 16,
                    float: false,
                    big_endian: false,
                    signed: true,
                    channels: self.channels as u32,
                    sample_rate: f64::from(self.sample_rate),
                };
                TrackConfig::new(SampleEntry::pcm(cfg), self.sample_rate)
            }
        }
    }
}

/// Decode `[start, end)` seconds of `src`'s audio and encode it for `format`. `Ok(None)` when the
/// clip has no audio track this build decodes (the movie is then written without audio).
pub fn encode_audio_range(
    src: &Path,
    start: f64,
    end: f64,
    format: ClipFormat,
    cancel: &AtomicBool,
) -> Result<Option<EncodedAudioTrack>, ExportClipError> {
    let Ok(mut reader) = AudioReader::open(src) else { return Ok(None) };
    let (sr, ch) = (reader.info().sample_rate, reader.info().channels);
    if !(8_000..=192_000).contains(&sr) || ch == 0 || !start.is_finite() || !end.is_finite() || end <= start {
        return Ok(None);
    }
    let out_ch = ch.min(2);
    let n = ((end - start).min(MAX_RANGE_SECONDS) * f64::from(sr)).round().max(0.0) as usize;
    let mut pcm: Vec<f32> = Vec::with_capacity(n.saturating_mul(out_ch).min(1 << 26));
    let mut got_any = false;
    for b in reader.samples_from(start) {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportClipError::Cancelled);
        }
        let b = b.map_err(|e| ExportClipError::Decode(format!("audio: {e}")))?;
        let frames = b.samples.len() / ch;
        if frames == 0 {
            continue;
        }
        got_any = true;
        let have = pcm.len() / out_ch;
        let pos = ((b.time - start) * f64::from(sr)).round();
        let pos = if pos.is_finite() { pos.clamp(-1e12, 1e12) as i64 } else { have as i64 };
        // pad a gap with silence (bounded by the range), skip what overlaps what we hold
        let gap = usize::try_from(pos.saturating_sub(have as i64)).unwrap_or(0).min(n.saturating_sub(have));
        pcm.resize(pcm.len() + gap * out_ch, 0.0);
        let skip = usize::try_from((have as i64).saturating_sub(pos)).unwrap_or(0);
        for f in b.samples.chunks_exact(ch).skip(skip) {
            if pcm.len() / out_ch >= n {
                break;
            }
            fold(f, out_ch, &mut pcm);
        }
        if pcm.len() / out_ch >= n {
            break;
        }
    }
    if !got_any {
        return Ok(None);
    }
    pcm.resize(n * out_ch, 0.0);

    let codec = match format {
        ClipFormat::H264Mp4 => AudioCodecOut::Aac,
        ClipFormat::ProResMov => AudioCodecOut::PcmS16,
    };
    let rate = match codec {
        AudioCodecOut::Aac if filmcraft_aac::sample_rate_index(sr).is_none() => FALLBACK_RATE,
        _ => sr,
    };
    if rate != sr {
        pcm = lcc_video::resample(&pcm, out_ch, sr, rate);
    }
    let track = match codec {
        AudioCodecOut::Aac => encode_aac(&pcm, out_ch, rate, cancel)?,
        AudioCodecOut::PcmS16 => encode_pcm(&pcm, out_ch, rate),
    };
    Ok(Some(track))
}

/// Encode interleaved PCM the caller already holds (`channels` 1 or 2, −1..1, e.g. slideshow
/// music) for `format`: AAC for MP4 (resampled to 48 kHz when `rate` is not an AAC rate), 16-bit
/// PCM for MOV.
pub fn encode_pcm_track(
    pcm: &[f32],
    channels: usize,
    rate: u32,
    format: ClipFormat,
    cancel: &AtomicBool,
) -> Result<EncodedAudioTrack, ExportClipError> {
    if !(1..=2).contains(&channels) || !(8_000..=192_000).contains(&rate) {
        return Err(ExportClipError::BadOptions(format!("audio {channels} channels at {rate} Hz is not supported")));
    }
    let frames = pcm.len() / channels;
    if frames as f64 > MAX_RANGE_SECONDS * f64::from(rate) {
        return Err(ExportClipError::BadOptions("the audio is too long".into()));
    }
    let clean: Vec<f32> =
        pcm.get(..frames * channels).unwrap_or_default().iter().map(|v| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 }).collect();
    match format {
        ClipFormat::H264Mp4 => {
            let out = if filmcraft_aac::sample_rate_index(rate).is_none() { FALLBACK_RATE } else { rate };
            let pcm = if out == rate { clean } else { lcc_video::resample(&clean, channels, rate, out) };
            encode_aac(&pcm, channels, out, cancel)
        }
        ClipFormat::ProResMov => Ok(encode_pcm(&clean, channels, rate)),
    }
}

/// Append one frame folded to `out_ch` channels (WAV order assumed beyond stereo: L R C LFE Ls Rs …).
fn fold(f: &[f32], out_ch: usize, out: &mut Vec<f32>) {
    let g = |i: usize| f.get(i).copied().filter(|v| v.is_finite()).unwrap_or(0.0);
    let (l, r) = match f.len() {
        0 => (0.0, 0.0),
        1 => (g(0), g(0)),
        2 => (g(0), g(1)),
        _ => {
            let (c, ls, rs) = (g(2), g(4), g(5));
            let norm = 1.0 / (1.0 + 2.0 * MINUS_3DB);
            ((g(0) + MINUS_3DB * (c + ls)) * norm * 1.5, (g(1) + MINUS_3DB * (c + rs)) * norm * 1.5)
        }
    };
    if out_ch == 1 {
        out.push(l);
    } else {
        out.push(l.clamp(-1.0, 1.0));
        out.push(r.clamp(-1.0, 1.0));
    }
}

fn encode_aac(pcm: &[f32], ch: usize, rate: u32, cancel: &AtomicBool) -> Result<EncodedAudioTrack, ExportClipError> {
    let cfg = filmcraft_aac::EncoderConfig::cbr(rate, ch, AAC_BPS_PER_CHANNEL * ch as u32);
    let mut enc = filmcraft_aac::Encoder::new(cfg).map_err(|e| ExportClipError::Encode(format!("audio: {e}")))?;
    let mut planar: Vec<Vec<f32>> = vec![Vec::with_capacity(1024); ch];
    let mut aus = Vec::new();
    for frame_block in pcm.chunks(1024 * ch) {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportClipError::Cancelled);
        }
        for p in &mut planar {
            p.clear();
        }
        for f in frame_block.chunks_exact(ch) {
            for (p, v) in planar.iter_mut().zip(f) {
                p.push(*v);
            }
        }
        let refs: Vec<&[f32]> = planar.iter().map(Vec::as_slice).collect();
        aus.extend(enc.encode(&refs));
    }
    aus.extend(enc.flush());
    Ok(EncodedAudioTrack {
        codec: AudioCodecOut::Aac,
        sample_rate: rate,
        channels: ch,
        priming: enc.priming_samples(),
        asc: enc.audio_specific_config(),
        packets: aus.into_iter().map(|data| AudioPacket { data, duration: 1024 }).collect(),
    })
}

fn encode_pcm(pcm: &[f32], ch: usize, rate: u32) -> EncodedAudioTrack {
    let packets = pcm
        .chunks(PCM_CHUNK * ch)
        .map(|c| {
            let data: Vec<u8> = c.iter().flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()).collect();
            AudioPacket { data, duration: (c.len() / ch) as u32 }
        })
        .collect();
    EncodedAudioTrack { codec: AudioCodecOut::PcmS16, sample_rate: rate, channels: ch, priming: 0, asc: Vec::new(), packets }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_downmixes_and_keeps_stereo() {
        let mut o = Vec::new();
        fold(&[0.5, -0.5], 2, &mut o);
        fold(&[0.25], 2, &mut o);
        fold(&[f32::NAN, 1.0, 0.0, 0.0, 0.0, 0.0], 2, &mut o);
        assert_eq!(&o[..4], &[0.5, -0.5, 0.25, 0.25]);
        assert_eq!(o[4], 0.0);
        assert!(o[5] > 0.5 && o[5] <= 1.0);
    }

    #[test]
    fn caller_pcm_encodes_and_bad_shapes_are_errors() {
        let no = AtomicBool::new(false);
        let pcm: Vec<f32> = (0..48_000 * 2).map(|i| (i as f32 * 0.01).sin() * 0.3).collect();
        let t = encode_pcm_track(&pcm, 2, 44_100, ClipFormat::H264Mp4, &no).unwrap();
        assert_eq!(t.codec, AudioCodecOut::Aac);
        assert!(!t.packets.is_empty());
        let t = encode_pcm_track(&[f32::NAN, 2.0], 2, 22_050, ClipFormat::ProResMov, &no).unwrap();
        assert_eq!(t.packets[0].data, vec![0, 0, 0xff, 0x7f]);
        assert!(encode_pcm_track(&pcm, 3, 48_000, ClipFormat::H264Mp4, &no).is_err());
        assert!(encode_pcm_track(&pcm, 2, 0, ClipFormat::H264Mp4, &no).is_err());
    }

    #[test]
    fn pcm_chunks_cover_every_frame() {
        let t = encode_pcm(&vec![0.5; 10_000 * 2], 2, 48_000);
        assert_eq!(t.packets.iter().map(|p| p.duration).sum::<u32>(), 10_000);
        assert_eq!(t.packets.iter().map(|p| p.data.len()).sum::<usize>(), 40_000);
    }
}
