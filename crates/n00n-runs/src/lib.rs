//! Canonical background-run domain and transactional SQLite repository.

mod adapter;
mod domain;
mod migrations;
mod service;
mod store;

pub use adapter::*;
pub use domain::*;
pub use service::RunService;
pub use store::RunStore;

use rusqlite::ErrorCode;
use std::io;

#[derive(Debug, thiserror::Error)]
pub enum RunStoreError {
    #[error("run {0} was not found in this project")]
    NotFound(RunId),
    #[error("host instance {0} was not found")]
    HostNotFound(HostInstanceId),
    #[error("host instance {0} has already shut down")]
    HostShutDown(HostInstanceId),
    #[error("run {run_id} revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict {
        run_id: RunId,
        expected: u64,
        actual: u64,
    },
    #[error("run {run_id} rejected a stale or missing ownership fence")]
    OwnershipFence {
        run_id: RunId,
        actual_instance: Option<HostInstanceId>,
        actual_epoch: Option<u64>,
    },
    #[error("operation ID {0} was reused for a different operation")]
    IdempotencyConflict(String),
    #[error("operation ID must not be empty")]
    InvalidOperationId,
    #[error("parent outbox delivery {0} was not found in this project")]
    OutboxNotFound(DeliveryId),
    #[error("resume requires a terminal prior run, found {0:?}")]
    ResumeRequiresTerminal(RunLifecycle),
    #[error("run does not support capability {0}")]
    UnsupportedCapability(&'static str),
    #[error("no adapter is registered for backend {0:?}")]
    AdapterUnavailable(ExecutionBackend),
    #[error("adapter operation failed ({code}): {message}")]
    Adapter { code: String, message: String },
    #[error("resume requires a paused or terminal run, found {0:?}")]
    ResumeRequiresPausedOrTerminal(RunLifecycle),
    #[error("cannot transfer ownership of terminal run in state {0:?}")]
    TerminalOwnerTransfer(RunLifecycle),
    #[error("run {0} revision overflow")]
    RevisionOverflow(RunId),
    #[error("run {0} owner epoch overflow")]
    OwnerEpochOverflow(RunId),
    #[error("requested page size {requested} exceeds allowed range 1..={maximum}")]
    InvalidLimit { requested: usize, maximum: usize },
    #[error("observation timeout is too large")]
    InvalidTimeout,
    #[error("legacy session metadata belongs to a different project")]
    LegacyProjectMismatch,
    #[error("SQLite run store is busy")]
    Busy,
    #[error("incompatible run-store schema: {0}")]
    IncompatibleSchema(String),
    #[error("run-store migration failed: {0}")]
    MigrationFailed(String),
    #[error("invalid run-store configuration: {0}")]
    InvalidConfiguration(String),
    #[error("corrupt run-store data: {0}")]
    CorruptData(String),
    #[error("system clock error: {0}")]
    Clock(String),
    #[error("run-store synchronization primitive was poisoned")]
    Synchronization,
    #[error("injected transaction failure")]
    InjectedFailure,
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("SQLite error: {0}")]
    Database(rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(io::Error),
    #[error("serialization error: {0}")]
    Serialization(serde_json::Error),
    #[error("session storage error: {0}")]
    Session(#[from] n00n_storage::sessions::SessionError),
}

impl RunStoreError {
    pub(crate) fn database(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Busy,
            _ => Self::Database(error),
        }
    }
}
