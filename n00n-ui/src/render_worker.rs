//! Thread pool for syntax highlighting. Threads scale up to CPU count and exit after
//! `IDLE_TIMEOUT` (5 s) of inactivity. Jobs carry monotonic u64 IDs so callers can
//! discard stale results.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tracing::error;

use crate::components::code_view::{self, RenderLimits};
use n00n_agent::{ToolInput, ToolOutput};
use ratatui::text::Line;

const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RENDER_THREADS: usize = 4;
const JOB_QUEUE_CAPACITY: usize = 64;
const RESULT_QUEUE_CAPACITY: usize = 64;

struct RenderJob {
    id: u64,
    identity: RenderIdentity,
    tool_input: Option<Arc<ToolInput>>,
    tool_output: Option<Arc<ToolOutput>>,
    limits: RenderLimits,
}

pub struct RenderResult {
    pub id: u64,
    pub lines: Vec<Line<'static>>,
}

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
pub struct RenderIdentity {
    latest_job_id: Arc<AtomicU64>,
}

impl RenderIdentity {
    pub fn cancel(&self) {
        self.latest_job_id.store(0, Ordering::Release);
    }

    fn set_latest(&self, id: u64) {
        self.latest_job_id.store(id, Ordering::Release);
    }

    fn is_latest(&self, id: u64) -> bool {
        self.latest_job_id.load(Ordering::Acquire) == id
    }
}

struct PoolInner {
    job_rx: flume::Receiver<RenderJob>,
    result_tx: flume::Sender<RenderResult>,
    result_publish_lock: Mutex<()>,
    active_threads: AtomicUsize,
    max_threads: usize,
}

pub struct RenderWorker {
    job_tx: flume::Sender<RenderJob>,
    job_publish_lock: Mutex<()>,
    inner: Arc<PoolInner>,
    result_rx: flume::Receiver<RenderResult>,
}

impl RenderWorker {
    pub fn new() -> Self {
        let (job_tx, job_rx) = flume::bounded(JOB_QUEUE_CAPACITY);
        let (result_tx, result_rx) = flume::bounded(RESULT_QUEUE_CAPACITY);
        let max_threads = thread::available_parallelism()
            .map_or(MAX_RENDER_THREADS, std::num::NonZero::get)
            .min(MAX_RENDER_THREADS);

        Self {
            job_tx,
            job_publish_lock: Mutex::new(()),
            inner: Arc::new(PoolInner {
                job_rx,
                result_tx,
                result_publish_lock: Mutex::new(()),
                active_threads: AtomicUsize::new(0),
                max_threads,
            }),
            result_rx,
        }
    }

    pub fn send(
        &self,
        tool_input: Option<Arc<ToolInput>>,
        tool_output: Option<Arc<ToolOutput>>,
        limits: RenderLimits,
    ) -> u64 {
        let identity = RenderIdentity::default();
        let (id, _) = self.enqueue(&identity, tool_input, tool_output, limits);
        id
    }

    pub fn send_latest(
        &self,
        identity: &RenderIdentity,
        tool_input: Option<Arc<ToolInput>>,
        tool_output: Option<Arc<ToolOutput>>,
        limits: RenderLimits,
    ) -> Option<u64> {
        let (id, queued) = self.enqueue(identity, tool_input, tool_output, limits);
        queued.then_some(id)
    }

    fn enqueue(
        &self,
        identity: &RenderIdentity,
        tool_input: Option<Arc<ToolInput>>,
        tool_output: Option<Arc<ToolOutput>>,
        limits: RenderLimits,
    ) -> (u64, bool) {
        let id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
        identity.set_latest(id);
        let Ok(_guard) = self.job_publish_lock.lock() else {
            error!("render job queue lock poisoned");
            return (id, false);
        };
        let queued = match self.job_tx.try_send(RenderJob {
            id,
            identity: identity.clone(),
            tool_input,
            tool_output,
            limits,
        }) {
            Ok(()) => true,
            Err(flume::TrySendError::Full(_)) => false,
            Err(flume::TrySendError::Disconnected(_)) => {
                error!("render job queue disconnected");
                false
            }
        };
        if queued {
            self.maybe_spawn_thread();
        } else {
            identity.cancel();
        }
        (id, queued)
    }

    #[allow(clippy::manual_ok_err)]
    pub fn try_recv(&self) -> Option<RenderResult> {
        match self.result_rx.try_recv() {
            Ok(result) => Some(result),
            Err(flume::TryRecvError::Empty | flume::TryRecvError::Disconnected) => None,
        }
    }

    #[cfg(test)]
    pub fn enqueue_result_for_test(&self, result: RenderResult) {
        publish_result(&self.inner, result);
    }

    #[cfg(test)]
    pub fn pending_results_for_test(&self) -> usize {
        self.result_rx.len()
    }

    fn maybe_spawn_thread(&self) {
        let current = self.inner.active_threads.load(Ordering::Acquire);
        if current >= self.inner.max_threads {
            return;
        }
        if self
            .inner
            .active_threads
            .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        let inner = Arc::clone(&self.inner);
        if let Err(e) = thread::Builder::new()
            .name("render".into())
            .spawn(move || worker_loop(&inner))
        {
            self.inner.active_threads.fetch_sub(1, Ordering::AcqRel);
            error!("failed to spawn render thread: {e}");
        }
    }
}

fn worker_loop(inner: &PoolInner) {
    while let Some(job) = recv_current_job(inner) {
        let content = code_view::render_tool_content(
            job.tool_input.as_deref(),
            job.tool_output.as_deref(),
            true,
            job.limits,
        );
        if job.identity.is_latest(job.id) {
            publish_result(
                inner,
                RenderResult {
                    id: job.id,
                    lines: content.lines,
                },
            );
        }
    }
    inner.active_threads.fetch_sub(1, Ordering::AcqRel);
}

fn recv_current_job(inner: &PoolInner) -> Option<RenderJob> {
    while let Ok(job) = inner.job_rx.recv_timeout(IDLE_TIMEOUT) {
        if job.identity.is_latest(job.id) {
            return Some(job);
        }
    }
    None
}

fn publish_result(inner: &PoolInner, result: RenderResult) {
    let Ok(_guard) = inner.result_publish_lock.lock() else {
        error!("render result queue lock poisoned");
        return;
    };
    if inner.result_tx.send(result).is_err() {
        error!("render result queue disconnected");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_worker(active: usize, max: usize) -> RenderWorker {
        let (job_tx, job_rx) = flume::bounded(JOB_QUEUE_CAPACITY);
        let (result_tx, result_rx) = flume::bounded(RESULT_QUEUE_CAPACITY);
        RenderWorker {
            job_tx,
            job_publish_lock: Mutex::new(()),
            inner: Arc::new(PoolInner {
                job_rx,
                result_tx,
                result_publish_lock: Mutex::new(()),
                active_threads: AtomicUsize::new(active),
                max_threads: max,
            }),
            result_rx,
        }
    }

    #[test]
    fn does_not_spawn_when_at_cap() {
        let worker = make_worker(2, 2);
        worker.maybe_spawn_thread();
        assert_eq!(worker.inner.active_threads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn superseded_job_is_skipped_before_rendering() {
        let worker = make_worker(1, 1);
        let identity = RenderIdentity::default();
        let limits = RenderLimits {
            script: 1,
            output: 1,
            details: 1,
        };
        let first = worker.send_latest(&identity, None, None, limits);
        let second = worker.send_latest(&identity, None, None, limits);

        let job = recv_current_job(&worker.inner).expect("latest job should remain queued");

        assert_ne!(first, second);
        assert_eq!(Some(job.id), second);
        assert!(worker.inner.job_rx.try_recv().is_err());
    }

    #[test]
    fn full_job_queue_rejects_new_work_without_losing_queued_jobs() {
        let worker = make_worker(1, 1);
        let limits = RenderLimits {
            script: 1,
            output: 1,
            details: 1,
        };
        for _ in 0..JOB_QUEUE_CAPACITY {
            let identity = RenderIdentity::default();
            assert!(worker.send_latest(&identity, None, None, limits).is_some());
        }

        let rejected = RenderIdentity::default();
        assert!(worker.send_latest(&rejected, None, None, limits).is_none());
        assert_eq!(worker.inner.job_rx.len(), JOB_QUEUE_CAPACITY);
        while let Ok(job) = worker.inner.job_rx.try_recv() {
            assert!(job.identity.is_latest(job.id));
        }
    }

    #[test]
    fn more_than_capacity_results_are_not_lost() {
        let worker = make_worker(1, 1);
        let result_count = RESULT_QUEUE_CAPACITY + 1;
        for id in 0..RESULT_QUEUE_CAPACITY as u64 {
            publish_result(
                &worker.inner,
                RenderResult {
                    id,
                    lines: Vec::new(),
                },
            );
        }

        thread::scope(|scope| {
            scope.spawn(|| {
                publish_result(
                    &worker.inner,
                    RenderResult {
                        id: RESULT_QUEUE_CAPACITY as u64,
                        lines: Vec::new(),
                    },
                );
            });

            for expected_id in 0..result_count as u64 {
                let result = worker
                    .result_rx
                    .recv_timeout(IDLE_TIMEOUT)
                    .expect("each completed result should remain available");
                assert_eq!(result.id, expected_id);
            }
        });
    }
}
