//! Video playback commands (LR-VID-PLAY, KEY-VIDEO-PLAY): play, pause, seek and step the active
//! photo when it is a video. The UI's Space key, the transport bar, the control channel and MCP
//! all run these; the loupe shows the frame [`Session::video_tick`] advances to.
//!
//! None of them changes the library: they are queries for the journal and undo.

use lightcraft_catalog::{PhotoId, Source};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::video_play::VideoPlayer;
use crate::{Result, Session};

/// The furthest one `video.step` goes (a minute at 60 fps); more is a seek.
const MAX_STEP: i64 = 3600;

impl Session {
    /// Advance playback to the frame due now: call once per UI frame. Stops playback when the
    /// active photo is no longer the playing clip. Returns how long until the next frame is due
    /// (for the UI's next repaint), `None` when nothing will change by itself.
    pub fn video_tick(&mut self) -> Option<f64> {
        let active = self.active();
        if self.video.player.as_ref().is_some_and(|p| Some(p.photo()) != active) {
            self.video_stop();
        }
        self.video.player.as_mut().and_then(VideoPlayer::tick)
    }

    /// Stop playback and let the decoder go (leaving the clip, the loupe or the library).
    pub fn video_stop(&mut self) -> bool {
        self.video.player.take().is_some()
    }

    /// Stop the player when its clip is no longer the active photo. Runs after every command, so
    /// a frontend that never ticks (MCP, the CLI) doesn't keep a decoder and its frames.
    pub(crate) fn video_reap(&mut self) {
        let active = self.active();
        if self.video.player.as_ref().is_some_and(|p| Some(p.photo()) != active) {
            self.video_stop();
        }
    }

    /// The player of the active photo, opened (paused on its first frame) when it is not yet.
    fn video_player(&mut self, cmd: &str) -> Result<&mut VideoPlayer> {
        let id = self.active().ok_or_else(|| bad(cmd, "no photo selected"))?;
        // a player whose decode thread ended (the clip could not be opened, the decoder failed)
        // is opened again: the drive may be awake now
        if self.video.player.as_ref().is_none_or(|p| p.photo() != id || p.finished()) {
            self.video_stop();
            let path = video_path(self, id).map_err(|e| bad(cmd, e))?;
            if !self.video.available() {
                return Err(bad(cmd, crate::video_source::NO_VIDEO_DECODING));
            }
            let opener = self.video.opener;
            let open = Box::new(move || opener(&path));
            let player = VideoPlayer::open_limited(id, open, self.video.clock.clone(), self.video.max_side).map_err(|e| bad(cmd, e))?;
            self.video.player = Some(player);
        }
        let player = self.video.player.as_mut().ok_or_else(|| bad(cmd, "no video is open"))?;
        player.tick();
        Ok(player)
    }

    fn video_state(&mut self) -> Value {
        let available = self.video.available();
        match self.video.player.as_mut() {
            Some(p) => {
                p.tick();
                let mut v = p.state();
                if let Some(o) = v.as_object_mut() {
                    o.insert("open".into(), json!(true));
                    o.insert("available".into(), json!(available));
                }
                v
            }
            None => json!({"open": false, "available": available}),
        }
    }
}

/// The file of video `id`, or why it can't be played.
fn video_path(s: &Session, id: PhotoId) -> std::result::Result<String, String> {
    let p = s.catalog.photo(id).ok_or("no such photo")?;
    if !p.is_video() {
        return Err("the active photo is not a video".into());
    }
    match &p.source {
        Source::File { path } => Ok(path.clone()),
        Source::Demo { .. } => Err("this video has no file".into()),
    }
}

/// Playback commands need the active photo to be a video.
fn active_video(s: &Session) -> std::result::Result<(), String> {
    let id = s.active().ok_or("no photo selected")?;
    video_path(s, id).map(|_| ())
}

fn state_after(s: &mut Session) -> Result<Value> {
    Ok(s.video_state())
}

fn play_pause(s: &mut Session, _: &Value) -> Result<Value> {
    s.video_player("video.playPause")?.toggle();
    state_after(s)
}

fn play(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.play";
    let at = seconds_param(p, "at", C)?;
    let player = s.video_player(C)?;
    if let Some(t) = at {
        player.seek(t);
    }
    player.play();
    state_after(s)
}

fn pause(s: &mut Session, _: &Value) -> Result<Value> {
    if let Some(p) = s.video.player.as_mut() {
        p.pause();
    }
    state_after(s)
}

fn seek(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.seek";
    let t = match (seconds_param(p, "time", C)?, p.get("fraction")) {
        (Some(t), _) => t,
        (None, Some(f)) => {
            let f = f.as_f64().filter(|f| f.is_finite() && (0.0..=1.0).contains(f)).ok_or_else(|| bad(C, "fraction must be a number from 0 to 1"))?;
            let player = s.video_player(C)?;
            // the clip's length is known once it is open; until then a fraction can't be placed
            let d = player.info().map(|i| i.duration).ok_or_else(|| bad(C, "the clip is still opening; try again"))?;
            f * d
        }
        (None, None) => return Err(bad(C, "give time (seconds) or fraction (0 to 1)")),
    };
    s.video_player(C)?.seek(t);
    state_after(s)
}

fn step(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.step";
    let frames = match p.get("frames") {
        None | Some(Value::Null) => 1,
        Some(v) => v
            .as_i64()
            .filter(|n| *n != 0 && n.abs() <= MAX_STEP)
            .ok_or_else(|| bad(C, format!("frames must be a whole number from -{MAX_STEP} to {MAX_STEP}, not 0")))?,
    };
    let frames = i32::try_from(frames).map_err(|_| bad(C, "frames out of range"))?;
    s.video_player(C)?.step(frames);
    state_after(s)
}

fn stop(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"stopped": s.video_stop()}))
}

fn state(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(s.video_state())
}

/// An optional time in seconds: a finite, non-negative number.
fn seconds_param(p: &Value, key: &str, cmd: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|t| t.is_finite() && *t >= 0.0)
            .map(Some)
            .ok_or_else(|| bad(cmd, format!("{key} must be a time in seconds (0 or more)"))),
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "video.playPause",
            "Play/Pause Video",
            [],
            None,
            "{}: plays the active video, or pauses it when playing (Space in the loupe); opens the clip first when needed. Playback stops when another photo becomes active",
            active_video,
            play_pause
        ),
        cmd!(
            query "video.play",
            "Play Video",
            [],
            None,
            "{at?: seconds (start there)}: plays the active video from the playhead (from the start when it has ended); audio is not played yet",
            active_video,
            play
        ),
        cmd!(query "video.pause", "Pause Video", [], None, "{}: pauses playback on the frame shown (decoded exactly); nothing playing is fine", always, pause),
        cmd!(
            query "video.seek",
            "Seek Video",
            [],
            None,
            "{time?: seconds | fraction?: 0 to 1 of the clip}: moves the playhead (clamped to the clip) and shows the frame at it; playing continues from there",
            active_video,
            seek
        ),
        cmd!(
            query "video.step",
            "Step Video Frame",
            [],
            None,
            "{frames?: whole number, default 1 (-1 = previous frame), at most ±3600}: pauses and moves by frames",
            active_video,
            step
        ),
        cmd!(query "video.stop", "Stop Video", [], None, "{}: stops playback and closes the clip → {stopped}", always, stop),
        cmd!(
            query "video.state",
            "Video Playback State",
            [],
            None,
            "{} → {open, available (this build decodes video), opening, playing, ended, position, duration (seconds), frameRate, width, height, orientation (EXIF 1 to 8), codec, frame: {pts, draft, width, height (the frame's own size: half while playback falls behind)}|null, behind (playback is dropping late frames), shown, dropped (late frames skipped), decoded, queued, queuedBytes, decoderBytes (what the decoder may hold for its pictures), memoryBytes (all playback holds: decoder, frames waiting, the frame shown), audio: {track, played: false}|null (audio is not played yet), error|null}",
            always,
            state
        ),
    ]
}

#[cfg(test)]
#[path = "video_tests.rs"]
mod tests;
