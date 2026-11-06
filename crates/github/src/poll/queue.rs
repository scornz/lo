//! One request queue per account. GitHub asks integrations to make requests serially rather than
//! concurrently, and to wait at least a second between mutations (research §7 "Secondary limits").

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui::BackgroundExecutor;
use parking_lot::Mutex;

/// Runs an account's requests one at a time, spacing its writes. Waiting requests take their
/// turns first in, first out, except that a waiting write goes before every waiting read: a
/// comment or a merge the user is waiting on doesn't queue behind polls. Clones share the queue.
#[derive(Clone)]
pub struct RequestQueue(Arc<Inner>);

struct Inner {
    state: Mutex<State>,
    executor: BackgroundExecutor,
    spacing: Duration,
}

#[derive(Default)]
struct State {
    /// Whether a request holds the queue.
    busy: bool,
    /// When the last write finished.
    last_write: Option<Instant>,
    /// Requests waiting for their turn, oldest first.
    writes: VecDeque<oneshot::Sender<Turn>>,
    reads: VecDeque<oneshot::Sender<Turn>>,
}

/// Holding the queue. Dropping it hands the queue to the next waiting request.
struct Turn(Option<Arc<Inner>>);

impl Drop for Turn {
    fn drop(&mut self) {
        if let Some(inner) = self.0.take() {
            inner.hand_on();
        }
    }
}

impl Inner {
    /// Gives the queue to the next waiting request, or frees it when none is waiting.
    fn hand_on(self: Arc<Self>) {
        loop {
            let next = {
                let mut state = self.state.lock();
                match state.writes.pop_front().or_else(|| state.reads.pop_front()) {
                    Some(next) => next,
                    None => {
                        state.busy = false;
                        return;
                    }
                }
            };
            // A request that stopped waiting has dropped its receiver: try the one after it.
            // Taking the queue back out of the refused turn keeps its drop from handing on too.
            match next.send(Turn(Some(self.clone()))) {
                Ok(()) => return,
                Err(mut refused) => {
                    refused.0.take();
                }
            }
        }
    }
}

impl RequestQueue {
    /// A queue whose writes start at least `spacing` after the previous write finished.
    pub fn new(executor: BackgroundExecutor, spacing: Duration) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            executor,
            spacing,
        }))
    }

    /// Waits for the queue: at once when it's free and nothing is waiting, else behind the
    /// requests already waiting (writes ahead of reads).
    async fn turn(&self, write: bool) -> Turn {
        loop {
            let waiting = {
                let mut state = self.0.state.lock();
                if !state.busy {
                    state.busy = true;
                    return Turn(Some(self.0.clone()));
                }
                let (sender, receiver) = oneshot::channel();
                if write {
                    state.writes.push_back(sender);
                } else {
                    state.reads.push_back(sender);
                }
                receiver
            };
            // A turn is only dropped unsent when this request stopped waiting, so this always
            // gets one; waiting again is just the safe answer to the impossible case.
            if let Ok(turn) = waiting.await {
                return turn;
            }
        }
    }

    /// Makes and runs a read once it's the read's turn. `request` is called only then, so a
    /// request that starts work when it's made, such as a spawned task, still waits its turn.
    pub async fn read<F, Fut>(&self, request: F) -> Fut::Output
    where
        F: FnOnce() -> Fut,
        Fut: Future,
    {
        let _turn = self.turn(false).await;
        request().await
    }

    /// Makes and runs a write (a mutation) once it's the write's turn, and at least the spacing
    /// after the previous write finished.
    pub async fn write<F, Fut>(&self, request: F) -> Fut::Output
    where
        F: FnOnce() -> Fut,
        Fut: Future,
    {
        let _turn = self.turn(true).await;
        let last_write = self.0.state.lock().last_write;
        if let Some(last) = last_write {
            let ready = last + self.0.spacing;
            let now = self.0.executor.now();
            if ready > now {
                self.0.executor.timer(ready - now).await;
            }
        }
        let output = request().await;
        self.0.state.lock().last_write = Some(self.0.executor.now());
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::TestAppContext;
    use parking_lot::Mutex as SyncMutex;

    /// What ran, and when it started and finished, in milliseconds from the start.
    type Log = Arc<SyncMutex<Vec<(&'static str, u128, u128)>>>;

    fn request(
        name: &'static str,
        takes: Duration,
        log: &Log,
        executor: &BackgroundExecutor,
        start: Instant,
    ) -> impl Future<Output = ()> + use<> {
        let (log, executor) = (log.clone(), executor.clone());
        async move {
            let began = (executor.now() - start).as_millis();
            executor.timer(takes).await;
            let ended = (executor.now() - start).as_millis();
            log.lock().push((name, began, ended));
        }
    }

    #[gpui::test]
    async fn requests_run_one_at_a_time(cx: &mut TestAppContext) {
        let executor = cx.executor();
        let queue = RequestQueue::new(executor.clone(), Duration::from_secs(1));
        let log = Log::default();
        let start = executor.now();
        for name in ["a", "b", "c"] {
            let queue = queue.clone();
            let request = request(name, Duration::from_millis(300), &log, &executor, start);
            executor
                .spawn(async move { queue.read(|| request).await })
                .detach();
            executor.run_until_parked();
        }
        for _ in 0..10 {
            executor.advance_clock(Duration::from_millis(100));
            executor.run_until_parked();
        }
        assert_eq!(
            *log.lock(),
            [("a", 0, 300), ("b", 300, 600), ("c", 600, 900)]
        );
    }

    #[gpui::test]
    async fn writes_start_a_second_after_the_previous_write(cx: &mut TestAppContext) {
        let executor = cx.executor();
        let queue = RequestQueue::new(executor.clone(), Duration::from_secs(1));
        let log = Log::default();
        let start = executor.now();
        let spawn = |name, write: bool, takes| {
            let queue = queue.clone();
            let request = request(name, Duration::from_millis(takes), &log, &executor, start);
            executor
                .spawn(async move {
                    if write {
                        queue.write(|| request).await
                    } else {
                        queue.read(|| request).await
                    }
                })
                .detach();
            executor.run_until_parked();
        };
        let advance_to = |millis: u64| {
            while executor.now() - start < Duration::from_millis(millis) {
                executor.advance_clock(Duration::from_millis(100));
                executor.run_until_parked();
            }
        };
        spawn("write a", true, 500);
        spawn("read", false, 200);
        advance_to(700);
        spawn("write b", true, 100);
        spawn("write c", true, 100);
        advance_to(4000);
        assert_eq!(
            *log.lock(),
            [
                ("write a", 0, 500),
                // A read doesn't wait for the spacing.
                ("read", 500, 700),
                // A second after "write a" finished.
                ("write b", 1500, 1600),
                ("write c", 2600, 2700),
            ]
        );
    }

    #[gpui::test]
    async fn a_request_is_made_only_once_it_holds_the_queue(cx: &mut TestAppContext) {
        let executor = cx.executor();
        let queue = RequestQueue::new(executor.clone(), Duration::from_secs(1));
        let log = Log::default();
        let start = executor.now();
        let made = Arc::new(SyncMutex::new(Vec::new()));
        for (name, write) in [("write", true), ("read", false)] {
            let (queue, made) = (queue.clone(), made.clone());
            let request = request(name, Duration::from_millis(500), &log, &executor, start);
            let make = move || {
                made.lock().push(name);
                request
            };
            executor
                .spawn(async move {
                    if write {
                        queue.write(make).await
                    } else {
                        queue.read(make).await
                    }
                })
                .detach();
            executor.run_until_parked();
        }
        // The read waits for the write without having been made: a spawned task would already
        // be running.
        assert_eq!(*made.lock(), ["write"]);
        for _ in 0..10 {
            executor.advance_clock(Duration::from_millis(100));
            executor.run_until_parked();
        }
        assert_eq!(*made.lock(), ["write", "read"]);
        assert_eq!(*log.lock(), [("write", 0, 500), ("read", 500, 1000)]);
    }

    #[gpui::test]
    async fn waiting_requests_run_in_order_writes_first(cx: &mut TestAppContext) {
        let executor = cx.executor();
        let queue = RequestQueue::new(executor.clone(), Duration::ZERO);
        let log = Log::default();
        let start = executor.now();
        for (name, write) in [
            ("read a", false),
            ("read b", false),
            ("read c", false),
            ("write d", true),
            ("read e", false),
            ("write f", true),
        ] {
            let queue = queue.clone();
            let request = request(name, Duration::from_millis(100), &log, &executor, start);
            executor
                .spawn(async move {
                    if write {
                        queue.write(|| request).await
                    } else {
                        queue.read(|| request).await
                    }
                })
                .detach();
            executor.run_until_parked();
        }
        for _ in 0..10 {
            executor.advance_clock(Duration::from_millis(100));
            executor.run_until_parked();
        }
        let order: Vec<&str> = log.lock().iter().map(|(name, ..)| *name).collect();
        assert_eq!(
            order,
            ["read a", "write d", "write f", "read b", "read c", "read e"]
        );
    }

    #[gpui::test]
    async fn a_request_given_up_on_while_waiting_doesnt_hold_up_the_rest(cx: &mut TestAppContext) {
        let executor = cx.executor();
        let queue = RequestQueue::new(executor.clone(), Duration::ZERO);
        let log = Log::default();
        let start = executor.now();
        let spawn = |name| {
            let queue = queue.clone();
            let request = request(name, Duration::from_millis(100), &log, &executor, start);
            let task = executor.spawn(async move { queue.read(|| request).await });
            executor.run_until_parked();
            task
        };
        let first = spawn("first");
        let abandoned = spawn("abandoned");
        let last = spawn("last");
        drop(abandoned);
        for _ in 0..5 {
            executor.advance_clock(Duration::from_millis(100));
            executor.run_until_parked();
        }
        first.await;
        last.await;
        assert_eq!(*log.lock(), [("first", 0, 100), ("last", 100, 200)]);
    }
}
