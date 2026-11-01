//! The render device, and getting it back after a failure.
//!
//! A device that is lost (driver reset, GPU hang, the Windows watchdog) or fails a render outright
//! used to turn the GPU off for the rest of the session: one bad moment during a 571-photo export
//! and every later photo rendered on the CPU. Now the failed render is redone on the CPU (as
//! before), the device is marked lost ([`lose`]), and after a short pause ([`backoff`]) the next
//! render creates a new device and goes on using it. Each step is told in plain words
//! ([`events`]); after [`MAX_RECOVERIES`] failed recoveries the GPU stays off until LightCraft is
//! started again.
//!
//! Buffers and cached stages made on a replaced device are never used on its successor: each device
//! has its own generation number ([`crate::ctx::Gpu::generation`]).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ctx::Gpu;

/// The most times a lost device is replaced in one session.
pub const MAX_RECOVERIES: u32 = 5;

/// What the render device is now.
enum State {
    /// Not created yet.
    NotTried,
    Ready(Arc<Gpu>),
    /// Creating it failed at the start (no adapter, software adapter, …): the CPU renders.
    Failed(String),
}

struct Slot {
    state: State,
    /// The device failed and is to be replaced (the GPU is not used meanwhile).
    lost: Option<Lost>,
    /// Devices created after a loss so far.
    recoveries: u32,
}

struct Lost {
    reason: String,
    /// When it failed, or when the last attempt to replace it did.
    at: Instant,
    /// Attempts to replace it that failed.
    attempts: u32,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot { state: State::NotTried, lost: None, recoveries: 0 });
/// One thread creates a device at a time (the others render on the CPU meanwhile, or wait for the first one).
static CREATING: Mutex<()> = Mutex::new(());

fn slot() -> std::sync::MutexGuard<'static, Slot> {
    SLOT.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The pause before replacing a lost device: longer after each failed attempt (3 s, 6 s, 12 s …).
pub fn backoff(attempts: u32) -> Duration {
    Duration::from_secs(3u64 << attempts.min(6))
}

/// The device if one is ready and not lost; never creates one.
pub(crate) fn current() -> Option<Arc<Gpu>> {
    let s = slot();
    match (&s.state, &s.lost) {
        (State::Ready(g), None) => Some(g.clone()),
        _ => None,
    }
}

/// The device if one has been created, even a lost one (its buffers go back to its own pool).
pub(crate) fn existing() -> Option<Arc<Gpu>> {
    match &slot().state {
        State::Ready(g) => Some(g.clone()),
        _ => None,
    }
}

/// Whether device creation has been tried (whatever came of it).
pub(crate) fn tried() -> bool {
    !matches!(slot().state, State::NotTried)
}

/// Why creating the device failed at the start, if it did.
pub(crate) fn creation_error() -> Option<String> {
    match &slot().state {
        State::Failed(e) => Some(e.clone()),
        _ => None,
    }
}

/// Why the device is lost and not replaced yet, if it is.
pub(crate) fn lost_reason() -> Option<String> {
    slot().lost.as_ref().map(|l| l.reason.clone())
}

/// Whether the device of generation `generation` is the one in use (callbacks of a replaced
/// device are ignored).
pub(crate) fn is_current(generation: u64) -> bool {
    matches!(&slot().state, State::Ready(g) if g.generation == generation)
}

/// The device, created with `create` the first time; after a loss, a new one once the pause is
/// over (`None` meanwhile: render on the CPU). The first one is made on this thread (others wait for it), a
/// replacement on a thread of its own.
pub(crate) fn get(create: fn() -> Result<Gpu, String>) -> Option<Arc<Gpu>> {
    {
        let s = slot();
        match (&s.state, &s.lost) {
            (State::Ready(g), None) => return Some(g.clone()),
            (State::Failed(_), None) => return None,
            // lost: the CPU renders until the new device is ready (made on a thread of its own:
            // building the kernels can take tens of seconds, which no render should wait for)
            (_, Some(l)) => {
                if due(l, s.recoveries) {
                    drop(s);
                    start_recovery(create);
                }
                return None;
            }
            (State::NotTried, None) => {}
        }
    }
    // the first device: made here, other renders wait for it (they would otherwise all render on the CPU)
    let _one = CREATING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match &slot().state {
        State::Ready(g) => return Some(g.clone()),
        State::Failed(_) => return None,
        State::NotTried => {}
    }
    let made = create();
    let mut s = slot();
    match made {
        Ok(g) => {
            let g = Arc::new(g);
            s.state = State::Ready(g.clone());
            drop(s);
            if lost_while_made(g.generation) {
                return None;
            }
            Some(g)
        }
        Err(why) => {
            log::warn!("gpu: rendering on the CPU: {why}");
            s.state = State::Failed(why);
            None
        }
    }
}

/// A device lost between its callbacks being set and its being put in place (the loss found no
/// device in use to mark) is marked lost now, so it isn't used (review of PR #3565). `true`: it was.
fn lost_while_made(generation: u64) -> bool {
    if !crate::ctx::is_lost(generation) {
        return false;
    }
    lose(generation, "the device was lost while it was being made");
    true
}

/// A replacement is being made ([`start_recovery`]).
static RECOVERING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Make a new device for the lost one on a thread of its own (once at a time).
fn start_recovery(create: fn() -> Result<Gpu, String>) {
    use std::sync::atomic::Ordering;
    if RECOVERING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new().name("lc-gpu-recover".into()).spawn(move || {
        // (not once the app is closing: creating a device is a long call into the driver)
        if let Some(_work) = crate::exit::enter() {
            let _one = CREATING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let still_due = {
                let s = slot();
                s.lost.as_ref().is_some_and(|l| due(l, s.recoveries))
            };
            if still_due {
                replace(create());
            }
        }
        RECOVERING.store(false, Ordering::Release);
    });
    if spawned.is_err() {
        RECOVERING.store(false, Ordering::Release);
    }
}

/// Put a new device in place of the lost one, or count the failed attempt.
fn replace(made: Result<Gpu, String>) {
    let mut s = slot();
    match made {
        Ok(g) => {
            let generation = g.generation;
            s.state = State::Ready(Arc::new(g));
            if let Some(l) = s.lost.take() {
                s.recoveries += 1;
                drop(s);
                log::warn!("gpu: a new device replaces the lost one ({})", l.reason);
                event(EventKind::Recovered, "LightCraft restarted the graphics card and uses it again.".into());
            } else {
                drop(s);
            }
            lost_while_made(generation);
        }
        Err(why) => {
            if let Some(l) = s.lost.as_mut() {
                l.attempts += 1;
                l.at = Instant::now();
            }
            let tried = s.recoveries.saturating_add(s.lost.as_ref().map_or(0, |l| l.attempts));
            drop(s);
            log::warn!("gpu: replacing the lost device failed ({why})");
            if tried >= MAX_RECOVERIES {
                event(EventKind::GaveUp, given_up_text());
            }
        }
    }
}

/// Use `g` (the window's device, created elsewhere) unless a device exists already. `Err`: the
/// one that exists (or the failure to create one) stays.
pub(crate) fn adopt(make: impl FnOnce() -> Result<Gpu, String>) -> Result<Arc<Gpu>, String> {
    let _one = CREATING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let State::Ready(g) = &slot().state {
        return Ok(g.clone());
    }
    if let State::Failed(e) = &slot().state {
        return Err(e.clone());
    }
    let made = make();
    let mut s = slot();
    match made {
        Ok(g) => {
            let g = Arc::new(g);
            s.state = State::Ready(g.clone());
            Ok(g)
        }
        Err(e) => {
            s.state = State::Failed(e.clone());
            Err(e)
        }
    }
}

fn due(l: &Lost, recoveries: u32) -> bool {
    !exhausted(l, recoveries) && l.at.elapsed() >= backoff(l.attempts)
}

/// No more attempts will be made to replace `l` (`recoveries` so far): successful recoveries and
/// failed attempts together reached [`MAX_RECOVERIES`].
fn exhausted(l: &Lost, recoveries: u32) -> bool {
    recoveries.saturating_add(l.attempts) >= MAX_RECOVERIES
}

/// The lost device will not be replaced this session ([`exhausted`]): the CPU renders from now on.
pub(crate) fn gave_up() -> bool {
    let s = slot();
    s.lost.as_ref().is_some_and(|l| exhausted(l, s.recoveries))
}

fn given_up_text() -> String {
    format!(
        "The graphics card failed {MAX_RECOVERIES} times; LightCraft renders on the processor until it is started again. Exports go on, more slowly."
    )
}

/// The device of generation `generation` failed (`generation` 0: whichever is in use): stop using
/// it, and replace it after a pause. Reported once per device.
pub(crate) fn lose(generation: u64, reason: &str) {
    let mut s = slot();
    let State::Ready(g) = &s.state else { return };
    if generation != 0 && g.generation != generation {
        return; // a device already replaced
    }
    if s.lost.is_some() {
        return;
    }
    s.lost = Some(Lost { reason: reason.to_string(), at: Instant::now(), attempts: 0 });
    *LAST_LOSS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.to_string());
    let left = MAX_RECOVERIES.saturating_sub(s.recoveries);
    drop(s);
    log::error!("gpu: device failure, rendering on the CPU until it is replaced: {reason}");
    event(
        EventKind::Lost,
        if left == 0 {
            given_up_text()
        } else {
            format!(
                "The graphics card stopped responding ({}). The photos it was working on are made on the processor instead; LightCraft restarts the graphics card in a few seconds.",
                short(reason)
            )
        },
    );
}

/// The first line of a driver message, at most 160 characters.
fn short(reason: &str) -> String {
    let line = reason.lines().next().unwrap_or("").trim();
    match line.char_indices().nth(160) {
        Some((i, _)) => format!("{}…", line.get(..i).unwrap_or(line)),
        None => line.to_string(),
    }
}

/// The latest device failure in this session, even one a new device has replaced since.
static LAST_LOSS: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn last_loss() -> Option<String> {
    LAST_LOSS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// Devices created after a loss so far in this session.
pub(crate) fn recoveries() -> u32 {
    slot().recoveries
}

/// What happened to the GPU, in plain words for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuEvent {
    /// Its number in the session (1, 2, …): new events have higher numbers.
    pub seq: u64,
    pub kind: EventKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// The device failed: renders go to the CPU until it is replaced.
    Lost,
    /// A new device replaced the lost one.
    Recovered,
    /// No more attempts: the CPU renders for the rest of the session.
    GaveUp,
}

static EVENTS: Mutex<(u64, Vec<GpuEvent>)> = Mutex::new((0, Vec::new()));
/// Events kept (the oldest go).
const EVENTS_KEPT: usize = 32;

pub(crate) fn event(kind: EventKind, text: String) {
    let mut e = EVENTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    e.0 += 1;
    let seq = e.0;
    e.1.push(GpuEvent { seq, kind, text });
    if e.1.len() > EVENTS_KEPT {
        e.1.remove(0);
    }
}

/// The events after number `after` (0: all that are kept).
pub(crate) fn events_after(after: u64) -> Vec<GpuEvent> {
    EVENTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1.iter().filter(|e| e.seq > after).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pause_grows_and_is_bounded() {
        assert_eq!(backoff(0), Duration::from_secs(3));
        assert_eq!(backoff(1), Duration::from_secs(6));
        assert_eq!(backoff(100), Duration::from_secs(3 << 6));
    }

    /// Review of PR #3499: five failed attempts to replace a lost device (no recovery at all)
    /// end the attempts as five recoveries do, and the status says "stopped" then.
    #[test]
    fn failed_attempts_end_the_recoveries_too() {
        let lost = |attempts| Lost { reason: "lost".into(), at: Instant::now(), attempts };
        assert!(!exhausted(&lost(0), 0));
        assert!(!exhausted(&lost(4), 0));
        assert!(exhausted(&lost(MAX_RECOVERIES), 0), "every attempt failed");
        assert!(exhausted(&lost(2), MAX_RECOVERIES - 2), "recoveries and failed attempts count together");
        assert!(!due(&lost(MAX_RECOVERIES), 0), "no attempt is due once they are used up");
    }

    #[test]
    fn a_long_driver_message_is_cut_to_one_line() {
        assert_eq!(short("device lost\nmore detail"), "device lost");
        let long = "x".repeat(400);
        assert_eq!(short(&long).chars().count(), 161);
        assert_eq!(short("é".repeat(200).as_str()).chars().count(), 161, "cut at a character boundary");
    }

    #[test]
    fn events_are_numbered_and_the_oldest_go() {
        let before = events_after(0).last().map_or(0, |e| e.seq);
        for i in 0..40 {
            event(EventKind::Recovered, format!("test {i}"));
        }
        let mine = events_after(before);
        assert!(mine.len() <= EVENTS_KEPT && !mine.is_empty());
        assert!(mine.windows(2).all(|w| w[0].seq < w[1].seq));
        assert_eq!(mine.last().map(|e| e.text.as_str()), Some("test 39"));
    }
}
