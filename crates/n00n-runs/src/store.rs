use crate::{
    DeliveryId, DomainError, HostInstance, HostInstanceId, HostLiveness, NewRunSpec, OutcomeStatus,
    OwnerFence, ParentOutboxRecord, ProcessIdentity, ProjectKey, ReconcilePolicy, ReconcileReport,
    ResumeRequest, RetentionPolicy, RetentionReport, RunCapabilities, RunChainId, RunEvent,
    RunEventId, RunEventPayload, RunId, RunKind, RunLifecycle, RunOutcome, RunRecord,
    RunStoreError, RunWaitResult, TransitionRequest, WaitReason, migrations, validate_event,
    validate_transition,
};
use n00n_storage::{
    StateDir,
    sessions::{LegacySessionMetadata, StoredSessionLifecycle, scan_legacy_child_sessions},
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const DATABASE_FILE: &str = "runs.sqlite3";
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const CROSS_PROCESS_POLL: Duration = Duration::from_millis(50);
const MAX_PAGE_SIZE: usize = 500;
const MAX_RECONCILE_RUNS_PER_HOST: usize = 256;
const LEGACY_NAMESPACE: Uuid = Uuid::from_bytes([
    0x68, 0x30, 0x0f, 0x12, 0xc2, 0xa1, 0x4c, 0x53, 0xa8, 0x47, 0x9c, 0x61, 0x7e, 0x09, 0xc4, 0x30,
]);
const ACTIVE_LIFECYCLES_SQL: &str =
    "'queued','starting','running','waiting_input','blocked','pausing','paused','cancelling'";
const TERMINAL_LIFECYCLES_SQL: &str =
    "'succeeded','failed','timed_out','cancelled','interrupted','lost'";

#[derive(Clone)]
pub struct RunStore {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    database_path: PathBuf,
    project_key: ProjectKey,
    busy_timeout: Duration,
    notifications: Mutex<HashMap<RunId, Arc<RevisionNotification>>>,
}

struct RevisionNotification {
    generation: Mutex<u64>,
    changed: Condvar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailurePoint {
    None,
    AfterRunUpdate,
    AfterEventInsert,
    AfterOutboxInsert,
}

#[derive(Debug)]
struct RawOutbox {
    delivery_id: String,
    source_event_id: String,
    child_run_id: String,
    parent_session_id: String,
    payload: String,
    state: String,
    attempt_count: i64,
    next_attempt_at: Option<i64>,
    created_at: i64,
    delivered_at: Option<i64>,
    acknowledged_at: Option<i64>,
    dead_letter_reason: Option<String>,
}

#[derive(Debug)]
struct RawRun {
    run_id: String,
    chain_id: String,
    predecessor_run_id: Option<String>,
    backend: String,
    session_id: Option<String>,
    legacy_session_id: Option<String>,
    workflow_journal_id: Option<String>,
    parent_run_id: Option<String>,
    parent_session_id: Option<String>,
    lifecycle: String,
    wait_reason_code: Option<String>,
    wait_reason_summary: Option<String>,
    outcome_json: Option<String>,
    capabilities_json: String,
    owner_instance_id: Option<String>,
    owner_epoch: Option<i64>,
    created_at: i64,
    queued_at: Option<i64>,
    started_at: Option<i64>,
    updated_at: i64,
    finished_at: Option<i64>,
    last_progress_at: Option<i64>,
    revision: i64,
}

impl RunStore {
    /// Opens the state-directory-wide database scoped to one immutable project identity.
    ///
    /// # Errors
    /// Returns a typed filesystem, SQLite, migration, or configuration error.
    pub fn open(state_dir: &StateDir, project_key: ProjectKey) -> Result<Self, RunStoreError> {
        Self::open_path(
            state_dir.path().join(DATABASE_FILE),
            project_key,
            DEFAULT_BUSY_TIMEOUT,
        )
    }

    /// Opens an explicit database path. Intended for controlled embedding and tests.
    ///
    /// # Errors
    /// Returns a typed filesystem, SQLite, migration, or configuration error.
    pub fn open_path(
        database_path: PathBuf,
        project_key: ProjectKey,
        busy_timeout: Duration,
    ) -> Result<Self, RunStoreError> {
        let parent = database_path.parent().ok_or_else(|| {
            RunStoreError::InvalidConfiguration("database path has no parent".to_owned())
        })?;
        fs::create_dir_all(parent).map_err(RunStoreError::Io)?;
        let store = Self {
            inner: Arc::new(StoreInner {
                database_path,
                project_key,
                busy_timeout,
                notifications: Mutex::new(HashMap::new()),
            }),
        };
        let mut connection = store.connection()?;
        migrations::migrate(&mut connection, now_millis()?)?;
        Ok(store)
    }

    #[must_use]
    pub fn project_key(&self) -> &ProjectKey {
        &self.inner.project_key
    }

    /// Creates a chain and its immutable first attempt in one immediate transaction.
    /// Repeating the same generated IDs returns the existing project-scoped record.
    ///
    /// # Errors
    /// Returns a typed validation, ownership, scope, or database error.
    pub fn create_run(&self, spec: &NewRunSpec) -> Result<RunRecord, RunStoreError> {
        validate_title(&spec.title)?;
        let now = now_millis()?;
        let mut connection = self.connection()?;
        let transaction = immediate(&mut connection)?;
        if let Some(existing) = query_run(&transaction, &self.inner.project_key, spec.run_id)? {
            transaction.commit().map_err(RunStoreError::database)?;
            return Ok(existing);
        }
        if let Some(parent_run_id) = spec.parent_run_id {
            require_run(&transaction, &self.inner.project_key, parent_run_id)?;
        }
        if let Some(owner) = spec.owner_instance_id {
            require_live_host(&transaction, owner)?;
        }
        transaction
            .execute(
                "INSERT INTO run_chains(chain_id, project_key, kind, created_at, root_session_id, title) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    spec.chain_id.to_string(),
                    self.inner.project_key.as_str(),
                    enum_text(spec.kind)?,
                    now,
                    spec.root_session_id,
                    spec.title,
                ],
            )
            .map_err(RunStoreError::database)?;
        let capabilities = json_text(&spec.capabilities)?;
        let owner_epoch = spec.owner_instance_id.map(|_| 1_i64);
        transaction
            .execute(
                "INSERT INTO runs(run_id, chain_id, backend, session_id, workflow_journal_id, parent_run_id, parent_session_id, lifecycle, capabilities_json, owner_instance_id, owner_epoch, created_at, queued_at, updated_at, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8, ?9, ?10, ?11, ?11, ?11, 1)",
                params![
                    spec.run_id.to_string(),
                    spec.chain_id.to_string(),
                    enum_text(spec.backend)?,
                    spec.session_id,
                    spec.workflow_journal_id,
                    spec.parent_run_id.map(|id| id.to_string()),
                    spec.parent_session_id,
                    capabilities,
                    spec.owner_instance_id.map(|id| id.to_string()),
                    owner_epoch,
                    now,
                ],
            )
            .map_err(RunStoreError::database)?;
        let event = RunEventPayload {
            summary: Some("Run queued".to_owned()),
            details: BTreeMap::default(),
        };
        insert_event(
            &transaction,
            spec.run_id,
            1,
            "run_created",
            &event,
            &format!("create:{}", spec.run_id),
            &format!("create:{}:{}", spec.chain_id, spec.run_id),
            now,
        )?;
        let record = require_run(&transaction, &self.inner.project_key, spec.run_id)?;
        transaction.commit().map_err(RunStoreError::database)?;
        self.notify(spec.run_id);
        Ok(record)
    }

    /// Reads one run only when it belongs to this store's project.
    ///
    /// # Errors
    /// Returns `NotFound` for absent and cross-project IDs.
    pub fn get_run(&self, run_id: RunId) -> Result<RunRecord, RunStoreError> {
        let connection = self.connection()?;
        query_run(&connection, &self.inner.project_key, run_id)?
            .ok_or(RunStoreError::NotFound(run_id))
    }

    /// Lists project runs in newest-first order with a hard page bound.
    ///
    /// # Errors
    /// Returns a typed error for an invalid limit or unreadable row.
    pub fn list_runs(&self, limit: usize) -> Result<Vec<RunRecord>, RunStoreError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(RunStoreError::InvalidLimit {
                requested: limit,
                maximum: MAX_PAGE_SIZE,
            });
        }
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(&format!("{RUN_SELECT} WHERE c.project_key = ?1 ORDER BY r.created_at DESC, r.run_id DESC LIMIT ?2"))
            .map_err(RunStoreError::database)?;
        let raws = statement
            .query_map(
                params![self.inner.project_key.as_str(), usize_to_i64(limit)?],
                raw_run,
            )
            .map_err(RunStoreError::database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(RunStoreError::database)?;
        raws.into_iter().map(decode_run).collect()
    }

    /// Applies a fenced optimistic transition and writes its event and optional parent outbox row atomically.
    ///
    /// # Errors
    /// Returns typed transition, revision, ownership, idempotency, scope, or database errors.
    pub fn transition(&self, request: &TransitionRequest) -> Result<RunRecord, RunStoreError> {
        self.transition_with_failure(request, FailurePoint::None)
    }

    fn transition_with_failure(
        &self,
        request: &TransitionRequest,
        failure: FailurePoint,
    ) -> Result<RunRecord, RunStoreError> {
        if request.operation_id.trim().is_empty() {
            return Err(RunStoreError::InvalidOperationId);
        }
        let fingerprint = transition_fingerprint(request)?;
        let now = now_millis()?;
        let mut connection = self.connection()?;
        let transaction = immediate(&mut connection)?;
        if let Some(record) = idempotent_result(
            &transaction,
            &self.inner.project_key,
            &request.operation_id,
            &fingerprint,
        )? {
            transaction.commit().map_err(RunStoreError::database)?;
            return Ok(record);
        }
        let current = require_run(&transaction, &self.inner.project_key, request.run_id)?;
        verify_revision_and_owner(&current, request.expected_revision, request.owner.as_ref())?;
        validate_transition(
            current.lifecycle,
            request.target,
            request.wait_reason.as_ref(),
            request.outcome.as_ref(),
            &request.event,
        )?;
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(RunStoreError::RevisionOverflow(request.run_id))?;
        let started_at = if request.target == RunLifecycle::Running && current.started_at.is_none()
        {
            Some(now)
        } else {
            current.started_at
        };
        let finished_at = request.target.is_terminal().then_some(now);
        let last_progress_at = if request.progress {
            Some(now)
        } else {
            current.last_progress_at
        };
        let wait_code = request
            .wait_reason
            .as_ref()
            .map(|reason| enum_text(reason.code))
            .transpose()?;
        let wait_summary = request
            .wait_reason
            .as_ref()
            .map(|reason| reason.summary.as_str());
        let outcome = request.outcome.as_ref().map(json_text).transpose()?;
        let changed = transaction
            .execute(
                "UPDATE runs SET lifecycle = ?1, wait_reason_code = ?2, wait_reason_summary = ?3, outcome_json = ?4, started_at = ?5, updated_at = ?6, finished_at = ?7, last_progress_at = ?8, revision = ?9 WHERE run_id = ?10 AND revision = ?11",
                params![
                    enum_text(request.target)?,
                    wait_code,
                    wait_summary,
                    outcome,
                    started_at,
                    now,
                    finished_at,
                    last_progress_at,
                    u64_to_i64(revision)?,
                    request.run_id.to_string(),
                    u64_to_i64(request.expected_revision)?,
                ],
            )
            .map_err(RunStoreError::database)?;
        if changed != 1 {
            return Err(RunStoreError::RevisionConflict {
                run_id: request.run_id,
                expected: request.expected_revision,
                actual: current.revision,
            });
        }
        injected_failure(failure, FailurePoint::AfterRunUpdate)?;
        let event_id = insert_event(
            &transaction,
            request.run_id,
            revision,
            &request.event_type,
            &request.event,
            &request.operation_id,
            &fingerprint,
            now,
        )?;
        injected_failure(failure, FailurePoint::AfterEventInsert)?;
        if should_notify_parent(request.target) {
            insert_outbox_if_parent(
                &transaction,
                &current,
                event_id,
                revision,
                request.target,
                request.wait_reason.as_ref(),
                now,
            )?;
        }
        injected_failure(failure, FailurePoint::AfterOutboxInsert)?;
        let record = require_run(&transaction, &self.inner.project_key, request.run_id)?;
        transaction.commit().map_err(RunStoreError::database)?;
        self.notify(request.run_id);
        Ok(record)
    }

    /// Creates a new queued attempt in the prior attempt's chain. The prior run is never mutated.
    ///
    /// # Errors
    /// Returns typed capability, revision, ownership, idempotency, scope, or database errors.
    pub fn resume_run(&self, request: &ResumeRequest) -> Result<RunRecord, RunStoreError> {
        if request.operation_id.trim().is_empty() {
            return Err(RunStoreError::InvalidOperationId);
        }
        let fingerprint = format!(
            "resume:{}:{}:{:?}",
            request.prior_run_id, request.expected_revision, request.owner_instance_id
        );
        let now = now_millis()?;
        let mut connection = self.connection()?;
        let transaction = immediate(&mut connection)?;
        if let Some(record) = idempotent_result(
            &transaction,
            &self.inner.project_key,
            &request.operation_id,
            &fingerprint,
        )? {
            transaction.commit().map_err(RunStoreError::database)?;
            return Ok(record);
        }
        let prior = require_run(&transaction, &self.inner.project_key, request.prior_run_id)?;
        if prior.revision != request.expected_revision {
            return Err(RunStoreError::RevisionConflict {
                run_id: prior.run_id,
                expected: request.expected_revision,
                actual: prior.revision,
            });
        }
        if !prior.lifecycle.is_terminal() {
            return Err(RunStoreError::ResumeRequiresTerminal(prior.lifecycle));
        }
        if !prior.capabilities.resume {
            return Err(RunStoreError::UnsupportedCapability("resume"));
        }
        if let Some(owner) = request.owner_instance_id {
            require_live_host(&transaction, owner)?;
        }
        let run_id = RunId::generate();
        transaction
            .execute(
                "INSERT INTO runs(run_id, chain_id, predecessor_run_id, backend, workflow_journal_id, parent_run_id, parent_session_id, lifecycle, capabilities_json, owner_instance_id, owner_epoch, created_at, queued_at, updated_at, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8, ?9, ?10, ?11, ?11, ?11, 1)",
                params![
                    run_id.to_string(),
                    prior.chain_id.to_string(),
                    prior.run_id.to_string(),
                    enum_text(prior.backend)?,
                    prior.workflow_journal_id,
                    prior.parent_run_id.map(|id| id.to_string()),
                    prior.parent_session_id,
                    json_text(&prior.capabilities)?,
                    request.owner_instance_id.map(|id| id.to_string()),
                    request.owner_instance_id.map(|_| 1_i64),
                    now,
                ],
            )
            .map_err(RunStoreError::database)?;
        let payload = RunEventPayload {
            summary: Some("Run resumed as a new attempt".to_owned()),
            details: [("predecessor_run_id".to_owned(), prior.run_id.to_string())]
                .into_iter()
                .collect(),
        };
        insert_event(
            &transaction,
            run_id,
            1,
            "run_resumed",
            &payload,
            &request.operation_id,
            &fingerprint,
            now,
        )?;
        let resumed = require_run(&transaction, &self.inner.project_key, run_id)?;
        transaction.commit().map_err(RunStoreError::database)?;
        self.notify(run_id);
        Ok(resumed)
    }

    /// Returns events after a revision, scoped to this project.
    ///
    /// # Errors
    /// Returns a typed error for invalid limits, cross-project IDs, or corrupt rows.
    pub fn events(
        &self,
        run_id: RunId,
        after_revision: u64,
        limit: usize,
    ) -> Result<Vec<RunEvent>, RunStoreError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(RunStoreError::InvalidLimit {
                requested: limit,
                maximum: MAX_PAGE_SIZE,
            });
        }
        let connection = self.connection()?;
        require_run(&connection, &self.inner.project_key, run_id)?;
        let mut statement = connection
            .prepare("SELECT event_id, run_id, revision, type, payload_json, created_at FROM run_events WHERE run_id = ?1 AND revision > ?2 ORDER BY revision LIMIT ?3")
            .map_err(RunStoreError::database)?;
        let rows = statement
            .query_map(
                params![
                    run_id.to_string(),
                    u64_to_i64(after_revision)?,
                    usize_to_i64(limit)?
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .map_err(RunStoreError::database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(RunStoreError::database)?;
        rows.into_iter()
            .map(
                |(event_id, stored_run_id, revision, event_type, payload, created_at)| {
                    Ok(RunEvent {
                        event_id: parse_id(&event_id, "event_id")?,
                        run_id: parse_id(&stored_run_id, "run_id")?,
                        revision: i64_to_u64(revision, "revision")?,
                        event_type,
                        payload: parse_json(&payload, "event payload")?,
                        created_at,
                    })
                },
            )
            .collect()
    }

    /// Waits until a newer revision is durably readable or the observation timeout expires.
    /// In-process changes notify immediately; periodic durable rereads observe other processes.
    ///
    /// # Errors
    /// Returns a typed scope, synchronization, or database error.
    pub fn wait_for_revision(
        &self,
        run_id: RunId,
        after_revision: u64,
        timeout: Duration,
    ) -> Result<RunWaitResult, RunStoreError> {
        let notification = self.notification(run_id)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(RunStoreError::InvalidTimeout)?;
        let mut last_run = None;
        loop {
            match self.get_run(run_id) {
                Ok(run) => {
                    if run.revision > after_revision || run.lifecycle.is_terminal() {
                        return Ok(RunWaitResult {
                            run,
                            observation_timed_out: false,
                        });
                    }
                    last_run = Some(run);
                }
                Err(RunStoreError::Busy) => {}
                Err(error) => return Err(error),
            }
            let now = Instant::now();
            if now >= deadline {
                let run = match last_run {
                    Some(run) => run,
                    None => self.get_run(run_id)?,
                };
                return Ok(RunWaitResult {
                    run,
                    observation_timed_out: true,
                });
            }
            let remaining = deadline.saturating_duration_since(now);
            let wait_for = remaining.min(CROSS_PROCESS_POLL);
            let generation = notification
                .generation
                .lock()
                .map_err(|_| RunStoreError::Synchronization)?;
            let (_guard, _result) = notification
                .changed
                .wait_timeout(generation, wait_for)
                .map_err(|_| RunStoreError::Synchronization)?;
        }
    }

    /// Registers a unique host identity. Process identity is persisted for lock/PID verification.
    ///
    /// # Errors
    /// Returns a typed serialization, clock, or database error.
    pub fn register_host(
        &self,
        process_identity: ProcessIdentity,
    ) -> Result<HostInstance, RunStoreError> {
        let host = HostInstance {
            instance_id: HostInstanceId::generate(),
            process_identity,
            started_at: now_millis()?,
            heartbeat_at: now_millis()?,
            shutdown_at: None,
        };
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO host_instances(instance_id, process_identity_json, started_at, heartbeat_at) VALUES (?1, ?2, ?3, ?4)",
                params![host.instance_id.to_string(), json_text(&host.process_identity)?, host.started_at, host.heartbeat_at],
            )
            .map_err(RunStoreError::database)?;
        Ok(host)
    }

    /// Refreshes a live host heartbeat without reviving a shut-down instance.
    ///
    /// # Errors
    /// Returns `HostNotFound` or `HostShutDown` when the host cannot heartbeat.
    pub fn heartbeat_host(&self, instance_id: HostInstanceId) -> Result<i64, RunStoreError> {
        let now = now_millis()?;
        let connection = self.connection()?;
        let changed = connection
            .execute(
                "UPDATE host_instances SET heartbeat_at = ?1 WHERE instance_id = ?2 AND shutdown_at IS NULL",
                params![now, instance_id.to_string()],
            )
            .map_err(RunStoreError::database)?;
        if changed == 1 {
            return Ok(now);
        }
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM host_instances WHERE instance_id = ?1)",
                [instance_id.to_string()],
                |row| row.get(0),
            )
            .map_err(RunStoreError::database)?;
        if exists {
            Err(RunStoreError::HostShutDown(instance_id))
        } else {
            Err(RunStoreError::HostNotFound(instance_id))
        }
    }

    /// Marks a host cleanly shut down. This is permanent for that instance ID.
    ///
    /// # Errors
    /// Returns a typed database error or `HostNotFound`.
    pub fn shutdown_host(&self, instance_id: HostInstanceId) -> Result<(), RunStoreError> {
        let now = now_millis()?;
        let connection = self.connection()?;
        let changed = connection
            .execute(
                "UPDATE host_instances SET shutdown_at = ?1 WHERE instance_id = ?2 AND shutdown_at IS NULL",
                params![now, instance_id.to_string()],
            )
            .map_err(RunStoreError::database)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(RunStoreError::HostNotFound(instance_id))
        }
    }

    /// Explicitly transfers ownership while fencing the exact prior owner and revision.
    /// This operation never performs implicit stealing.
    ///
    /// # Errors
    /// Returns typed owner, revision, host, scope, or database errors.
    pub fn transfer_owner(
        &self,
        run_id: RunId,
        expected_revision: u64,
        prior_owner: &OwnerFence,
        new_owner: HostInstanceId,
        operation_id: &str,
    ) -> Result<RunRecord, RunStoreError> {
        if operation_id.trim().is_empty() {
            return Err(RunStoreError::InvalidOperationId);
        }
        let fingerprint = format!(
            "owner:{run_id}:{expected_revision}:{}:{}:{new_owner}",
            prior_owner.instance_id, prior_owner.epoch
        );
        let now = now_millis()?;
        let mut connection = self.connection()?;
        let transaction = immediate(&mut connection)?;
        if let Some(record) = idempotent_result(
            &transaction,
            &self.inner.project_key,
            operation_id,
            &fingerprint,
        )? {
            transaction.commit().map_err(RunStoreError::database)?;
            return Ok(record);
        }
        require_live_host(&transaction, new_owner)?;
        let current = require_run(&transaction, &self.inner.project_key, run_id)?;
        verify_revision_and_owner(&current, expected_revision, Some(prior_owner))?;
        if current.lifecycle.is_terminal() {
            return Err(RunStoreError::TerminalOwnerTransfer(current.lifecycle));
        }
        let revision = current
            .revision
            .checked_add(1)
            .ok_or(RunStoreError::RevisionOverflow(run_id))?;
        let epoch = prior_owner
            .epoch
            .checked_add(1)
            .ok_or(RunStoreError::OwnerEpochOverflow(run_id))?;
        transaction
            .execute(
                "UPDATE runs SET owner_instance_id = ?1, owner_epoch = ?2, revision = ?3, updated_at = ?4 WHERE run_id = ?5 AND revision = ?6 AND owner_instance_id = ?7 AND owner_epoch = ?8",
                params![new_owner.to_string(), u64_to_i64(epoch)?, u64_to_i64(revision)?, now, run_id.to_string(), u64_to_i64(expected_revision)?, prior_owner.instance_id.to_string(), u64_to_i64(prior_owner.epoch)?],
            )
            .map_err(RunStoreError::database)?;
        let payload = RunEventPayload {
            summary: Some("Run ownership transferred".to_owned()),
            details: [("owner_epoch".to_owned(), epoch.to_string())]
                .into_iter()
                .collect(),
        };
        insert_event(
            &transaction,
            run_id,
            revision,
            "owner_transferred",
            &payload,
            operation_id,
            &fingerprint,
            now,
        )?;
        let record = require_run(&transaction, &self.inner.project_key, run_id)?;
        transaction.commit().map_err(RunStoreError::database)?;
        self.notify(run_id);
        Ok(record)
    }

    /// Reconciles only bounded stale-owner candidates whose shutdown or external identity proof says they are gone.
    /// The verifier runs without a transaction or service lock held.
    ///
    /// # Errors
    /// Returns typed validation, database, ownership, or transition errors.
    pub fn reconcile_stale<F>(
        &self,
        policy: &ReconcilePolicy,
        mut verify: F,
    ) -> Result<ReconcileReport, RunStoreError>
    where
        F: FnMut(&HostInstance) -> HostLiveness,
    {
        if policy.max_hosts == 0 || policy.max_hosts > MAX_PAGE_SIZE {
            return Err(RunStoreError::InvalidLimit {
                requested: policy.max_hosts,
                maximum: MAX_PAGE_SIZE,
            });
        }
        let candidates = self.stale_hosts(policy)?;
        let mut report = ReconcileReport::default();
        for host in candidates {
            let liveness = if host.shutdown_at.is_some() {
                HostLiveness::Gone
            } else {
                verify(&host)
            };
            match liveness {
                HostLiveness::Live => report.live_owners += 1,
                HostLiveness::Unknown => report.unverified_owners += 1,
                HostLiveness::Gone => {
                    for run in self.active_runs_for_owner(host.instance_id)? {
                        let request = TransitionRequest {
                            run_id: run.run_id,
                            expected_revision: run.revision,
                            owner: Some(OwnerFence {
                                instance_id: host.instance_id,
                                epoch: run.owner_epoch.ok_or(RunStoreError::CorruptData(
                                    "owned run has no owner epoch".to_owned(),
                                ))?,
                            }),
                            target: RunLifecycle::Interrupted,
                            wait_reason: None,
                            outcome: Some(RunOutcome::status(OutcomeStatus::Interrupted)),
                            event_type: "owner_gone".to_owned(),
                            event: RunEventPayload {
                                summary: Some(
                                    "Run interrupted because its verified owner exited".to_owned(),
                                ),
                                details: BTreeMap::default(),
                            },
                            operation_id: format!(
                                "reconcile:{}:{}:{}",
                                run.run_id,
                                host.instance_id,
                                run.owner_epoch.ok_or(RunStoreError::CorruptData(
                                    "owned run has no owner epoch".to_owned(),
                                ))?
                            ),
                            progress: false,
                        };
                        match self.transition(&request) {
                            Ok(_) => report.interrupted.push(run.run_id),
                            Err(
                                RunStoreError::RevisionConflict { .. }
                                | RunStoreError::OwnershipFence { .. }
                                | RunStoreError::Domain(DomainError::InvalidTransition { .. })
                                | RunStoreError::NotFound(_),
                            ) => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
        }
        Ok(report)
    }

    /// Imports legacy child-session metadata once, scoped by exact project identity.
    /// Active and ambiguous idle sessions become interrupted; idle outcomes remain explicitly unknown.
    ///
    /// # Errors
    /// Returns a typed session scan, validation, serialization, or database error.
    pub fn import_legacy_sessions(
        &self,
        state_dir: &StateDir,
    ) -> Result<Vec<RunRecord>, RunStoreError> {
        let metadata = scan_legacy_child_sessions(self.inner.project_key.as_str(), state_dir)?;
        metadata
            .iter()
            .map(|legacy| self.import_legacy_one(legacy))
            .collect()
    }

    fn import_legacy_one(
        &self,
        legacy: &LegacySessionMetadata,
    ) -> Result<RunRecord, RunStoreError> {
        if legacy.cwd != self.inner.project_key.as_str() {
            return Err(RunStoreError::LegacyProjectMismatch);
        }
        let session_id = legacy.session_id.to_string();
        let chain_name = format!("chain:{}:{session_id}", self.inner.project_key);
        let run_name = format!("run:{}:{session_id}", self.inner.project_key);
        let chain_id = RunChainId::deterministic(&LEGACY_NAMESPACE, chain_name.as_bytes());
        let run_id = RunId::deterministic(&LEGACY_NAMESPACE, run_name.as_bytes());
        let created_at = u64_to_i64(legacy.created_at)?;
        let updated_at = u64_to_i64(legacy.updated_at)?;
        let (lifecycle, outcome) = legacy_outcome(legacy.lifecycle);
        let mut connection = self.connection()?;
        let transaction = immediate(&mut connection)?;
        if let Some(existing) = query_run(&transaction, &self.inner.project_key, run_id)? {
            transaction.commit().map_err(RunStoreError::database)?;
            return Ok(existing);
        }
        transaction
            .execute(
                "INSERT INTO run_chains(chain_id, project_key, kind, created_at, root_session_id, title) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![chain_id.to_string(), self.inner.project_key.as_str(), enum_text(if legacy.workflow { RunKind::Workflow } else { RunKind::Task })?, created_at, legacy.root_session_id.map(|id| id.to_string()), legacy.title],
            )
            .map_err(RunStoreError::database)?;
        let capabilities = RunCapabilities {
            events: true,
            logs: true,
            ..RunCapabilities::default()
        };
        transaction
            .execute(
                "INSERT INTO runs(run_id, chain_id, backend, session_id, legacy_session_id, parent_session_id, lifecycle, outcome_json, capabilities_json, created_at, queued_at, updated_at, finished_at, revision) VALUES (?1, ?2, 'tui_session', ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?9, 1)",
                params![run_id.to_string(), chain_id.to_string(), session_id, legacy.parent_id.to_string(), enum_text(lifecycle)?, json_text(&outcome)?, json_text(&capabilities)?, created_at, updated_at],
            )
            .map_err(RunStoreError::database)?;
        let payload = RunEventPayload {
            summary: Some("Imported legacy child session metadata".to_owned()),
            details: [(
                "legacy_session_id".to_owned(),
                legacy.session_id.to_string(),
            )]
            .into_iter()
            .collect(),
        };
        let event_id = insert_event(
            &transaction,
            run_id,
            1,
            "legacy_imported",
            &payload,
            &format!("legacy:{run_id}"),
            &format!("legacy:{chain_id}:{run_id}"),
            updated_at,
        )?;
        if should_notify_parent(lifecycle) {
            let record = require_run(&transaction, &self.inner.project_key, run_id)?;
            insert_outbox_if_parent(
                &transaction,
                &record,
                event_id,
                1,
                lifecycle,
                None,
                updated_at,
            )?;
        }
        let record = require_run(&transaction, &self.inner.project_key, run_id)?;
        transaction.commit().map_err(RunStoreError::database)?;
        self.notify(run_id);
        Ok(record)
    }

    /// Deletes bounded finalized delivery records and unreferenced old terminal history.
    /// Pending outbox rows always protect their source event and run.
    ///
    /// # Errors
    /// Returns a typed limit, database, or clock conversion error.
    pub fn compact(&self, policy: &RetentionPolicy) -> Result<RetentionReport, RunStoreError> {
        if policy.max_rows == 0 || policy.max_rows > MAX_PAGE_SIZE {
            return Err(RunStoreError::InvalidLimit {
                requested: policy.max_rows,
                maximum: MAX_PAGE_SIZE,
            });
        }
        let mut connection = self.connection()?;
        let transaction = immediate(&mut connection)?;
        let limit = usize_to_i64(policy.max_rows)?;
        let outbox_rows = transaction
            .execute(
                "DELETE FROM parent_outbox WHERE delivery_id IN (SELECT delivery_id FROM parent_outbox WHERE state IN ('delivered','acknowledged','dead_letter') AND created_at < ?1 ORDER BY created_at LIMIT ?2)",
                params![policy.finalized_outbox_before, limit],
            )
            .map_err(RunStoreError::database)?;
        let runs = transaction
            .execute(
                &format!("DELETE FROM runs WHERE run_id IN (SELECT r.run_id FROM runs r JOIN run_chains c ON c.chain_id = r.chain_id WHERE c.project_key = ?1 AND r.lifecycle IN ({TERMINAL_LIFECYCLES_SQL}) AND r.finished_at < ?2 AND NOT EXISTS (SELECT 1 FROM parent_outbox o WHERE o.child_run_id = r.run_id) AND NOT EXISTS (SELECT 1 FROM runs child WHERE child.predecessor_run_id = r.run_id OR child.parent_run_id = r.run_id) ORDER BY r.finished_at LIMIT ?3)"),
                params![self.inner.project_key.as_str(), policy.terminal_before, limit],
            )
            .map_err(RunStoreError::database)?;
        let chains = transaction
            .execute(
                "DELETE FROM run_chains WHERE chain_id IN (SELECT c.chain_id FROM run_chains c WHERE c.project_key = ?1 AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.chain_id = c.chain_id) LIMIT ?2)",
                params![self.inner.project_key.as_str(), limit],
            )
            .map_err(RunStoreError::database)?;
        let hosts = transaction
            .execute(
                "DELETE FROM host_instances WHERE instance_id IN (SELECT h.instance_id FROM host_instances h WHERE h.shutdown_at < ?1 AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.owner_instance_id = h.instance_id) LIMIT ?2)",
                params![policy.shutdown_host_before, limit],
            )
            .map_err(RunStoreError::database)?;
        transaction.commit().map_err(RunStoreError::database)?;
        Ok(RetentionReport {
            outbox_rows,
            runs,
            chains,
            hosts,
        })
    }

    /// Reads due outbox rows for this project.
    ///
    /// # Errors
    /// Returns a typed limit, database, or corrupt-row error.
    pub fn pending_outbox(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<ParentOutboxRecord>, RunStoreError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(RunStoreError::InvalidLimit {
                requested: limit,
                maximum: MAX_PAGE_SIZE,
            });
        }
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT o.delivery_id, o.source_event_id, o.child_run_id, o.parent_session_id, o.payload_json, o.state, o.attempt_count, o.next_attempt_at, o.created_at, o.delivered_at, o.acknowledged_at, o.dead_letter_reason FROM parent_outbox o JOIN runs r ON r.run_id = o.child_run_id JOIN run_chains c ON c.chain_id = r.chain_id WHERE c.project_key = ?1 AND o.state = 'pending' AND (o.next_attempt_at IS NULL OR o.next_attempt_at <= ?2) ORDER BY o.created_at LIMIT ?3")
            .map_err(RunStoreError::database)?;
        let rows = statement
            .query_map(
                params![self.inner.project_key.as_str(), now, usize_to_i64(limit)?],
                |row| {
                    Ok(RawOutbox {
                        delivery_id: row.get(0)?,
                        source_event_id: row.get(1)?,
                        child_run_id: row.get(2)?,
                        parent_session_id: row.get(3)?,
                        payload: row.get(4)?,
                        state: row.get(5)?,
                        attempt_count: row.get(6)?,
                        next_attempt_at: row.get(7)?,
                        created_at: row.get(8)?,
                        delivered_at: row.get(9)?,
                        acknowledged_at: row.get(10)?,
                        dead_letter_reason: row.get(11)?,
                    })
                },
            )
            .map_err(RunStoreError::database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(RunStoreError::database)?;
        rows.into_iter().map(decode_outbox).collect()
    }

    pub(crate) fn dispatchable_outbox(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<ParentOutboxRecord>, RunStoreError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(RunStoreError::InvalidLimit {
                requested: limit,
                maximum: MAX_PAGE_SIZE,
            });
        }
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT o.delivery_id, o.source_event_id, o.child_run_id, o.parent_session_id, o.payload_json, o.state, o.attempt_count, o.next_attempt_at, o.created_at, o.delivered_at, o.acknowledged_at, o.dead_letter_reason FROM parent_outbox o JOIN runs r ON r.run_id = o.child_run_id JOIN run_chains c ON c.chain_id = r.chain_id WHERE c.project_key = ?1 AND o.state IN ('pending', 'delivered') AND (o.state = 'delivered' OR o.next_attempt_at IS NULL OR o.next_attempt_at <= ?2) ORDER BY CASE o.state WHEN 'pending' THEN 0 ELSE 1 END, o.created_at LIMIT ?3")
            .map_err(RunStoreError::database)?;
        let rows = statement
            .query_map(
                params![self.inner.project_key.as_str(), now, usize_to_i64(limit)?],
                |row| {
                    Ok(RawOutbox {
                        delivery_id: row.get(0)?,
                        source_event_id: row.get(1)?,
                        child_run_id: row.get(2)?,
                        parent_session_id: row.get(3)?,
                        payload: row.get(4)?,
                        state: row.get(5)?,
                        attempt_count: row.get(6)?,
                        next_attempt_at: row.get(7)?,
                        created_at: row.get(8)?,
                        delivered_at: row.get(9)?,
                        acknowledged_at: row.get(10)?,
                        dead_letter_reason: row.get(11)?,
                    })
                },
            )
            .map_err(RunStoreError::database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(RunStoreError::database)?;
        rows.into_iter().map(decode_outbox).collect()
    }
    /// Marks an idempotently inserted parent delivery as delivered.
    ///
    /// # Errors
    /// Returns a typed scope or database error.
    pub fn mark_outbox_delivered(
        &self,
        delivery_id: DeliveryId,
        delivered_at: i64,
    ) -> Result<(), RunStoreError> {
        self.update_outbox(
            delivery_id,
            "UPDATE parent_outbox SET state = 'delivered', attempt_count = CASE WHEN state = 'pending' THEN attempt_count + 1 ELSE attempt_count END, next_attempt_at = NULL, delivered_at = COALESCE(delivered_at, ?2) WHERE delivery_id = ?1 AND state IN ('pending', 'delivered') AND EXISTS (SELECT 1 FROM runs r JOIN run_chains c ON c.chain_id = r.chain_id WHERE r.run_id = parent_outbox.child_run_id AND c.project_key = ?3)",
            delivered_at,
            None,
        )
    }

    /// Acknowledges consumption, including the crash boundary before delivered was recorded.
    ///
    /// # Errors
    /// Returns a typed scope or database error.
    pub fn acknowledge_outbox(
        &self,
        delivery_id: DeliveryId,
        acknowledged_at: i64,
    ) -> Result<(), RunStoreError> {
        self.update_outbox(
            delivery_id,
            "UPDATE parent_outbox SET state = 'acknowledged', delivered_at = COALESCE(delivered_at, ?2), acknowledged_at = COALESCE(acknowledged_at, ?2), next_attempt_at = NULL WHERE delivery_id = ?1 AND state IN ('pending', 'delivered', 'acknowledged') AND EXISTS (SELECT 1 FROM runs r JOIN run_chains c ON c.chain_id = r.chain_id WHERE r.run_id = parent_outbox.child_run_id AND c.project_key = ?3)",
            acknowledged_at,
            None,
        )
    }

    /// Schedules a bounded retry for a transient parent delivery failure.
    ///
    /// # Errors
    /// Returns a typed scope or database error.
    pub fn retry_outbox(
        &self,
        delivery_id: DeliveryId,
        next_attempt_at: i64,
    ) -> Result<(), RunStoreError> {
        self.update_outbox(
            delivery_id,
            "UPDATE parent_outbox SET attempt_count = attempt_count + 1, next_attempt_at = ?2 WHERE delivery_id = ?1 AND state = 'pending' AND EXISTS (SELECT 1 FROM runs r JOIN run_chains c ON c.chain_id = r.chain_id WHERE r.run_id = parent_outbox.child_run_id AND c.project_key = ?3)",
            next_attempt_at,
            None,
        )
    }

    /// Moves a permanently undeliverable row to visible dead-letter state.
    ///
    /// # Errors
    /// Returns a typed scope, validation, or database error.
    pub fn dead_letter_outbox(
        &self,
        delivery_id: DeliveryId,
        reason: &str,
        failed_at: i64,
    ) -> Result<(), RunStoreError> {
        if reason.len() > crate::MAX_SUMMARY_BYTES {
            return Err(DomainError::TextTooLong {
                field: "dead-letter reason",
                maximum: crate::MAX_SUMMARY_BYTES,
            }
            .into());
        }
        self.update_outbox(
            delivery_id,
            "UPDATE parent_outbox SET state = 'dead_letter', attempt_count = attempt_count + 1, next_attempt_at = NULL, dead_letter_reason = ?4 WHERE delivery_id = ?1 AND state IN ('pending', 'delivered', 'dead_letter') AND EXISTS (SELECT 1 FROM runs r JOIN run_chains c ON c.chain_id = r.chain_id WHERE r.run_id = parent_outbox.child_run_id AND c.project_key = ?3)",
            failed_at,
            Some(reason),
        )
    }

    fn update_outbox(
        &self,
        delivery_id: DeliveryId,
        statement: &str,
        timestamp: i64,
        reason: Option<&str>,
    ) -> Result<(), RunStoreError> {
        let connection = self.connection()?;
        let delivery = delivery_id.to_string();
        let changed = match reason {
            Some(reason) => connection.execute(
                statement,
                params![delivery, timestamp, self.inner.project_key.as_str(), reason],
            ),
            None => connection.execute(
                statement,
                params![delivery, timestamp, self.inner.project_key.as_str()],
            ),
        }
        .map_err(RunStoreError::database)?;
        if changed == 0 {
            return Err(RunStoreError::OutboxNotFound(delivery_id));
        }
        Ok(())
    }

    fn stale_hosts(&self, policy: &ReconcilePolicy) -> Result<Vec<HostInstance>, RunStoreError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(&format!("SELECT DISTINCT h.instance_id, h.process_identity_json, h.started_at, h.heartbeat_at, h.shutdown_at FROM host_instances h JOIN runs r ON r.owner_instance_id = h.instance_id JOIN run_chains c ON c.chain_id = r.chain_id WHERE c.project_key = ?1 AND r.lifecycle IN ({ACTIVE_LIFECYCLES_SQL}) AND (h.shutdown_at IS NOT NULL OR h.heartbeat_at < ?2) ORDER BY h.heartbeat_at LIMIT ?3"))
            .map_err(RunStoreError::database)?;
        let rows = statement
            .query_map(
                params![
                    self.inner.project_key.as_str(),
                    policy.stale_before,
                    usize_to_i64(policy.max_hosts)?
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            )
            .map_err(RunStoreError::database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(RunStoreError::database)?;
        rows.into_iter()
            .map(|(id, identity, started_at, heartbeat_at, shutdown_at)| {
                Ok(HostInstance {
                    instance_id: parse_id(&id, "host instance id")?,
                    process_identity: parse_json(&identity, "process identity")?,
                    started_at,
                    heartbeat_at,
                    shutdown_at,
                })
            })
            .collect()
    }

    fn active_runs_for_owner(
        &self,
        owner: HostInstanceId,
    ) -> Result<Vec<RunRecord>, RunStoreError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(&format!("{RUN_SELECT} WHERE c.project_key = ?1 AND r.owner_instance_id = ?2 AND r.lifecycle IN ({ACTIVE_LIFECYCLES_SQL}) ORDER BY r.updated_at LIMIT ?3"))
            .map_err(RunStoreError::database)?;
        let rows = statement
            .query_map(
                params![
                    self.inner.project_key.as_str(),
                    owner.to_string(),
                    usize_to_i64(MAX_RECONCILE_RUNS_PER_HOST)?
                ],
                raw_run,
            )
            .map_err(RunStoreError::database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(RunStoreError::database)?;
        rows.into_iter().map(decode_run).collect()
    }

    fn connection(&self) -> Result<Connection, RunStoreError> {
        let connection =
            Connection::open(&self.inner.database_path).map_err(RunStoreError::database)?;
        connection
            .busy_timeout(self.inner.busy_timeout)
            .map_err(RunStoreError::database)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(RunStoreError::database)?;
        let mut journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .map_err(RunStoreError::database)?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(RunStoreError::database)?;
            journal_mode = connection
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .map_err(RunStoreError::database)?;
        }
        if !journal_mode.eq_ignore_ascii_case("wal") {
            return Err(RunStoreError::InvalidConfiguration(format!(
                "SQLite refused WAL mode and selected {journal_mode}"
            )));
        }
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(RunStoreError::database)?;
        Ok(connection)
    }

    fn notification(&self, run_id: RunId) -> Result<Arc<RevisionNotification>, RunStoreError> {
        let mut notifications = self
            .inner
            .notifications
            .lock()
            .map_err(|_| RunStoreError::Synchronization)?;
        Ok(Arc::clone(notifications.entry(run_id).or_insert_with(
            || {
                Arc::new(RevisionNotification {
                    generation: Mutex::new(0),
                    changed: Condvar::new(),
                })
            },
        )))
    }

    fn notify(&self, run_id: RunId) {
        let Ok(notification) = self.notification(run_id) else {
            return;
        };
        let Ok(mut generation) = notification.generation.lock() else {
            return;
        };
        *generation = generation.saturating_add(1);
        notification.changed.notify_all();
    }
}

const RUN_SELECT: &str = "SELECT r.run_id, r.chain_id, r.predecessor_run_id, r.backend, r.session_id, r.legacy_session_id, r.workflow_journal_id, r.parent_run_id, r.parent_session_id, r.lifecycle, r.wait_reason_code, r.wait_reason_summary, r.outcome_json, r.capabilities_json, r.owner_instance_id, r.owner_epoch, r.created_at, r.queued_at, r.started_at, r.updated_at, r.finished_at, r.last_progress_at, r.revision FROM runs r JOIN run_chains c ON c.chain_id = r.chain_id";

fn raw_run(row: &Row<'_>) -> rusqlite::Result<RawRun> {
    Ok(RawRun {
        run_id: row.get(0)?,
        chain_id: row.get(1)?,
        predecessor_run_id: row.get(2)?,
        backend: row.get(3)?,
        session_id: row.get(4)?,
        legacy_session_id: row.get(5)?,
        workflow_journal_id: row.get(6)?,
        parent_run_id: row.get(7)?,
        parent_session_id: row.get(8)?,
        lifecycle: row.get(9)?,
        wait_reason_code: row.get(10)?,
        wait_reason_summary: row.get(11)?,
        outcome_json: row.get(12)?,
        capabilities_json: row.get(13)?,
        owner_instance_id: row.get(14)?,
        owner_epoch: row.get(15)?,
        created_at: row.get(16)?,
        queued_at: row.get(17)?,
        started_at: row.get(18)?,
        updated_at: row.get(19)?,
        finished_at: row.get(20)?,
        last_progress_at: row.get(21)?,
        revision: row.get(22)?,
    })
}

fn decode_run(raw: RawRun) -> Result<RunRecord, RunStoreError> {
    let wait_reason = match (raw.wait_reason_code, raw.wait_reason_summary) {
        (Some(code), Some(summary)) => Some(WaitReason {
            code: parse_enum(&code, "wait reason code")?,
            summary,
        }),
        (None, None) => None,
        _ => {
            return Err(RunStoreError::CorruptData(
                "partial wait reason in run row".to_owned(),
            ));
        }
    };
    Ok(RunRecord {
        run_id: parse_id(&raw.run_id, "run id")?,
        chain_id: parse_id(&raw.chain_id, "chain id")?,
        predecessor_run_id: raw
            .predecessor_run_id
            .as_deref()
            .map(|value| parse_id(value, "predecessor run id"))
            .transpose()?,
        backend: parse_enum(&raw.backend, "backend")?,
        session_id: raw.session_id,
        legacy_session_id: raw.legacy_session_id,
        workflow_journal_id: raw.workflow_journal_id,
        parent_run_id: raw
            .parent_run_id
            .as_deref()
            .map(|value| parse_id(value, "parent run id"))
            .transpose()?,
        parent_session_id: raw.parent_session_id,
        lifecycle: parse_enum(&raw.lifecycle, "lifecycle")?,
        wait_reason,
        outcome: raw
            .outcome_json
            .as_deref()
            .map(|value| parse_json(value, "outcome"))
            .transpose()?,
        capabilities: parse_json(&raw.capabilities_json, "capabilities")?,
        owner_instance_id: raw
            .owner_instance_id
            .as_deref()
            .map(|value| parse_id(value, "owner instance id"))
            .transpose()?,
        owner_epoch: raw
            .owner_epoch
            .map(|value| i64_to_u64(value, "owner epoch"))
            .transpose()?,
        created_at: raw.created_at,
        queued_at: raw.queued_at,
        started_at: raw.started_at,
        updated_at: raw.updated_at,
        finished_at: raw.finished_at,
        last_progress_at: raw.last_progress_at,
        revision: i64_to_u64(raw.revision, "revision")?,
    })
}

fn query_run(
    connection: &Connection,
    project_key: &ProjectKey,
    run_id: RunId,
) -> Result<Option<RunRecord>, RunStoreError> {
    let raw = connection
        .query_row(
            &format!("{RUN_SELECT} WHERE c.project_key = ?1 AND r.run_id = ?2"),
            params![project_key.as_str(), run_id.to_string()],
            raw_run,
        )
        .optional()
        .map_err(RunStoreError::database)?;
    raw.map(decode_run).transpose()
}

fn require_run(
    connection: &Connection,
    project_key: &ProjectKey,
    run_id: RunId,
) -> Result<RunRecord, RunStoreError> {
    query_run(connection, project_key, run_id)?.ok_or(RunStoreError::NotFound(run_id))
}

fn immediate(connection: &mut Connection) -> Result<Transaction<'_>, RunStoreError> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(RunStoreError::database)
}

fn insert_event(
    transaction: &Transaction<'_>,
    run_id: RunId,
    revision: u64,
    event_type: &str,
    payload: &RunEventPayload,
    operation_id: &str,
    fingerprint: &str,
    now: i64,
) -> Result<RunEventId, RunStoreError> {
    validate_event(payload)?;
    let event_id = RunEventId::generate();
    transaction
        .execute(
            "INSERT INTO run_events(event_id, run_id, revision, type, payload_json, operation_id, operation_fingerprint, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![event_id.to_string(), run_id.to_string(), u64_to_i64(revision)?, event_type, json_text(payload)?, operation_id, fingerprint, now],
        )
        .map_err(RunStoreError::database)?;
    Ok(event_id)
}

fn idempotent_result(
    transaction: &Transaction<'_>,
    project_key: &ProjectKey,
    operation_id: &str,
    fingerprint: &str,
) -> Result<Option<RunRecord>, RunStoreError> {
    let existing = transaction
        .query_row(
            "SELECT e.run_id, e.operation_fingerprint FROM run_events e JOIN runs r ON r.run_id = e.run_id JOIN run_chains c ON c.chain_id = r.chain_id WHERE c.project_key = ?1 AND e.operation_id = ?2",
            params![project_key.as_str(), operation_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(RunStoreError::database)?;
    let Some((run_id, stored_fingerprint)) = existing else {
        return Ok(None);
    };
    if stored_fingerprint != fingerprint {
        return Err(RunStoreError::IdempotencyConflict(operation_id.to_owned()));
    }
    let run_id = parse_id(&run_id, "idempotent run id")?;
    require_run(transaction, project_key, run_id).map(Some)
}

fn insert_outbox_if_parent(
    transaction: &Transaction<'_>,
    run: &RunRecord,
    event_id: RunEventId,
    revision: u64,
    lifecycle: RunLifecycle,
    reason: Option<&WaitReason>,
    now: i64,
) -> Result<(), RunStoreError> {
    let Some(parent_session_id) = &run.parent_session_id else {
        return Ok(());
    };
    let delivery_id = DeliveryId::generate();
    let mut details = std::collections::BTreeMap::from([
        ("child_run_id".to_owned(), run.run_id.to_string()),
        ("revision".to_owned(), revision.to_string()),
        ("lifecycle".to_owned(), enum_text(lifecycle)?),
    ]);
    if let Some(wait_reason) = reason {
        details.insert("wait_reason".to_owned(), enum_text(wait_reason.code)?);
    }
    let payload = RunEventPayload {
        summary: Some(format!("Child run is {lifecycle:?}").to_lowercase()),
        details,
    };
    validate_event(&payload)?;
    transaction
        .execute(
            "INSERT INTO parent_outbox(delivery_id, source_event_id, child_run_id, parent_session_id, payload_json, state, attempt_count, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6)",
            params![delivery_id.to_string(), event_id.to_string(), run.run_id.to_string(), parent_session_id, json_text(&payload)?, now],
        )
        .map_err(RunStoreError::database)?;
    Ok(())
}

fn should_notify_parent(lifecycle: RunLifecycle) -> bool {
    lifecycle.is_terminal()
        || matches!(
            lifecycle,
            RunLifecycle::WaitingInput | RunLifecycle::Blocked
        )
}

fn verify_revision_and_owner(
    run: &RunRecord,
    expected_revision: u64,
    owner: Option<&OwnerFence>,
) -> Result<(), RunStoreError> {
    if run.revision != expected_revision {
        return Err(RunStoreError::RevisionConflict {
            run_id: run.run_id,
            expected: expected_revision,
            actual: run.revision,
        });
    }
    match (run.owner_instance_id, run.owner_epoch, owner) {
        (None, None, None) => Ok(()),
        (Some(actual_instance), Some(actual_epoch), Some(expected))
            if actual_instance == expected.instance_id && actual_epoch == expected.epoch =>
        {
            Ok(())
        }
        (Some(actual_instance), Some(actual_epoch), _) => Err(RunStoreError::OwnershipFence {
            run_id: run.run_id,
            actual_instance: Some(actual_instance),
            actual_epoch: Some(actual_epoch),
        }),
        _ => Err(RunStoreError::CorruptData(
            "partial owner fence in run row".to_owned(),
        )),
    }
}

fn require_live_host(
    connection: &Connection,
    instance_id: HostInstanceId,
) -> Result<(), RunStoreError> {
    let state = connection
        .query_row(
            "SELECT shutdown_at FROM host_instances WHERE instance_id = ?1",
            [instance_id.to_string()],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(RunStoreError::database)?;
    match state {
        Some(None) => Ok(()),
        Some(Some(_)) => Err(RunStoreError::HostShutDown(instance_id)),
        None => Err(RunStoreError::HostNotFound(instance_id)),
    }
}

fn transition_fingerprint(request: &TransitionRequest) -> Result<String, RunStoreError> {
    json_text(&json!({
        "run_id": request.run_id,
        "expected_revision": request.expected_revision,
        "owner": request.owner.as_ref().map(|owner| json!({"instance_id": owner.instance_id, "epoch": owner.epoch})),
        "target": request.target,
        "wait_reason": request.wait_reason,
        "outcome": request.outcome,
        "event_type": request.event_type,
        "event": request.event,
        "progress": request.progress,
    }))
}

fn legacy_outcome(lifecycle: StoredSessionLifecycle) -> (RunLifecycle, RunOutcome) {
    match lifecycle {
        StoredSessionLifecycle::Succeeded => (
            RunLifecycle::Succeeded,
            RunOutcome::status(OutcomeStatus::Succeeded),
        ),
        StoredSessionLifecycle::Failed => (
            RunLifecycle::Failed,
            RunOutcome::status(OutcomeStatus::Failed),
        ),
        StoredSessionLifecycle::Cancelled => (
            RunLifecycle::Cancelled,
            RunOutcome::status(OutcomeStatus::Cancelled),
        ),
        StoredSessionLifecycle::Idle => (
            RunLifecycle::Interrupted,
            RunOutcome::status(OutcomeStatus::Unknown),
        ),
        StoredSessionLifecycle::Queued
        | StoredSessionLifecycle::Bootstrapping
        | StoredSessionLifecycle::Running
        | StoredSessionLifecycle::WaitingInput
        | StoredSessionLifecycle::Paused => (
            RunLifecycle::Interrupted,
            RunOutcome::status(OutcomeStatus::Interrupted),
        ),
    }
}

fn decode_outbox(raw: RawOutbox) -> Result<ParentOutboxRecord, RunStoreError> {
    Ok(ParentOutboxRecord {
        delivery_id: parse_id(&raw.delivery_id, "delivery id")?,
        source_event_id: parse_id(&raw.source_event_id, "source event id")?,
        child_run_id: parse_id(&raw.child_run_id, "child run id")?,
        parent_session_id: raw.parent_session_id,
        payload: parse_json(&raw.payload, "outbox payload")?,
        state: parse_enum(&raw.state, "outbox state")?,
        attempt_count: u32::try_from(raw.attempt_count)
            .map_err(|_| RunStoreError::CorruptData("invalid attempt count".to_owned()))?,
        next_attempt_at: raw.next_attempt_at,
        created_at: raw.created_at,
        delivered_at: raw.delivered_at,
        acknowledged_at: raw.acknowledged_at,
        dead_letter_reason: raw.dead_letter_reason,
    })
}

fn enum_text<T: Serialize>(value: T) -> Result<String, RunStoreError> {
    let serialized = serde_json::to_value(value).map_err(RunStoreError::Serialization)?;
    serialized
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| RunStoreError::CorruptData("enum did not serialize as a string".to_owned()))
}

fn parse_enum<T: DeserializeOwned>(value: &str, field: &'static str) -> Result<T, RunStoreError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|error| RunStoreError::CorruptData(format!("invalid {field}: {error}")))
}

fn json_text<T: Serialize>(value: &T) -> Result<String, RunStoreError> {
    serde_json::to_string(value).map_err(RunStoreError::Serialization)
}

fn parse_json<T: DeserializeOwned>(value: &str, field: &'static str) -> Result<T, RunStoreError> {
    serde_json::from_str(value)
        .map_err(|error| RunStoreError::CorruptData(format!("invalid {field}: {error}")))
}

fn parse_id<T: std::str::FromStr>(value: &str, field: &'static str) -> Result<T, RunStoreError>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| RunStoreError::CorruptData(format!("invalid {field}: {error}")))
}

fn now_millis() -> Result<i64, RunStoreError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| RunStoreError::Clock(error.to_string()))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| RunStoreError::Clock("epoch milliseconds exceed SQLite integer".to_owned()))
}

fn u64_to_i64(value: u64) -> Result<i64, RunStoreError> {
    i64::try_from(value)
        .map_err(|_| RunStoreError::CorruptData("integer exceeds SQLite range".to_owned()))
}

fn i64_to_u64(value: i64, field: &'static str) -> Result<u64, RunStoreError> {
    u64::try_from(value).map_err(|_| RunStoreError::CorruptData(format!("negative {field}")))
}

fn usize_to_i64(value: usize) -> Result<i64, RunStoreError> {
    i64::try_from(value)
        .map_err(|_| RunStoreError::InvalidConfiguration("limit exceeds SQLite range".to_owned()))
}

fn validate_title(title: &str) -> Result<(), RunStoreError> {
    if title.len() > crate::MAX_SUMMARY_BYTES {
        return Err(DomainError::TextTooLong {
            field: "title",
            maximum: crate::MAX_SUMMARY_BYTES,
        }
        .into());
    }
    Ok(())
}

fn injected_failure(actual: FailurePoint, expected: FailurePoint) -> Result<(), RunStoreError> {
    if actual == expected {
        Err(RunStoreError::InjectedFailure)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionBackend, RunKind};
    use n00n_storage::{
        id::n00nId,
        sessions::{Session, StoredSessionLifecycle, TitleSource, scan_legacy_child_sessions},
    };
    use rusqlite::OpenFlags;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use std::thread;
    use tempfile::TempDir;

    #[derive(Clone, Default, Deserialize, Serialize)]
    struct TestMessage(String);

    impl TitleSource for TestMessage {
        fn first_user_text(&self) -> Option<&str> {
            Some(&self.0)
        }
    }

    fn store(temp: &TempDir, project: &str) -> RunStore {
        RunStore::open_path(
            temp.path().join("runs.sqlite3"),
            ProjectKey::new(project).unwrap(),
            Duration::from_millis(75),
        )
        .unwrap()
    }

    fn host(store: &RunStore, marker: &str) -> HostInstance {
        store
            .register_host(ProcessIdentity {
                pid: 1,
                start_marker: marker.to_owned(),
                lock_identity: format!("lock-{marker}"),
            })
            .unwrap()
    }

    fn spec(owner: Option<HostInstanceId>, parent: bool) -> NewRunSpec {
        let mut spec = NewRunSpec::new(RunKind::Task, ExecutionBackend::TuiSession, "test run");
        spec.owner_instance_id = owner;
        spec.parent_session_id = parent.then(|| "parent-session".to_owned());
        spec.capabilities.resume = true;
        spec
    }

    fn request(
        run: &RunRecord,
        owner: Option<OwnerFence>,
        target: RunLifecycle,
        operation: &str,
    ) -> TransitionRequest {
        let outcome = target.is_terminal().then(|| {
            RunOutcome::status(match target {
                RunLifecycle::Succeeded => OutcomeStatus::Succeeded,
                RunLifecycle::Failed => OutcomeStatus::Failed,
                RunLifecycle::TimedOut => OutcomeStatus::TimedOut,
                RunLifecycle::Cancelled => OutcomeStatus::Cancelled,
                RunLifecycle::Interrupted => OutcomeStatus::Interrupted,
                RunLifecycle::Lost => OutcomeStatus::Lost,
                _ => unreachable!(),
            })
        });
        TransitionRequest {
            run_id: run.run_id,
            expected_revision: run.revision,
            owner,
            target,
            wait_reason: None,
            outcome,
            event_type: "test_transition".to_owned(),
            event: RunEventPayload::empty(),
            operation_id: operation.to_owned(),
            progress: false,
        }
    }

    #[test]
    fn concurrent_revision_conflict_and_idempotent_retry() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp, "/project");
        let run = store.create_run(&spec(None, false)).unwrap();
        let transition = request(&run, None, RunLifecycle::Starting, "start-op");
        let updated = store.transition(&transition).unwrap();
        assert_eq!(updated.revision, 2);
        let retried = store.transition(&transition).unwrap();
        assert_eq!(retried, updated);

        let conflict = TransitionRequest {
            operation_id: "different-op".to_owned(),
            ..transition
        };
        assert!(matches!(
            store.transition(&conflict),
            Err(RunStoreError::RevisionConflict { actual: 2, .. })
        ));
    }

    #[test]
    fn transition_event_and_outbox_are_atomic_at_every_failure_boundary() {
        for failure in [
            FailurePoint::AfterRunUpdate,
            FailurePoint::AfterEventInsert,
            FailurePoint::AfterOutboxInsert,
        ] {
            let temp = TempDir::new().unwrap();
            let store = store(&temp, "/project");
            let initial = store.create_run(&spec(None, true)).unwrap();
            let transition = request(&initial, None, RunLifecycle::Cancelled, "cancel-op");
            assert!(matches!(
                store.transition_with_failure(&transition, failure),
                Err(RunStoreError::InjectedFailure)
            ));
            assert_eq!(store.get_run(initial.run_id).unwrap(), initial);
            assert_eq!(store.events(initial.run_id, 1, 10).unwrap().len(), 0);
            assert_eq!(store.pending_outbox(i64::MAX, 10).unwrap().len(), 0);
        }
    }

    #[test]
    fn resume_creates_new_attempt_in_same_chain() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp, "/project");
        let initial = store.create_run(&spec(None, false)).unwrap();
        let terminal = store
            .transition(&request(
                &initial,
                None,
                RunLifecycle::Cancelled,
                "terminal",
            ))
            .unwrap();
        let resumed = store
            .resume_run(&ResumeRequest {
                prior_run_id: terminal.run_id,
                expected_revision: terminal.revision,
                owner_instance_id: None,
                operation_id: "resume".to_owned(),
            })
            .unwrap();
        assert_ne!(resumed.run_id, terminal.run_id);
        assert_eq!(resumed.chain_id, terminal.chain_id);
        assert_eq!(resumed.predecessor_run_id, Some(terminal.run_id));
        assert_eq!(store.get_run(terminal.run_id).unwrap(), terminal);
    }

    #[test]
    fn wal_foreign_keys_full_sync_and_bounded_busy_timeout_apply_to_each_connection() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp, "/project");
        let connection = store.connection().unwrap();
        let journal: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        let synchronous: i64 = connection
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        assert_eq!(foreign_keys, 1);
        assert_eq!(synchronous, 2);

        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        let started = Instant::now();
        let error = store.create_run(&spec(None, false)).unwrap_err();
        assert!(matches!(error, RunStoreError::Busy));
        assert!(started.elapsed() >= Duration::from_millis(50));
        connection.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn proven_stale_owner_is_interrupted_but_live_owner_is_untouched() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp, "/project");
        let stale = host(&store, "stale");
        let live = host(&store, "live");
        let stale_run = store
            .create_run(&spec(Some(stale.instance_id), false))
            .unwrap();
        let live_run = store
            .create_run(&spec(Some(live.instance_id), false))
            .unwrap();
        let connection = store.connection().unwrap();
        connection
            .execute("UPDATE host_instances SET heartbeat_at = 1", [])
            .unwrap();
        let report = store
            .reconcile_stale(
                &ReconcilePolicy {
                    stale_before: 2,
                    max_hosts: 10,
                },
                |candidate| {
                    if candidate.instance_id == stale.instance_id {
                        HostLiveness::Gone
                    } else {
                        HostLiveness::Live
                    }
                },
            )
            .unwrap();
        assert_eq!(report.interrupted, vec![stale_run.run_id]);
        assert_eq!(report.live_owners, 1);
        assert_eq!(
            store.get_run(stale_run.run_id).unwrap().lifecycle,
            RunLifecycle::Interrupted
        );
        assert_eq!(store.get_run(live_run.run_id).unwrap(), live_run);
    }

    #[test]
    fn obsolete_owner_epoch_and_revision_are_rejected() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp, "/project");
        let first = host(&store, "first");
        let second = host(&store, "second");
        let run = store
            .create_run(&spec(Some(first.instance_id), false))
            .unwrap();
        let transferred = store
            .transfer_owner(
                run.run_id,
                run.revision,
                &OwnerFence {
                    instance_id: first.instance_id,
                    epoch: 1,
                },
                second.instance_id,
                "transfer",
            )
            .unwrap();
        let late = request(
            &transferred,
            Some(OwnerFence {
                instance_id: first.instance_id,
                epoch: 1,
            }),
            RunLifecycle::Starting,
            "late",
        );
        assert!(matches!(
            store.transition(&late),
            Err(RunStoreError::OwnershipFence { .. })
        ));
        let stale_revision = TransitionRequest {
            expected_revision: run.revision,
            owner: Some(OwnerFence {
                instance_id: second.instance_id,
                epoch: 2,
            }),
            operation_id: "stale-revision".to_owned(),
            ..late
        };
        assert!(matches!(
            store.transition(&stale_revision),
            Err(RunStoreError::RevisionConflict { .. })
        ));
    }

    #[test]
    fn wait_observes_in_process_notification_and_cross_connection_durable_change() {
        let temp = TempDir::new().unwrap();
        let first = store(&temp, "/project");
        let second = store(&temp, "/project");
        let run = first.create_run(&spec(None, false)).unwrap();
        let waiter = first;
        let handle = thread::spawn(move || {
            waiter
                .wait_for_revision(run.run_id, run.revision, Duration::from_secs(2))
                .unwrap()
        });
        second
            .transition(&request(
                &run,
                None,
                RunLifecycle::Starting,
                "cross-process",
            ))
            .unwrap();
        let result = handle.join().unwrap();
        assert!(!result.observation_timed_out);
        assert_eq!(result.run.revision, 2);
    }

    #[test]
    fn project_scope_hides_foreign_run_events_and_outbox() {
        let temp = TempDir::new().unwrap();
        let first = store(&temp, "/project-a");
        let second = store(&temp, "/project-b");
        let run = first.create_run(&spec(None, true)).unwrap();
        first
            .transition(&request(&run, None, RunLifecycle::Cancelled, "cancel"))
            .unwrap();
        assert!(matches!(
            second.get_run(run.run_id),
            Err(RunStoreError::NotFound(_))
        ));
        assert!(matches!(
            second.events(run.run_id, 0, 10),
            Err(RunStoreError::NotFound(_))
        ));
        assert!(second.pending_outbox(i64::MAX, 10).unwrap().is_empty());
    }

    #[test]
    fn retention_never_prunes_a_run_with_pending_outbox() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp, "/project");
        let run = store.create_run(&spec(None, true)).unwrap();
        let terminal = store
            .transition(&request(&run, None, RunLifecycle::Cancelled, "cancel"))
            .unwrap();
        let report = store
            .compact(&RetentionPolicy {
                terminal_before: i64::MAX,
                finalized_outbox_before: i64::MAX,
                shutdown_host_before: i64::MAX,
                max_rows: 10,
            })
            .unwrap();
        assert_eq!(report.runs, 0);
        assert_eq!(store.get_run(terminal.run_id).unwrap(), terminal);
    }

    #[test]
    fn outbox_delivery_updates_are_idempotent_project_scoped_and_crash_safe() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp, "/project");
        let run = store.create_run(&spec(None, true)).unwrap();
        store
            .transition(&request(&run, None, RunLifecycle::Cancelled, "cancel"))
            .unwrap();
        let delivery = store.pending_outbox(i64::MAX, 10).unwrap().remove(0);

        store
            .mark_outbox_delivered(delivery.delivery_id, 10)
            .unwrap();
        store
            .mark_outbox_delivered(delivery.delivery_id, 11)
            .unwrap();
        let connection = store.connection().unwrap();
        let delivered: (String, i64) = connection
            .query_row(
                "SELECT state, attempt_count FROM parent_outbox WHERE delivery_id = ?1",
                [delivery.delivery_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(delivered, ("delivered".to_owned(), 1));

        store.acknowledge_outbox(delivery.delivery_id, 12).unwrap();
        let acknowledged: String = connection
            .query_row(
                "SELECT state FROM parent_outbox WHERE delivery_id = ?1",
                [delivery.delivery_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(acknowledged, "acknowledged");

        let foreign = self::store(&temp, "/other-project");
        assert!(matches!(
            foreign.acknowledge_outbox(delivery.delivery_id, 13),
            Err(RunStoreError::OutboxNotFound(_))
        ));
    }

    #[test]
    fn pending_delivery_can_be_acknowledged_or_dead_lettered_at_crash_boundaries() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp, "/project");
        let first = store.create_run(&spec(None, true)).unwrap();
        store
            .transition(&request(&first, None, RunLifecycle::Cancelled, "first"))
            .unwrap();
        let first_delivery = store.pending_outbox(i64::MAX, 10).unwrap().remove(0);
        store
            .acknowledge_outbox(first_delivery.delivery_id, 20)
            .unwrap();

        let second = store.create_run(&spec(None, true)).unwrap();
        store
            .transition(&request(&second, None, RunLifecycle::Cancelled, "second"))
            .unwrap();
        let second_delivery = store.pending_outbox(i64::MAX, 10).unwrap().remove(0);
        store
            .dead_letter_outbox(second_delivery.delivery_id, "parent_deleted", 21)
            .unwrap();
        let connection = store.connection().unwrap();
        let row: (String, String) = connection
            .query_row(
                "SELECT state, dead_letter_reason FROM parent_outbox WHERE delivery_id = ?1",
                [second_delivery.delivery_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("dead_letter".to_owned(), "parent_deleted".to_owned()));
    }

    #[test]
    fn legacy_import_is_crash_retry_idempotent_unknown_safe_and_project_scoped() {
        let temp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(temp.path().to_path_buf());
        let mut idle = Session::<TestMessage, Value, Value>::new("model", "/project-a");
        idle.title = "ambiguous legacy child".to_owned();
        idle.meta.parent_id = Some(n00nId::generate());
        idle.meta.lifecycle = StoredSessionLifecycle::Idle;
        idle.save(&state_dir).unwrap();
        let mut active = Session::<TestMessage, Value, Value>::new("model", "/project-a");
        active.meta.parent_id = Some(n00nId::generate());
        active.meta.lifecycle = StoredSessionLifecycle::Running;
        active.save(&state_dir).unwrap();
        let mut foreign = Session::<TestMessage, Value, Value>::new("model", "/project-b");
        foreign.meta.parent_id = Some(n00nId::generate());
        foreign.meta.lifecycle = StoredSessionLifecycle::Succeeded;
        foreign.save(&state_dir).unwrap();

        let project_a = store(&temp, "/project-a");
        let metadata = scan_legacy_child_sessions("/project-a", &state_dir).unwrap();
        assert_eq!(metadata.len(), 2);
        let idle_metadata = metadata
            .iter()
            .find(|session| session.lifecycle == StoredSessionLifecycle::Idle)
            .unwrap();
        let partial = project_a.import_legacy_one(idle_metadata).unwrap();
        let first_retry = project_a.import_legacy_sessions(&state_dir).unwrap();
        let second_retry = project_a.import_legacy_sessions(&state_dir).unwrap();
        assert_eq!(first_retry.len(), 2);
        assert_eq!(second_retry.len(), 2);
        assert_eq!(project_a.list_runs(10).unwrap().len(), 2);
        let imported_idle = project_a.get_run(partial.run_id).unwrap();
        assert_eq!(imported_idle.lifecycle, RunLifecycle::Interrupted);
        assert_eq!(
            imported_idle.outcome.map(|outcome| outcome.status),
            Some(OutcomeStatus::Unknown)
        );

        let project_b = store(&temp, "/project-b");
        let foreign_imports = project_b.import_legacy_sessions(&state_dir).unwrap();
        assert_eq!(foreign_imports.len(), 1);
        assert!(matches!(
            project_b.get_run(partial.run_id),
            Err(RunStoreError::NotFound(_))
        ));
        assert_eq!(project_b.list_runs(10).unwrap().len(), 1);
    }

    #[test]
    fn second_raw_connection_honors_database_lock() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp, "/project");
        let first = store.connection().unwrap();
        first.execute_batch("BEGIN IMMEDIATE").unwrap();
        let second = Connection::open_with_flags(
            temp.path().join("runs.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .unwrap();
        second.busy_timeout(Duration::from_millis(25)).unwrap();
        let error = second.execute_batch("BEGIN IMMEDIATE").unwrap_err();
        assert!(matches!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
        ));
        first.execute_batch("ROLLBACK").unwrap();
    }
}
