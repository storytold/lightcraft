//! The `lcc-video` backend (pure-Rust FilmCraft decoders). Keeps the last clip's reader open so
//! scrubbing and frame steps don't re-open the file.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use super::{FrameImage, FrameIter, VideoBackend, VideoInfo};

#[derive(Default)]
pub struct LccVideo {
    reader: Mutex<Option<(PathBuf, lcc_video::FrameReader)>>,
    /// The last clip asked about by `can_play_audio` and the answer (opening its audio costs a demux).
    audio: Mutex<Option<(PathBuf, bool)>>,
}

impl VideoBackend for LccVideo {
    fn probe(&self, path: &Path) -> Result<VideoInfo, String> {
        let i = lcc_video::probe(path).map_err(|e| e.to_string())?;
        if !i.decodable {
            return Err(format!("unsupported video: {} can't be decoded", i.codec));
        }
        Ok(VideoInfo {
            duration: i.duration,
            fps_num: i.fps_num,
            fps_den: i.fps_den,
            width: i.width,
            height: i.height,
            codec: i.codec,
            has_audio: i.has_audio,
        })
    }

    fn can_play_audio(&self, path: &Path) -> bool {
        if !lcc_audio_out::CAN_PLAY {
            return false;
        }
        let mut slot = self.audio.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((p, ok)) = slot.as_ref()
            && p == path
        {
            return *ok;
        }
        let ok = lcc_video::AudioReader::open(path).is_ok();
        *slot = Some((path.to_path_buf(), ok));
        ok
    }

    fn frame_at(&self, path: &Path, t: f64, max_side: Option<u32>) -> Result<FrameImage, String> {
        let mut slot = self.reader.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref().is_none_or(|(p, _)| p != path) {
            *slot = None;
            let r = lcc_video::FrameReader::open(path).map_err(|e| e.to_string())?;
            *slot = Some((path.to_path_buf(), r));
        }
        let Some((_, reader)) = slot.as_mut() else { return Err("the video reader isn't open".into()) };
        match reader.frame_at(t, max_side) {
            Ok(f) => Ok(f.image),
            Err(e) => {
                // a failed decode may leave the reader mid-stream: open afresh next time
                *slot = None;
                Err(e.to_string())
            }
        }
    }

    fn frames_from(&self, path: &Path, t: f64, max_side: Option<u32>) -> Result<FrameIter, String> {
        let mut r = lcc_video::FrameReader::open(path).map_err(|e| e.to_string())?;
        r.seek(t);
        let mut failed = false;
        Ok(Box::new(std::iter::from_fn(move || {
            if failed {
                return None;
            }
            match r.next_frame(max_side) {
                Ok(Some(f)) => Some(Ok((f.time, f.image))),
                Ok(None) => None,
                Err(e) => {
                    failed = true;
                    Some(Err(e.to_string()))
                }
            }
        })))
    }
}
