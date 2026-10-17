//! Sound through a PulseAudio server (PulseAudio itself or PipeWire's `pipewire-pulse`, the
//! usual sound server on Linux and the BSDs), spoken over its native protocol by the pure-Rust
//! `pulseaudio` crate: no C library, so this path is on in every build for those systems.
//!
//! The server pulls samples (it asks for a number of bytes, the callback writes them as f32 LE);
//! the buffer target is kept short so the [`AudioClock`], which counts frames handed over, runs
//! at most [`TARGET_MS`] ahead of what is heard.
//!
//! Sources: own design over the `pulseaudio` crate's public API (MIT).

use std::ffi::CString;

use pulseaudio::{AsPlaybackSource, Client, PlaybackStream, protocol};

use crate::{AudioClock, Fill};

/// Target length of the server-side buffer, in milliseconds.
pub const TARGET_MS: u32 = 60;

/// Bytes per sample (f32).
const SAMPLE_BYTES: usize = 4;

/// Most samples converted per request (a hostile or odd server can't make us allocate more).
const MAX_REQUEST_SAMPLES: usize = 1 << 20;

fn client() -> Result<Client, String> {
    let name = CString::new(lcc_brand::DISPLAY_NAME).unwrap_or_default();
    Client::from_env(name).map_err(|e| format!("audio output: {e}"))
}

/// The server's sample rate, when a server is reachable.
pub fn default_rate() -> Option<u32> {
    let c = client().ok()?;
    futures::executor::block_on(c.server_info()).ok().map(|i| i.sample_spec.sample_rate).filter(|r| *r > 0)
}

/// A playing stream; dropping it deletes the stream on the server.
pub struct PulseOut {
    _stream: PlaybackStream,
    _client: Client,
}

/// Convert `fill`'s samples for a request of `buf.len()` bytes; returns the bytes written
/// (whole frames only). Never 0 for a request of at least one frame, which the server would
/// read as the end of the stream.
fn serve(buf: &mut [u8], channels: usize, scratch: &mut Vec<f32>, fill: &mut Fill, clock: &AudioClock) -> usize {
    let frame = SAMPLE_BYTES * channels.max(1);
    let frames = (buf.len() / frame).min(MAX_REQUEST_SAMPLES / channels.max(1));
    if frames == 0 {
        // smaller than a frame (never asked by a real server): silence, never "end of stream"
        buf.fill(0);
        return buf.len();
    }
    scratch.clear();
    scratch.resize(frames * channels, 0.0);
    fill(scratch);
    for (dst, s) in buf.as_chunks_mut::<SAMPLE_BYTES>().0.iter_mut().zip(scratch.iter()) {
        let v = if s.is_finite() { s.clamp(-1.0, 1.0) } else { 0.0 };
        *dst = v.to_le_bytes();
    }
    clock.advance(frames as u64);
    frames * frame
}

/// Start a stream of interleaved f32 at `rate` Hz, mono or stereo, on the default sink.
pub fn start(rate: u32, channels: u16, mut fill: Fill, clock: AudioClock) -> Result<PulseOut, String> {
    let channel_map = match channels {
        1 => protocol::ChannelMap::mono(),
        2 => protocol::ChannelMap::stereo(),
        n => return Err(format!("audio output: {n} channels (mono or stereo only)")),
    };
    let c = client()?;
    let frame_bytes = SAMPLE_BYTES as u32 * u32::from(channels);
    let target = (rate / 1000).saturating_mul(TARGET_MS).saturating_mul(frame_bytes).max(frame_bytes);
    let params = protocol::PlaybackStreamParams {
        sample_spec: protocol::SampleSpec { format: protocol::SampleFormat::Float32Le, channels: channels as u8, sample_rate: rate },
        channel_map,
        cvolume: Some(protocol::ChannelVolume::norm(channels as u8)),
        sink_name: Some(protocol::DEFAULT_SINK.to_owned()),
        buffer_attr: protocol::stream::BufferAttr { target_length: target, ..Default::default() },
        ..Default::default()
    };
    let ch = usize::from(channels);
    let mut scratch = Vec::new();
    let source = move |buf: &mut [u8]| serve(buf, ch, &mut scratch, &mut fill, &clock);
    let stream =
        futures::executor::block_on(c.create_playback_stream(params, source.as_playback_source())).map_err(|e| format!("audio output: {e}"))?;
    Ok(PulseOut { _stream: stream, _client: c })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_become_whole_frames_of_clamped_samples() {
        let clock = AudioClock::new(48_000);
        let mut fill: Fill = Box::new(|out: &mut [f32]| {
            for (i, s) in out.iter_mut().enumerate() {
                *s = [0.5, 2.0, f32::NAN][i % 3];
            }
        });
        let mut scratch = Vec::new();
        let mut buf = vec![0xAAu8; 8 * 3 + 5];
        let n = serve(&mut buf, 2, &mut scratch, &mut fill, &clock);
        assert_eq!(n, 24, "three whole stereo frames");
        assert_eq!(clock.frames(), 3);
        assert_eq!(&buf[0..4], &0.5f32.to_le_bytes());
        assert_eq!(&buf[4..8], &1.0f32.to_le_bytes());
        assert_eq!(&buf[8..12], &0.0f32.to_le_bytes());
        // a request smaller than a frame is silence, not the end of the stream
        let mut tiny = [1u8; 3];
        assert_eq!(serve(&mut tiny, 2, &mut scratch, &mut fill, &clock), 3);
        assert_eq!(tiny, [0; 3]);
    }

    /// Against the session's server when there is one (silence for a moment): the server pulls
    /// samples and the clock moves. Skips without a server (CI).
    #[test]
    fn plays_silence_on_a_running_server() {
        let Some(rate) = default_rate() else { return };
        let clock = AudioClock::new(rate);
        let out = start(rate, 2, Box::new(|o: &mut [f32]| o.fill(0.0)), clock.clone()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(clock.frames() > 0, "the server pulled samples");
        drop(out);
    }

    #[test]
    fn bad_channel_counts_are_errors() {
        assert!(start(48_000, 6, Box::new(|_| {}), AudioClock::new(48_000)).is_err());
    }
}
