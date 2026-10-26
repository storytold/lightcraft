//! The GPS track log loaded into the Map view (Lightroom Classic's Map ▸ Tracklog): session state,
//! never saved in the catalog. `map.trackLog` loads a GPX file (or text), picks the active track
//! of a file that has several, sets the camera's time zone offset and clears it;
//! `photo.autoTagTracklog {loaded: true}` tags photos from the active track with that offset.
//!
//! A file is read whole, so it is bounded: [`MAX_GPX_BYTES`] on disk (or as text) and
//! [`MAX_TRACK_POINTS`] timed points; past either the load is refused with an error saying so, and
//! the track already loaded stays. Each track is projected and ranked for drawing once, here
//! ([`TrackShape`]), so the map only filters and clips per frame.

use std::collections::BTreeMap;
use std::sync::Arc;

use lightcraft_map::TrackShape;
use lightcraft_meta::{DateTime, Tracklog, parse_gpx};
use serde_json::{Value, json};

use crate::cmd::bad;
use crate::{Result, Session};

/// Largest GPX file loaded (bytes).
pub const MAX_GPX_BYTES: u64 = 50 * 1024 * 1024;
/// Most timed track points loaded.
pub const MAX_TRACK_POINTS: usize = 2_000_000;
/// Default `maxGap` of tagging (seconds): how far from a logged point a photo may be.
pub const DEFAULT_MAX_GAP: f64 = 600.0;

/// One `<trk>` of the loaded file.
#[derive(Clone, Debug)]
pub struct TrackSummary {
    /// Its `<name>` (empty when it has none).
    pub name: String,
    /// Timed points, and continuous logging spans with points.
    pub points: usize,
    pub segments: usize,
    /// First and last point times (seconds since the epoch, UTC); `None` for an empty track.
    pub span: Option<(f64, f64)>,
}

/// The loaded track log (see the module docs).
#[derive(Debug)]
pub struct LoadedTrack {
    /// The file it came from (`None`: loaded from text).
    pub path: Option<String>,
    /// Its file name (or "GPX").
    pub name: String,
    pub log: Arc<Tracklog>,
    pub tracks: Vec<TrackSummary>,
    /// Each track's lines, ready to draw (one per `<trk>`, in file order).
    pub shapes: Vec<Arc<TrackShape>>,
    /// The track tagging uses and the map highlights; `None`: all of them.
    pub active: Option<usize>,
    /// The points tagging uses (the active track's, or all).
    pub active_log: Arc<Tracklog>,
    /// The camera clock's offset from UTC in minutes (east positive) for photos whose capture time
    /// has no zone; `None`: read them as UTC.
    pub offset_minutes: Option<i64>,
    /// Time to parse, project and rank (ms).
    pub load_ms: f64,
}

impl LoadedTrack {
    /// The active track's (or all tracks') first and last point times, UTC.
    pub fn span(&self) -> Option<(f64, f64)> {
        self.active_log.span()
    }

    fn set_active(&mut self, active: Option<usize>) {
        self.active = active;
        self.active_log = match active.and_then(|i| self.log.only_track(i)) {
            Some(one) => Arc::new(one),
            None => self.log.clone(),
        };
    }
}

/// A capture time in UTC seconds: its recorded zone wins, else `offset` minutes east of UTC
/// (none: read as UTC).
pub(crate) fn capture_utc(dt: &DateTime, offset: Option<i64>) -> f64 {
    let secs = match dt.offset_minutes {
        Some(_) => dt.unix_seconds(),
        None => dt.unix_seconds().saturating_sub(offset.unwrap_or(0).saturating_mul(60)),
    };
    secs as f64 + f64::from(dt.millis) / 1000.0
}

/// A position as tagging stores it (7 decimals, about a centimetre).
pub(crate) fn rounded(g: &lightcraft_meta::Gps) -> (f64, f64) {
    ((g.latitude * 1e7).round() / 1e7, (g.longitude * 1e7).round() / 1e7)
}

/// Where `photo.autoTagTracklog {loaded: true, replace: true}` would put each of `ids` (those
/// with a capture time within the active track's reach): the same reading of capture times, the
/// track's offset and the default 10-minute gap. Nothing is written or journaled (the map's marks).
pub fn positions(s: &Session, ids: &[lightcraft_catalog::PhotoId]) -> Vec<(u64, f64, f64)> {
    let Some(t) = s.map.track.as_ref() else { return Vec::new() };
    ids.iter()
        .filter_map(|id| {
            let dt = s.catalog.photo(*id)?.captured.as_deref().and_then(DateTime::parse_iso)?;
            let (g, _) = t.active_log.locate(capture_utc(&dt, t.offset_minutes), DEFAULT_MAX_GAP)?;
            let (lat, lon) = rounded(&g);
            Some((id.0, lat, lon))
        })
        .collect()
}

/// Seconds as an ISO date-time (no zone) at `offset` minutes from UTC.
pub fn iso(t: f64, offset: i64) -> String {
    lightcraft_catalog::dates::civil((t.floor() as i64).saturating_add(offset.saturating_mul(60)))
}

/// `+02:00` for 120 minutes.
pub fn fmt_offset(minutes: i64) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.unsigned_abs();
    format!("{sign}{:02}:{:02}", m / 60, m % 60)
}

/// A camera time zone offset parameter: `+HH:MM` text or a number of hours (at most ±18).
/// `Ok(None)`: empty or null (zone-less capture times are read as UTC).
pub(crate) fn parse_offset(cmd: &str, v: &Value) -> Result<Option<i64>> {
    match v {
        Value::Null => Ok(None),
        Value::String(o) if o.trim().is_empty() => Ok(None),
        Value::Number(h) => {
            let h = h.as_f64().filter(|h| h.is_finite()).ok_or_else(|| bad(cmd, "offset is a number of hours"))?;
            if h.abs() > 18.0 {
                return Err(bad(cmd, "offset is at most ±18 hours"));
            }
            Ok(Some((h * 60.0).round() as i64))
        }
        Value::String(o) => {
            let m = DateTime::parse_offset(o).ok_or_else(|| bad(cmd, format!("can't read offset `{o}` (e.g. +02:00 or -5)")))?;
            if m.unsigned_abs() > 18 * 60 {
                return Err(bad(cmd, "offset is at most ±18 hours"));
            }
            Ok(Some(i64::from(m)))
        }
        _ => Err(bad(cmd, "offset is `+HH:MM` or a number of hours")),
    }
}

/// The GPX text a command was given: `gpx` (the text) or `path` (a file, at most
/// [`MAX_GPX_BYTES`]), and the path.
pub(crate) fn read_gpx(cmd: &str, p: &Value) -> Result<Option<(String, Option<String>)>> {
    match (p.get("gpx"), p.get("path")) {
        (Some(Value::String(t)), _) => {
            if t.len() as u64 > MAX_GPX_BYTES {
                return Err(bad(cmd, format!("the track log is larger than {} MB", MAX_GPX_BYTES >> 20)));
            }
            Ok(Some((t.clone(), None)))
        }
        (None | Some(Value::Null), Some(Value::String(path))) => {
            let meta = std::fs::metadata(path).map_err(|e| bad(cmd, format!("can't read {path}: {e}")))?;
            if !meta.is_file() {
                return Err(bad(cmd, format!("{path} is not a file")));
            }
            if meta.len() > MAX_GPX_BYTES {
                return Err(bad(cmd, format!("{path} is larger than {} MB: too big for a track log", MAX_GPX_BYTES >> 20)));
            }
            let bytes = std::fs::read(path).map_err(|e| bad(cmd, format!("can't read {path}: {e}")))?;
            let text = String::from_utf8(bytes).map_err(|_| bad(cmd, format!("{path} is not a text (GPX) file")))?;
            Ok(Some((text, Some(path.clone()))))
        }
        (None | Some(Value::Null), None | Some(Value::Null)) => Ok(None),
        _ => Err(bad(cmd, "`path` is a file path and `gpx` the file's text")),
    }
}

/// Parse and prepare a track log.
pub(crate) fn load(cmd: &str, text: &str, path: Option<String>) -> Result<LoadedTrack> {
    let t0 = web_time::Instant::now();
    let log = parse_gpx(text).map_err(|e| bad(cmd, e.to_string()))?;
    if log.points.len() > MAX_TRACK_POINTS {
        return Err(bad(cmd, format!("the track log has {} timed points; at most {MAX_TRACK_POINTS} are loaded", log.points.len())));
    }
    if log.points.is_empty() {
        let why = if log.tracks == 0 {
            "the file has no tracks (<trk>)".to_string()
        } else if log.untimed > 0 {
            format!("the track log has no timed track points ({} without a time)", log.untimed)
        } else {
            "the track log has no track points".to_string()
        };
        return Err(bad(cmd, why));
    }
    // each track's lines: its segments, points in time order
    let mut lines: Vec<BTreeMap<u32, Vec<(f64, f64)>>> = vec![BTreeMap::new(); log.tracks];
    let mut tracks: Vec<TrackSummary> = log.names.iter().map(|n| TrackSummary { name: n.clone(), points: 0, segments: 0, span: None }).collect();
    for p in &log.points {
        let i = usize::try_from(p.track).unwrap_or(usize::MAX);
        if let Some(l) = lines.get_mut(i) {
            l.entry(p.segment).or_default().push((p.latitude, p.longitude));
        }
        if let Some(t) = tracks.get_mut(i) {
            t.points += 1;
            t.span = Some(match t.span {
                None => (p.time, p.time),
                Some((a, b)) => (a.min(p.time), b.max(p.time)),
            });
        }
    }
    for (t, l) in tracks.iter_mut().zip(&lines) {
        t.segments = l.len();
    }
    let shapes = lines.into_iter().map(|l| Arc::new(TrackShape::new(l.into_values()))).collect();
    let name = path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "GPX".to_string());
    let log = Arc::new(log);
    Ok(LoadedTrack {
        path,
        name,
        active_log: log.clone(),
        log,
        tracks,
        shapes,
        active: None,
        offset_minutes: None,
        load_ms: t0.elapsed().as_secs_f64() * 1000.0,
    })
}

/// `map.trackLog`'s answer: what is loaded.
pub fn report(t: Option<&LoadedTrack>, generation: u64) -> Value {
    let Some(t) = t else { return json!({"loaded": false, "generation": generation}) };
    let off = t.offset_minutes.unwrap_or(0);
    let tracks: Vec<Value> = t
        .tracks
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "index": i,
                "name": s.name,
                "points": s.points,
                "segments": s.segments,
                "start": s.span.map(|(a, _)| iso(a, 0) + "Z"),
                "end": s.span.map(|(_, b)| iso(b, 0) + "Z"),
                "duration": s.span.map(|(a, b)| b - a),
            })
        })
        .collect();
    let span = t.span();
    json!({
        "loaded": true,
        "path": t.path,
        "name": t.name,
        "points": t.log.points.len(),
        "untimed": t.log.untimed,
        "tracks": tracks,
        "active": t.active,
        "activePoints": t.active_log.points.len(),
        "start": span.map(|(a, _)| iso(a, 0) + "Z"),
        "end": span.map(|(_, b)| iso(b, 0) + "Z"),
        "duration": span.map(|(a, b)| b - a),
        "offset": t.offset_minutes.map(fmt_offset),
        "startLocal": span.map(|(a, _)| iso(a, off)),
        "endLocal": span.map(|(_, b)| iso(b, off)),
        "loadMs": t.load_ms,
        "generation": generation,
    })
}

/// `map.trackLog`: see its command spec.
pub(crate) fn track_log(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.trackLog";
    if !p.is_object() && !p.is_null() {
        return Err(bad(C, "params are an object"));
    }
    let clear = match p.get("clear") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad(C, "`clear` is true or false"))?,
    };
    // everything is checked before anything changes
    let loaded = match read_gpx(C, p)? {
        Some((text, path)) => Some(load(C, &text, path)?),
        None => None,
    };
    let offset = match p.get("offset") {
        None => None,
        Some(v) => Some(parse_offset(C, v)?),
    };
    let active = match p.get("active") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(a)) if a.eq_ignore_ascii_case("all") => Some(None),
        Some(v) => Some(Some(v.as_u64().and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(C, "`active` is a track's index or \"all\""))?)),
    };
    if clear && (loaded.is_some() || offset.is_some() || active.is_some()) {
        return Err(bad(C, "`clear` goes alone"));
    }
    let tracks_after = loaded.as_ref().or(s.map.track.as_ref()).map(|t| t.tracks.len());
    if let Some(Some(i)) = active {
        match tracks_after {
            None => return Err(bad(C, "no track log is loaded")),
            Some(n) if i >= n => return Err(bad(C, format!("no track {i}: the log has {n} (0…{})", n.saturating_sub(1)))),
            _ => {}
        }
    }
    if (offset.is_some() || active.is_some()) && tracks_after.is_none() {
        return Err(bad(C, "no track log is loaded (give `path`)"));
    }
    let before = s.map.track_generation;
    if clear && s.map.track.take().is_some() {
        s.map.track_generation += 1;
    }
    if let Some(mut t) = loaded {
        // a new file keeps the offset chosen for the last one (the same camera, most likely)
        t.offset_minutes = s.map.track.as_ref().and_then(|o| o.offset_minutes);
        s.map.track = Some(t);
    }
    if let Some(t) = s.map.track.as_mut() {
        if let Some(a) = active {
            t.set_active(a);
        }
        if let Some(o) = offset {
            t.offset_minutes = o;
        }
    }
    if s.map.track_generation == before
        && (p.get("path").is_some_and(|v| !v.is_null()) || p.get("gpx").is_some_and(|v| !v.is_null()) || offset.is_some() || active.is_some())
    {
        s.map.track_generation += 1;
    }
    Ok(report(s.map.track.as_ref(), s.map.track_generation))
}
