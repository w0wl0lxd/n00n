use crate::{
    AnswerRunRequest, CancelRunRequest, ExecutionBackend, MAX_PREVIEW_BYTES, MAX_SUMMARY_BYTES,
    NewRunSpec, ParentDispatchReport, ParentInboxAdapter, ParentInsertResult, PauseRunRequest,
    ResumeRequest, ResumeRunControlRequest, RunAdapter, RunAdapterError, RunId, RunLifecycle,
    RunProbe, RunRecord, RunStore, RunStoreError, RunWaitResult, SendRunRequest, TransitionRequest,
};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

#[derive(Clone)]
pub struct RunService {
    store: RunStore,
    adapters: Arc<RwLock<HashMap<ExecutionBackend, Arc<dyn RunAdapter>>>>,
}

impl RunService {
    #[must_use]
    pub fn new(store: RunStore) -> Self {
        Self {
            store,
            adapters: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn store(&self) -> &RunStore {
        &self.store
    }

    /// Registers or replaces one execution-backend adapter.
    ///
    /// # Errors
    /// Returns an error if the adapter registry lock is poisoned.
    pub fn register_adapter(
        &self,
        backend: ExecutionBackend,
        adapter: Arc<dyn RunAdapter>,
    ) -> Result<(), RunStoreError> {
        self.adapters
            .write()
            .map_err(|_| RunStoreError::Synchronization)?
            .insert(backend, adapter);
        Ok(())
    }

    /// Creates a new chain and first run without blocking the async executor.
    ///
    /// # Errors
    /// Returns the repository's typed error.
    pub async fn create_run(&self, spec: NewRunSpec) -> Result<RunRecord, RunStoreError> {
        let store = self.store.clone();
        smol::unblock(move || store.create_run(&spec)).await
    }

    /// Reads one project-scoped run without blocking the async executor.
    ///
    /// # Errors
    /// Returns the repository's typed error.
    pub async fn get_run(&self, run_id: RunId) -> Result<RunRecord, RunStoreError> {
        let store = self.store.clone();
        smol::unblock(move || store.get_run(run_id)).await
    }

    /// Applies an atomic run transition without blocking the async executor.
    ///
    /// # Errors
    /// Returns the repository's typed error.
    pub async fn transition(&self, request: TransitionRequest) -> Result<RunRecord, RunStoreError> {
        let store = self.store.clone();
        smol::unblock(move || store.transition(&request)).await
    }

    /// Creates a linked new attempt without blocking the async executor.
    ///
    /// # Errors
    /// Returns the repository's typed error.
    pub async fn resume_run(&self, request: ResumeRequest) -> Result<RunRecord, RunStoreError> {
        let store = self.store.clone();
        smol::unblock(move || store.resume_run(&request)).await
    }

    /// Waits for a newer durable revision without blocking the async executor.
    ///
    /// # Errors
    /// Returns the repository's typed error.
    pub async fn wait_for_revision(
        &self,
        run_id: RunId,
        after_revision: u64,
        timeout: Duration,
    ) -> Result<RunWaitResult, RunStoreError> {
        let store = self.store.clone();
        smol::unblock(move || store.wait_for_revision(run_id, after_revision, timeout)).await
    }

    /// Dispatches due parent deliveries with durable at-least-once attempts.
    ///
    /// # Errors
    /// Returns typed store, validation, or clock-overflow errors.
    pub async fn dispatch_parent_outbox(
        &self,
        inbox: &dyn ParentInboxAdapter,
        now: i64,
        retry_delay_millis: i64,
        limit: usize,
    ) -> Result<ParentDispatchReport, RunStoreError> {
        let store = self.store.clone();
        let deliveries = smol::unblock(move || store.dispatchable_outbox(now, limit)).await?;
        let mut report = ParentDispatchReport::default();
        for delivery in deliveries {
            match inbox.insert(&delivery).await {
                Ok(ParentInsertResult::Inserted | ParentInsertResult::AlreadyPresent) => {
                    let store = self.store.clone();
                    let delivery_id = delivery.delivery_id;
                    smol::unblock(move || store.mark_outbox_delivered(delivery_id, now)).await?;
                    report.delivered += 1;
                }
                Ok(ParentInsertResult::AlreadyConsumed) => {
                    let store = self.store.clone();
                    let delivery_id = delivery.delivery_id;
                    smol::unblock(move || store.acknowledge_outbox(delivery_id, now)).await?;
                    report.delivered += 1;
                }
                Ok(ParentInsertResult::PermanentUnavailable { reason }) => {
                    validate_control_text("dead-letter reason", &reason, MAX_SUMMARY_BYTES)?;
                    let store = self.store.clone();
                    let delivery_id = delivery.delivery_id;
                    smol::unblock(move || store.dead_letter_outbox(delivery_id, &reason, now))
                        .await?;
                    report.dead_lettered += 1;
                }
                Err(_) => {
                    let retry_at = now.checked_add(retry_delay_millis).ok_or_else(|| {
                        RunStoreError::Clock("parent outbox retry deadline overflow".to_owned())
                    })?;
                    let store = self.store.clone();
                    let delivery_id = delivery.delivery_id;
                    smol::unblock(move || store.retry_outbox(delivery_id, retry_at)).await?;
                    report.retried += 1;
                }
            }
        }
        Ok(report)
    }

    /// Acknowledges durable consumption of a parent delivery.
    ///
    /// # Errors
    /// Returns a typed scope or database error.
    pub async fn acknowledge_parent_delivery(
        &self,
        delivery_id: crate::DeliveryId,
        acknowledged_at: i64,
    ) -> Result<(), RunStoreError> {
        let store = self.store.clone();
        smol::unblock(move || store.acknowledge_outbox(delivery_id, acknowledged_at)).await
    }

    /// Routes text to a capability-compatible adapter.
    ///
    /// # Errors
    /// Returns typed scope, revision, capability, registry, validation, or adapter errors.
    pub async fn send(&self, request: SendRunRequest) -> Result<RunRecord, RunStoreError> {
        validate_control_text("send text", &request.text, MAX_PREVIEW_BYTES)?;
        let run = self
            .checked_run(request.run_id, request.expected_revision)
            .await?;
        require_capability(run.capabilities.send, "send")?;
        let adapter = self.adapter(run.backend)?;
        adapter
            .send(&run, &request.text, request.delivery)
            .await
            .map_err(map_adapter_error)?;
        self.get_run(run.run_id).await
    }

    /// Routes an answer to a capability-compatible adapter.
    ///
    /// # Errors
    /// Returns typed scope, revision, capability, registry, validation, or adapter errors.
    pub async fn answer(&self, request: AnswerRunRequest) -> Result<RunRecord, RunStoreError> {
        validate_control_text("answer", &request.response, MAX_PREVIEW_BYTES)?;
        let run = self
            .checked_run(request.run_id, request.expected_revision)
            .await?;
        require_capability(run.capabilities.answer, "answer")?;
        let adapter = self.adapter(run.backend)?;
        adapter
            .answer(&run, &request.response)
            .await
            .map_err(map_adapter_error)?;
        self.get_run(run.run_id).await
    }

    /// Requests cancellation without claiming terminal acknowledgement.
    ///
    /// # Errors
    /// Returns typed scope, revision, capability, registry, validation, or adapter errors.
    pub async fn request_cancel(
        &self,
        request: CancelRunRequest,
    ) -> Result<RunRecord, RunStoreError> {
        if let Some(reason) = &request.reason {
            validate_control_text("cancel reason", reason, MAX_SUMMARY_BYTES)?;
        }
        let run = self
            .checked_run(request.run_id, request.expected_revision)
            .await?;
        require_capability(run.capabilities.cancel, "cancel")?;
        let adapter = self.adapter(run.backend)?;
        adapter
            .request_cancel(&run, request.reason.as_deref())
            .await
            .map_err(map_adapter_error)?;
        self.get_run(run.run_id).await
    }

    /// Requests pause without reporting paused before adapter acknowledgement.
    ///
    /// # Errors
    /// Returns typed scope, revision, capability, registry, or adapter errors.
    pub async fn request_pause(
        &self,
        request: PauseRunRequest,
    ) -> Result<RunRecord, RunStoreError> {
        let run = self
            .checked_run(request.run_id, request.expected_revision)
            .await?;
        require_capability(run.capabilities.pause, "pause")?;
        let adapter = self.adapter(run.backend)?;
        adapter
            .request_pause(&run)
            .await
            .map_err(map_adapter_error)?;
        self.get_run(run.run_id).await
    }

    /// Resumes an acknowledged pause in place or creates a linked attempt from a terminal run.
    ///
    /// # Errors
    /// Returns typed lifecycle, scope, revision, capability, registry, validation, or adapter errors.
    pub async fn resume(
        &self,
        request: ResumeRunControlRequest,
    ) -> Result<RunRecord, RunStoreError> {
        if let Some(guidance) = &request.guidance {
            validate_control_text("resume guidance", guidance, MAX_PREVIEW_BYTES)?;
        }
        let prior = self
            .checked_run(request.run_id, request.expected_revision)
            .await?;
        require_capability(prior.capabilities.resume, "resume")?;
        let adapter = self.adapter(prior.backend)?;
        if prior.lifecycle == RunLifecycle::Paused {
            adapter
                .resume_paused(&prior, request.guidance.as_deref())
                .await
                .map_err(map_adapter_error)?;
            return self.get_run(prior.run_id).await;
        }
        if !prior.lifecycle.is_terminal() {
            return Err(RunStoreError::ResumeRequiresPausedOrTerminal(
                prior.lifecycle,
            ));
        }
        let next = self
            .resume_run(ResumeRequest {
                prior_run_id: prior.run_id,
                expected_revision: prior.revision,
                owner_instance_id: request.owner_instance_id,
                operation_id: request.operation_id,
            })
            .await?;
        adapter
            .resume_from(&prior, &next, request.guidance.as_deref())
            .await
            .map_err(map_adapter_error)?;
        Ok(next)
    }

    /// Probes backend liveness without holding a service lock or database transaction.
    ///
    /// # Errors
    /// Returns typed scope, revision, registry, or adapter errors.
    pub async fn probe(
        &self,
        run_id: RunId,
        expected_revision: u64,
    ) -> Result<RunProbe, RunStoreError> {
        let run = self.checked_run(run_id, expected_revision).await?;
        let adapter = self.adapter(run.backend)?;
        adapter.probe(&run).await.map_err(map_adapter_error)
    }

    fn adapter(&self, backend: ExecutionBackend) -> Result<Arc<dyn RunAdapter>, RunStoreError> {
        self.adapters
            .read()
            .map_err(|_| RunStoreError::Synchronization)?
            .get(&backend)
            .cloned()
            .ok_or(RunStoreError::AdapterUnavailable(backend))
    }

    async fn checked_run(
        &self,
        run_id: RunId,
        expected_revision: u64,
    ) -> Result<RunRecord, RunStoreError> {
        let run = self.get_run(run_id).await?;
        if run.revision != expected_revision {
            return Err(RunStoreError::RevisionConflict {
                run_id,
                expected: expected_revision,
                actual: run.revision,
            });
        }
        Ok(run)
    }
}

fn require_capability(enabled: bool, capability: &'static str) -> Result<(), RunStoreError> {
    if enabled {
        Ok(())
    } else {
        Err(RunStoreError::UnsupportedCapability(capability))
    }
}

fn validate_control_text(
    field: &'static str,
    text: &str,
    maximum: usize,
) -> Result<(), RunStoreError> {
    if text.len() > maximum {
        return Err(crate::DomainError::TextTooLong { field, maximum }.into());
    }
    Ok(())
}

fn map_adapter_error(error: RunAdapterError) -> RunStoreError {
    RunStoreError::Adapter {
        code: error.code,
        message: error.message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AdapterFuture, ExecutionBackend, OutcomeStatus, RunCapabilities, RunDelivery,
        RunEventPayload, RunKind, RunOutcome,
    };
    use std::{
        collections::HashSet,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };
    use tempfile::TempDir;

    #[derive(Default)]
    struct FakeAdapter {
        calls: Mutex<Vec<&'static str>>,
    }

    impl FakeAdapter {
        fn record(&self, call: &'static str) -> AdapterFuture<'_, ()> {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(call);
            }
            Box::pin(async { Ok(()) })
        }
    }

    impl RunAdapter for FakeAdapter {
        fn send<'a>(
            &'a self,
            _run: &'a RunRecord,
            _text: &'a str,
            _delivery: RunDelivery,
        ) -> AdapterFuture<'a, ()> {
            self.record("send")
        }

        fn answer<'a>(&'a self, _run: &'a RunRecord, _response: &'a str) -> AdapterFuture<'a, ()> {
            self.record("answer")
        }

        fn request_cancel<'a>(
            &'a self,
            _run: &'a RunRecord,
            _reason: Option<&'a str>,
        ) -> AdapterFuture<'a, ()> {
            self.record("cancel")
        }

        fn request_pause<'a>(&'a self, _run: &'a RunRecord) -> AdapterFuture<'a, ()> {
            self.record("pause")
        }

        fn resume_paused<'a>(
            &'a self,
            _run: &'a RunRecord,
            _guidance: Option<&'a str>,
        ) -> AdapterFuture<'a, ()> {
            self.record("resume_paused")
        }

        fn resume_from<'a>(
            &'a self,
            _prior: &'a RunRecord,
            _next: &'a RunRecord,
            _guidance: Option<&'a str>,
        ) -> AdapterFuture<'a, ()> {
            self.record("resume_from")
        }

        fn probe<'a>(&'a self, _run: &'a RunRecord) -> AdapterFuture<'a, RunProbe> {
            Box::pin(async {
                Ok(RunProbe {
                    live: true,
                    process_identity_verified: true,
                    summary: None,
                })
            })
        }
    }

    #[derive(Default)]
    struct FakeInbox {
        logical_insertions: Mutex<HashSet<String>>,
        fail_after_insert: AtomicBool,
        unavailable: AtomicBool,
        consumed: AtomicBool,
    }

    impl ParentInboxAdapter for FakeInbox {
        fn insert<'a>(
            &'a self,
            delivery: &'a crate::ParentOutboxRecord,
        ) -> AdapterFuture<'a, ParentInsertResult> {
            let result = if self.consumed.load(Ordering::SeqCst) {
                Ok(ParentInsertResult::AlreadyConsumed)
            } else if self.unavailable.load(Ordering::SeqCst) {
                Ok(ParentInsertResult::PermanentUnavailable {
                    reason: "parent_deleted".to_owned(),
                })
            } else {
                let Ok(mut insertions) = self.logical_insertions.lock() else {
                    return Box::pin(async {
                        Err(RunAdapterError {
                            code: "poisoned".to_owned(),
                            message: "inbox lock poisoned".to_owned(),
                        })
                    });
                };
                let inserted = insertions.insert(delivery.delivery_id.to_string());
                if inserted && self.fail_after_insert.swap(false, Ordering::SeqCst) {
                    Err(RunAdapterError {
                        code: "crash_after_insert".to_owned(),
                        message: "simulated crash boundary".to_owned(),
                    })
                } else if inserted {
                    Ok(ParentInsertResult::Inserted)
                } else {
                    Ok(ParentInsertResult::AlreadyPresent)
                }
            };
            Box::pin(async move { result })
        }
    }

    fn service(
        capabilities: RunCapabilities,
    ) -> (TempDir, RunService, Arc<FakeAdapter>, RunRecord) {
        let temp = TempDir::new().unwrap();
        let store = RunStore::open_path(
            temp.path().join("runs.sqlite3"),
            crate::ProjectKey::new("/project").unwrap(),
            Duration::from_millis(50),
        )
        .unwrap();
        let service = RunService::new(store);
        let adapter = Arc::new(FakeAdapter::default());
        service
            .register_adapter(
                ExecutionBackend::TuiSession,
                Arc::<FakeAdapter>::clone(&adapter),
            )
            .unwrap();
        let mut spec = NewRunSpec::new(RunKind::Task, ExecutionBackend::TuiSession, "test");
        spec.capabilities = capabilities;
        let run = smol::block_on(service.create_run(spec)).unwrap();
        (temp, service, adapter, run)
    }

    fn transition(run: &RunRecord, target: RunLifecycle, operation_id: &str) -> TransitionRequest {
        TransitionRequest {
            run_id: run.run_id,
            expected_revision: run.revision,
            owner: None,
            target,
            wait_reason: None,
            outcome: target.is_terminal().then(|| {
                RunOutcome::status(match target {
                    RunLifecycle::Cancelled => OutcomeStatus::Cancelled,
                    _ => OutcomeStatus::Succeeded,
                })
            }),
            event_type: "test".to_owned(),
            event: RunEventPayload::empty(),
            operation_id: operation_id.to_owned(),
            progress: false,
        }
    }

    #[test]
    fn unsupported_capability_is_typed_and_does_not_call_adapter() {
        let (_temp, service, adapter, run) = service(RunCapabilities::default());
        let result = smol::block_on(service.request_pause(PauseRunRequest {
            run_id: run.run_id,
            expected_revision: run.revision,
        }));
        assert!(matches!(
            result,
            Err(RunStoreError::UnsupportedCapability("pause"))
        ));
        assert!(adapter.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn pause_and_cancel_remain_requests_until_adapter_events_acknowledge() {
        let capabilities = RunCapabilities {
            cancel: true,
            pause: true,
            ..RunCapabilities::default()
        };
        let (_temp, service, adapter, queued) = service(capabilities);
        let starting = smol::block_on(service.transition(transition(
            &queued,
            RunLifecycle::Starting,
            "starting",
        )))
        .unwrap();
        let running = smol::block_on(service.transition(transition(
            &starting,
            RunLifecycle::Running,
            "running",
        )))
        .unwrap();
        let paused_request = smol::block_on(service.request_pause(PauseRunRequest {
            run_id: running.run_id,
            expected_revision: running.revision,
        }))
        .unwrap();
        assert_eq!(paused_request.lifecycle, RunLifecycle::Running);
        let cancelled_request = smol::block_on(service.request_cancel(CancelRunRequest {
            run_id: running.run_id,
            expected_revision: running.revision,
            reason: Some("operator request".to_owned()),
        }))
        .unwrap();
        assert_eq!(cancelled_request.lifecycle, RunLifecycle::Running);
        assert_eq!(*adapter.calls.lock().unwrap(), ["pause", "cancel"]);
    }

    #[test]
    fn resume_paused_keeps_run_id_but_terminal_resume_creates_linked_attempt() {
        let capabilities = RunCapabilities {
            resume: true,
            ..RunCapabilities::default()
        };
        let (_temp, service, adapter, queued) = service(capabilities);
        let starting = smol::block_on(service.transition(transition(
            &queued,
            RunLifecycle::Starting,
            "starting",
        )))
        .unwrap();
        let running = smol::block_on(service.transition(transition(
            &starting,
            RunLifecycle::Running,
            "running",
        )))
        .unwrap();
        let pausing = smol::block_on(service.transition(transition(
            &running,
            RunLifecycle::Pausing,
            "pausing",
        )))
        .unwrap();
        let paused = smol::block_on(service.transition(transition(
            &pausing,
            RunLifecycle::Paused,
            "paused",
        )))
        .unwrap();
        let same = smol::block_on(service.resume(ResumeRunControlRequest {
            run_id: paused.run_id,
            expected_revision: paused.revision,
            owner_instance_id: None,
            operation_id: "resume-paused".to_owned(),
            guidance: None,
        }))
        .unwrap();
        assert_eq!(same.run_id, paused.run_id);

        let running_again = smol::block_on(service.transition(transition(
            &paused,
            RunLifecycle::Running,
            "running-again",
        )))
        .unwrap();
        let terminal = smol::block_on(service.transition(transition(
            &running_again,
            RunLifecycle::Succeeded,
            "succeeded",
        )))
        .unwrap();
        let next = smol::block_on(service.resume(ResumeRunControlRequest {
            run_id: terminal.run_id,
            expected_revision: terminal.revision,
            owner_instance_id: None,
            operation_id: "resume-terminal".to_owned(),
            guidance: Some("continue".to_owned()),
        }))
        .unwrap();
        assert_ne!(next.run_id, terminal.run_id);
        assert_eq!(next.chain_id, terminal.chain_id);
        assert_eq!(next.predecessor_run_id, Some(terminal.run_id));
        assert_eq!(
            *adapter.calls.lock().unwrap(),
            ["resume_paused", "resume_from"]
        );
    }

    #[test]
    fn dispatcher_reconciles_every_parent_delivery_crash_boundary() {
        let temp = TempDir::new().unwrap();
        let store = RunStore::open_path(
            temp.path().join("runs.sqlite3"),
            crate::ProjectKey::new("/project").unwrap(),
            Duration::from_millis(50),
        )
        .unwrap();
        let service = RunService::new(store);
        let mut spec = NewRunSpec::new(RunKind::Task, ExecutionBackend::TuiSession, "child");
        spec.parent_session_id = Some("parent".to_owned());
        let queued = smol::block_on(service.create_run(spec)).unwrap();
        smol::block_on(service.transition(transition(
            &queued,
            RunLifecycle::Cancelled,
            "cancelled",
        )))
        .unwrap();

        let inbox = FakeInbox::default();
        inbox.fail_after_insert.store(true, Ordering::SeqCst);
        let first = smol::block_on(service.dispatch_parent_outbox(&inbox, 10, 5, 10)).unwrap();
        assert_eq!(first.retried, 1);
        assert_eq!(inbox.logical_insertions.lock().unwrap().len(), 1);

        let second = smol::block_on(service.dispatch_parent_outbox(&inbox, 15, 5, 10)).unwrap();
        assert_eq!(second.delivered, 1);
        assert_eq!(inbox.logical_insertions.lock().unwrap().len(), 1);
        assert!(
            service
                .store()
                .pending_outbox(i64::MAX, 10)
                .unwrap()
                .is_empty()
        );

        inbox.consumed.store(true, Ordering::SeqCst);
        let third = smol::block_on(service.dispatch_parent_outbox(&inbox, 16, 5, 10)).unwrap();
        assert_eq!(third.delivered, 1);
        assert!(
            service
                .store()
                .dispatchable_outbox(i64::MAX, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn dispatcher_dead_letters_permanently_missing_parent() {
        let temp = TempDir::new().unwrap();
        let store = RunStore::open_path(
            temp.path().join("runs.sqlite3"),
            crate::ProjectKey::new("/project").unwrap(),
            Duration::from_millis(50),
        )
        .unwrap();
        let service = RunService::new(store);
        let mut spec = NewRunSpec::new(RunKind::Task, ExecutionBackend::TuiSession, "child");
        spec.parent_session_id = Some("deleted-parent".to_owned());
        let queued = smol::block_on(service.create_run(spec)).unwrap();
        smol::block_on(service.transition(transition(
            &queued,
            RunLifecycle::Cancelled,
            "cancelled-deleted",
        )))
        .unwrap();

        let inbox = FakeInbox::default();
        inbox.unavailable.store(true, Ordering::SeqCst);
        let report = smol::block_on(service.dispatch_parent_outbox(&inbox, 10, 5, 10)).unwrap();
        assert_eq!(report.dead_lettered, 1);
        assert!(
            service
                .store()
                .pending_outbox(i64::MAX, 10)
                .unwrap()
                .is_empty()
        );
    }
}
