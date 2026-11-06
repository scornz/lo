//! One request queue per account. GitHub asks integrations to make requests serially rather than
//! concurrently, and to wait at least a second between mutations (research §7 "Secondary limits").

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::lock::Mutex;
use gpui::BackgroundExecutor;

/// Runs an account's requests one at a time, spacing its writes. Clones share the queue.
#[derive(Clone)]
pub struct RequestQueue(Arc<Inner>);

struct Inner {
    /// Held while a request runs. Holds when the last write finished.
    last_write: Mutex<Option<Instant>>,
    executor: BackgroundExecutor,
    spacing: Duration,
}

impl RequestQueue {
    /// A queue whose writes start at least `spacing` after the previous write finished.
    pub fn new(executor: BackgroundExecutor, spacing: Duration) -> Self {
        Self(Arc::new(Inner {
            last_write: Mutex::new(None),
            executor,
            spacing,
        }))
    }

    /// Runs a read once the account has nothing else in flight.
    pub async fn read<F: Future>(&self, request: F) -> F::Output {
        let _turn = self.0.last_write.lock().await;
        request.await
    }

    /// Runs a write (a mutation) once the account has nothing else in flight, and at least the
    /// spacing after the previous write finished.
    pub async fn write<F: Future>(&self, request: F) -> F::Output {
        let mut last_write = self.0.last_write.lock().await;
        if let Some(last) = *last_write {
            let ready = last + self.0.spacing;
            let now = self.0.executor.now();
            if ready > now {
                self.0.executor.timer(ready - now).await;
            }
        }
        let output = request.await;
        *last_write = Some(self.0.executor.now());
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
                .spawn(async move { queue.read(request).await })
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
                        queue.write(request).await
                    } else {
                        queue.read(request).await
                    }
                })
                .detach();
            executor.run_until_parked();
        };
        spawn("write a", true, 500);
        spawn("read", false, 200);
        spawn("write b", true, 100);
        spawn("write c", true, 100);
        for _ in 0..40 {
            executor.advance_clock(Duration::from_millis(100));
            executor.run_until_parked();
        }
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
}
