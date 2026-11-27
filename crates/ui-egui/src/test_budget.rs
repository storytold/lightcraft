//! For tests that change the process-wide memory budget ([`lightcraft_engine::memory::budget`]).
//!
//! The budget is one number for the whole test process: the decode, export and GPU gates, every
//! new session's caches and the demo scene cache size from it, and other tests run at the same
//! time. A test that changes it (through `app.memoryBudget` or Settings ▸ Performance) holds a
//! [`BudgetGuard`] for as long as the change lasts. Tests that change the budget then run one at
//! a time, so one can't change it under another's assertion. The budget goes back to exactly what
//! it was before, also when the test fails: putting back the value found, not `default_budget()`,
//! keeps a budget set from outside (`LIGHTCRAFT_MEMORY_MB`, a test harness) (issue #3065).
//!
//! Tests that only read the budget don't take the lock; keep the change short.
//! The lock is per process, so per test binary, which is all the budget is shared by.

use std::sync::{Mutex, MutexGuard, PoisonError};

use lightcraft_engine::memory;

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
