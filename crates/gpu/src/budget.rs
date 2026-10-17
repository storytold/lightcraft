//! Device memory shared by concurrent renders (upstream issue #1764).
//!
//! Each render asks for room for what it is expected to hold on the device and waits while the
//! renders in flight leave too little; a render always gets in when nothing else is in flight,
//! however large it is (the device's own limits still apply). wgpu reports no memory size, so the
//! budget starts from the largest buffer the device allows and adapts to the device it meets:
//! it is halved when a render runs out of device memory and grows back a step with each render
//! that succeeds (the additive-increase / multiplicative-decrease rule of TCP congestion control).
//! A 2 GB card settles near what it holds instead of failing every render past its memory.
//! Renders get in in the order they asked (a fair semaphore): a large preview is not passed over
//! for ever by a stream of thumbnails that each fit.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

/// The budget never shrinks below this: one render always fits anyway, so this only bounds how
/// far a run of failures can push it.
const FLOOR: u64 = 256 << 20;

/// Device memory a render is expected to hold: the source, two working copies and the output,
/// as float RGBA, at the source's size.
pub(crate) fn footprint(width: usize, height: usize) -> u64 {
    (width as u64).saturating_mul(height as u64).saturating_mul(16 * 4)
}

/// Where the budget starts: 1.5 × the largest buffer the device allows (about the device's
/// memory on small cards), at least 1 GiB.
pub(crate) fn initial(max_buffer: u64) -> u64 {
    (max_buffer / 2).saturating_mul(3).max(1 << 30)
}

struct State {
    /// Bytes held by renders in flight.
    used: u64,
    /// Renders in flight.
    running: u32,
    /// Bytes renders may hold together now.
    limit: u64,
    /// What the budget grows back to.
    ceiling: u64,
    /// Tickets of the renders waiting, in the order they asked.
    queue: VecDeque<u64>,
    next_ticket: u64,
}

pub(crate) struct Budget {
    state: Mutex<State>,
    room: Condvar,
}

/// Room on the device for one render, released when dropped.
pub(crate) struct Permit<'a> {
    budget: &'a Budget,
    bytes: u64,
}

impl Budget {
    pub fn new(ceiling: u64) -> Budget {
        Budget {
            state: Mutex::new(State { used: 0, running: 0, limit: ceiling, ceiling, queue: VecDeque::new(), next_ticket: 0 }),
            room: Condvar::new(),
        }
    }

    /// Wait for room for `bytes`; `alone` waits until nothing else is in flight (the retry of a
    /// render that ran out of memory). `None` when `stop` turns true while waiting (the process is
    /// ending): the caller renders on the CPU.
    pub fn admit(&self, bytes: u64, alone: bool, stop: impl Fn() -> bool) -> Option<Permit<'_>> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let ticket = st.next_ticket;
        st.next_ticket = st.next_ticket.wrapping_add(1);
        st.queue.push_back(ticket);
        while st.queue.front() != Some(&ticket) || (st.running > 0 && (alone || st.used.saturating_add(bytes) > st.limit)) {
            if stop() {
                st.queue.retain(|t| *t != ticket);
                // the render behind may be the next to fit
                self.room.notify_all();
                return None;
            }
            st = self.room.wait_timeout(st, Duration::from_millis(100)).unwrap_or_else(PoisonError::into_inner).0;
        }
        st.queue.pop_front();
        // the next in line may fit beside this one
        self.room.notify_all();
        st.used = st.used.saturating_add(bytes);
        st.running += 1;
        Some(Permit { budget: self, bytes })
    }

    /// A render ran out of device memory: halve the budget (not below what one render needs).
    pub fn out_of_memory(&self) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.limit = (st.limit / 2).max(FLOOR);
    }

    /// A render succeeded: grow the budget back by a sixteenth of its ceiling.
    pub fn succeeded(&self) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.limit = st.limit.saturating_add(st.ceiling / 16).min(st.ceiling);
    }

    /// (bytes in flight, renders in flight, current budget).
    #[cfg(test)]
    fn snapshot(&self) -> (u64, u32, u64) {
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (st.used, st.running, st.limit)
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut st = self.budget.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.used = st.used.saturating_sub(self.bytes);
        st.running = st.running.saturating_sub(1);
        self.budget.room.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const GIB: u64 = 1 << 30;

    #[test]
    fn renders_wait_for_room_and_a_lone_render_always_gets_in() {
        let b = Arc::new(Budget::new(4 * GIB));
        let first = b.admit(3 * GIB, false, || false).unwrap();
        let waiter = {
            let b = Arc::clone(&b);
            std::thread::spawn(move || {
                let p = b.admit(2 * GIB, false, || false).unwrap();
                let seen = b.snapshot();
                drop(p);
                seen
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(b.snapshot().1, 1, "3 + 2 GiB is over the 4 GiB budget: the second render waits");
        drop(first);
        assert_eq!(waiter.join().unwrap().0, 2 * GIB);
        assert_eq!(b.snapshot().0, 0);
        // nothing in flight: a render bigger than the whole budget still runs
        let big = b.admit(10 * GIB, false, || false).unwrap();
        assert_eq!(b.snapshot().0, 10 * GIB);
        drop(big);
    }

    #[test]
    fn a_retry_alone_waits_for_every_other_render() {
        let b = Arc::new(Budget::new(4 * GIB));
        let small = b.admit(GIB, false, || false).unwrap();
        let retry = {
            let b = Arc::clone(&b);
            std::thread::spawn(move || b.admit(GIB, true, || false).map(|_| ()).is_some())
        };
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(b.snapshot().1, 1, "the retry must not run beside another render, even with room");
        drop(small);
        assert!(retry.join().unwrap());
    }

    #[test]
    fn renders_get_in_in_the_order_they_asked() {
        let b = Arc::new(Budget::new(4 * GIB));
        let held = b.admit(3 * GIB, false, || false).unwrap();
        // a big render waits for room...
        let big = {
            let b = Arc::clone(&b);
            std::thread::spawn(move || {
                let p = b.admit(3 * GIB, false, || false).unwrap();
                let seen = b.snapshot().1;
                drop(p);
                seen
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        // ...and a small one that would fit now queues behind it instead of passing it
        let small = {
            let b = Arc::clone(&b);
            std::thread::spawn(move || b.admit(GIB / 2, false, || false).map(|_| ()).is_some())
        };
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(b.snapshot().1, 1, "the small render must not pass the big one waiting before it");
        drop(held);
        assert!(big.join().unwrap() >= 1);
        assert!(small.join().unwrap());
    }

    #[test]
    fn waiting_ends_when_the_process_stops() {
        let b = Budget::new(GIB);
        let _held = b.admit(GIB, false, || false).unwrap();
        assert!(b.admit(GIB, false, || true).is_none());
        assert_eq!(b.snapshot().1, 1, "a refused render holds nothing");
    }

    #[test]
    fn the_budget_halves_on_out_of_memory_and_grows_back_on_success() {
        let b = Budget::new(4 * GIB);
        b.out_of_memory();
        assert_eq!(b.snapshot().2, 2 * GIB);
        for _ in 0..10 {
            b.out_of_memory();
        }
        assert_eq!(b.snapshot().2, FLOOR, "never below the floor");
        for _ in 0..100 {
            b.succeeded();
        }
        assert_eq!(b.snapshot().2, 4 * GIB, "never above where it started");
    }

    #[test]
    fn sizes_are_saturating_and_sized_for_big_raws() {
        // a 60-megapixel raw is about 3.8 GB: two never run together under a 4 GiB budget
        let one = footprint(9504, 6336);
        assert!(one > 3 * GIB && one < 4 * GIB, "{one}");
        assert_eq!(footprint(usize::MAX, usize::MAX), u64::MAX);
        assert_eq!(initial(2 * GIB), 3 * GIB);
        assert_eq!(initial(128 << 20), GIB, "small limits start at 1 GiB");
    }
}
