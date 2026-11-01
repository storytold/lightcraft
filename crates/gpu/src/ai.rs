//! Which graphics card the AI models run on (AI Denoise's network, the AI mask models' matrix
//! products, the face models): the card for editing, or the second graphics card (the helper).
//!
//! Each AI module keeps a device of its own on each card it uses ([`PerCard`]), created on first
//! use and given up on (that card only) after a device error or loss: the other card and the CPU
//! carry on. Background AI work goes to the helper first (31 Oct 2026, §8 of the multi-GPU design):
//! it does not drive the screen, so the card for editing stays free for the photo being edited.
//! [`preferred`] is that choice; [`on_card`] routes one thread's work to a card explicitly (the
//! denoise batch hands each photo to whichever card is free).
//!
//! The helper is the card `cards::resolve_helper` picks: `LIGHTCRAFT_GPU_HELPER` (`same` makes a
//! second device on the editing card: the two-card logic on a one-card machine), else the
//! setting, else Automatic (another dedicated card than the editing one). Every failure is an
//! `Err`, never a panic.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

pub use crate::Role;

/// The cards in the order background AI work takes them: the second graphics card first (§8).
pub const BACKGROUND_ORDER: [Role; 2] = [Role::Helper, Role::Primary];

thread_local! {
    static CURRENT: Cell<Option<Role>> = const { Cell::new(None) };
}

/// Run `f` with this thread's AI work on `card` (the AI modules' device choice; nested calls
/// restore the outer choice, a panic included).
pub fn on_card<T>(card: Role, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Role>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(CURRENT.with(|c| c.replace(Some(card))));
    f()
}

/// The card [`on_card`] chose for this thread, if any.
pub fn current() -> Option<Role> {
    CURRENT.with(Cell::get)
}

/// The card this thread's AI work goes to: the one [`on_card`] chose, else [`preferred`].
pub(crate) fn wanted() -> Role {
    current().unwrap_or_else(preferred)
}

/// Where AI work goes by default: the helper when there is one, else the editing card. Lists the
/// cards the first time (a call into each driver: never on the UI thread).
pub fn preferred() -> Role {
    if helper_name().is_some() { Role::Helper } else { Role::Primary }
}

/// The second graphics card AI work would use (its name), or `None`: there is none, it is turned
/// off (Settings, `LIGHTCRAFT_GPU_HELPER`), or the GPU is. The card is looked up once per process
/// (a call into each driver the first time); turning the helper off later takes effect at once.
pub fn helper_name() -> Option<String> {
    static FOUND: OnceLock<Option<String>> = OnceLock::new();
    if crate::switched_off().is_some() || crate::cards::helper_choice() == crate::cards::HelperChoice::Off {
        return None;
    }
    if let Some(f) = FOUND.get() {
        return f.clone();
    }
    let backends = crate::backend::compute_backends()?;
    let _work = crate::exit::enter()?;
    FOUND.get_or_init(|| std::panic::catch_unwind(|| crate::helper_pick(backends).ok().map(|(a, _, _)| a.get_info().name)).ok().flatten()).clone()
}

/// The adapter for `card` on `instance`, with its key when the listing shows it. Helper: `Err`
/// when there is no second card.
pub(crate) fn pick(
    instance: &wgpu::Instance,
    backends: wgpu::Backends,
    card: Role,
) -> Result<(wgpu::Adapter, Option<crate::cards::AdapterKey>), String> {
    match card {
        Role::Primary => crate::backend::pick_editing(instance, backends, false).map(|p| (p.adapter, p.key)),
        Role::Helper => crate::helper_pick(backends).map(|(adapter, key, _)| (adapter, key)).map_err(|e| format!("{NO_HELPER} ({e})")),
    }
}

/// Why the helper's device isn't there.
pub const NO_HELPER: &str = "there is no second graphics card";

/// Wait until background AI work may submit to `card`. Decision §8: the helper at once; the
/// editing card only while the interface is idle (the device pool's gate: [`crate::interface_busy`]),
/// checked before each short submission (a denoise tile, a face model run).
pub fn background_turn(card: Role) {
    if card != Role::Primary {
        return;
    }
    // (waits only while the desktop app tracks the interface: `track_interaction`)
    while crate::interface_busy() && !crate::shutting_down() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A device that stopped being used after an error or a loss, and why.
pub(crate) struct Broken {
    flag: AtomicBool,
    why: Mutex<Option<String>>,
}

impl Broken {
    pub(crate) const fn new() -> Broken {
        Broken { flag: AtomicBool::new(false), why: Mutex::new(None) }
    }

    /// Stop using it (the first reason is kept).
    pub(crate) fn mark(&self, why: &str) {
        let mut w = self.why.lock().unwrap_or_else(PoisonError::into_inner);
        if w.is_none() {
            *w = Some(why.to_string());
        }
        self.flag.store(true, Ordering::Relaxed);
    }

    pub(crate) fn reason(&self) -> Option<String> {
        self.flag
            .load(Ordering::Relaxed)
            .then(|| self.why.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_else(|| "device error".into()))
    }
}

/// One AI module's device on each card (created on first use), and which of them broke.
pub(crate) struct PerCard<T: 'static> {
    devices: [OnceLock<Result<T, String>>; 2],
    broken: [Broken; 2],
}

impl<T: 'static> PerCard<T> {
    pub(crate) const fn new() -> PerCard<T> {
        PerCard { devices: [const { OnceLock::new() }, const { OnceLock::new() }], broken: [const { Broken::new() }, const { Broken::new() }] }
    }

    /// The broken flag of `card`'s device (for its error callbacks).
    pub(crate) fn broken(&'static self, card: Role) -> &'static Broken {
        let [editing, helper] = &self.broken;
        match card {
            Role::Primary => editing,
            Role::Helper => helper,
        }
    }

    /// Why `card`'s device stopped being used, if it did.
    pub(crate) fn broken_reason(&'static self, card: Role) -> Option<String> {
        self.broken(card).reason()
    }

    /// `card`'s device, made by `make` (adapter, its broken flag, the card's key) the first time; `Err` when it
    /// can't be used (GPU switched off, no such card, it failed). `role` names the module in the log.
    pub(crate) fn get(
        &'static self,
        card: Role,
        role: &str,
        make: impl FnOnce(wgpu::Adapter, &'static Broken, Option<crate::cards::AdapterKey>) -> Result<T, String>,
    ) -> Result<&'static T, String> {
        if let Some(why) = self.broken_reason(card) {
            return Err(format!("stopped after a GPU failure: {why}"));
        }
        if let Some(why) = crate::switched_off() {
            return Err(why);
        }
        let [editing, helper] = &self.devices;
        let slot = match card {
            Role::Primary => editing,
            Role::Helper => helper,
        };
        // creating the device is GPU work too: not once the process is ending (issue #620)
        let _work = match slot.get() {
            Some(_) => None,
            None => Some(crate::exit::enter().ok_or("LightCraft is closing")?),
        };
        let broken = self.broken(card);
        slot.get_or_init(|| {
            let Some(backends) = crate::backend::compute_backends() else { return Err("disabled by LIGHTCRAFT_GPU_BACKEND=off".into()) };
            let instance = crate::backend::compute_instance(backends);
            let (adapter, key) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pick(&instance, backends, card)))
                .unwrap_or_else(|_| Err("listing the graphics cards panicked".into()))?;
            log::debug!("gpu ({role}, {} card): creating the device", card.id());
            let create = || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make(adapter, broken, key.clone())))
                    .unwrap_or_else(|_| Err("device creation panicked".into()))
            };
            // a crash creating the helper's device costs the next launch the helper only (its marker names it);
            // on the editing card it is a crash in the GPU's own start, as before
            match card {
                Role::Helper => crate::backend::with_init_marker_for(backends, key.as_ref(), "helper", create),
                Role::Primary => crate::backend::with_init_marker(backends, create),
            }
        })
        .as_ref()
        .map_err(Clone::clone)
    }
}

/// Run driver work `what` (kernel compilation) for a device on `card` (its key `key`) under the crash marker; on the
/// helper the marker names the card, so a crash there turns only the helper off at the next launch.
#[cfg(feature = "denoise")]
pub(crate) fn with_card_marker<T>(what: &str, card: Role, key: Option<&crate::cards::AdapterKey>, f: impl FnOnce() -> T) -> T {
    match (card, key) {
        (Role::Helper, Some(k)) => crate::backend::with_marker(&helper_scope(what, k), f),
        _ => crate::backend::with_marker(what, f),
    }
}

/// The marker's words for driver work on the helper (`backend::marker_scopes` reads the card back).
#[cfg(feature = "denoise")]
fn helper_scope(what: &str, key: &crate::cards::AdapterKey) -> String {
    format!("{what} [card={key}; role=helper]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_threads_card_is_its_own_and_restored() {
        assert_eq!(current(), None);
        on_card(Role::Helper, || {
            assert_eq!(current(), Some(Role::Helper));
            on_card(Role::Primary, || assert_eq!(current(), Some(Role::Primary)));
            assert_eq!(current(), Some(Role::Helper), "the outer choice is back");
            std::thread::spawn(|| assert_eq!(current(), None, "another thread has its own")).join().unwrap();
        });
        let r = std::panic::catch_unwind(|| on_card(Role::Helper, || panic!("in a job")));
        assert!(r.is_err());
        assert_eq!(current(), None, "a panic restores it too");
        assert_eq!(crate::ai::BACKGROUND_ORDER, [Role::Helper, Role::Primary], "background work takes the helper first");
    }

    /// A crash compiling AI kernels on the helper names the helper, so the next launch turns only it off.
    #[test]
    #[cfg(feature = "denoise")]
    fn helper_kernel_work_names_the_helper_in_the_crash_marker() {
        let key = crate::cards::AdapterKey::parse("dx12:10de:2783:NVIDIA GeForce RTX 4070 SUPER#1").unwrap();
        let line = format!("GPU {} started (unix time 1)", helper_scope("denoise kernel compilation", &key));
        assert_eq!(crate::backend::marker_scopes(&line), vec![crate::backend::MarkerScope { card: Some((key, "helper".into())) }]);
    }

    #[test]
    fn a_broken_card_keeps_its_first_reason_and_leaves_the_other() {
        static DEVS: PerCard<u32> = PerCard::new();
        assert_eq!(DEVS.broken_reason(Role::Helper), None);
        DEVS.broken(Role::Helper).mark("device lost");
        DEVS.broken(Role::Helper).mark("a later error");
        assert_eq!(DEVS.broken_reason(Role::Helper).as_deref(), Some("device lost"));
        assert_eq!(DEVS.broken_reason(Role::Primary), None, "the editing card's device is separate");
        let e = DEVS.get(Role::Helper, "test", |_, _, _| Ok(1)).err().unwrap_or_default();
        assert!(e.contains("device lost"), "{e}");
    }
}
