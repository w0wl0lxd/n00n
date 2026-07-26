//! Coalescing write-behind cache with incremental JSONL persistence.
//!
//! Apps post session snapshots keyed by session id; the writer thread drains
//! the newest snapshot of every session per wake and performs O(delta)
//! appends. Deletes run on the same thread, so an append and a delete of the
//! same session can never race: a queued save cannot resurrect deleted files.

use std::collections::HashMap;
use std::io;
use std::mem;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use n00n_storage::StateDir;
use n00n_storage::id::N00nId;
use n00n_storage::sessions::{SESSIONS_DIR, SessionError, SessionLog};
use tracing::warn;

use crate::AppSession;

struct PendingSnapshot {
    revision: u64,
    retry_attempts: u32,
    session: Box<AppSession>,
}

type Pending = Arc<Mutex<HashMap<N00nId, PendingSnapshot>>>;

type DeleteCallback = Box<dyn FnOnce(Result<(), SessionError>) + Send>;
type PersistCallback = Box<dyn FnOnce(Result<(), SessionError>) + Send>;

const RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_ATTEMPTS: u32 = 5;

enum Op {
    Flush,
    Persist {
        session: Box<AppSession>,
        done: PersistCallback,
    },
    Delete {
        id: N00nId,
        done: DeleteCallback,
    },
}

pub struct StorageWriter {
    pending: Pending,
    ops: flume::Sender<Op>,
    done_rx: flume::Receiver<()>,
}

impl StorageWriter {
    pub fn new(dir: StateDir) -> std::io::Result<Self> {
        let pending: Pending = Arc::default();
        let writer_pending = Arc::clone(&pending);
        let (ops, ops_rx) = flume::unbounded::<Op>();
        let (done_tx, done_rx) = flume::bounded::<()>(1);

        std::thread::Builder::new()
            .name("storage-writer".into())
            .spawn(move || {
                let mut logs: HashMap<N00nId, SessionLog> = HashMap::new();
                let mut durable_revisions: HashMap<N00nId, u64> = HashMap::new();
                loop {
                    let op = if lock(&writer_pending).is_empty() {
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
                            flush(&writer_pending, &mut logs, &mut durable_revisions, &dir);
                        }
                        Op::Persist { session, done } => {
                            flush(&writer_pending, &mut logs, &mut durable_revisions, &dir);
                            done(persist_session(
                                &writer_pending,
                                &mut logs,
                                &mut durable_revisions,
                                &dir,
                                &session,
                            ));
                        }
                        Op::Delete { id, done } => {
                            lock(&writer_pending).remove(&id);
                            logs.remove(&id);
                            done(AppSession::delete(id, &dir));
                        }
                    }
                }
                flush(&writer_pending, &mut logs, &mut durable_revisions, &dir);
                let _ = done_tx.send(());
            })?;

        Ok(Self {
            pending,
            ops,
            done_rx,
        })
    }

    pub fn send(&self, session: Box<AppSession>) {
        let mut pending = lock(&self.pending);
        let was_empty = pending.is_empty();
        let revision = session.meta.revision;
        let replace = pending
            .get(&session.id)
            .is_none_or(|current| current.revision <= revision);
        if replace {
            pending.insert(
                session.id,
                PendingSnapshot {
                    revision,
                    retry_attempts: 0,
                    session,
                },
            );
        }
        drop(pending);
        if was_empty {
            let _ = self.ops.send(Op::Flush);
        }
    }

    /// Persists this snapshot before invoking `done` on the writer thread.
    pub fn persist(
        &self,
        session: Box<AppSession>,
        done: impl FnOnce(Result<(), SessionError>) + Send + 'static,
    ) {
        let op = Op::Persist {
            session,
            done: Box::new(done),
        };
        if let Err(flume::SendError(Op::Persist { done, .. })) = self.ops.send(op) {
            done(Err(writer_gone()));
        }
    }

    /// Delete a session's files on the writer thread, discarding any pending
    /// snapshot first. Runs after already-queued flushes; `done` fires on the
    /// writer thread, so callers never block on disk.
    pub fn delete(&self, id: N00nId, done: impl FnOnce(Result<(), SessionError>) + Send + 'static) {
        let op = Op::Delete {
            id,
            done: Box::new(done),
        };
        if let Err(flume::SendError(Op::Delete { done, .. })) = self.ops.send(op) {
            done(Err(writer_gone()));
        }
    }

    pub fn shutdown(self, timeout: Duration) {
        drop(self.ops);
        if self.done_rx.recv_timeout(timeout).is_err() {
            warn!("storage writer did not drain within {timeout:?}");
        }
    }
}

fn lock(pending: &Pending) -> std::sync::MutexGuard<'_, HashMap<N00nId, PendingSnapshot>> {
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn writer_gone() -> SessionError {
    n00n_storage::StorageError::Io(io::Error::other("storage writer unavailable")).into()
}

fn retain_failed_snapshot(
    pending: &mut HashMap<N00nId, PendingSnapshot>,
    id: N00nId,
    mut snapshot: PendingSnapshot,
) {
    snapshot.retry_attempts += 1;
    if snapshot.retry_attempts >= MAX_RETRY_ATTEMPTS {
        warn!(
            retry_count = snapshot.retry_attempts,
            %id,
            revision = snapshot.revision,
            "storage writer exhausted retry attempts, dropping pending snapshot"
        );
    } else {
        pending.insert(id, snapshot);
    }
}

fn flush(
    pending: &Pending,
    logs: &mut HashMap<N00nId, SessionLog>,
    durable_revisions: &mut HashMap<N00nId, u64>,
    dir: &StateDir,
) {
    let mut pending_guard = lock(pending);
    let batch = mem::take(&mut *pending_guard);
    if batch.is_empty() {
        return;
    }
    let sessions_dir = match dir.ensure_subdir(SESSIONS_DIR) {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "failed to ensure sessions dir");
            for (id, snapshot) in batch {
                retain_failed_snapshot(&mut pending_guard, id, snapshot);
            }
            return;
        }
    };
    for (id, snapshot) in batch {
        if let Err(error) = write_session(&sessions_dir, logs, durable_revisions, &snapshot.session)
        {
            warn!(error = %error, id = %snapshot.session.id, "session write failed");
            retain_failed_snapshot(&mut pending_guard, id, snapshot);
        }
    }
}

fn persist_session(
    pending: &Pending,
    logs: &mut HashMap<N00nId, SessionLog>,
    durable_revisions: &mut HashMap<N00nId, u64>,
    dir: &StateDir,
    session: &AppSession,
) -> Result<(), SessionError> {
    let mut pending_guard = lock(pending);
    if pending_guard
        .get(&session.id)
        .is_some_and(|snapshot| snapshot.revision > session.meta.revision)
    {
        let snapshot = pending_guard.get(&session.id).ok_or_else(|| {
            n00n_storage::StorageError::Io(io::Error::other("pending snapshot disappeared"))
        })?;
        write_session(
            &dir.ensure_subdir(SESSIONS_DIR)?,
            logs,
            durable_revisions,
            &snapshot.session,
        )?;
        pending_guard.remove(&session.id);
    }
    drop(pending_guard);
    write_session_if_newer(logs, durable_revisions, dir, session)
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
    logs: &mut HashMap<N00nId, SessionLog>,
    durable_revisions: &mut HashMap<N00nId, u64>,
    session: &AppSession,
) -> Result<(), SessionError> {
    if durable_revisions
        .get(&session.id)
        .is_some_and(|revision| *revision > session.meta.revision)
    {
        return Ok(());
    }
    if let Some(log) = logs.get_mut(&session.id) {
        if durable_revisions.get(&session.id) == Some(&session.meta.revision) {
            log.compact(sessions_dir, session)?;
        } else {
            append_or_compact_result(log, sessions_dir, session)?;
        }
        durable_revisions.insert(session.id, session.meta.revision);
        return Ok(());
    }
    let (mut log, on_disk_revision) = open_or_create_log(sessions_dir, session)?;
    if on_disk_revision > session.meta.revision {
        durable_revisions.insert(session.id, on_disk_revision);
        return Ok(());
    }
    if on_disk_revision == session.meta.revision {
        log.compact(sessions_dir, session)?;
    } else {
        append_or_compact_result(&mut log, sessions_dir, session)?;
    }
    logs.insert(session.id, log);
    durable_revisions.insert(session.id, session.meta.revision);
    Ok(())
}

fn write_session_if_newer(
    logs: &mut HashMap<N00nId, SessionLog>,
    durable_revisions: &mut HashMap<N00nId, u64>,
    dir: &StateDir,
    session: &AppSession,
) -> Result<(), SessionError> {
    let sessions_dir = dir.ensure_subdir(SESSIONS_DIR)?;
    write_session(&sessions_dir, logs, durable_revisions, session)
}

fn open_or_create_log(
    sessions_dir: &Path,
    session: &AppSession,
) -> Result<(SessionLog, u64), n00n_storage::sessions::SessionError> {
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
    use tempfile::TempDir;

    const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

    fn state_dir() -> (TempDir, StateDir) {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        (tmp, dir)
    }

    /// Snapshots must coalesce per session id, not into one `latest` slot:
    /// two racing sessions used to silently drop one.
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
        writer.shutdown(DRAIN_TIMEOUT);

        assert!(AppSession::load(a_id, &dir).is_ok());
        assert_eq!(AppSession::load(b_id, &dir).unwrap().title, "renamed");
    }

    #[test]
    fn persist_reports_success_after_writing_snapshot() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir.clone()).unwrap();
        let session = AppSession::new("test-model", "/tmp/persist");
        let id = session.id;
        let (done_tx, done_rx) = flume::bounded(1);
        writer.persist(Box::new(session), move |result| {
            let _ = done_tx.send(result);
        });

        assert!(done_rx.recv_timeout(DRAIN_TIMEOUT).unwrap().is_ok());
        assert!(AppSession::load(id, &dir).is_ok());
        writer.shutdown(DRAIN_TIMEOUT);
    }

    #[test]
    fn equal_revision_different_snapshot_is_not_dropped() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir.clone()).unwrap();
        let first = AppSession::new("test-model", "/tmp/equal");
        let id = first.id;
        let (done_tx, done_rx) = flume::bounded(1);
        writer.persist(Box::new(first), move |result| {
            let _ = done_tx.send(result);
        });
        assert!(done_rx.recv_timeout(DRAIN_TIMEOUT).unwrap().is_ok());

        let mut second = AppSession::load(id, &dir).unwrap();
        second.title = "same revision, new snapshot".into();
        writer.send(Box::new(second));
        writer.shutdown(DRAIN_TIMEOUT);

        assert_eq!(
            AppSession::load(id, &dir).unwrap().title,
            "same revision, new snapshot"
        );
    }

    #[test]
    fn persist_cannot_overwrite_newer_periodic_snapshot() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir.clone()).unwrap();
        let mut older = AppSession::new("test-model", "/tmp/race");
        older.meta.revision = 1;
        older.title = "submission".into();
        let id = older.id;
        let mut newer = older.clone();
        newer.meta.revision = 2;
        newer.title = "periodic save".into();
        writer.send(Box::new(newer));

        let (done_tx, done_rx) = flume::bounded(1);
        writer.persist(Box::new(older), move |result| {
            let _ = done_tx.send(result);
        });

        assert!(done_rx.recv_timeout(DRAIN_TIMEOUT).unwrap().is_ok());
        assert_eq!(AppSession::load(id, &dir).unwrap().title, "periodic save");
        writer.shutdown(DRAIN_TIMEOUT);
    }

    #[test]
    fn failed_snapshot_remains_pending_for_retry() {
        let (_tmp, dir) = state_dir();
        let session = AppSession::new("test-model", "/tmp/retry");
        let id = session.id;
        let revision = session.meta.revision;
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR).unwrap();
        let blocked_log = sessions_dir.join(format!("{id}.jsonl"));
        std::fs::create_dir(&blocked_log).unwrap();

        let pending: Pending = Arc::default();
        lock(&pending).insert(
            id,
            PendingSnapshot {
                revision,
                retry_attempts: 0,
                session: Box::new(session),
            },
        );
        let mut logs = HashMap::new();
        let mut durable_revisions = HashMap::new();

        flush(&pending, &mut logs, &mut durable_revisions, &dir);
        let pending_guard = lock(&pending);
        let snapshot = &pending_guard[&id];
        assert_eq!(snapshot.revision, revision);
        assert_eq!(snapshot.retry_attempts, 1);
        drop(pending_guard);

        std::fs::remove_dir(&blocked_log).unwrap();
        flush(&pending, &mut logs, &mut durable_revisions, &dir);
        assert!(lock(&pending).is_empty());
        assert!(AppSession::load(id, &dir).is_ok());
    }

    #[test]
    fn persist_failure_retains_newer_pending_snapshot() {
        let (_tmp, dir) = state_dir();
        let mut older = AppSession::new("test-model", "/tmp/persist-retry");
        older.meta.revision = 1;
        let id = older.id;
        let mut newer = older.clone();
        newer.meta.revision = 2;
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR).unwrap();
        std::fs::create_dir(sessions_dir.join(format!("{id}.jsonl"))).unwrap();
        let pending: Pending = Arc::default();
        lock(&pending).insert(
            id,
            PendingSnapshot {
                revision: newer.meta.revision,
                retry_attempts: 0,
                session: Box::new(newer),
            },
        );

        let result = persist_session(
            &pending,
            &mut HashMap::new(),
            &mut HashMap::new(),
            &dir,
            &older,
        );

        assert!(result.is_err());
        assert_eq!(
            lock(&pending).get(&id).map(|snapshot| snapshot.revision),
            Some(2)
        );
    }

    #[test]
    fn failed_snapshot_is_dropped_after_retry_limit() {
        let (_tmp, dir) = state_dir();
        let session = AppSession::new("test-model", "/tmp/exhausted");
        let id = session.id;
        let sessions_dir = dir.ensure_subdir(SESSIONS_DIR).unwrap();
        std::fs::create_dir(sessions_dir.join(format!("{id}.jsonl"))).unwrap();
        let pending: Pending = Arc::default();
        lock(&pending).insert(
            id,
            PendingSnapshot {
                revision: session.meta.revision,
                retry_attempts: 0,
                session: Box::new(session),
            },
        );
        let mut logs = HashMap::new();
        let mut durable_revisions = HashMap::new();

        for _ in 0..MAX_RETRY_ATTEMPTS {
            flush(&pending, &mut logs, &mut durable_revisions, &dir);
        }

        assert!(lock(&pending).is_empty());
    }

    #[test]
    fn equal_revision_replacement_resets_retry_attempts() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir).unwrap();
        let session = AppSession::new("test-model", "/tmp/replacement");
        let id = session.id;
        let revision = session.meta.revision;
        lock(&writer.pending).insert(
            id,
            PendingSnapshot {
                revision,
                retry_attempts: MAX_RETRY_ATTEMPTS - 1,
                session: Box::new(session.clone()),
            },
        );

        writer.send(Box::new(session));

        assert_eq!(lock(&writer.pending)[&id].retry_attempts, 0);
        lock(&writer.pending).clear();
        writer.shutdown(DRAIN_TIMEOUT);
    }

    #[test]
    fn delete_discards_pending_snapshot() {
        let (_tmp, dir) = state_dir();
        let writer = StorageWriter::new(dir.clone()).unwrap();
        let session = AppSession::new("test-model", "/tmp/c");
        let id = session.id;
        writer.send(Box::new(session));
        let (done_tx, done_rx) = flume::bounded(1);
        writer.delete(id, move |res| {
            let _ = done_tx.send(res);
        });
        writer.shutdown(DRAIN_TIMEOUT);

        assert!(done_rx.recv().unwrap().is_ok());
        assert!(AppSession::load(id, &dir).is_err());
    }
}
