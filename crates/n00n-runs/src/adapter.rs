use crate::{ParentOutboxRecord, RunId, RunRecord};
use std::{future::Future, pin::Pin};

pub type AdapterFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, RunAdapterError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunDelivery {
    TurnEnd,
    Steering,
    Immediate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunProbe {
    pub live: bool,
    pub process_identity_verified: bool,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("adapter operation failed ({code}): {message}")]
pub struct RunAdapterError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParentInsertResult {
    Inserted,
    AlreadyPresent,
    AlreadyConsumed,
    PermanentUnavailable { reason: String },
}

pub trait ParentInboxAdapter: Send + Sync {
    fn insert<'a>(
        &'a self,
        delivery: &'a ParentOutboxRecord,
    ) -> AdapterFuture<'a, ParentInsertResult>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParentDispatchReport {
    pub delivered: usize,
    pub retried: usize,
    pub dead_lettered: usize,
}

pub trait RunAdapter: Send + Sync {
    fn send<'a>(
        &'a self,
        run: &'a RunRecord,
        text: &'a str,
        delivery: RunDelivery,
    ) -> AdapterFuture<'a, ()>;

    fn answer<'a>(&'a self, run: &'a RunRecord, response: &'a str) -> AdapterFuture<'a, ()>;

    fn request_cancel<'a>(
        &'a self,
        run: &'a RunRecord,
        reason: Option<&'a str>,
    ) -> AdapterFuture<'a, ()>;

    fn request_pause<'a>(&'a self, run: &'a RunRecord) -> AdapterFuture<'a, ()>;

    fn resume_paused<'a>(
        &'a self,
        run: &'a RunRecord,
        guidance: Option<&'a str>,
    ) -> AdapterFuture<'a, ()>;

    fn resume_from<'a>(
        &'a self,
        prior: &'a RunRecord,
        next: &'a RunRecord,
        guidance: Option<&'a str>,
    ) -> AdapterFuture<'a, ()>;

    fn probe<'a>(&'a self, run: &'a RunRecord) -> AdapterFuture<'a, RunProbe>;

    fn events<'a>(
        &'a self,
        _run: &'a RunRecord,
        _after_revision: u64,
        _limit: usize,
    ) -> AdapterFuture<'a, Vec<String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn logs<'a>(
        &'a self,
        _run: &'a RunRecord,
        _after: Option<&'a str>,
        _limit: usize,
    ) -> AdapterFuture<'a, Vec<String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[derive(Clone, Debug)]
pub struct SendRunRequest {
    pub run_id: RunId,
    pub expected_revision: u64,
    pub text: String,
    pub delivery: RunDelivery,
}

#[derive(Clone, Debug)]
pub struct AnswerRunRequest {
    pub run_id: RunId,
    pub expected_revision: u64,
    pub response: String,
}

#[derive(Clone, Debug)]
pub struct CancelRunRequest {
    pub run_id: RunId,
    pub expected_revision: u64,
    pub reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PauseRunRequest {
    pub run_id: RunId,
    pub expected_revision: u64,
}

#[derive(Clone, Debug)]
pub struct ResumeRunControlRequest {
    pub run_id: RunId,
    pub expected_revision: u64,
    pub owner_instance_id: Option<crate::HostInstanceId>,
    pub operation_id: String,
    pub guidance: Option<String>,
}
