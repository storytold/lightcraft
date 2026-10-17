//! Native implementation: container dispatch ([`crate::demux`]), codec dispatch
//! ([`crate::decode`]), seeking.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use crate::decode::{self, Decoder, Pic};
use crate::demux::{self, Clip};
use crate::{Frame, VideoError, VideoInfo, colour};

/// Decoded pictures kept while searching for the target (B-frame reordering needs a few).
const MAX_PENDING: usize = 32;
/// Samples decoded past the target before giving up on finding its picture.
const MAX_OVERRUN: usize = 64;

/// Run a FilmCraft call; a panic inside it becomes [`VideoError::Internal`].
pub(crate) fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T, VideoError>) -> Result<T, VideoError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
            Err(VideoError::Internal(format!("{what}: {msg}")))
        }
    }
}

pub fn probe(path: &Path) -> Result<VideoInfo, VideoError> {
    guarded("demux", || demux::open(path)).map(|c| c.info)
}

pub struct Reader {
    clip: Clip,
    codec: decode::Codec,
    dec: Option<Decoder>,
    /// Next sample (decode order) to feed `dec`; meaningful only while `dec` is Some.
    next: usize,
    /// Sync sample the current decoder started from.
    gop: usize,
    pending: Vec<Pic>,
    /// Pts of the last picture returned by the current decoder.
    last: Option<i64>,
    /// Position in presentation order (`table.order`) of the frame [`Reader::next_frame`] returns.
    cursor: usize,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self, VideoError> {
        let clip = guarded("demux", || demux::open(path))?;
        let Some(codec) = clip.codec.clone() else {
            let why = clip.unsupported.clone().unwrap_or_else(|| format!("{} video", clip.info.codec));
            return Err(VideoError::Unsupported(why));
        };
        Ok(Self { clip, codec, dec: None, next: 0, gop: 0, pending: Vec::new(), last: None, cursor: 0 })
    }

    pub fn info(&self) -> &VideoInfo {
        &self.clip.info
    }

    /// Pts of the first shown frame: frame times count from it.
    fn origin(&self) -> i64 {
        let t = &self.clip.table;
        t.order.first().and_then(|&i| t.pts(i)).unwrap_or(0)
    }

    pub fn frame_times(&self) -> Vec<f64> {
        let t = &self.clip.table;
        let o = self.origin();
        t.order.iter().filter_map(|&i| t.pts(i)).map(|p| t.seconds(p.saturating_sub(o))).collect()
    }

    /// Position in presentation order of the frame shown at `t`.
    fn position_at(&self, t: f64) -> Option<usize> {
        let table = &self.clip.table;
        let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
        let idx = table.sample_at(table.units(t).saturating_add(self.origin()))?;
        let target = table.pts(idx)?;
        Some(table.order.partition_point(|&i| table.pts(i).is_some_and(|p| p < target)))
    }

    pub fn seek(&mut self, t: f64) {
        self.cursor = self.position_at(t).unwrap_or(0);
    }

    pub fn next_frame(&mut self, max_side: Option<u32>) -> Result<Option<Frame>, VideoError> {
        let Some(&idx) = self.clip.table.order.get(self.cursor) else { return Ok(None) };
        self.cursor = self.cursor.saturating_add(1);
        self.decode_index(idx, max_side).map(Some)
    }

    pub fn frame_at(&mut self, t: f64, max_side: Option<u32>) -> Result<Frame, VideoError> {
        let pos = self.position_at(t).ok_or_else(|| VideoError::Invalid("video track has no samples".into()))?;
        let idx = *self.clip.table.order.get(pos).ok_or_else(|| VideoError::Invalid("sample index".into()))?;
        self.cursor = pos.saturating_add(1);
        self.decode_index(idx, max_side)
    }

    /// Decode shown sample `idx` (decode order index).
    fn decode_index(&mut self, idx: usize, max_side: Option<u32>) -> Result<Frame, VideoError> {
        let r = guarded("decode", || self.frame_at_inner(idx, max_side));
        if r.is_err() {
            // a failed or panicked decoder is in an unknown state: start over next time
            self.dec = None;
            self.pending.clear();
        }
        r
    }

    fn frame_at_inner(&mut self, idx: usize, max_side: Option<u32>) -> Result<Frame, VideoError> {
        let table = &self.clip.table;
        let target = table.pts(idx).ok_or_else(|| VideoError::Invalid("sample index".into()))?;
        let gop = table.sync_before(idx);
        let count = table.len();

        if let Some(p) = self.take_pending(target) {
            return self.finish(p, max_side);
        }
        // reuse the decoder when the target lies ahead and its GOP starts no later than the next
        // sample to feed (sequential playback runs on across GOPs without re-seeking)
        let reuse = self.dec.is_some() && self.gop <= gop && gop <= self.next && self.last.is_none_or(|l| target > l);
        if reuse {
            self.gop = gop;
        } else {
            self.dec = Some(Decoder::new(&self.codec)?);
            self.next = gop;
            self.gop = gop;
            self.pending.clear();
            self.last = None;
        }
        let colr = self.clip.colr;
        let mut overrun = 0usize;
        while self.next < count {
            let i = self.next;
            self.next += 1;
            let data = self.clip.read(i)?;
            let pts = self.clip.table.pts(i).unwrap_or(0);
            let Some(dec) = self.dec.as_mut() else { break };
            let pics = dec.decode(&data, pts, colr)?;
            self.push(pics);
            if let Some(p) = self.take_pending(target) {
                return self.finish(p, max_side);
            }
            if i >= idx {
                overrun += 1;
                if overrun > MAX_OVERRUN {
                    break;
                }
            }
        }
        if let Some(dec) = self.dec.as_mut() {
            let pics = dec.flush(colr);
            self.push(pics);
            self.dec = None;
        }
        // end of stream: the target's picture, else the nearest one decoded
        let best = self.take_pending(target).or_else(|| {
            let k = self.pending.iter().enumerate().min_by_key(|(_, p)| p.pts.abs_diff(target)).map(|(k, _)| k)?;
            Some(self.pending.swap_remove(k))
        });
        match best {
            Some(p) => self.finish(p, max_side),
            None => Err(VideoError::Decode("no picture decoded at this time".into())),
        }
    }

    fn push(&mut self, pics: Vec<Pic>) {
        self.pending.extend(pics);
        if self.pending.len() > MAX_PENDING {
            self.pending.sort_by_key(|p| p.pts);
            let drop = self.pending.len() - MAX_PENDING;
            self.pending.drain(..drop);
        }
    }

    fn take_pending(&mut self, target: i64) -> Option<Pic> {
        let k = self.pending.iter().position(|p| p.pts == target)?;
        let p = self.pending.swap_remove(k);
        self.last = Some(target);
        // pictures before the target are no longer needed for forward playback
        self.pending.retain(|q| q.pts > target);
        Some(p)
    }

    fn finish(&self, p: Pic, max_side: Option<u32>) -> Result<Frame, VideoError> {
        let mut f = p.frame;
        if f.width == 0 || f.height == 0 || !decode::complete(&f) {
            return Err(VideoError::Decode("decoder returned an incomplete picture".into()));
        }
        let info = &self.clip.info;
        let mut par = (info.par_num.max(1), info.par_den.max(1));
        let rot = info.rotation;
        if rot != 0 {
            f = f.rotated(rot);
            if rot % 2 == 1 {
                par = (par.1, par.0);
            }
        }
        let (dw, dh) = demux::display_size(f.width, f.height, par);
        let n = colour::decimation(dw, dh, max_side);
        // an even box decimation already blends the two fields; otherwise rebuild one field
        if let Some(top_first) = p.field
            && n % 2 == 1
        {
            decode::deinterlace(&mut f, top_first);
        }
        let mut image = colour::to_rgb(&f, n);
        if par.0 != par.1 {
            let (tw, th) = fit(dw, dh, max_side);
            image = lcc_raster::resample::resize(&image, tw as usize, th as usize, lcc_raster::resample::Filter::Bilinear);
        }
        let time = self.clip.table.seconds(p.pts.saturating_sub(self.origin()));
        Ok(Frame { image, time: time.max(0.0) })
    }
}

/// `w × h` scaled down (aspect kept) so the longer side is at most `max_side`.
fn fit(w: u32, h: u32, max_side: Option<u32>) -> (u32, u32) {
    let long = w.max(h).max(1);
    match max_side {
        Some(m) if m > 0 && long > m => {
            let s = f64::from(m) / f64::from(long);
            (((f64::from(w) * s).round() as u32).max(1), ((f64::from(h) * s).round() as u32).max(1))
        }
        _ => (w.max(1), h.max(1)),
    }
}
