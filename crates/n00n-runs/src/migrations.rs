use crate::RunStoreError;
use rusqlite::{Connection, TransactionBehavior};

pub(crate) const SCHEMA_VERSION: i64 = 1;
pub(crate) const APPLICATION_ID: i64 = 0x6e_30_30_6e;

const SCHEMA_V1: &str = r"
CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
) STRICT;
CREATE TABLE run_chains (
    chain_id TEXT PRIMARY KEY,
    project_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    root_session_id TEXT,
    title TEXT NOT NULL
) STRICT;
CREATE INDEX run_chains_project_created ON run_chains(project_key, created_at DESC);
CREATE TABLE host_instances (
    instance_id TEXT PRIMARY KEY,
    process_identity_json TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    heartbeat_at INTEGER NOT NULL,
    shutdown_at INTEGER
) STRICT;
CREATE INDEX host_instances_heartbeat ON host_instances(heartbeat_at);
CREATE TABLE runs (
    run_id TEXT PRIMARY KEY,
    chain_id TEXT NOT NULL REFERENCES run_chains(chain_id) ON DELETE CASCADE,
    predecessor_run_id TEXT REFERENCES runs(run_id) ON DELETE RESTRICT,
    backend TEXT NOT NULL,
    session_id TEXT,
    legacy_session_id TEXT,
    workflow_journal_id TEXT,
    parent_run_id TEXT REFERENCES runs(run_id) ON DELETE RESTRICT,
    parent_session_id TEXT,
    lifecycle TEXT NOT NULL,
    wait_reason_code TEXT,
    wait_reason_summary TEXT,
    outcome_json TEXT,
    capabilities_json TEXT NOT NULL,
    owner_instance_id TEXT REFERENCES host_instances(instance_id) ON DELETE RESTRICT,
    owner_epoch INTEGER,
    created_at INTEGER NOT NULL,
    queued_at INTEGER,
    started_at INTEGER,
    updated_at INTEGER NOT NULL,
    finished_at INTEGER,
    last_progress_at INTEGER,
    revision INTEGER NOT NULL CHECK (revision > 0),
    CHECK ((owner_instance_id IS NULL) = (owner_epoch IS NULL)),
    CHECK ((wait_reason_code IS NULL) = (wait_reason_summary IS NULL))
) STRICT;
CREATE INDEX runs_chain_created ON runs(chain_id, created_at DESC);
CREATE INDEX runs_owner_lifecycle ON runs(owner_instance_id, lifecycle);
CREATE UNIQUE INDEX runs_legacy_session ON runs(chain_id, legacy_session_id) WHERE legacy_session_id IS NOT NULL;
CREATE TABLE run_events (
    event_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    operation_fingerprint TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(run_id, revision)
) STRICT;
CREATE INDEX run_events_run_revision ON run_events(run_id, revision);
CREATE TABLE parent_outbox (
    delivery_id TEXT PRIMARY KEY,
    source_event_id TEXT NOT NULL UNIQUE REFERENCES run_events(event_id) ON DELETE RESTRICT,
    child_run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE RESTRICT,
    parent_session_id TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'delivered', 'acknowledged', 'dead_letter')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_attempt_at INTEGER,
    created_at INTEGER NOT NULL,
    delivered_at INTEGER,
    acknowledged_at INTEGER,
    dead_letter_reason TEXT
) STRICT;
CREATE INDEX parent_outbox_pending ON parent_outbox(state, next_attempt_at, created_at);
";

pub(crate) fn migrate(connection: &mut Connection, now: i64) -> Result<(), RunStoreError> {
    migrate_inner(connection, now, false)
}

fn migrate_inner(
    connection: &mut Connection,
    now: i64,
    inject_failure: bool,
) -> Result<(), RunStoreError> {
    let application_id: i64 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(RunStoreError::database)?;
    if application_id != 0 && application_id != APPLICATION_ID {
        return Err(RunStoreError::IncompatibleSchema(
            "database belongs to another application".to_owned(),
        ));
    }
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(RunStoreError::database)?;
    if version > SCHEMA_VERSION {
        return Err(RunStoreError::IncompatibleSchema(format!(
            "database schema version {version} is newer than supported version {SCHEMA_VERSION}"
        )));
    }
    if version == 0 && has_run_tables(connection)? {
        return Err(RunStoreError::IncompatibleSchema(
            "run tables exist without a recognized schema version".to_owned(),
        ));
    }
    if version == SCHEMA_VERSION {
        verify_schema(connection)?;
        return Ok(());
    }

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(RunStoreError::database)?;
    transaction
        .execute_batch(SCHEMA_V1)
        .map_err(RunStoreError::database)?;
    if inject_failure {
        return Err(RunStoreError::MigrationFailed(
            "injected migration failure".to_owned(),
        ));
    }
    transaction
        .execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            (SCHEMA_VERSION, now),
        )
        .map_err(RunStoreError::database)?;
    transaction
        .pragma_update(None, "application_id", APPLICATION_ID)
        .map_err(RunStoreError::database)?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(RunStoreError::database)?;
    transaction.commit().map_err(RunStoreError::database)
}

fn has_run_tables(connection: &Connection) -> Result<bool, RunStoreError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name IN ('run_chains', 'runs', 'run_events', 'parent_outbox', 'host_instances'))",
            [],
            |row| row.get(0),
        )
        .map_err(RunStoreError::database)
}

fn verify_schema(connection: &Connection) -> Result<(), RunStoreError> {
    for table in [
        "schema_migrations",
        "run_chains",
        "host_instances",
        "runs",
        "run_events",
        "parent_outbox",
    ] {
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(RunStoreError::database)?;
        if !present {
            return Err(RunStoreError::IncompatibleSchema(format!(
                "schema version {SCHEMA_VERSION} is missing table {table}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_migration_rolls_back_all_schema_changes() {
        let mut connection = Connection::open_in_memory().unwrap();
        let error = migrate_inner(&mut connection, 1, true).unwrap_err();
        assert!(matches!(error, RunStoreError::MigrationFailed(_)));
        assert!(!has_run_tables(&connection).unwrap());
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 0);
    }

    #[test]
    fn corrupt_unversioned_schema_is_rejected_without_mutation() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE runs(broken TEXT)", [])
            .unwrap();
        let error = migrate(&mut connection, 1).unwrap_err();
        assert!(matches!(error, RunStoreError::IncompatibleSchema(_)));
        let columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('runs')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 1);
    }
}
