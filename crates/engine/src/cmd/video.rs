//! Video commands (M14.4 / M14.6, `plan/video-filmcraft.md` §7): playback (`video.play`,
//! `pause`, `togglePlay`, `seek`, `stepFrame`), trim and poster frame (undoable), Capture Frame
//! and the `video.get` query. They act on the active photo when it is a video, and need a decoder
//! backend ([`crate::video::VideoBackend`]); without one they answer
//! [`crate::video::UNAVAILABLE`].

use std::path::{Path, PathBuf};

use lcc_catalog::PhotoId;
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd};
use crate::video::{VideoMeta, adapter, player};
use crate::{EngineError, Result, Session};

/// Enabled when the active photo is a video and a decoder is installed.
pub fn active_video(s: &Session) -> std::result::Result<(), String> {
    let id = s.active().ok_or("no photo selected")?;
    if !adapter::is_video(s, id) {
        return Err("the active photo isn't a video".into());
    }
    if s.video.backend.is_none() {
        return Err(crate::video::UNAVAILABLE.into());
    }
    Ok(())
}

/// The active clip, its metadata, play range, frame rate and the clock, with the player attached.
struct Ctx {
    id: PhotoId,
    meta: VideoMeta,
    bounds: (f64, f64),
    fps: f64,
    now: f64,
}

fn ctx(s: &mut Session, c: &str) -> Result<Ctx> {
    let id = s.active().ok_or_else(|| bad(c, "no photo selected"))?;
    let meta = adapter::meta(s, id)?;
    let bounds = player::bounds(&meta);
    let fps = player::fps(&meta);
    let now = (s.video.clock)();
    s.video.player.attach(id, bounds);
    Ok(Ctx { id, meta, bounds, fps, now })
}

/// The default poster time: 10 % into the clip, inside the trim.
fn poster_of(m: &VideoMeta) -> f64 {
    let (tin, out) = player::bounds(m);
    m.poster_time.filter(|t| t.is_finite()).unwrap_or(m.duration * 0.1).clamp(tin, out)
}

fn state(s: &mut Session, x: &Ctx) -> Value {
    let current = s.video.player.tick(x.now, x.bounds);
    let p = &s.video.player;
    let m = &x.meta;
    json!({
        "id": x.id.0,
        "duration": m.duration,
        "fps": x.fps,
        "frameRate": [m.fps_num, m.fps_den],
        "width": m.width,
        "height": m.height,
        "codec": m.codec,
        "hasAudio": m.has_audio,
        // true only in `audio-out` builds for clips with decodable sound (else playback is muted)
        "canPlayAudio": m.has_audio && can_play_audio(s, x.id),
        "current": current,
        "frame": if x.fps > 0.0 { (current * x.fps + 1e-6).floor() } else { 0.0 },
        "playing": p.playing,
        "looping": p.looping,
        "trimIn": x.bounds.0,
        "trimOut": x.bounds.1,
        "trimmed": m.trim_in > 0.0 || m.trim_out.is_some(),
        "poster": poster_of(m),
        "posterSet": m.poster_time.is_some(),
    })
}

/// Whether the clip's sound can play: `audio-out` builds with a decodable audio track.
fn can_play_audio(s: &Session, id: PhotoId) -> bool {
    match (adapter::backend(s), adapter::clip_path(s, id)) {
        (Ok(be), Ok(path)) => be.can_play_audio(&path),
        _ => false,
    }
}

/// A finite time parameter within `[0, duration]`.
fn time_param(p: &Value, key: &str, c: &str, m: &VideoMeta) -> Result<f64> {
    let t = super::f64_req(p, key, c)?;
    if !t.is_finite() || t < 0.0 || t > m.duration {
        return Err(bad(c, format!("`{key}` {t} is outside the clip (0 to {:.3} s)", m.duration)));
    }
    Ok(t)
}

fn get(s: &mut Session, _: &Value) -> Result<Value> {
    let x = ctx(s, "video.get")?;
    Ok(state(s, &x))
}

fn play(s: &mut Session, p: &Value) -> Result<Value> {
    let x = ctx(s, "video.play")?;
    if let Some(l) = p.get("loop").and_then(Value::as_bool) {
        s.video.player.looping = l;
    }
    s.video.player.play(x.now, x.bounds);
    Ok(state(s, &x))
}

fn pause(s: &mut Session, _: &Value) -> Result<Value> {
    let x = ctx(s, "video.pause")?;
    s.video.player.pause(x.now, x.bounds);
    Ok(state(s, &x))
}

fn toggle(s: &mut Session, _: &Value) -> Result<Value> {
    let x = ctx(s, "video.togglePlay")?;
    s.video.player.tick(x.now, x.bounds);
    if s.video.player.playing {
        s.video.player.pause(x.now, x.bounds);
    } else {
        s.video.player.play(x.now, x.bounds);
    }
    Ok(state(s, &x))
}

fn seek(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.seek";
    let x = ctx(s, C)?;
    let t = super::f64_req(p, "time", C)?;
    s.video.player.seek(t, x.now, x.bounds, x.fps);
    Ok(state(s, &x))
}

fn step(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.stepFrame";
    let x = ctx(s, C)?;
    let d = match p.get("delta") {
        None | Some(Value::Null) => 1,
        Some(v) => v.as_i64().ok_or_else(|| bad(C, "`delta` must be a whole number of frames"))?,
    };
    if d.unsigned_abs() > player::MAX_STEP as u64 {
        return Err(bad(C, format!("`delta` {d}: at most {} frames", player::MAX_STEP)));
    }
    s.video.player.step(d, x.now, x.bounds, x.fps);
    Ok(state(s, &x))
}

fn set_trim(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.setTrim";
    let x = ctx(s, C)?;
    let tin = time_param(p, "in", C, &x.meta)?;
    let out = match p.get("out") {
        None | Some(Value::Null) => x.meta.duration,
        Some(_) => time_param(p, "out", C, &x.meta)?,
    };
    if tin >= out {
        return Err(bad(C, format!("`in` ({tin:.3} s) must be before `out` ({out:.3} s)")));
    }
    let trim_out = (out < x.meta.duration).then_some(out);
    if (tin, trim_out) != (x.meta.trim_in, x.meta.trim_out) {
        adapter::commit_trim(s, x.id, tin, trim_out, "Trim Video")?;
    }
    finish_change(s, C)
}

fn reset_trim(s: &mut Session, _: &Value) -> Result<Value> {
    const C: &str = "video.resetTrim";
    let x = ctx(s, C)?;
    if x.meta.trim_in != 0.0 || x.meta.trim_out.is_some() {
        adapter::commit_trim(s, x.id, 0.0, None, "Reset Trim")?;
    }
    finish_change(s, C)
}

fn set_poster(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.setPoster";
    let x = ctx(s, C)?;
    let time = match p.get("time") {
        Some(Value::Null) => None,
        _ => Some(player::snap(time_param(p, "time", C, &x.meta)?, x.fps).max(0.0)),
    };
    if time != x.meta.poster_time {
        adapter::commit_poster(s, x.id, time, "Set Poster Frame")?;
    }
    finish_change(s, C)
}

/// After a trim/poster change: keep the player inside the new range and report the state.
fn finish_change(s: &mut Session, c: &str) -> Result<Value> {
    let x = ctx(s, c)?;
    s.video.player.clamp_to(x.now, x.bounds);
    Ok(state(s, &x))
}

/// Most numbered names Capture Frame tries before giving up.
const MAX_NAME_TRIES: u32 = 1000;

/// `<dir>/<stem>-<mm>m<ss>s<fff>.jpg`, numbered when taken (never overwrites).
fn frame_path(clip: &Path, t: f64) -> Result<PathBuf> {
    let dir = clip.parent().ok_or_else(|| EngineError::Other("the clip has no folder".into()))?;
    let stem = clip.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "frame".into());
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    let base = format!("{stem}-{}m{:02}s{:03}", ms / 60_000, (ms / 1000) % 60, ms % 1000);
    for n in 0..MAX_NAME_TRIES {
        let name = if n == 0 { format!("{base}.jpg") } else { format!("{base}-{}.jpg", n + 1) };
        let path = dir.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(EngineError::Other("can't find a free file name for the frame".into()))
}

fn capture(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "video.captureFrame";
    let x = ctx(s, C)?;
    let t = match p.get("time") {
        None | Some(Value::Null) => s.video.player.tick(x.now, x.bounds),
        Some(_) => time_param(p, "time", C, &x.meta)?,
    };
    let t = player::snap(t, x.fps).clamp(0.0, x.meta.duration);
    let be = adapter::backend(s)?;
    let clip = adapter::clip_path(s, x.id)?;
    let _task = s.activity.start("video.captureFrame", "Capturing a video frame", crate::activity::Cancel::No);
    let frame = be.frame_at(&clip, t, None).map_err(|e| EngineError::Other(format!("can't decode the frame at {t:.3} s: {e}")))?;
    if frame.width == 0 || frame.height == 0 || frame.data.len() != frame.width.saturating_mul(frame.height) {
        return Err(EngineError::Other("the decoder returned an empty frame".into()));
    }
    // Bake the clip's grade into the still (like Lightroom's Capture Frame): the JPEG looks like
    // the graded clip and the new photo starts with default settings, so it isn't graded twice.
    let develop = s.catalog.photo(x.id).map(|ph| ph.develop.clone()).unwrap_or_default();
    // …and its crop, as the loupe and export show it
    let frame = if lcc_pipeline::grade::video_is_framed(&develop) {
        let framing = lcc_pipeline::grade::video_framing(&develop);
        let (w, h) = lcc_pipeline::native_output_size(frame.width, frame.height, &framing);
        let (w, h) = ((w.round() as usize).clamp(1, frame.width.max(frame.height)), (h.round() as usize).clamp(1, frame.width.max(frame.height)));
        lcc_pipeline::grade::frame_video(&frame, &framing, w, h)
    } else {
        frame
    };
    let graded = lcc_pipeline::grade::grade_frame(&frame, &develop, lcc_pipeline::Quality::Full);
    let opts = crate::export::ExportOptions::default();
    let bytes = crate::export::encode_image(&graded, &opts).map_err(|e| EngineError::Other(format!("can't encode the frame: {e}")))?;
    let out = frame_path(&clip, t)?;
    let out_s = out.to_string_lossy().to_string();
    crate::export::write_file_durable(&out_s, &bytes).map_err(|e| EngineError::Other(format!("can't write {out_s}: {e}")))?;
    let before = s.undo.len();
    let r = s.execute("library.import", &json!({"paths": [out_s]}))?;
    let new = r["imported"].get(0).and_then(Value::as_u64).ok_or_else(|| EngineError::Other(format!("the frame {out_s} could not be added")))?;
    // stacked with the clip, the clip on top, expanded so both show
    let _ = s.execute("stack.group", &json!({"ids": [x.id.0, new], "top": x.id.0, "collapsed": false}));
    let steps = s.undo.len().saturating_sub(before);
    s.merge_undo(steps, "Capture Frame");
    Ok(json!({"id": new, "path": out_s, "time": t, "clip": x.id.0}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "video.get", "Video State", [], None,
            "{} — the active clip: {id, duration, fps, frameRate:[num,den], width, height, codec, hasAudio, canPlayAudio, current, frame, playing, looping, trimIn, trimOut, trimmed, poster, posterSet}",
            active_video, get),
        cmd!(query "video.play", "Play", ["Photo", "Video"], None,
            "{loop?: bool} — play the active clip from the current position (from the trim start when at the end) → video.get state",
            active_video, play),
        cmd!(query "video.pause", "Pause", ["Photo", "Video"], None, "{} — pause playback → video.get state", active_video, pause),
        cmd!(query "video.togglePlay", "Play / Pause", ["Photo", "Video"], None,
            "{} — play or pause the active clip → video.get state", active_video, toggle),
        cmd!(query "video.seek", "Go to Time", [], None,
            "{time: seconds} — jump to the frame at `time`, kept inside the trim → video.get state", active_video, seek),
        cmd!(query "video.stepFrame", "Step Frame", [], None,
            "{delta?: whole frames (default 1, negative = back, at most ±1000000)} — pause and move by frames, inside the trim → video.get state",
            active_video, step),
        cmd!(
            "video.setTrim",
            "Trim Video",
            [],
            None,
            "{in: seconds, out?: seconds (default/null = end)} — the play range, 0 ≤ in < out ≤ duration; one undo step → video.get state",
            active_video,
            set_trim
        ),
        cmd!(
            "video.resetTrim",
            "Reset Trim",
            ["Photo", "Video"],
            None,
            "{} — play the whole clip again (undoable) → video.get state",
            active_video,
            reset_trim
        ),
        cmd!(
            "video.setPoster",
            "Set Poster Frame",
            ["Photo", "Video"],
            None,
            "{time: seconds | null (default: 10 % in)} — the frame the grid shows for the clip, snapped to a frame; undoable → video.get state",
            active_video,
            set_poster
        ),
        cmd!(
            "video.captureFrame",
            "Capture Frame",
            ["Photo", "Video"],
            None,
            "{time?: seconds (default: the current position)} — save the frame, graded with the clip's settings, as a JPEG next to the clip (the new photo starts with default settings), add it to the library stacked with the clip → {id, path, time, clip}",
            active_video,
            capture
        ),
    ]
}
