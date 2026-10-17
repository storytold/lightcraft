//! Audio output (L0): the single sound-card path the app uses (slideshow music, video playback),
//! and [`AudioClock`], the device's position, which playback can follow instead of the system
//! clock so pictures stay in step with the sound.
//!
//! On Linux and the BSDs sound goes to a PulseAudio server (PulseAudio, or PipeWire through
//! `pipewire-pulse`) over its native protocol in pure Rust ([`pulse`]), in every build. Elsewhere
//! the `audio-out` cargo feature plays through cpal (CoreAudio, WASAPI; on Linux it would link the
//! C library libasound, so there it is only a fallback when no PulseAudio server answers). With
//! neither, [`CAN_PLAY`] is false and [`Output::start`] returns an error: callers play silently.
//!
//! Sources: own design (a sample counter advanced by the device callback).

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "dragonfly", target_os = "netbsd", target_os = "openbsd"))]
pub mod pulse;

/// Whether this build has a sound path (PulseAudio / PipeWire on Linux and the BSDs, cpal with
/// the `audio-out` feature). Playing still needs a running server or device.
pub const CAN_PLAY: bool = cfg!(any(
    feature = "audio-out",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
));

/// Fills an interleaved output buffer; called on the audio thread.
pub type Fill = Box<dyn FnMut(&mut [f32]) + Send>;

/// Frames handed to the output device, as seconds. Clones share one counter; it only moves while
/// the device pulls samples (a stalled or stopped stream freezes it), so it is monotonic.
#[derive(Clone, Debug)]
pub struct AudioClock {
    frames: Arc<AtomicU64>,
    rate: u32,
}

impl AudioClock {
    /// A clock at `rate` frames per second, at zero.
    pub fn new(rate: u32) -> Self {
        Self { frames: Arc::new(AtomicU64::new(0)), rate: rate.max(1) }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Count `frames` more as played.
    pub fn advance(&self, frames: u64) {
        let _ = self.frames.try_update(Ordering::Relaxed, Ordering::Relaxed, |f| Some(f.saturating_add(frames)));
    }

    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// Seconds played since the clock started.
    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate)
    }
}

/// The default output device's preferred sample rate (None without a device or a sound path).
pub fn default_rate() -> Option<u32> {
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "dragonfly", target_os = "netbsd", target_os = "openbsd"))]
    if let Some(r) = pulse::default_rate() {
        return Some(r);
    }
    #[cfg(feature = "audio-out")]
    {
        use cpal::traits::{DeviceTrait, HostTrait};
        let d = cpal::default_host().default_output_device()?;
        d.default_output_config().ok().map(|c| c.sample_rate().0)
    }
    #[cfg(not(feature = "audio-out"))]
    {
        None
    }
}

/// A stream playing on the default output device; dropping it stops the sound.
pub struct Output {
    #[cfg(feature = "audio-out")]
    _stream: Option<cpal::Stream>,
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "dragonfly", target_os = "netbsd", target_os = "openbsd"))]
    _pulse: Option<pulse::PulseOut>,
    clock: AudioClock,
}

impl Output {
    /// Start playing interleaved f32 at `rate` Hz with `channels` channels, pulling samples from
    /// `fill` (which should write silence when it has nothing). Errors without the feature or a device.
    pub fn start(rate: u32, channels: u16, fill: Fill) -> Result<Output, String> {
        if rate == 0 || channels == 0 {
            return Err(format!("audio output: invalid format ({rate} Hz, {channels} channels)"));
        }
        let clock = AudioClock::new(rate);
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "dragonfly", target_os = "netbsd", target_os = "openbsd"))]
        let fill = {
            // PulseAudio / PipeWire first; without a server, hand `fill` back for cpal (if built)
            let shared = std::sync::Arc::new(std::sync::Mutex::new(Some(fill)));
            let s2 = shared.clone();
            let relay: Fill = Box::new(move |out: &mut [f32]| {
                if let Some(f) = s2.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
                    f(out);
                }
            });
            match pulse::start(rate, channels, relay, clock.clone()) {
                Ok(p) => {
                    return Ok(Output {
                        #[cfg(feature = "audio-out")]
                        _stream: None,
                        _pulse: Some(p),
                        clock,
                    });
                }
                Err(e) => {
                    log::info!("{e}");
                    let taken = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
                    match taken {
                        Some(f) => f,
                        None => return Err(e),
                    }
                }
            }
        };
        #[cfg(feature = "audio-out")]
        {
            use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
            let mut fill = fill;
            let device = cpal::default_host().default_output_device().ok_or("no audio output device")?;
            let config = cpal::StreamConfig { channels, sample_rate: cpal::SampleRate(rate), buffer_size: cpal::BufferSize::Default };
            let c = clock.clone();
            let n = u64::from(channels);
            let stream = device
                .build_output_stream(
                    &config,
                    move |out: &mut [f32], _| {
                        out.fill(0.0);
                        fill(out);
                        c.advance(out.len() as u64 / n);
                    },
                    |e| log::warn!("audio output: {e}"),
                    None,
                )
                .map_err(|e| format!("audio output: {e}"))?;
            stream.play().map_err(|e| format!("audio output: {e}"))?;
            Ok(Output {
                _stream: Some(stream),
                #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "dragonfly", target_os = "netbsd", target_os = "openbsd"))]
                _pulse: None,
                clock,
            })
        }
        #[cfg(not(feature = "audio-out"))]
        {
            let _ = (fill, clock);
            if CAN_PLAY {
                Err("audio output: no PulseAudio or PipeWire server to play through".into())
            } else {
                Err("this build plays without sound (built without the audio-out feature)".into())
            }
        }
    }

    /// The device clock of this stream.
    pub fn clock(&self) -> AudioClock {
        self.clock.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_counts_frames_and_saturates() {
        let c = AudioClock::new(48_000);
        let d = c.clone();
        d.advance(24_000);
        assert!((c.seconds() - 0.5).abs() < 1e-12);
        c.advance(u64::MAX);
        assert_eq!(d.frames(), u64::MAX);
        assert_eq!(AudioClock::new(0).rate(), 1);
    }

    #[test]
    fn start_without_the_feature_or_with_a_bad_format_is_an_error() {
        assert!(Output::start(0, 2, Box::new(|_| {})).is_err());
        if !CAN_PLAY {
            assert!(Output::start(48_000, 2, Box::new(|_| {})).is_err());
            assert_eq!(default_rate(), None);
        }
    }
}
