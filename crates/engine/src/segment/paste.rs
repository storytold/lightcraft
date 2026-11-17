//! Pasted AI masks are computed on the destination's proposed look before the batch is committed.
//! Keep the old settings until every selection succeeds: never display another photo's cached
//! shape, never leave half a paste behind, and keep the whole operation one undo step.

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_develop::{DevelopSettings, MaskShape};
use serde_json::{Value, json};

use crate::{Result, Session};

const MAX_SELECTIONS: usize = 10_000;
const TOO_MANY: &str = "AI mask pastes can queue at most 10,000 selections. Paste to fewer photos at once, or wait for queued pastes to finish.";
#[cfg(feature = "sam")]
const MAX_PASTES: usize = 64;

fn count_ai(d: &DevelopSettings) -> usize {
    d.masks.iter().flat_map(|m| &m.components).filter(|c| matches!(c.shape, MaskShape::Object { .. } | MaskShape::Prompt { .. })).count()
}

impl Session {
    pub(crate) fn paste_settings(&mut self, ids: &[PhotoId], partial: &Value, label: &str) -> Result<Value> {
        let copied_ai =
            if partial.get("masks").is_some() { count_ai(&lightcraft_develop::apply_partial(&DevelopSettings::default(), partial, 1.0)) } else { 0 };
        self.paste_into(ids, copied_ai, label, false, |before| lightcraft_develop::apply_partial(before, partial, 1.0))
    }

    /// Paste Mask: `mask` added to each photo's own masks (with the next free id), its Object
    /// and Describe components recomputed on each photo as [`Session::paste_settings`] does. The
    /// photos' own masks are left as they are.
    pub(crate) fn paste_mask(&mut self, ids: &[PhotoId], mask: &lightcraft_develop::Mask, label: &str) -> Result<Value> {
        let copied_ai = mask.components.iter().filter(|c| matches!(c.shape, MaskShape::Object { .. } | MaskShape::Prompt { .. })).count();
        self.paste_into(ids, copied_ai, label, true, |before| {
            let mut d = before.clone();
            let mut m = mask.clone();
            m.id = d.next_mask_id();
            d.masks.push(m);
            d
        })
    }

    /// Gives each photo of `ids` the settings `paste` makes of its own, as one undo step. The new
    /// settings hold `copied_ai` Object / Describe components a photo, which are recomputed on it
    /// first: in every mask, or only in its last one when `last_only`.
    fn paste_into(
        &mut self,
        ids: &[PhotoId],
        copied_ai: usize,
        label: &str,
        last_only: bool,
        paste: impl Fn(&DevelopSettings) -> DevelopSettings,
    ) -> Result<Value> {
        // Bound the work before cloning the clipboard's segmentation grids for every target.
        if copied_ai.saturating_mul(ids.len()) > MAX_SELECTIONS {
            return Err(crate::EngineError::Other(TOO_MANY.into()));
        }
        #[cfg(feature = "sam")]
        if copied_ai > 0 {
            self.check_paste_queue(ids, copied_ai)?;
        }
        let targets: Vec<_> = ids.iter().filter_map(|id| self.develop_of(*id).map(|before| (*id, paste(&before)))).collect();
        // Settings that did not include Masking must not recompute or discard existing masks.
        if copied_ai == 0 || targets.is_empty() {
            let ops: Vec<_> = targets.into_iter().filter_map(|(id, d)| self.develop_op(id, d, label)).collect();
            let changed = ops.len();
            self.commit(label, Op::Batch { ops })?;
            return Ok(json!({"changed": changed}));
        }
        self.segmenter.require_model()?;
        #[cfg(feature = "sam")]
        {
            let mut pending = Pending::recomputing(self, targets, label, last_only).map_err(crate::EngineError::Other)?;
            let changed = pending.targets.len();
            if self.segmenter.background {
                let task = pending.task.id();
                // Do not jump ahead of jobs waiting between the previous completion and the next poll.
                let queued = self.segmenter.paste.is_some() || !self.segmenter.paste_queue.is_empty();
                if queued {
                    pending.task.label("Queued AI mask paste");
                    self.segmenter.paste_queue.push_back(pending);
                } else {
                    pending.submit_next(self).map_err(crate::EngineError::Other)?;
                    self.segmenter.paste = Some(pending);
                }
                return Ok(json!({"pending": true, "changed": 0, "targets": changed, "queued": queued, "task": task}));
            }
            pending.submit_next(self).map_err(crate::EngineError::Other)?;
            while let Some(flight) = pending.flight.take() {
                let outcome =
                    flight.rx.recv_timeout(super::WAIT).map_err(|_| crate::EngineError::Other("the AI model took too long; try again".into()))?;
                self.segmenter.messages.extend(outcome.notice.clone());
                pending.accept(flight.selection, outcome).map_err(crate::EngineError::Other)?;
                pending.submit_next(self).map_err(crate::EngineError::Other)?;
            }
            pending.finish(self)?;
            Ok(json!({"changed": changed}))
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = last_only;
            Err(crate::EngineError::Other("AI masks are not available in this build".into()))
        }
    }

    #[cfg(feature = "sam")]
    fn check_paste_queue(&self, ids: &[PhotoId], copied_ai: usize) -> Result<()> {
        let pending = || self.segmenter.paste.iter().chain(&self.segmenter.paste_queue);
        if pending().count() >= MAX_PASTES {
            return Err(crate::EngineError::Other("At most 64 AI mask pastes can be pending. Wait or cancel a paste in Activity first.".into()));
        }
        // Bound all captured settings and inference work, not just each individual click.
        let selections = pending().fold(copied_ai.saturating_mul(ids.len()) as u64, |n, p| n.saturating_add(p.total));
        if selections > MAX_SELECTIONS as u64 {
            return Err(crate::EngineError::Other(TOO_MANY.into()));
        }
        if pending().any(|p| Arc::ptr_eq(&p.library, &self.library_identity) && p.targets.iter().any(|t| ids.contains(&t.id))) {
            return Err(crate::EngineError::Other(
                "A destination photo already has a pending AI mask paste. No new paste was queued; wait or cancel its paste in Activity first."
                    .into(),
            ));
        }
        if !self.segmenter.background && pending().next().is_some() {
            return Err(crate::EngineError::Other("Wait or cancel the pending AI mask pastes in Activity before a waiting-mode paste.".into()));
        }
        Ok(())
    }
}

#[cfg(feature = "sam")]
use {
    super::{Polled, Tag, worker},
    crate::activity::{Cancel, TaskGuard},
    std::{collections::VecDeque, sync::Arc, sync::mpsc, time::Instant},
};

#[cfg(feature = "sam")]
struct Target {
    id: PhotoId,
    before: Arc<DevelopSettings>,
    content: String,
    settings: DevelopSettings,
}

#[cfg(feature = "sam")]
#[derive(Clone, Copy)]
struct Selection {
    target: usize,
    mask: usize,
    component: usize,
}

#[cfg(feature = "sam")]
struct Flight {
    selection: Selection,
    rx: mpsc::Receiver<worker::Outcome>,
    started: Instant,
}

#[cfg(feature = "sam")]
pub(super) struct Pending {
    targets: Vec<Target>,
    queue: VecDeque<Selection>,
    flight: Option<Flight>,
    label: String,
    task: TaskGuard,
    total: u64,
    done: u64,
    library: Arc<()>,
}

#[cfg(feature = "sam")]
impl Pending {
    #[cfg(test)]
    fn new(s: &Session, proposed: Vec<(PhotoId, DevelopSettings)>, label: &str) -> std::result::Result<Self, String> {
        Self::recomputing(s, proposed, label, false)
    }

    /// [`Pending::new`], recomputing only each photo's last mask when `last_only` (Paste Mask:
    /// the photo's own masks are not touched).
    fn recomputing(s: &Session, proposed: Vec<(PhotoId, DevelopSettings)>, label: &str, last_only: bool) -> std::result::Result<Self, String> {
        let mut targets = Vec::new();
        let mut queue = VecDeque::new();
        for (id, mut settings) in proposed {
            let photo = s.catalog.photo(id).ok_or("no such photo")?;
            // Legacy mask clicks must be moved onto the photo before being sent to the model.
            let drawn = settings.clone();
            s.attach_masks(id, &drawn, &mut settings);
            let first = if last_only { settings.masks.len().saturating_sub(1) } else { 0 };
            for (mask, m) in settings.masks.iter_mut().enumerate().skip(first) {
                for (component, c) in m.components.iter_mut().enumerate() {
                    match &mut c.shape {
                        MaskShape::Object { hint, exclude, seg, detail, .. } => {
                            if hint.is_empty() {
                                return Err(format!(
                                    "Object mask “{}” needs an include point. Select the object on the source photo before copying it.",
                                    m.name
                                ));
                            }
                            if hint.len().saturating_add(exclude.len()) > super::MAX_CLICKS {
                                return Err(format!("an Object selection takes at most {} clicks", super::MAX_CLICKS));
                            }
                            *seg = None;
                            detail.clear();
                        }
                        MaskShape::Prompt { text, seg, detail, .. } => {
                            if text.trim().is_empty() {
                                return Err(format!("Describe mask “{}” needs a description before it can be pasted.", m.name));
                            }
                            *seg = None;
                            detail.clear();
                        }
                        _ => continue,
                    }
                    // Bound queued inference work even for a hostile clipboard.
                    if queue.len() >= MAX_SELECTIONS {
                        return Err(TOO_MANY.into());
                    }
                    queue.push_back(Selection { target: targets.len(), mask, component });
                }
            }
            targets.push(Target { id, before: photo.develop.clone(), content: crate::media::content_key(photo), settings });
        }
        let task = s.activity.start("segment", "Recomputing AI masks", Cancel::Yes);
        if let Some(photo) = targets.first().and_then(|t| s.catalog.photo(t.id)) {
            task.detail(&photo.file_name);
        }
        let total = queue.len() as u64;
        task.progress(0, total);
        Ok(Self { targets, queue, flight: None, label: label.into(), task, total, done: 0, library: s.library_identity.clone() })
    }

    fn submit_next(&mut self, s: &mut Session) -> std::result::Result<(), String> {
        if self.task.is_cancelled() {
            return Err("AI mask paste cancelled; no settings were changed.".into());
        }
        // A queued request keeps the settings/photo snapshot from its click, not later edits.
        if !self.queue.is_empty() {
            self.check_current(s).map_err(|e| e.to_string())?;
        }
        let Some(selection) = self.queue.pop_front() else { return Ok(()) };
        if self.done == 0 {
            self.task.label("Recomputing AI masks");
        }
        let target = self.targets.get(selection.target).ok_or("no such paste target")?;
        let shape = target.settings.masks.get(selection.mask).and_then(|m| m.components.get(selection.component)).ok_or("no such mask")?;
        let kind = match &shape.shape {
            MaskShape::Object { hint, exclude, .. } => worker::Kind::Clicks(Session::model_clicks(&super::clicks_of(hint, exclude))),
            MaskShape::Prompt { text, .. } => worker::Kind::Text(text.clone()),
            _ => return Err("not an AI selection".into()),
        };
        let (key, settings) = s.segment_key_for(target.id, &target.settings).ok_or("no such photo")?;
        let render = s.preview_job(target.id, super::INPUT_EDGE, super::INPUT_EDGE, false, &settings).ok_or("no such photo")?;
        let photo = s.catalog.photo(target.id).ok_or("no such photo")?;
        self.task.detail(&photo.file_name);
        let missing = match &photo.source {
            lightcraft_catalog::Source::File { path } if !std::path::Path::new(path).exists() => Some(path.clone()),
            _ => None,
        };
        let (tx, rx) = mpsc::channel();
        let job = worker::Job { dir: s.segmenter.model_dir()?, key, render: Some(render), missing, kind, tag: Tag::Wait, reply: tx };
        s.segmenter.worker.submit(job)?;
        self.flight = Some(Flight { selection, rx, started: Instant::now() });
        Ok(())
    }

    fn accept(&mut self, selection: Selection, outcome: worker::Outcome) -> std::result::Result<(), String> {
        let result = outcome.result?;
        let target = self.targets.get_mut(selection.target).ok_or("no such paste target")?;
        let c = target.settings.masks.get_mut(selection.mask).and_then(|m| m.components.get_mut(selection.component)).ok_or("no such mask")?;
        // No match is an empty selection, never the source photo's selection or Object's fallback.
        let result = Some(result.unwrap_or_else(|| lightcraft_develop::SegMask::from_logits(1, &[-16.0])));
        match &mut c.shape {
            MaskShape::Object { seg, .. } | MaskShape::Prompt { seg, .. } => *seg = result,
            _ => return Err("not an AI selection".into()),
        }
        self.done = self.done.saturating_add(1);
        self.task.progress(self.done, self.total);
        Ok(())
    }

    fn finish(self, s: &mut Session) -> Result<()> {
        // Once commit starts it cannot be interrupted; an already accepted cancel still wins.
        self.task.set_cancellable(false);
        if self.task.is_cancelled() {
            return Err(crate::EngineError::Other("AI mask paste cancelled; no settings were changed.".into()));
        }
        // Even an unchanged gesture holds a pre-paste snapshot: ending or cancelling it would
        // restore or re-commit pasted settings outside this batch's undo step.
        if s.interaction.as_ref().is_some_and(|i| self.targets.iter().any(|t| t.id == i.photo)) {
            return Err(crate::EngineError::Other(
                "A destination photo has an open edit interaction. No settings were pasted; finish or cancel the edit and try again.".into(),
            ));
        }
        self.check_current(s)?;
        let ops = self.targets.into_iter().filter_map(|t| s.develop_op(t.id, t.settings, &self.label)).collect();
        s.commit(&self.label, Op::Batch { ops })
    }

    fn check_current(&self, s: &Session) -> Result<()> {
        // A user edit, deletion, file replacement or library switch while waiting or computing
        // must not be overwritten. Check the whole batch before starting work or committing it.
        if !Arc::ptr_eq(&self.library, &s.library_identity)
            || !self.targets.iter().all(|t| {
                s.catalog.photo(t.id).is_some_and(|p| !p.deleted && Arc::ptr_eq(&p.develop, &t.before) && crate::media::content_key(p) == t.content)
            })
        {
            return Err(crate::EngineError::Other(
                "A destination photo changed while its AI masks were computed. No settings were pasted; try again.".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(feature = "sam")]
impl Session {
    pub(super) fn poll_mask_paste(&mut self, polled: &mut Polled) {
        let mut cancelled = false;
        self.segmenter.paste_queue.retain(|pending| {
            if pending.task.is_cancelled() {
                cancelled = true;
                false
            } else {
                true
            }
        });
        if cancelled {
            polled.messages.push("Cancelled a queued AI mask paste.".into());
        }
        let Some(mut pending) = self.segmenter.paste.take().or_else(|| self.segmenter.paste_queue.pop_front()) else { return };
        // An unanswered worker must not keep a cancelled paste's row and queue slot alive.
        if pending.task.is_cancelled() {
            polled.messages.push("AI mask paste cancelled; no settings were changed.".into());
            return;
        }
        let result = if let Some(flight) = pending.flight.take() {
            match flight.rx.try_recv() {
                Ok(outcome) => {
                    polled.messages.extend(outcome.notice.clone());
                    pending.accept(flight.selection, outcome)
                }
                Err(mpsc::TryRecvError::Empty) if flight.started.elapsed() < super::WAIT => {
                    pending.flight = Some(flight);
                    self.segmenter.paste = Some(pending);
                    return;
                }
                Err(_) => Err("The AI mask worker stopped or took too long. No settings were pasted; try again.".into()),
            }
        } else {
            Ok(())
        };
        if let Err(e) = result.and_then(|()| pending.submit_next(self)) {
            polled.messages.push(e);
            return;
        }
        if pending.flight.is_some() {
            self.segmenter.paste = Some(pending);
        } else {
            let log_start = self.pending_log.len();
            match pending.finish(self) {
                Ok(()) => {
                    polled.changed = true;
                    if self.xmp.auto_write {
                        self.auto_write_sidecars_since(log_start);
                    }
                    if let Err(e) = self.persist() {
                        polled.messages.push(e.to_string());
                    }
                }
                Err(e) => polled.messages.push(e.to_string()),
            }
        }
    }
}
