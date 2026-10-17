//! Video playback, trim, poster frame and frame capture: the engine half of M14.4 / M14.6
//! (`plan/video-filmcraft.md` §7).
//!
//! The engine never decodes video itself: a host that can installs a [`VideoBackend`] in
//! [`VideoState::backend`] (the desktop app and CLI wrap `lcc-video`). Without one every `video.*`
//! command fails with [`UNAVAILABLE`], so the web build and tests without a decoder stay clean.
//!
//! - [`player`]: the playback state machine (play/pause, clock-driven position, frame steps,
//!   trim bounds).
//! - [`adapter`]: the few functions that read and change a clip's [`VideoMeta`]; the only code
//!   that knows where it is stored.
//! - [`native`]: the `lcc-video` backend, installed by default on native builds.
//!
//! Sources: own design.

pub mod adapter;
#[cfg(test)]
mod bench_playback;
#[cfg(not(target_arch = "wasm32"))]
pub mod native;
pub mod playback;
pub mod player;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;

pub use lcc_catalog::VideoMeta;
pub use player::Player;

/// What every video command answers when no decoder backend is installed.
pub const UNAVAILABLE: &str = "video playback isn't available in this build";

/// A decoded frame: linear-light sRGB-primaries RGB (what the pipeline takes from a JPEG/TIFF).
pub type FrameImage = lcc_raster::Rgb32f;

/// What a backend's probe reports about a clip.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoInfo {
    pub duration: f64,
    pub fps_num: u32,
    pub fps_den: u32,
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub has_audio: bool,
}

impl VideoInfo {
    /// The catalog record of a freshly probed clip: no trim, default poster.
    pub fn to_meta(&self) -> VideoMeta {
        VideoMeta {
            duration: self.duration,
            fps_num: self.fps_num,
            fps_den: self.fps_den,
            width: self.width,
            height: self.height,
            codec: self.codec.clone(),
            has_audio: self.has_audio,
            trim_in: 0.0,
            trim_out: None,
            poster_time: None,
        }
    }
}

/// A video decoder the host provides (`lcc-video` on native). Errors are user-facing messages.
pub trait VideoBackend: Send + Sync {
    /// Duration, frame rate, size and codec of the clip at `path`.
    fn probe(&self, path: &Path) -> Result<VideoInfo, String>;
    /// The frame shown at `t` seconds, scaled so its longer side is at most `max_side` (None = full size).
    fn frame_at(&self, path: &Path, t: f64, max_side: Option<u32>) -> Result<FrameImage, String>;
    /// Frames in presentation order from the one shown at `t` (a reader of its own, for a
    /// playback worker). Each item is `(presentation time, frame)`; the iterator ends at the clip's end.
    fn frames_from(&self, path: &Path, t: f64, max_side: Option<u32>) -> Result<FrameIter, String> {
        let _ = (path, t, max_side);
        Err("sequential decoding isn't available in this build".into())
    }
    /// Whether this build can play the clip's sound: built with `audio-out` and the clip has a
    /// decodable audio track. Default: false (muted).
    fn can_play_audio(&self, _path: &Path) -> bool {
        false
    }
}

/// What [`VideoBackend::frames_from`] returns.
pub type FrameIter = Box<dyn Iterator<Item = Result<(f64, FrameImage), String>> + Send>;

/// Seconds on a monotonic clock (injectable for tests).
pub type Clock = Box<dyn Fn() -> f64 + Send>;

/// The session's video state.
pub struct VideoState {
    /// The decoder (`lcc-video` on native by default); `None` = video isn't available in this build.
    pub backend: Option<Arc<dyn VideoBackend>>,
    pub player: Player,
    /// Monotonic time source driving playback.
    pub clock: Clock,
}

impl Default for VideoState {
    fn default() -> Self {
        Self { backend: default_backend(), player: Player::default(), clock: default_clock() }
    }
}

impl VideoState {
    /// Drive playback from an audio device clock (`audio-out` builds playing a clip's sound), so
    /// frames follow the sound. Continues from the current time: the position doesn't jump.
    pub fn follow_audio_clock(&mut self, audio: lcc_audio_out::AudioClock) {
        let offset = (self.clock)() - audio.seconds();
        self.clock = Box::new(move || offset + audio.seconds());
    }

    /// Back to the monotonic system clock (sound stopped), continuing from the current time.
    pub fn follow_system_clock(&mut self) {
        let offset = (self.clock)();
        let system = default_clock();
        let start = system();
        self.clock = Box::new(move || offset + system() - start);
    }
}

impl std::fmt::Debug for VideoState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoState").field("backend", &self.backend.is_some()).field("player", &self.player).finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn default_backend() -> Option<Arc<dyn VideoBackend>> {
    Some(Arc::new(native::LccVideo::default()))
}

#[cfg(target_arch = "wasm32")]
fn default_backend() -> Option<Arc<dyn VideoBackend>> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn default_clock() -> Clock {
    let start = std::time::Instant::now();
    Box::new(move || start.elapsed().as_secs_f64())
}

/// The web build has no backend, so playback never runs there.
#[cfg(target_arch = "wasm32")]
fn default_clock() -> Clock {
    Box::new(|| 0.0)
}
