//! Wait for several futures at once on the page's one thread, with no executor crate: start-up's
//! browser-storage reads (library files, the `segments` folder probe, the thumbnail index and
//! listing) are all in flight together, so start-up waits about as long as the slowest one, not
//! for the sum of their round trips (#3989).
//!
//! Each future is polled whenever the joined one is woken, until every one is done; the results
//! come back in the order the futures were given. Compiled natively too, so it has tests.

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

/// A boxed future of the page's thread (browser futures aren't `Send`).
pub type Local<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Both futures' outputs, once both are done.
pub async fn join2<A, B>(a: impl Future<Output = A>, b: impl Future<Output = B>) -> (A, B) {
    let (mut a, mut b) = (Box::pin(a), Box::pin(b));
    let (mut ra, mut rb) = (None, None);
    std::future::poll_fn(move |cx| {
        if ra.is_none()
            && let Poll::Ready(v) = a.as_mut().poll(cx)
        {
            ra = Some(v);
        }
        if rb.is_none()
            && let Poll::Ready(v) = b.as_mut().poll(cx)
        {
            rb = Some(v);
        }
        match (ra.take(), rb.take()) {
            (Some(x), Some(y)) => Poll::Ready((x, y)),
            (x, y) => {
                (ra, rb) = (x, y);
                Poll::Pending
            }
        }
    })
    .await
}

/// Every future's output, in the order given, once all are done.
pub async fn join_all<'a, T>(futures: Vec<Local<'a, T>>) -> Vec<T> {
    let mut slots: Vec<(Option<Local<'a, T>>, Option<T>)> = futures.into_iter().map(|f| (Some(f), None)).collect();
    std::future::poll_fn(move |cx| {
        let mut pending = false;
        for (future, out) in slots.iter_mut() {
            if let Some(f) = future {
                match f.as_mut().poll(cx) {
                    Poll::Ready(v) => {
                        *out = Some(v);
                        *future = None;
                    }
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending { Poll::Pending } else { Poll::Ready(slots.iter_mut().filter_map(|(_, out)| out.take()).collect()) }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::task::{Context, Waker};

    /// Pending `turns` times (waking itself each time, like a browser promise settling later),
    /// then ready with `value`; logs each poll as `(name, turn)`.
    fn slow<T: Clone + 'static>(name: &'static str, turns: u32, value: T, log: Rc<RefCell<Vec<(&'static str, u32)>>>) -> Local<'static, T> {
        let mut turn = 0;
        Box::pin(std::future::poll_fn(move |cx| {
            log.borrow_mut().push((name, turn));
            if turn < turns {
                turn += 1;
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(value.clone())
            }
        }))
    }

    /// Poll `f` until it is ready (at most `limit` times).
    fn run<T>(f: impl Future<Output = T>, limit: u32) -> T {
        let mut f = Box::pin(f);
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..limit {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
        panic!("not ready after {limit} polls");
    }

    /// The futures are in flight together: the second is polled before the first is done, and
    /// the whole takes as many turns as the slowest, not their sum. Results keep their order.
    #[test]
    fn join2_waits_for_the_slowest_not_the_sum() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let (a, b) = run(join2(slow("a", 3, 1, log.clone()), slow("b", 1, "two", log.clone())), 4);
        assert_eq!((a, b), (1, "two"));
        let log = log.borrow();
        assert_eq!(log[..2], [("a", 0), ("b", 0)], "both started on the first poll");
        assert_eq!(log.iter().filter(|(n, _)| *n == "b").count(), 2, "a finished future isn't polled again");
    }

    #[test]
    fn join_all_keeps_the_order_given() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let futures = vec![slow("x", 2, 'x', log.clone()), slow("y", 0, 'y', log.clone()), slow("z", 1, 'z', log.clone())];
        assert_eq!(run(join_all(futures), 3), ['x', 'y', 'z']);
        assert_eq!(log.borrow()[..3], [("x", 0), ("y", 0), ("z", 0)], "all started on the first poll");
        assert_eq!(run(join_all(Vec::<Local<'static, u8>>::new()), 1), Vec::<u8>::new());
    }
}
