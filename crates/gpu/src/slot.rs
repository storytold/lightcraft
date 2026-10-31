//! The render devices — one per [`Role`]: the editing card's and the second card's — and getting
//! each back after a failure.
//!
//! A device that is lost (driver reset, GPU hang, the Windows watchdog) or fails a render outright
//! used to turn the GPU off for the rest of the session: one bad moment during a 571-photo export
//! and every later photo rendered on the CPU. Now the failed render is redone on the CPU (as
//! before), the device is marked lost ([`lose`]), and after a short pause ([`backoff`]) the next
//! render creates a new device and goes on using it. Each step is told in plain words
//! ([`events`]); after [`MAX_RECOVERIES`] failed recoveries that device stays off until LightCraft
//! is started again. Each role recovers on its own: one NVIDIA driver reset can drop both cards,
//! and each comes back by itself while the CPU carries on.
//!
//! Buffers and cached stages made on a replaced device are never used on its successor: each device
//! has its own generation number ([`crate::ctx::Gpu::generation`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::Role;
use crate::ctx::Gpu;

/// The most times a lost device is replaced in one session (per role).
pub const MAX_RECOVERIES: u32 = 5;

/// What a role's device is now.
enum State {
    /// Not created yet.
    NotTried,
    Ready(Arc<Gpu>),
    /// Creating it failed (no adapter, software adapter, kernels that don't build…): the CPU
    /// renders (the editing card), or nothing helps (the second card).
    Failed(String),
    /// There is none to make, and why (the second card: turned off, or no other card).
    Absent(String),
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

static SLOTS: [Mutex<Slot>; 2] =
    [Mutex::new(Slot { state: State::NotTried, lost: None, recoveries: 0 }), Mutex::new(Slot { state: State::NotTried, lost: None, recoveries: 0 })];
/// One thread creates a role's device at a time (the others render on the CPU meanwhile, or wait
/// for the first one).
static CREATING: [Mutex<()>; 2] = [Mutex::new(()), Mutex::new(())];
/// A replacement is being made ([`start_recovery`]).
static RECOVERING: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

fn at(role: Role) -> usize {
    match role {
        Role::Primary => 0,
        Role::Helper => 1,
    }
}

fn slot(role: Role) -> std::sync::MutexGuard<'static, Slot> {
    SLOTS[at(role)].lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The pause before replacing a lost device: longer after each failed attempt (3 s, 6 s, 12 s …).
pub fn backoff(attempts: u32) -> Duration {
    Duration::from_secs(3u64 << attempts.min(6))
}

/// The device if one is ready and not lost; never creates one.
pub(crate) fn current(role: Role) -> Option<Arc<Gpu>> {
    let s = slot(role);
    match (&s.state, &s.lost) {
        (State::Ready(g), None) => Some(g.clone()),
        _ => None,
    }
}

/// The device if one has been created, even a lost one (its buffers go back to its own pool).
pub(crate) fn existing(role: Role) -> Option<Arc<Gpu>> {
    match &slot(role).state {
        State::Ready(g) => Some(g.clone()),
        _ => None,
    }
}

/// The device of generation `generation`, of whichever role, if it still exists.
pub(crate) fn with_generation(generation: u64) -> Option<Arc<Gpu>> {
    Role::ALL.into_iter().find_map(|r| existing(r).filter(|g| g.generation == generation))
}

/// Whether device creation has been tried (whatever came of it).
pub(crate) fn tried(role: Role) -> bool {
    !matches!(slot(role).state, State::NotTried)
}

/// Why creating the device failed, if it did.
pub(crate) fn creation_error(role: Role) -> Option<String> {
    match &slot(role).state {
        State::Failed(e) => Some(e.clone()),
        _ => None,
    }
}

/// Why there is no device to make, if there isn't (the second card off or not found).
pub(crate) fn absent_reason(role: Role) -> Option<String> {
    match &slot(role).state {
        State::Absent(e) => Some(e.clone()),
        _ => None,
    }
}

/// Why the device is lost and not replaced yet, if it is.
pub(crate) fn lost_reason(role: Role) -> Option<String> {
    slot(role).lost.as_ref().map(|l| l.reason.clone())
}

/// Whether the device of generation `generation` is one in use (callbacks of a replaced device
/// are ignored).
pub(crate) fn is_current(generation: u64) -> bool {
    Role::ALL.into_iter().any(|r| matches!(&slot(r).state, State::Ready(g) if g.generation == generation))
}

/// The device, created with `create` the first time (`first_here`: on this thread, other renders
/// waiting for it; else not at all — the second card is made only by its warm-up); after a loss,
/// a new one once the pause is over (`None` meanwhile: render elsewhere). A replacement is made on
/// a thread of its own.
pub(crate) fn get(role: Role, create: fn(Role) -> Result<Gpu, String>, first_here: bool) -> Option<Arc<Gpu>> {
    {
        let s = slot(role);
        match (&s.state, &s.lost) {
            (State::Ready(g), None) => return Some(g.clone()),
            (State::Failed(_) | State::Absent(_), None) => return None,
            // lost: the CPU renders until the new device is ready (made on a thread of its own:
            // building the kernels can take tens of seconds, which no render should wait for)
            (_, Some(l)) => {
                if due(l, s.recoveries) {
                    drop(s);
                    start_recovery(role, create);
                }
                return None;
            }
            (State::NotTried, None) if !first_here => return None,
            (State::NotTried, None) => {}
        }
    }
    // the first device: made here, other renders wait for it (they would otherwise all render on the CPU)
    let _one = creating(role);
    match &slot(role).state {
        State::Ready(g) => return Some(g.clone()),
        State::Failed(_) | State::Absent(_) => return None,
        State::NotTried => {}
    }
    let made = create(role);
    let mut s = slot(role);
    match made {
        Ok(g) => {
            let g = Arc::new(g);
            s.state = State::Ready(g.clone());
            Some(g)
        }
        Err(why) => {
            log::warn!("gpu ({}): rendering on the CPU: {why}", role.id());
            s.state = State::Failed(why);
            None
        }
    }
}

/// Make a new device for the lost one on a thread of its own (once at a time per role).
fn start_recovery(role: Role, create: fn(Role) -> Result<Gpu, String>) {
    let flag = &RECOVERING[at(role)];
    if flag.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new().name(format!("lc-gpu-recover-{}", role.id())).spawn(move || {
        // (not once the app is closing: creating a device is a long call into the driver)
        if let Some(_work) = crate::exit::enter() {
            let _one = creating(role);
            let still_due = {
                let s = slot(role);
                s.lost.as_ref().is_some_and(|l| due(l, s.recoveries))
            };
            if still_due {
                replace(role, create(role));
            }
        }
        RECOVERING[at(role)].store(false, Ordering::Release);
    });
    if spawned.is_err() {
        flag.store(false, Ordering::Release);
    }
}

/// Put a new device in place of the lost one, or count the failed attempt.
fn replace(role: Role, made: Result<Gpu, String>) {
    let mut s = slot(role);
    match made {
        Ok(g) => {
            let name = g.info.name.clone();
            s.state = State::Ready(Arc::new(g));
            if let Some(l) = s.lost.take() {
                s.recoveries += 1;
                drop(s);
                log::warn!("gpu ({}): a new device replaces the lost one ({})", role.id(), l.reason);
                event(
                    role,
                    EventKind::Recovered,
                    match role {
                        Role::Primary => "LightCraft restarted the graphics card and uses it again.".into(),
                        Role::Helper => format!("LightCraft restarted the second graphics card ({name}) and uses it again."),
                    },
                );
            }
        }
        Err(why) => {
            if let Some(l) = s.lost.as_mut() {
                l.attempts += 1;
                l.at = Instant::now();
            }
            let tried = s.recoveries.saturating_add(s.lost.as_ref().map_or(0, |l| l.attempts));
            drop(s);
            log::warn!("gpu ({}): replacing the lost device failed ({why})", role.id());
            if tried >= MAX_RECOVERIES {
                event(role, EventKind::GaveUp, given_up_text(role, None));
            }
        }
    }
}

/// Use `g` (the window's device, created elsewhere) unless a device exists already. `Err`: the
/// one that exists (or the failure to create one) stays.
pub(crate) fn adopt(make: impl FnOnce() -> Result<Gpu, String>) -> Result<Arc<Gpu>, String> {
    let _one = creating(Role::Primary);
    match &slot(Role::Primary).state {
        State::Ready(g) => return Ok(g.clone()),
        State::Failed(e) | State::Absent(e) => return Err(e.clone()),
        State::NotTried => {}
    }
    let made = make();
    let mut s = slot(Role::Primary);
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

/// Hold while making a device for `role`'s slot outside [`get`] / [`adopt`] (a switch to another
/// card, the second card's warm-up): one device is made at a time per role.
pub(crate) fn creating(role: Role) -> std::sync::MutexGuard<'static, ()> {
    CREATING[at(role)].lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Use `g` for `role` from now on (another card was chosen, the second card is ready): renders
/// that hold the old device finish on it; a loss waiting to be replaced is over. Hold
/// [`creating`] while making `g`.
pub(crate) fn put(role: Role, g: Gpu) {
    let old = {
        let mut s = slot(role);
        s.lost = None;
        std::mem::replace(&mut s.state, State::Ready(Arc::new(g)))
    };
    // (the old device goes once its last render lets go of it, outside the lock)
    drop(old);
}

/// `role` has no device from now on: none to make (`Absent`, the second card turned off or not
/// found), or making it failed (`Failed`). Renders that hold the old device finish on it; then it
/// is released. Hold [`creating`].
pub(crate) fn clear(role: Role, failed: bool, why: String) {
    let old = {
        let mut s = slot(role);
        s.lost = None;
        std::mem::replace(&mut s.state, if failed { State::Failed(why) } else { State::Absent(why) })
    };
    drop(old);
}

/// Forget `role`'s device and how it went (tests).
#[cfg(test)]
pub(crate) fn forget(role: Role) {
    let old = {
        let mut s = slot(role);
        s.lost = None;
        std::mem::replace(&mut s.state, State::NotTried)
    };
    drop(old);
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
pub(crate) fn gave_up(role: Role) -> bool {
    let s = slot(role);
    s.lost.as_ref().is_some_and(|l| exhausted(l, s.recoveries))
}

fn given_up_text(role: Role, name: Option<&str>) -> String {
    match role {
        Role::Primary => format!(
            "The graphics card failed {MAX_RECOVERIES} times; LightCraft renders on the processor until it is started again. Exports go on, more slowly."
        ),
        Role::Helper => format!(
            "The second graphics card{} failed {MAX_RECOVERIES} times; LightCraft doesn't use it until it is started again. Work goes on on the other graphics card.",
            name.map(|n| format!(" ({n})")).unwrap_or_default()
        ),
    }
}

/// The device of generation `generation` failed (`generation` 0: whichever this thread renders
/// on, [`crate::current_role`]): stop using it, and replace it after a pause. Reported once per
/// device.
pub(crate) fn lose(generation: u64, reason: &str) {
    let role = if generation == 0 {
        crate::current_role()
    } else {
        match Role::ALL.into_iter().find(|r| matches!(&slot(*r).state, State::Ready(g) if g.generation == generation)) {
            Some(r) => r,
            None => return, // a device already replaced
        }
    };
    let mut s = slot(role);
    let State::Ready(g) = &s.state else { return };
    if generation != 0 && g.generation != generation {
        return; // replaced meanwhile
    }
    if s.lost.is_some() {
        return;
    }
    let name = g.info.name.clone();
    s.lost = Some(Lost { reason: reason.to_string(), at: Instant::now(), attempts: 0 });
    *LAST_LOSS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.to_string());
    let left = MAX_RECOVERIES.saturating_sub(s.recoveries);
    drop(s);
    log::error!("gpu ({}): device failure, not using it until it is replaced: {reason}", role.id());
    let text = if left == 0 {
        given_up_text(role, Some(&name))
    } else {
        match role {
            Role::Primary => format!(
                "The graphics card stopped responding ({}). The photos it was working on are made on the processor instead; LightCraft restarts the graphics card in a few seconds.",
                short(reason)
            ),
            Role::Helper => format!(
                "The {name} stopped responding ({}); work continues on the other graphics card. LightCraft restarts it in a few seconds.",
                short(reason)
            ),
        }
    };
    event(role, EventKind::Lost, text);
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
pub(crate) fn recoveries(role: Role) -> u32 {
    slot(role).recoveries
}

/// What happened to the GPU, in plain words for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuEvent {
    /// Its number in the session (1, 2, …): new events have higher numbers.
    pub seq: u64,
    pub kind: EventKind,
    /// Which card it is about.
    pub role: Role,
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
    /// The graphics card chosen in Settings isn't there (or can't render): Automatic is used.
    CardMissing,
    /// Photos render on another graphics card now (a new choice in Settings), or the switch
    /// failed and they stay where they were.
    CardSwitched,
}

static EVENTS: Mutex<(u64, Vec<GpuEvent>)> = Mutex::new((0, Vec::new()));
/// Events kept (the oldest go).
const EVENTS_KEPT: usize = 32;

pub(crate) fn event(role: Role, kind: EventKind, text: String) {
    let mut e = EVENTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    e.0 += 1;
    let seq = e.0;
    e.1.push(GpuEvent { seq, kind, role, text });
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
            event(Role::Primary, EventKind::Recovered, format!("test {i}"));
        }
        let mine = events_after(before);
        assert!(mine.len() <= EVENTS_KEPT && !mine.is_empty());
        assert!(mine.windows(2).all(|w| w[0].seq < w[1].seq));
        assert_eq!(mine.last().map(|e| e.text.as_str()), Some("test 39"));
    }

    /// The second card's slot is its own: nothing made on demand, a reason when there is none,
    /// and clearing it leaves the editing card's alone.
    #[test]
    fn the_second_cards_slot_is_its_own() {
        fn never(_: Role) -> Result<Gpu, String> {
            Err("no device in this test".into())
        }
        // not made by a render: only its warm-up makes it
        assert!(get(Role::Helper, never, false).is_none());
        let primary_tried = tried(Role::Primary);
        {
            let _one = creating(Role::Helper);
            clear(Role::Helper, false, "turned off in Settings".into());
        }
        assert_eq!(absent_reason(Role::Helper).as_deref(), Some("turned off in Settings"));
        assert!(get(Role::Helper, never, false).is_none());
        assert_eq!(tried(Role::Primary), primary_tried, "the editing card's slot is untouched");
        {
            let _one = creating(Role::Helper);
            forget(Role::Helper);
        }
        assert!(!tried(Role::Helper));
        assert!(given_up_text(Role::Helper, Some("RTX 4070")).contains("second graphics card (RTX 4070)"));
    }
}
