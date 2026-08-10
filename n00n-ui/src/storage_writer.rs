//! Coalescing write-behind cache with incremental JSONL persistence.
//!
//! Callers only assign an ordering generation and enqueue commands. The single
//! writer thread owns all mutable persistence state, coalesces snapshots, and
//! performs filesystem I/O. Generations make command effects deterministic even
//! when concurrent callers enqueue in the opposite order. Per-session command
//! tracking retains ordering barriers only while an older reserved command can
//! still arrive, then collects them after that command or wake is consumed.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::mem;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use n00n_storage::id::n00nId;
use n00n_storage::sessions::{SESSIONS_DIR, SessionError, SessionLog};
use n00n_storage::{StateDir, StorageError};
use tracing::warn;

use crate::AppSession;

const RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_ATTEMPTS: u32 = 5;

#[derive(Clone)]
struct PendingSnapshot {
    version: SnapshotVersion,
    session: Arc<AppSession>,
}

impl PendingSnapshot {
    fn new(generation: u64, session: Box<AppSession>) -> Self {
        Self {
            version: SnapshotVersion {
                generation,
                revision: session.meta.revision,
            },
            session: Arc::from(session),
        }
    }
}

#[derive(Default)]
struct SnapshotInboxState {
    snapshots: HashMap<n00nId, PendingSnapshot>,
    wake_queued: bool,
}

type SnapshotInbox = Arc<Mutex<SnapshotInboxState>>;

#[derive(Default)]
struct CommandTrackerState {
    next_generation: u64,
    outstanding: HashMap<n00nId, BTreeSet<u64>>,
}

type CommandTracker = Arc<Mutex<CommandTrackerState>>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct SnapshotVersion {
    generation: u64,
    revision: u64,
}

struct FailedSnapshot {
    version: SnapshotVersion,
    error: Option<SessionError>,
}

type FailedSnapshots = HashMap<n00nId, FailedSnapshot>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum StorageWriterShutdownError {
    #[error("storage writer stopped with {count} unpersisted snapshot(s)")]
    UnpersistedSnapshots { count: usize },
    #[error("storage writer did not drain within {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("storage writer completion channel disconnected")]
    Disconnected,
}

#[derive(Default)]
struct RetryState {
    attempts: HashMap<(n00nId, u64), u32>,
    exhausted: HashMap<n00nId, SnapshotVersion>,
}

#[derive(Default)]
struct WriterState {
    pending: HashMap<n00nId, PendingSnapshot>,
    latest_generations: HashMap<n00nId, u64>,
    latest_snapshots: HashMap<n00nId, PendingSnapshot>,
    delete_generations: HashMap<n00nId, u64>,
    logs: HashMap<n00nId, SessionLog>,
    durable_versions: HashMap<n00nId, SnapshotVersion>,
    retries: RetryState,
}

type DeleteCallback = Box<dyn FnOnce(Result<(), SessionError>) + Send>;
type PersistCallback = Box<dyn FnOnce(Result<(), SessionError>) + Send>;

enum Op {
    Flush,
    Wake,
    Persist {
        generation: u64,
        session: Box<AppSession>,
        done: PersistCallback,
    },
    Delete {
        id: n00nId,
        generation: u64,
        done: DeleteCallback,
    },
    #[cfg(test)]
    Pause {
        entered: flume::Sender<()>,
        release: flume::Receiver<()>,
    },
    #[cfg(test)]
    Inspect {
        done: flume::Sender<WriterStateCounts>,
    },
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
struct WriterStateCounts {
    latest_generations: usize,
    delete_generations: usize,
    outstanding_commands: usize,
}

pub struct StorageWriter {
    tracker: CommandTracker,
    inbox: SnapshotInbox,
    ops: flume::Sender<Op>,
    done_rx: flume::Receiver<Result<(), usize>>,
}

impl StorageWriter {
    pub fn new(dir: StateDir) -> std::io::Result<Self> {
        let inbox = SnapshotInbox::default();
        let writer_inbox = Arc::clone(&inbox);
        let tracker = CommandTracker::default();
        let writer_tracker = Arc::clone(&tracker);
        let (ops, ops_rx) = flume::unbounded::<Op>();
        let (done_tx, done_rx) = flume::bounded::<Result<(), usize>>(1);

        std::thread::Builder::new()
            .name("storage-writer".into())
            .spawn(move || {
                let mut state = WriterState::default();
                loop {
                    let op = if state.pending.is_empty() {
                        ops_rx.recv().ok()
                    } else {
                        match ops_rx.recv_timeout(RETRY_DELAY) {
                            Ok(op) => Some(op),
                            Err(flume::RecvTimeoutError::Timeout) => Some(Op::Flush),
                            Err(flume::RecvTimeoutError::Disconnected) => None,
                        }
                    };
                    let Some(op) = op else { break };
                    match op {
                        Op::Flush => {
                            state.flush(&dir);
                        }
                        Op::Wake => {
                            state.stage_snapshots(take_snapshots(&writer_inbox), &writer_tracker);
                            if ops_rx.is_empty() {
                                state.flush(&dir);
                            }
                        }
                        Op::Persist {
                            generation,
                            session,
                            done,
                        } => {
                            state.stage_snapshots(take_snapshots(&writer_inbox), &writer_tracker);
                            let id = session.id;
                            let result = state.persist(generation, session, &dir);
                            complete_command(&writer_tracker, id, generation);
                            state.collect_barriers(id, &writer_tracker);
                            done(result);
                        }
                        Op::Delete {
                            id,
                            generation,
                            done,
                        } => {
                            state.stage_snapshots(take_snapshots(&writer_inbox), &writer_tracker);
                            let result = state.delete(id, generation, &dir);
                            complete_command(&writer_tracker, id, generation);
                            state.collect_barriers(id, &writer_tracker);
                            done(result);
                        }
                        #[cfg(test)]
                        Op::Pause { entered, release } => {
                            let _ = entered.send(());
                            let _ = release.recv();
                        }
                        #[cfg(test)]
                        Op::Inspect { done } => {
                            let outstanding_commands = lock_tracker(&writer_tracker)
                                .outstanding
                                .values()
                                .map(BTreeSet::len)
                                .sum();
                            let _ = done.send(WriterStateCounts {
                                latest_generations: state.latest_generations.len(),
                                delete_generations: state.delete_generations.len(),
                                outstanding_commands,
                            });
                        }
                    }
                }
                state.stage_snapshots(take_snapshots(&writer_inbox), &writer_tracker);
                let failed = state.flush(&dir);
                let unpersisted = state.retries.unpersisted_count(&failed);
                let completion = if unpersisted == 0 {
                    Ok(())
                } else {
                    Err(unpersisted)
                };
                let _ = done_tx.send(completion);
            })?;

        Ok(Self {
            tracker,
            inbox,
            ops,
            done_rx,
        })
    }

    pub fn send(&self, session: Box<AppSession>) {
        let id = session.id;
        let generation = reserve_command(&self.tracker, id);
        self.enqueue_reserved_snapshot(generation, session);
    }

    /// Persists this snapshot before invoking `done` on the writer thread.
    pub fn persist(
        &self,
        session: Box<AppSession>,
        done: impl FnOnce(Result<(), SessionError>) + Send + 'static,
    ) {
        let id = session.id;
        let generation = reserve_command(&self.tracker, id);
        let op = Op::Persist {
            generation,
            session,
            done: Box::new(done),
        };
        if let Err(flume::SendError(Op::Persist { done, .. })) = self.ops.send(op) {
            complete_command(&self.tracker, id, generation);
            done(Err(writer_gone()));
        }
    }

    /// Deletes a session on the writer thread after superseded commands have
    /// been rejected by generation. The caller never waits for filesystem I/O.
    pub fn delete(&self, id: n00nId, done: impl FnOnce(Result<(), SessionError>) + Send + 'static) {
        let generation = reserve_command(&self.tracker, id);
        let op = Op::Delete {
            id,
            generation,
            done: Box::new(done),
        };
        if let Err(flume::SendError(Op::Delete { done, .. })) = self.ops.send(op) {
            complete_command(&self.tracker, id, generation);
            done(Err(writer_gone()));
        }
    }

    #[cfg(test)]
    fn enqueue_snapshot(&self, generation: u64, session: Box<AppSession>) {
        reserve_explicit_command(&self.tracker, session.id, generation);
        self.enqueue_reserved_snapshot(generation, session);
    }

    fn enqueue_reserved_snapshot(&self, generation: u64, session: Box<AppSession>) {
        let snapshot = PendingSnapshot::new(generation, session);
        let id = snapshot.session.id;
        let (should_wake, completed_generation) = {
            let mut inbox = lock_inbox(&self.inbox);
            let replace = inbox
                .snapshots
                .get(&id)
                .is_none_or(|current| current.version.generation < generation);
            if replace {
                let replaced = inbox
                    .snapshots
                    .insert(id, snapshot)
                    .map(|snapshot| snapshot.version.generation);
                let should_wake = if inbox.wake_queued {
                    false
                } else {
                    inbox.wake_queued = true;
                    true
                };
                (should_wake, replaced)
            } else {
                (false, Some(generation))
            }
        };
        if let Some(completed_generation) = completed_generation {
            complete_command(&self.tracker, id, completed_generation);
        }
        if should_wake && self.ops.send(Op::Wake).is_err() {
            lock_inbox(&self.inbox).wake_queued = false;
        }
    }

    pub(crate) fn shutdown(self, timeout: Duration) -> Result<(), StorageWriterShutdownError> {
        self.wait_for_shutdown(timeout)
    }

    pub(crate) fn wait_for_shutdown(
        self,
        timeout: Duration,
    ) -> Result<(), StorageWriterShutdownError> {
        let Self { ops, done_rx, .. } = self;
        drop(ops);
        match done_rx.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(count)) => Err(StorageWriterShutdownError::UnpersistedSnapshots { count }),
            Err(flume::RecvTimeoutError::Timeout) => {
                Err(StorageWriterShutdownError::Timeout { timeout })
            }
            Err(flume::RecvTimeoutError::Disconnected) => {
                Err(StorageWriterShutdownError::Disconnected)
            }
        }
    }
}

fn lock_inbox(inbox: &SnapshotInbox) -> std::sync::MutexGuard<'_, SnapshotInboxState> {
    inbox
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl CommandTrackerState {
    fn reserve(&mut self, id: n00nId) -> u64 {
        self.next_generation += 1;
        let generation = self.next_generation;
        self.outstanding.entry(id).or_default().insert(generation);
        generation
    }

    #[cfg(test)]
    fn reserve_explicit(&mut self, id: n00nId, generation: u64) {
        self.next_generation = self.next_generation.max(generation);
        self.outstanding.entry(id).or_default().insert(generation);
    }

    fn complete(&mut self, id: n00nId, generation: u64) {
        let remove_id = self.outstanding.get_mut(&id).is_some_and(|generations| {
            generations.remove(&generation);
            generations.is_empty()
        });
        if remove_id {
            self.outstanding.remove(&id);
        }
    }

    fn has_outstanding_through(&self, id: n00nId, generation: u64) -> bool {
        self.outstanding
            .get(&id)
            .and_then(BTreeSet::first)
            .is_some_and(|oldest| *oldest <= generation)
    }
}

fn lock_tracker(tracker: &CommandTracker) -> std::sync::MutexGuard<'_, CommandTrackerState> {
    tracker
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn reserve_command(tracker: &CommandTracker, id: n00nId) -> u64 {
    lock_tracker(tracker).reserve(id)
}

#[cfg(test)]
fn reserve_explicit_command(tracker: &CommandTracker, id: n00nId, generation: u64) {
    lock_tracker(tracker).reserve_explicit(id, generation);
}

fn complete_command(tracker: &CommandTracker, id: n00nId, generation: u64) {
    lock_tracker(tracker).complete(id, generation);
}

fn take_snapshots(inbox: &SnapshotInbox) -> HashMap<n00nId, PendingSnapshot> {
    let mut inbox = lock_inbox(inbox);
    inbox.wake_queued = false;
    mem::take(&mut inbox.snapshots)
}

impl WriterState {
    fn stage_snapshots(
        &mut self,
        snapshots: HashMap<n00nId, PendingSnapshot>,
        tracker: &CommandTracker,
    ) {
        for snapshot in snapshots.into_values() {
            let id = snapshot.session.id;
            let generation = snapshot.version.generation;
            self.stage_snapshot(snapshot);
            complete_command(tracker, id, generation);
            self.collect_barriers(id, tracker);
        }
    }

    fn collect_barriers(&mut self, id: n00nId, tracker: &CommandTracker) {
        let tracker = lock_tracker(tracker);
        if self
            .latest_generations
            .get(&id)
            .is_some_and(|generation| !tracker.has_outstanding_through(id, *generation))
        {
            self.latest_generations.remove(&id);
        }
        if self
            .delete_generations
            .get(&id)
            .is_some_and(|generation| !tracker.has_outstanding_through(id, *generation))
        {
            self.delete_generations.remove(&id);
        }
    }

    fn stage_snapshot(&mut self, snapshot: PendingSnapshot) -> Option<SnapshotVersion> {
        let id = snapshot.session.id;
        let generation = snapshot.version.generation;
        if self
            .latest_generations
            .get(&id)
            .is_some_and(|latest| *latest >= generation)
            || self
                .delete_generations
                .get(&id)
                .is_some_and(|deleted| *deleted >= generation)
        {
            return self
                .pending
                .get(&id)
                .map(|snapshot| snapshot.version.clone());
        }

        self.latest_generations.insert(id, generation);
        self.latest_snapshots.insert(id, snapshot.clone());
        self.queue_pending_snapshot(snapshot);
        self.pending.get(&id).map(|pending| pending.version.clone())
    }

    fn queue_pending_snapshot(&mut self, snapshot: PendingSnapshot) {
        let id = snapshot.session.id;
        let replace = self.pending.get(&id).is_none_or(|current| {
            current.version.revision < snapshot.version.revision
                || (current.version.revision == snapshot.version.revision
                    && current.version.generation < snapshot.version.generation)
        });
        if replace {
            self.pending.insert(id, snapshot);
        }
    }

    fn flush(&mut self, dir: &StateDir) -> FailedSnapshots {
        self.flush_target(dir, None)
    }

    fn flush_target(
        &mut self,
        dir: &StateDir,
        target: Option<(n00nId, &SnapshotVersion)>,
    ) -> FailedSnapshots {
        let (failed, persisted) = flush(
            &mut self.pending,
            &mut self.logs,
            &mut self.durable_versions,
            dir,
            target,
        );
        for (id, version) in &persisted {
            let snapshot_is_durable = self.durable_versions.get(id) == Some(version);
            if snapshot_is_durable
                && self
                    .latest_snapshots
                    .get(id)
                    .is_some_and(|snapshot| snapshot.version == *version)
            {
                self.latest_snapshots.remove(id);
            }
        }
        self.retries.record_successes(&persisted);
        self.retries.record_failures(&mut self.pending, &failed);
        failed
    }

    fn persist(
        &mut self,
        generation: u64,
        session: Box<AppSession>,
        dir: &StateDir,
    ) -> Result<(), SessionError> {
        let snapshot = PendingSnapshot::new(generation, session);
        let id = snapshot.session.id;
        let target = self.stage_snapshot(snapshot);
        let mut failed = self.flush_target(dir, target.as_ref().map(|version| (id, version)));
        if let Some(target) = target
            && let Some(mut failure) = failed.remove(&id)
            && failure.version == target
        {
            return Err(failure.error.take().unwrap_or_else(unpersisted_snapshot));
        }
        if self.retries.exhausted.contains_key(&id) {
            return Err(unpersisted_snapshot());
        }
        Ok(())
    }

    fn delete(&mut self, id: n00nId, generation: u64, dir: &StateDir) -> Result<(), SessionError> {
        let delete_generation = self
            .delete_generations
            .entry(id)
            .and_modify(|current| *current = (*current).max(generation))
            .or_insert(generation);
        let delete_generation = *delete_generation;
        if self
            .latest_generations
            .get(&id)
            .is_none_or(|latest| *latest < generation)
        {
            self.latest_generations.insert(id, generation);
        }
        self.pending.retain(|session_id, snapshot| {
            *session_id != id || snapshot.version.generation > delete_generation
        });
        self.latest_snapshots.retain(|session_id, snapshot| {
            *session_id != id || snapshot.version.generation > delete_generation
        });

        let durable_is_newer = self
            .durable_versions
            .get(&id)
            .is_some_and(|version| version.generation > delete_generation);
        if durable_is_newer {
            return Ok(());
        }

        let crossing_snapshot = self.latest_snapshots.get(&id).cloned();
        let crossing_target = crossing_snapshot
            .as_ref()
            .map(|snapshot| snapshot.version.clone());
        if let Some(snapshot) = crossing_snapshot {
            self.queue_pending_snapshot(snapshot);
        }

        let primary_path = dir.path().join(SESSIONS_DIR).join(format!("{id}.jsonl"));
        self.logs.remove(&id);
        let delete_result = AppSession::delete(id, dir);
        if !primary_path.exists() {
            self.durable_versions.remove(&id);
            self.retries.clear(id);
        }
        match delete_result {
            Ok(()) | Err(SessionError::Storage(StorageError::NotFound(_))) => {}
            Err(error) => return Err(error),
        }

        let Some(target) = crossing_target else {
            return Ok(());
        };
        let mut failed = self.flush_target(dir, Some((id, &target)));
        if let Some(mut failure) = failed.remove(&id) {
            return Err(failure.error.take().unwrap_or_else(unpersisted_snapshot));
        }
        Ok(())
    }
}

impl RetryState {
    fn record_failures(
        &mut self,
        pending: &mut HashMap<n00nId, PendingSnapshot>,
        failed: &FailedSnapshots,
    ) {
        for (id, failure) in failed {
            let attempts = self
                .attempts
                .entry((*id, failure.version.generation))
                .or_default();
            *attempts += 1;
            if *attempts < MAX_RETRY_ATTEMPTS {
                continue;
            }
            warn!(
                retry_count = *attempts,
                %id,
                revision = failure.version.revision,
                "storage writer exhausted retry attempts, dropping snapshot"
            );
            if pending
                .get(id)
                .is_some_and(|snapshot| snapshot.version == failure.version)
            {
                pending.remove(id);
                self.exhausted.insert(*id, failure.version.clone());
            }
        }
        self.attempts.retain(|(session_id, generation), _| {
            pending
                .get(session_id)
                .is_some_and(|snapshot| snapshot.version.generation == *generation)
        });
    }

    fn record_successes(&mut self, persisted: &[(n00nId, SnapshotVersion)]) {
        for (session_id, snapshot) in persisted {
            let supersedes_exhausted = self.exhausted.get(session_id).is_some_and(|exhausted| {
                exhausted.generation != snapshot.generation
                    && exhausted.revision <= snapshot.revision
            });
            if supersedes_exhausted {
                self.exhausted.remove(session_id);
            }
        }
    }

    fn clear(&mut self, id: n00nId) {
        self.attempts.retain(|(session_id, _), _| *session_id != id);
        self.exhausted.remove(&id);
    }

    fn unpersisted_count(&self, failed: &FailedSnapshots) -> usize {
        failed.len()
            + self
                .exhausted
                .keys()
                .filter(|id| !failed.contains_key(id))
                .count()
    }
}

fn writer_gone() -> SessionError {
    StorageError::Io(io::Error::other("storage writer unavailable")).into()
}

fn unpersisted_snapshot() -> SessionError {
    StorageError::Io(io::Error::other(
        "newer session snapshot remains unpersisted",
    ))
    .into()
}

fn flush(
    pending: &mut HashMap<n00nId, PendingSnapshot>,
    logs: &mut HashMap<n00nId, SessionLog>,
    durable_versions: &mut HashMap<n00nId, SnapshotVersion>,
    dir: &StateDir,
    target: Option<(n00nId, &SnapshotVersion)>,
) -> (FailedSnapshots, Vec<(n00nId, SnapshotVersion)>) {
    let batch = mem::take(pending);
    if batch.is_empty() {
        return (FailedSnapshots::new(), Vec::new());
    }
    let sessions_dir = match dir.ensure_subdir(SESSIONS_DIR) {
        Ok(sessions_dir) => sessions_dir,
        Err(error) => {
            warn!(error = %error, "failed to ensure sessions dir");
            let mut target_error = Some(SessionError::from(error));
            let mut failed = FailedSnapshots::with_capacity(batch.len());
            for (id, snapshot) in batch {
                let error = if target.is_some_and(|(target_id, version)| {
                    target_id == id && *version == snapshot.version
                }) {
                    target_error.take()
                } else {
                    None
                };
                failed.insert(
                    id,
                    FailedSnapshot {
                        version: snapshot.version.clone(),
                        error,
                    },
                );
                pending.insert(id, snapshot);
            }
            return (failed, Vec::new());
        }
    };

    let mut failed = FailedSnapshots::new();
    let mut persisted = Vec::new();
    for snapshot in batch.into_values() {
        let id = snapshot.session.id;
        match write_session(
            &sessions_dir,
            logs,
            durable_versions,
            &snapshot.version,
            &snapshot.session,
        ) {
            Ok(()) => persisted.push((id, snapshot.version)),
            Err(error) => {
                warn!(error = %error, %id, "session write failed");
                let is_target = target.is_some_and(|(target_id, version)| {
                    target_id == id && *version == snapshot.version
                });
                failed.insert(
                    id,
                    FailedSnapshot {
                        version: snapshot.version.clone(),
                        error: is_target.then_some(error),
                    },
                );
                pending.insert(id, snapshot);
            }
        }
    }
    (failed, persisted)
}

fn append_or_compact_result(
    log: &mut SessionLog,
    sessions_dir: &Path,
    session: &AppSession,
) -> Result<(), SessionError> {
    match log.append(session) {
        Ok(()) => Ok(()),
        Err(SessionError::CursorAhead { .. }) => log.compact(sessions_dir, session),
        Err(error) => Err(error),
    }
}

fn write_session(
    sessions_dir: &Path,
    logs: &mut HashMap<n00nId, SessionLog>,
    durable_versions: &mut HashMap<n00nId, SnapshotVersion>,
    version: &SnapshotVersion,
    session: &AppSession,
) -> Result<(), SessionError> {
    if durable_versions
        .get(&session.id)
        .is_some_and(|durable| durable.revision > version.revision)
    {
        return Ok(());
    }
    if let Some(log) = logs.get_mut(&session.id) {
        if durable_versions
            .get(&session.id)
            .is_some_and(|durable| durable.revision == version.revision)
        {
            log.compact(sessions_dir, session)?;
        } else {
            append_or_compact_result(log, sessions_dir, session)?;
        }
        durable_versions.insert(session.id, version.clone());
        return Ok(());
    }
    let (mut log, on_disk_revision) = open_or_create_log(sessions_dir, session)?;
    if on_disk_revision > version.revision {
        durable_versions.insert(
            session.id,
            SnapshotVersion {
                generation: 0,
                revision: on_disk_revision,
            },
        );
        return Ok(());
    }
    if on_disk_revision == version.revision {
        log.compact(sessions_dir, session)?;
    } else {
        append_or_compact_result(&mut log, sessions_dir, session)?;
    }
    logs.insert(session.id, log);
    durable_versions.insert(session.id, version.clone());
    Ok(())
}

fn open_or_create_log(
    sessions_dir: &Path,
    session: &AppSession,
) -> Result<(SessionLog, u64), SessionError> {
    let jsonl_path = sessions_dir.join(format!("{}.jsonl", session.id));
    if jsonl_path.exists() {
        let id = session.id;
        let (loaded, log) = SessionLog::open::<
            n00n_providers::Message,
            n00n_providers::TokenUsage,
            n00n_agent::ToolOutput,
        >(sessions_dir, id)?;
        Ok((log, loaded.meta.revision))
    } else {
        Ok((
            AppSession::migrate_to_jsonl(sessions_dir, session)?,
            session.meta.revision,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use n00n_storage::sessions::lock_openai_response_chain;
    use tempfile::TempDir;

    const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
    const BLOCKED_TIMEOUT: Duration = Duration::from_millis(100);
    const NONBLOCKING_TIMEOUT: Duration = Duration::from_secs(2);
    const OPENAI_RESPONSE_SUFFIX: &str = "openai-response.json";
    const STRESS_SNAPSHOT_COUNT: u64 = 10_000;

    fn state_dir() -> (TempDir, StateDir) {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        (tmp, dir)
    }

    fn pause_writer(writer: &StorageWriter) -> flume::Sender<()> {
        let (entered_tx, entered_rx) = flume::bounded(1);
        let (release_tx, release_rx) = flume::bounded(1);
        writer
            .ops
            .send(Op::Pause {
                entered: entered_tx,
                release: release_rx,
            })
            .unwrap();
        entered_rx.recv_timeout(DRAIN_TIMEOUT).unwrap();
        release_tx
    }

    fn inspect_writer(writer: &StorageWriter) -> WriterStateCounts {
        let (done_tx, done_rx) = flume::bounded(1);
        writer.ops.send(Op::Inspect { done: done_tx }).unwrap();
        done_rx.recv_timeout(DRAIN_TIMEOUT).unwrap()
    }

    fn persist_and_wait(writer: &StorageWriter, session: AppSession) -> Result<(), SessionError> {
        let (done_tx, done_rx) = flume::bounded(1);
        writer.persist(Box::new(session), move |result| {
            done_tx.send(result).unwrap();
        });
        done_rx.recv_timeout(DRAIN_TIMEOUT).unwrap()
    }

    fn delete_and_wait(writer: &StorageWriter, id: n00nId) -> Result<(), SessionError> {
        let (done_tx, done_rx) = flume::bounded(1);
        writer.delete(id, move |result| {
            done_tx.send(result).unwrap();
        });
        done_rx.recv_timeout(DRAIN_TIMEOUT).unwrap()
    }

    #[test]
    fn shutdown_drains_newest_snapshot_of_every_session() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir.clone()).unwrap();
        let a = AppSession::new("test-model", "/tmp/a");
        let mut b = AppSession::new("test-model", "/tmp/b");
        let (a_id, b_id) = (a.id, b.id);
        writer.send(Box::new(a));
        writer.send(Box::new(b.clone()));
        b.title = "renamed".into();
        writer.send(Box::new(b));

        writer.shutdown(DRAIN_TIMEOUT).unwrap();

        assert!(AppSession::load(a_id, &dir).is_ok());
        assert_eq!(AppSession::load(b_id, &dir).unwrap().title, "renamed");
    }

    #[test]
    fn blocked_writer_coalesces_same_session_snapshots_and_persists_latest() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir.clone()).unwrap();
        let release = pause_writer(&writer);
        let mut session = AppSession::new("test-model", "/tmp/coalesced");
        let id = session.id;

        for revision in 1..=STRESS_SNAPSHOT_COUNT {
            session.meta.revision = revision;
            session.meta.input_draft = Some(format!("snapshot {revision}"));
            writer.send(Box::new(session.clone()));
        }

        let inbox = lock_inbox(&writer.inbox);
        assert_eq!(inbox.snapshots.len(), 1);
        assert!(inbox.wake_queued);
        assert_eq!(
            inbox.snapshots[&id].session.meta.input_draft.as_deref(),
            Some("snapshot 10000")
        );
        drop(inbox);
        assert_eq!(writer.ops.len(), 1);

        release.send(()).unwrap();
        writer.shutdown(DRAIN_TIMEOUT).unwrap();
        let loaded = AppSession::load(id, &dir).unwrap();
        assert_eq!(loaded.meta.revision, STRESS_SNAPSHOT_COUNT);
        assert_eq!(loaded.meta.input_draft.as_deref(), Some("snapshot 10000"));
    }

    #[test]
    fn inverse_delete_save_enqueue_order_obeys_generation() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir.clone()).unwrap();
        let deleted = AppSession::new("test-model", "/tmp/delete-newer");
        let deleted_id = deleted.id;
        let saved = AppSession::new("test-model", "/tmp/save-newer");
        let saved_id = saved.id;
        persist_and_wait(&writer, deleted.clone()).unwrap();
        persist_and_wait(&writer, saved.clone()).unwrap();
        let release = pause_writer(&writer);

        reserve_explicit_command(&writer.tracker, deleted_id, 20);
        writer
            .ops
            .send(Op::Delete {
                id: deleted_id,
                generation: 20,
                done: Box::new(|result| assert!(result.is_ok())),
            })
            .unwrap();
        writer.enqueue_snapshot(10, Box::new(deleted));
        let mut newer_saved = saved;
        newer_saved.title = "newer save".into();
        writer.enqueue_snapshot(40, Box::new(newer_saved));
        reserve_explicit_command(&writer.tracker, saved_id, 30);
        writer
            .ops
            .send(Op::Delete {
                id: saved_id,
                generation: 30,
                done: Box::new(|result| assert!(result.is_ok())),
            })
            .unwrap();
        release.send(()).unwrap();

        writer.shutdown(DRAIN_TIMEOUT).unwrap();

        assert!(AppSession::load(deleted_id, &dir).is_err());
        assert_eq!(
            AppSession::load(saved_id, &dir).unwrap().title,
            "newer save"
        );
    }

    #[test]
    fn delayed_delete_barrier_discards_pre_delete_higher_revision() {
        let (_tmp, dir) = state_dir();
        let mut state = WriterState::default();
        let mut pre_delete = AppSession::new("test-model", "/tmp/delete-crossing");
        pre_delete.title = "pre-delete revision five".into();
        pre_delete.meta.revision = 5;
        let id = pre_delete.id;
        let mut post_delete = pre_delete.clone();
        post_delete.title = "post-delete revision four".into();
        post_delete.meta.revision = 4;

        state.stage_snapshot(PendingSnapshot::new(1, Box::new(pre_delete)));
        assert!(state.flush(&dir).is_empty());
        state.stage_snapshot(PendingSnapshot::new(3, Box::new(post_delete)));
        assert!(state.flush(&dir).is_empty());
        state.delete(id, 2, &dir).unwrap();

        let loaded = AppSession::load(id, &dir).unwrap();
        assert_eq!(loaded.meta.revision, 4);
        assert_eq!(loaded.title, "post-delete revision four");
    }

    #[test]
    fn delayed_delete_preserves_highest_revision_post_delete_snapshot() {
        let (_tmp, dir) = state_dir();
        let mut state = WriterState::default();
        let mut revision_nine = AppSession::new("test-model", "/tmp/delete-post-delete-order");
        revision_nine.title = "generation five revision nine".into();
        revision_nine.meta.revision = 9;
        let id = revision_nine.id;
        let mut revision_four = revision_nine.clone();
        revision_four.title = "generation seven revision four".into();
        revision_four.meta.revision = 4;

        state.stage_snapshot(PendingSnapshot::new(5, Box::new(revision_nine)));
        state.stage_snapshot(PendingSnapshot::new(7, Box::new(revision_four)));
        state.delete(id, 3, &dir).unwrap();

        let loaded = AppSession::load(id, &dir).unwrap();
        assert_eq!(loaded.meta.revision, 9);
        assert_eq!(loaded.title, "generation five revision nine");
    }

    #[test]
    fn partial_delete_requeues_crossing_generation_lower_revision() {
        let (_tmp, dir) = state_dir();
        let mut state = WriterState::default();
        let mut pre_delete = AppSession::new("test-model", "/tmp/partial-delete-crossing");
        pre_delete.title = "pre-delete revision five".into();
        pre_delete.meta.revision = 5;
        let id = pre_delete.id;
        let mut post_delete = pre_delete.clone();
        post_delete.title = "post-delete revision four".into();
        post_delete.meta.revision = 4;
        let mut durable = pre_delete.clone();
        durable.title = "durable revision zero".into();
        durable.meta.revision = 0;

        state.stage_snapshot(PendingSnapshot::new(0, Box::new(durable)));
        assert!(state.flush(&dir).is_empty());
        state.stage_snapshot(PendingSnapshot::new(1, Box::new(pre_delete)));
        state.stage_snapshot(PendingSnapshot::new(3, Box::new(post_delete)));
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR).unwrap();
        let sidecar_path = sessions_dir.join(format!("{id}.{OPENAI_RESPONSE_SUFFIX}"));
        fs::create_dir(&sidecar_path).unwrap();

        assert!(state.delete(id, 2, &dir).is_err());
        assert!(!sessions_dir.join(format!("{id}.jsonl")).exists());
        assert_eq!(state.pending[&id].version.generation, 3);
        assert_eq!(state.pending[&id].version.revision, 4);

        assert!(state.flush(&dir).is_empty());
        let loaded = AppSession::load(id, &dir).unwrap();
        assert_eq!(loaded.meta.revision, 4);
        assert_eq!(loaded.title, "post-delete revision four");
    }

    #[test]
    fn completed_unique_nonexistent_deletes_do_not_accumulate_tombstones() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir).unwrap();
        let (done_tx, done_rx) = flume::unbounded();

        for _ in 0..1_000 {
            let done_tx = done_tx.clone();
            writer.delete(n00nId::generate(), move |result| {
                done_tx.send(result).unwrap();
            });
        }
        drop(done_tx);
        for _ in 0..1_000 {
            assert!(done_rx.recv_timeout(DRAIN_TIMEOUT).unwrap().is_ok());
        }

        assert_eq!(
            inspect_writer(&writer),
            WriterStateCounts {
                latest_generations: 0,
                delete_generations: 0,
                outstanding_commands: 0,
            }
        );
        writer.shutdown(DRAIN_TIMEOUT).unwrap();
    }

    #[test]
    fn explicit_persist_preserves_ensure_subdir_io_error() {
        let tmp = TempDir::new().unwrap();
        let state_path = tmp.path().join("state-file");
        fs::write(&state_path, b"not a directory").unwrap();
        let expected = fs::create_dir_all(state_path.join(SESSIONS_DIR)).unwrap_err();
        let writer = StorageWriter::new(StateDir::from_path(state_path)).unwrap();
        let session = AppSession::new("test-model", "/tmp/ensure-subdir-error");

        let error = persist_and_wait(&writer, session).unwrap_err();
        let SessionError::Storage(StorageError::Io(error)) = error else {
            panic!("expected storage I/O error");
        };
        assert_eq!(error.kind(), expected.kind());
        assert_eq!(error.raw_os_error(), expected.raw_os_error());
        assert!(matches!(
            writer.shutdown(DRAIN_TIMEOUT),
            Err(StorageWriterShutdownError::UnpersistedSnapshots { count: 1 })
        ));
    }

    #[test]
    fn stale_equal_revision_persist_cannot_overwrite_newer_save() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir.clone()).unwrap();
        let mut stale = AppSession::new("test-model", "/tmp/equal-race");
        stale.title = "stale persist".into();
        let id = stale.id;
        let mut newer = stale.clone();
        newer.title = "newer save".into();
        let release = pause_writer(&writer);
        let (done_tx, done_rx) = flume::bounded(1);

        reserve_explicit_command(&writer.tracker, id, 1);
        writer.enqueue_snapshot(2, Box::new(newer));
        writer
            .ops
            .send(Op::Persist {
                generation: 1,
                session: Box::new(stale),
                done: Box::new(move |result| done_tx.send(result).unwrap()),
            })
            .unwrap();
        release.send(()).unwrap();

        assert!(done_rx.recv_timeout(DRAIN_TIMEOUT).unwrap().is_ok());
        writer.shutdown(DRAIN_TIMEOUT).unwrap();
        assert_eq!(AppSession::load(id, &dir).unwrap().title, "newer save");
    }

    #[test]
    fn failed_stale_persist_retains_newer_snapshot_for_shutdown() {
        let (tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir).unwrap();
        let mut stale = AppSession::new("test-model", "/tmp/persist-failure");
        stale.title = "stale persist".into();
        let id = stale.id;
        let mut newer = stale.clone();
        newer.title = "newer pending".into();
        fs::create_dir_all(tmp.path().join(SESSIONS_DIR).join(format!("{id}.jsonl"))).unwrap();
        let release = pause_writer(&writer);
        let (done_tx, done_rx) = flume::bounded(1);

        reserve_explicit_command(&writer.tracker, id, 1);
        writer.enqueue_snapshot(2, Box::new(newer));
        writer
            .ops
            .send(Op::Persist {
                generation: 1,
                session: Box::new(stale),
                done: Box::new(move |result| done_tx.send(result).unwrap()),
            })
            .unwrap();
        release.send(()).unwrap();

        assert!(done_rx.recv_timeout(DRAIN_TIMEOUT).unwrap().is_err());
        assert!(matches!(
            writer.shutdown(DRAIN_TIMEOUT),
            Err(StorageWriterShutdownError::UnpersistedSnapshots { count: 1 })
        ));
    }

    #[test]
    fn retry_exhaustion_remains_accounted_at_shutdown() {
        let (tmp, dir) = state_dir();
        let mut state = WriterState::default();
        let session = AppSession::new("test-model", "/tmp/exhausted");
        let id = session.id;
        fs::create_dir_all(tmp.path().join(SESSIONS_DIR).join(format!("{id}.jsonl"))).unwrap();
        state.stage_snapshot(PendingSnapshot::new(1, Box::new(session)));

        for _ in 0..MAX_RETRY_ATTEMPTS {
            state.flush(&dir);
        }

        assert!(state.pending.is_empty());
        assert_eq!(state.retries.unpersisted_count(&FailedSnapshots::new()), 1);
    }

    #[test]
    fn successful_delete_clears_exhausted_retry_state() {
        let (_tmp, dir) = state_dir();
        let mut state = WriterState::default();
        let mut session = AppSession::new("test-model", "/tmp/delete-exhausted");
        let id = session.id;
        state.stage_snapshot(PendingSnapshot::new(1, Box::new(session.clone())));
        assert!(state.flush(&dir).is_empty());

        session.meta.revision += 1;
        let failed_snapshot = PendingSnapshot::new(2, Box::new(session));
        let failed_version = failed_snapshot.version.clone();
        state.stage_snapshot(failed_snapshot);
        let failed = FailedSnapshots::from([(
            id,
            FailedSnapshot {
                version: failed_version.clone(),
                error: None,
            },
        )]);
        for _ in 0..MAX_RETRY_ATTEMPTS {
            state.retries.record_failures(&mut state.pending, &failed);
        }
        assert_eq!(state.retries.exhausted.get(&id), Some(&failed_version));
        assert_eq!(state.retries.unpersisted_count(&FailedSnapshots::new()), 1);

        state.delete(id, 3, &dir).unwrap();

        assert!(!state.retries.exhausted.contains_key(&id));
        assert_eq!(state.retries.unpersisted_count(&FailedSnapshots::new()), 0);
        assert!(AppSession::load(id, &dir).is_err());
    }

    #[test]
    fn public_operations_do_not_block_behind_writer_filesystem_io() {
        let (_tmp, dir) = state_dir();
        let writer = Arc::new(StorageWriter::new(dir.clone()).unwrap());
        let session = AppSession::new("test-model", "/tmp/nonblocking");
        let id = session.id;
        persist_and_wait(&writer, session).unwrap();
        let response_lock = lock_openai_response_chain(&dir, id).unwrap();
        let (delete_tx, delete_rx) = flume::bounded(1);
        writer.delete(id, move |result| delete_tx.send(result).unwrap());
        let barrier = AppSession::new("test-model", "/tmp/io-barrier");
        let (barrier_tx, barrier_rx) = flume::bounded(1);
        writer.persist(Box::new(barrier), move |result| {
            barrier_tx.send(result).unwrap();
        });
        assert!(matches!(
            barrier_rx.recv_timeout(BLOCKED_TIMEOUT),
            Err(flume::RecvTimeoutError::Timeout)
        ));

        let caller_writer = Arc::clone(&writer);
        let (returned_tx, returned_rx) = flume::bounded(1);
        let caller = std::thread::spawn(move || {
            caller_writer.send(Box::new(AppSession::new(
                "test-model",
                "/tmp/nonblocking-save",
            )));
            caller_writer.delete(n00nId::generate(), |_| {});
            returned_tx.send(()).unwrap();
        });
        returned_rx.recv_timeout(NONBLOCKING_TIMEOUT).unwrap();
        caller.join().unwrap();

        drop(response_lock);
        assert!(delete_rx.recv_timeout(DRAIN_TIMEOUT).unwrap().is_ok());
        assert!(barrier_rx.recv_timeout(DRAIN_TIMEOUT).unwrap().is_ok());
        Arc::into_inner(writer)
            .unwrap()
            .shutdown(DRAIN_TIMEOUT)
            .unwrap();
    }

    #[test]
    fn partial_delete_closes_cached_log_before_later_persist() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir.clone()).unwrap();
        let mut session = AppSession::new("test-model", "/tmp/partial-delete");
        let id = session.id;
        persist_and_wait(&writer, session.clone()).unwrap();
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR).unwrap();
        let log_path = sessions_dir.join(format!("{id}.jsonl"));
        let sidecar_path = sessions_dir.join(format!("{id}.{OPENAI_RESPONSE_SUFFIX}"));
        fs::create_dir(&sidecar_path).unwrap();

        assert!(delete_and_wait(&writer, id).is_err());
        assert!(!log_path.exists());
        fs::remove_dir(sidecar_path).unwrap();

        session.meta.input_draft = Some("persisted after partial delete".into());
        session.meta.revision += 1;
        persist_and_wait(&writer, session).unwrap();
        assert_eq!(
            AppSession::load(id, &dir)
                .unwrap()
                .meta
                .input_draft
                .as_deref(),
            Some("persisted after partial delete")
        );
        writer.shutdown(DRAIN_TIMEOUT).unwrap();
    }

    #[test]
    fn persist_reports_success_after_writing_snapshot() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir.clone()).unwrap();
        let session = AppSession::new("test-model", "/tmp/persist");
        let id = session.id;

        assert!(persist_and_wait(&writer, session).is_ok());
        assert!(AppSession::load(id, &dir).is_ok());
        writer.shutdown(DRAIN_TIMEOUT).unwrap();
    }
}
