//! Panic-catching concurrent task set for tool execution.
//!
//! Tool panics must not crash the agent; every spawned task is wrapped in `catch_unwind` and returns `Err(String)` instead.

use std::backtrace::Backtrace;
use std::collections::VecDeque;
use std::future::Future;
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;

use futures::stream::{FuturesUnordered, StreamExt};
use futures_lite::FutureExt;

type CaughtTask<T> = Pin<Box<dyn Future<Output = (usize, Result<T, String>)> + Send>>;

pub(crate) struct TaskSet<T> {
    max_concurrent: NonZeroUsize,
    tasks: VecDeque<CaughtTask<T>>,
}

impl<T: Send + 'static> TaskSet<T> {
    pub fn new(max_concurrent: NonZeroUsize) -> Self {
        Self {
            max_concurrent,
            tasks: VecDeque::new(),
        }
    }

    pub fn spawn<F>(&mut self, future: F)
    where
        F: Future<Output = T> + Send + 'static,
    {
        let position = self.tasks.len();
        self.tasks.push_back(Box::pin(async move {
            let result = AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .map_err(|error| panic_to_string(&error));
            (position, result)
        }));
    }

    pub async fn join_all(mut self) -> Vec<Result<T, String>> {
        let task_count = self.tasks.len();
        let mut active = FuturesUnordered::new();
        for _ in 0..self.max_concurrent.get().min(task_count) {
            if let Some(task) = self.tasks.pop_front() {
                active.push(task);
            }
        }

        let mut completed = Vec::with_capacity(task_count);
        while let Some(result) = active.next().await {
            completed.push(result);
            if let Some(task) = self.tasks.pop_front() {
                active.push(task);
            }
        }
        completed.sort_unstable_by_key(|(position, _)| *position);
        completed.into_iter().map(|(_, result)| result).collect()
    }
}

fn panic_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    let msg = if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    };
    let bt = Backtrace::force_capture();
    format!("{msg}\n\nBacktrace:\n{bt}")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn caps_concurrency_and_refills_on_completion() {
        smol::block_on(async {
            let active = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let (started_tx, started_rx) = flume::unbounded();
            let (release_tx, release_rx) = flume::unbounded();
            let mut set = TaskSet::new(NonZeroUsize::new(2).expect("non-zero limit"));

            for index in 0..5 {
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                let started_tx = started_tx.clone();
                let release_rx = release_rx.clone();
                set.spawn(async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    started_tx.send(index).expect("record task start");
                    release_rx.recv_async().await.expect("release task");
                    active.fetch_sub(1, Ordering::SeqCst);
                    index
                });
            }

            let join = smol::spawn(set.join_all());
            started_rx.recv_async().await.expect("first task starts");
            started_rx.recv_async().await.expect("second task starts");
            assert!(
                started_rx.try_recv().is_err(),
                "third task started before capacity freed"
            );

            release_tx.send(()).expect("release one task");
            started_rx
                .recv_async()
                .await
                .expect("queued task refills slot");
            assert_eq!(peak.load(Ordering::SeqCst), 2);

            for _ in 0..4 {
                release_tx.send(()).expect("release remaining task");
            }
            let results = join.await;
            assert_eq!(
                results
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .expect("all tasks succeed"),
                vec![0, 1, 2, 3, 4]
            );
        });
    }

    #[test]
    fn preserves_insertion_order_when_tasks_finish_out_of_order() {
        smol::block_on(async {
            let (release_first_tx, release_first_rx) = flume::bounded(1);
            let mut set = TaskSet::new(NonZeroUsize::new(2).expect("non-zero limit"));
            set.spawn(async move {
                release_first_rx.recv_async().await.expect("release first");
                1
            });
            set.spawn(async { 2 });

            let join = smol::spawn(set.join_all());
            smol::future::yield_now().await;
            release_first_tx.send(()).expect("release first");
            let results = join.await;
            assert_eq!(
                results
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .expect("all tasks succeed"),
                vec![1, 2]
            );
        });
    }

    #[test]
    fn isolates_panics() {
        smol::block_on(async {
            let mut set: TaskSet<i32> = TaskSet::new(NonZeroUsize::new(2).expect("non-zero limit"));
            set.spawn(async { 42 });
            set.spawn(async { panic!("oops") });
            set.spawn(async { 7 });
            let results = set.join_all().await;
            assert_eq!(results.len(), 3);
            assert_eq!(results[0].as_ref().expect("first result"), &42);
            let err = results[1].as_ref().expect_err("panic becomes error");
            assert!(err.starts_with("oops"), "unexpected error: {err}");
            assert!(err.contains("Backtrace:"), "missing backtrace in: {err}");
            assert_eq!(results[2].as_ref().expect("third result"), &7);
        });
    }
}
