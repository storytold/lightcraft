//! Memory budgets in tests: each test app's own cache budget, and the tests that change the
//! process-wide budget ([`lightcraft_engine::memory::budget`]).
//!
//! **Each test app's caches** (the session's [`MediaCache`](lightcraft_engine::media::MediaCache)
//! and the renderer's stage caches) are sized for its test's share of the machine, not for the
//! whole machine as an app's are: libtest runs several tests at once in one process, and each
//! would take the full share. [`LightcraftApp::new`] calls [`cap_instance`]: the app takes what it
//! would on a machine with [`lightcraft_testkit::memory::per_test_bytes`] of RAM, never more than
//! [`memory::default_budget`] (issue #3066). Both are read once per test process, at the first
//! app, so a test that changes the process-wide budget meanwhile doesn't change the caps of the
//! apps other tests build. The process-wide budget, the decode, export and GPU gates and the demo
//! scene cache stay as they are.
//!
//! What it bounds is the most those two caches may hold per test app, so a run with many tests at
//! once (or little memory free) can't hold a whole machine's share per test. It doesn't bound a
//! test's working memory. At 4 tests at once it changed neither the peak nor the time of the
//! `ui-egui` lib tests on a 16-core, 21.5 GiB machine: the caches rarely fill (median 40 MB held
//! of 526 MiB), and the peaks are short spikes of working memory. The share depends on the memory
//! free when the run starts; every test passes at the floor (256 MiB).
//!
//! **Changing the process-wide budget.** It is one number for the whole test process: the decode,
//! export and GPU gates, the caches of sessions made outside a test app and the demo scene cache
//! size from it, and other tests run at the same time. A test that changes it (through
//! `app.memoryBudget` or Settings ▸ Performance) holds a [`BudgetGuard`] for as long as the change
//! lasts. Tests that change the budget then run one at a time, so one can't change it under
//! another's assertion. The budget goes back to exactly what it was before, also when the test
//! fails: putting back the value found, not `default_budget()`, keeps a budget set from outside
//! (`LIGHTCRAFT_MEMORY_MB`, a test harness) (issue #3065).
//!
//! Tests that only read the budget don't take the lock; keep the change short.
//! The lock is per process, so per test binary, which is all the budget is shared by.

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use lightcraft_engine::memory;

use crate::LightcraftApp;

/// The least budget a test app gets, as [`memory::default_budget`] gives the smallest machine.
const MIN_INSTANCE_BUDGET: usize = 256 << 20;
/// The most budget a test app gets, as [`memory::default_budget`] gives the largest machine.
const MAX_INSTANCE_BUDGET: usize = 3 << 29;

/// Size `app`'s own caches for its test's share of the machine (issue #3066): the session's cache
/// share and the renderer's stage budget come from [`instance_budget`] instead of the process-wide
/// budget. A test that sets either itself afterwards still can.
pub(crate) fn cap_instance(mut app: LightcraftApp) -> LightcraftApp {
    let budget = instance_budget();
    app.session.media.set_budget(memory::cache_share(budget));
    app.renderer.stage_budget_override = Some(budget / crate::render::STAGE_BUDGET_SHARE);
    app
}

thread_local! {
    /// A budget the apps built on this thread take instead of [`baseline`] ([`InstanceBudget`]).
    static INSTANCE_OVERRIDE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The memory budget a test app sizes its own caches from: the [`baseline`], unless a test on
/// this thread set its own with [`InstanceBudget`].
pub(crate) fn instance_budget() -> usize {
    INSTANCE_OVERRIDE.with(std::cell::Cell::get).unwrap_or_else(baseline)
}

/// [`budget_for_share`] of this test's share of the machine and [`memory::default_budget`] (which
/// honours `LIGHTCRAFT_MEMORY_MB`), taken once per test process: not the process-wide budget at
/// the time, which a test changing it may have set to anything for a few frames.
fn baseline() -> usize {
    static BASELINE: OnceLock<usize> = OnceLock::new();
    *BASELINE.get_or_init(|| budget_for_share(lightcraft_testkit::memory::per_test_bytes(), memory::default_budget()))
}

/// What [`memory::default_budget`] gives a machine with `per_test` bytes of RAM (a quarter, from
/// 256 MiB to 1.5 GiB), and never more than `process`, so a lower `LIGHTCRAFT_MEMORY_MB` still
/// wins.
fn budget_for_share(per_test: u64, process: usize) -> usize {
    let quarter = usize::try_from(per_test / 4).unwrap_or(usize::MAX);
    quarter.clamp(MIN_INSTANCE_BUDGET, MAX_INSTANCE_BUDGET).min(process)
}

/// While held, the apps built on this thread size their caches from the given budget instead of
/// the [`baseline`]: a test that needs a known cap, whatever the machine. Dropping it goes back.
/// Not `Send`: dropped on another thread, it would put back that thread's override, not this one's.
pub(crate) struct InstanceBudget(Option<usize>, std::marker::PhantomData<std::rc::Rc<()>>);

impl InstanceBudget {
    pub(crate) fn set(bytes: usize) -> InstanceBudget {
        InstanceBudget(INSTANCE_OVERRIDE.with(|o| o.replace(Some(bytes))), std::marker::PhantomData)
    }
}

impl Drop for InstanceBudget {
    fn drop(&mut self) {
        INSTANCE_OVERRIDE.with(|o| o.set(self.0));
    }
}

/// Taken by every test that changes the process-wide budget.
static CHANGING_BUDGET: Mutex<()> = Mutex::new(());

/// Held while a test changes the process-wide memory budget: the other tests that change it wait,
/// and the budget is put back as it was found when the guard drops.
pub(crate) struct BudgetGuard {
    // fields drop in order: the budget is put back before the next test may take its turn
    _restore: Restore,
    _turn: MutexGuard<'static, ()>,
}

impl BudgetGuard {
    /// Wait for the other tests that change the budget, then remember the budget as it is now.
    pub(crate) fn hold() -> BudgetGuard {
        // poisoned: a test failed holding it, and its guard put the budget back on unwind
        let turn = CHANGING_BUDGET.lock().unwrap_or_else(PoisonError::into_inner);
        BudgetGuard { _restore: Restore::new(), _turn: turn }
    }
}

/// Puts the process-wide budget back to what it was when made.
struct Restore(usize);

impl Restore {
    fn new() -> Restore {
        Restore(memory::budget())
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        // doesn't panic; the value came from `budget()`, so it's within what `set_budget` keeps
        memory::set_budget(self.0);
    }
}

mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    // A test app takes what an app would on a machine with its test's share of memory: a quarter,
    // in the app's range, never more than the process's own budget (issue #3066)
    #[test]
    fn a_test_apps_budget_is_a_quarter_of_its_share() {
        let process = 3 << 29;
        // 16 GiB available, 4 tests at once: 4 GiB each, 1 GiB of budget
        assert_eq!(budget_for_share(4 * GIB, process), 1 << 30);
        assert_eq!(budget_for_share(2 * GIB, process), 512 << 20);
        assert_eq!(budget_for_share(lightcraft_testkit::memory::MIN_PER_TEST, process), MIN_INSTANCE_BUDGET);
        assert_eq!(budget_for_share(0, process), MIN_INSTANCE_BUDGET, "never below the app's least");
        assert_eq!(budget_for_share(u64::MAX, process), MAX_INSTANCE_BUDGET, "never above the app's most");
        assert_eq!(budget_for_share(4 * GIB, 384 << 20), 384 << 20, "a lower LIGHTCRAFT_MEMORY_MB wins");
        // (usize: 2 GiB still fits on 32-bit targets)
        assert_eq!(budget_for_share(4 * GIB, 2usize << 30), 1 << 30, "a higher one doesn't");
        assert_eq!(budget_for_share(4 * GIB, usize::MAX), 1 << 30);
    }

    fn app() -> crate::headless::Headless {
        crate::headless::Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Default::default()), [800.0, 600.0], 1.0)
    }

    /// The caps `h`'s app was built with: its session's cache budget and its renderer's stage budget.
    fn caps(h: &crate::headless::Headless) -> (usize, usize) {
        (h.app.session.media.budget(), h.app.renderer.stage_budget())
    }

    /// The caps an app built with `budget` has.
    fn caps_of(budget: usize) -> (usize, usize) {
        (memory::cache_share(budget), budget / crate::render::STAGE_BUDGET_SHARE)
    }

    // Given a test's share below the process-wide budget, a test app's session caches and its
    // renderer's stage caches take that share, not the whole machine's (issue #3066)
    #[test]
    fn a_test_app_is_sized_for_its_tests_share() {
        // a share no machine gives and no budget derives: 1 GiB less 4 MiB
        let share = (1 << 30) - (4 << 20);
        let _share = InstanceBudget::set(share);
        let h = app();
        assert_eq!(caps(&h), caps_of(share));
        assert_eq!(h.app.renderer.memory()["stageCaches"]["budgetBytes"], share / crate::render::STAGE_BUDGET_SHARE, "and reports it");
    }

    // Given a test that has the process-wide budget at an odd value for a few frames, an app
    // another test builds meanwhile still takes the baseline, and keeps it after (issue #3066)
    #[test]
    fn an_app_built_while_the_budget_is_changed_takes_the_baseline() {
        // never the baseline, whatever LIGHTCRAFT_MEMORY_MB says (and above set_budget's 64 MiB least)
        let odd = baseline() + (7 << 20);
        let h = {
            let _turn = BudgetGuard::hold();
            assert_eq!(memory::set_budget(odd), odd);
            app()
        };
        assert_eq!(caps(&h), caps_of(baseline()));
        assert_ne!(caps(&h), caps_of(odd));
        assert!(baseline() >= MIN_INSTANCE_BUDGET.min(memory::default_budget()));
    }

    // The budget is put back as it was found, not to the default, also when the test fails
    // (issue #3065)
    #[test]
    fn the_budget_is_put_back_as_it_was_found() {
        let _turn = BudgetGuard::hold();
        // a budget set from outside the test, unlike the default
        let outside = if memory::budget() == 700 << 20 { 600 << 20 } else { 700 << 20 };
        memory::set_budget(outside);

        {
            let _r = Restore::new();
            memory::set_budget(1024 << 20);
            assert_eq!(memory::budget(), 1024 << 20);
        }
        assert_eq!(memory::budget(), outside, "the budget found, not the default");

        let failed = std::panic::catch_unwind(|| {
            let _r = Restore::new();
            memory::set_budget(96 << 20);
            panic!("an assertion fails while the budget is changed");
        });
        assert!(failed.is_err());
        assert_eq!(memory::budget(), outside, "put back on unwind too");
    }

    // A test that fails holding the lock doesn't make the next one fail
    #[test]
    fn a_failed_test_does_not_block_the_next() {
        let failed = std::thread::spawn(|| {
            let _g = BudgetGuard::hold();
            panic!("an assertion fails while holding the turn");
        })
        .join();
        assert!(failed.is_err());
        let _g = BudgetGuard::hold();
    }
}
