use n00n_storage::id::n00nId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::BTreeMap, fmt, path::Path, str::FromStr};
use uuid::Uuid;

pub const MAX_SUMMARY_BYTES: usize = 4_096;
pub const MAX_PREVIEW_BYTES: usize = 16_384;
pub const MAX_EVENT_DETAIL_ENTRIES: usize = 32;
pub const MAX_EVENT_DETAIL_BYTES: usize = 8_192;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub fn generate() -> Self {
                Self(Uuid::from_bytes(*n00nId::generate().as_bytes()))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

uuid_id!(RunChainId);
uuid_id!(RunId);
uuid_id!(RunEventId);
uuid_id!(DeliveryId);
uuid_id!(HostInstanceId);

impl RunChainId {
    #[must_use]
    pub(crate) fn deterministic(namespace: &Uuid, name: &[u8]) -> Self {
        Self(Uuid::new_v5(namespace, name))
    }
}

impl RunId {
    #[must_use]
    pub(crate) fn deterministic(namespace: &Uuid, name: &[u8]) -> Self {
        Self(Uuid::new_v5(namespace, name))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectKey(String);

impl ProjectKey {
    /// Creates a project key from an already canonical, trusted project identity.
    ///
    /// # Errors
    /// Returns an error when the identity is empty or contains a NUL byte.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.trim().is_empty() || value.contains('\0') {
            return Err(DomainError::InvalidProjectKey);
        }
        Ok(Self(value))
    }

    /// Canonicalizes a project path before using it as an immutable project key.
    ///
    /// # Errors
    /// Returns an error when the path cannot be canonicalized or represented as UTF-8.
    pub fn from_path(path: &Path) -> Result<Self, DomainError> {
        let canonical = path
            .canonicalize()
            .map_err(|error| DomainError::ProjectPath(error.to_string()))?;
        let value = canonical.to_str().ok_or(DomainError::NonUtf8ProjectPath)?;
        Self::new(value)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Task,
    Team,
    Workflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackend {
    TuiSession,
    WorkerProcess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunLifecycle {
    Queued,
    Starting,
    Running,
    WaitingInput,
    Blocked,
    Pausing,
    Paused,
    Cancelling,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    Interrupted,
    Lost,
}

impl RunLifecycle {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::TimedOut
                | Self::Cancelled
                | Self::Interrupted
                | Self::Lost
        )
    }

    #[must_use]
    pub fn can_transition_to(self, target: Self) -> bool {
        if self.is_terminal() || self == target {
            return false;
        }
        if matches!(target, Self::Interrupted | Self::Lost) {
            return true;
        }
        match self {
            Self::Queued => matches!(
                target,
                Self::Starting | Self::Cancelling | Self::Cancelled | Self::TimedOut
            ),
            Self::Starting => matches!(
                target,
                Self::Running | Self::Cancelling | Self::Failed | Self::TimedOut
            ),
            Self::Running => matches!(
                target,
                Self::WaitingInput
                    | Self::Blocked
                    | Self::Pausing
                    | Self::Cancelling
                    | Self::Succeeded
                    | Self::Failed
                    | Self::TimedOut
            ),
            Self::WaitingInput | Self::Blocked | Self::Paused => {
                matches!(target, Self::Running | Self::Cancelling)
            }
            Self::Pausing => matches!(
                target,
                Self::Paused | Self::Running | Self::Failed | Self::Cancelling
            ),
            Self::Cancelling => {
                matches!(target, Self::Cancelled | Self::TimedOut | Self::Failed)
            }
            Self::Succeeded
            | Self::Failed
            | Self::TimedOut
            | Self::Cancelled
            | Self::Interrupted
            | Self::Lost => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReasonCode {
    UserInput,
    Permission,
    Authentication,
    Dependency,
    Policy,
    Declared,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitReason {
    pub code: WaitReasonCode,
    pub summary: String,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCapabilities {
    pub send: bool,
    pub answer: bool,
    pub cancel: bool,
    pub pause: bool,
    pub resume: bool,
    pub events: bool,
    pub logs: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    Interrupted,
    Lost,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutput {
    pub kind: String,
    pub reference: Option<String>,
    pub bounded_preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFailure {
    pub code: String,
    pub message: String,
    pub source: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunUsage {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunVerification {
    pub status: String,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub status: OutcomeStatus,
    pub summary: Option<String>,
    pub output: Option<RunOutput>,
    pub error: Option<RunFailure>,
    pub stop_reason: Option<String>,
    pub usage: Option<RunUsage>,
    pub cost: Option<f64>,
    pub cleanup_error: Option<RunFailure>,
    pub verification: Option<RunVerification>,
}

impl RunOutcome {
    #[must_use]
    pub fn status(status: OutcomeStatus) -> Self {
        Self {
            status,
            summary: None,
            output: None,
            error: None,
            stop_reason: None,
            usage: None,
            cost: None,
            cleanup_error: None,
            verification: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEventPayload {
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}

impl RunEventPayload {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            summary: None,
            details: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunChain {
    pub chain_id: RunChainId,
    pub project_key: ProjectKey,
    pub kind: RunKind,
    pub created_at: i64,
    pub root_session_id: Option<String>,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub chain_id: RunChainId,
    pub predecessor_run_id: Option<RunId>,
    pub backend: ExecutionBackend,
    pub session_id: Option<String>,
    pub legacy_session_id: Option<String>,
    pub workflow_journal_id: Option<String>,
    pub parent_run_id: Option<RunId>,
    pub parent_session_id: Option<String>,
    pub lifecycle: RunLifecycle,
    pub wait_reason: Option<WaitReason>,
    pub outcome: Option<RunOutcome>,
    pub capabilities: RunCapabilities,
    pub owner_instance_id: Option<HostInstanceId>,
    pub owner_epoch: Option<u64>,
    pub created_at: i64,
    pub queued_at: Option<i64>,
    pub started_at: Option<i64>,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
    pub last_progress_at: Option<i64>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvent {
    pub event_id: RunEventId,
    pub run_id: RunId,
    pub revision: u64,
    pub event_type: String,
    pub payload: RunEventPayload,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxState {
    Pending,
    Delivered,
    Acknowledged,
    DeadLetter,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentOutboxRecord {
    pub delivery_id: DeliveryId,
    pub source_event_id: RunEventId,
    pub child_run_id: RunId,
    pub parent_session_id: String,
    pub payload: RunEventPayload,
    pub state: OutboxState,
    pub attempt_count: u32,
    pub next_attempt_at: Option<i64>,
    pub created_at: i64,
    pub delivered_at: Option<i64>,
    pub acknowledged_at: Option<i64>,
    pub dead_letter_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_marker: String,
    pub lock_identity: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInstance {
    pub instance_id: HostInstanceId,
    pub process_identity: ProcessIdentity,
    pub started_at: i64,
    pub heartbeat_at: i64,
    pub shutdown_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerFence {
    pub instance_id: HostInstanceId,
    pub epoch: u64,
}

#[derive(Clone, Debug)]
pub struct NewRunSpec {
    pub chain_id: RunChainId,
    pub run_id: RunId,
    pub kind: RunKind,
    pub title: String,
    pub root_session_id: Option<String>,
    pub backend: ExecutionBackend,
    pub session_id: Option<String>,
    pub workflow_journal_id: Option<String>,
    pub parent_run_id: Option<RunId>,
    pub parent_session_id: Option<String>,
    pub capabilities: RunCapabilities,
    pub owner_instance_id: Option<HostInstanceId>,
}

impl NewRunSpec {
    #[must_use]
    pub fn new(kind: RunKind, backend: ExecutionBackend, title: impl Into<String>) -> Self {
        Self {
            chain_id: RunChainId::generate(),
            run_id: RunId::generate(),
            kind,
            title: title.into(),
            root_session_id: None,
            backend,
            session_id: None,
            workflow_journal_id: None,
            parent_run_id: None,
            parent_session_id: None,
            capabilities: RunCapabilities::default(),
            owner_instance_id: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransitionRequest {
    pub run_id: RunId,
    pub expected_revision: u64,
    pub owner: Option<OwnerFence>,
    pub target: RunLifecycle,
    pub wait_reason: Option<WaitReason>,
    pub outcome: Option<RunOutcome>,
    pub event_type: String,
    pub event: RunEventPayload,
    pub operation_id: String,
    pub progress: bool,
}

#[derive(Clone, Debug)]
pub struct ResumeRequest {
    pub prior_run_id: RunId,
    pub expected_revision: u64,
    pub owner_instance_id: Option<HostInstanceId>,
    pub operation_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunWaitResult {
    pub run: RunRecord,
    pub observation_timed_out: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostLiveness {
    Live,
    Gone,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcilePolicy {
    pub stale_before: i64,
    pub max_hosts: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub interrupted: Vec<RunId>,
    pub live_owners: usize,
    pub unverified_owners: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub terminal_before: i64,
    pub finalized_outbox_before: i64,
    pub shutdown_host_before: i64,
    pub max_rows: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub outbox_rows: usize,
    pub runs: usize,
    pub chains: usize,
    pub hosts: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("project key must be non-empty and contain no NUL bytes")]
    InvalidProjectKey,
    #[error("failed to canonicalize project path: {0}")]
    ProjectPath(String),
    #[error("canonical project path is not valid UTF-8")]
    NonUtf8ProjectPath,
    #[error("invalid transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: RunLifecycle,
        to: RunLifecycle,
    },
    #[error("{field} exceeds its {maximum}-byte limit")]
    TextTooLong { field: &'static str, maximum: usize },
    #[error("event contains more than {MAX_EVENT_DETAIL_ENTRIES} detail entries")]
    TooManyEventDetails,
    #[error("event details exceed their {MAX_EVENT_DETAIL_BYTES}-byte limit")]
    EventDetailsTooLarge,
    #[error("wait reason is required only for waiting_input or blocked")]
    InvalidWaitReason,
    #[error("terminal transitions require an outcome with the matching status")]
    InvalidOutcome,
}

pub(crate) fn validate_transition(
    from: RunLifecycle,
    target: RunLifecycle,
    wait_reason: Option<&WaitReason>,
    outcome: Option<&RunOutcome>,
    event: &RunEventPayload,
) -> Result<(), DomainError> {
    if !from.can_transition_to(target) {
        return Err(DomainError::InvalidTransition { from, to: target });
    }
    let expects_reason = matches!(target, RunLifecycle::WaitingInput | RunLifecycle::Blocked);
    if expects_reason != wait_reason.is_some() {
        return Err(DomainError::InvalidWaitReason);
    }
    if let Some(reason) = wait_reason {
        validate_text("wait reason summary", &reason.summary, MAX_SUMMARY_BYTES)?;
    }
    validate_event(event)?;
    if target.is_terminal() {
        let value = outcome.ok_or(DomainError::InvalidOutcome)?;
        if !outcome_matches(target, value.status) {
            return Err(DomainError::InvalidOutcome);
        }
        validate_outcome(value)
    } else if outcome.is_some() {
        Err(DomainError::InvalidOutcome)
    } else {
        Ok(())
    }
}

pub(crate) fn validate_event(event: &RunEventPayload) -> Result<(), DomainError> {
    if let Some(summary) = &event.summary {
        validate_text("event summary", summary, MAX_SUMMARY_BYTES)?;
    }
    if event.details.len() > MAX_EVENT_DETAIL_ENTRIES {
        return Err(DomainError::TooManyEventDetails);
    }
    let bytes = event
        .details
        .iter()
        .map(|(key, value)| key.len() + value.len())
        .sum::<usize>();
    if bytes > MAX_EVENT_DETAIL_BYTES {
        return Err(DomainError::EventDetailsTooLarge);
    }
    Ok(())
}

fn validate_outcome(outcome: &RunOutcome) -> Result<(), DomainError> {
    if let Some(summary) = &outcome.summary {
        validate_text("outcome summary", summary, MAX_SUMMARY_BYTES)?;
    }
    if let Some(output) = &outcome.output
        && let Some(preview) = &output.bounded_preview
    {
        validate_text("output preview", preview, MAX_PREVIEW_BYTES)?;
    }
    for failure in [&outcome.error, &outcome.cleanup_error]
        .into_iter()
        .flatten()
    {
        validate_text("error message", &failure.message, MAX_SUMMARY_BYTES)?;
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), DomainError> {
    if value.len() > maximum {
        return Err(DomainError::TextTooLong { field, maximum });
    }
    Ok(())
}

fn outcome_matches(lifecycle: RunLifecycle, status: OutcomeStatus) -> bool {
    matches!(
        (lifecycle, status),
        (RunLifecycle::Succeeded, OutcomeStatus::Succeeded)
            | (RunLifecycle::Failed, OutcomeStatus::Failed)
            | (RunLifecycle::TimedOut, OutcomeStatus::TimedOut)
            | (RunLifecycle::Cancelled, OutcomeStatus::Cancelled)
            | (
                RunLifecycle::Interrupted,
                OutcomeStatus::Interrupted | OutcomeStatus::Unknown
            )
            | (RunLifecycle::Lost, OutcomeStatus::Lost)
    )
}

#[cfg(test)]
mod tests {
    use super::{ExecutionBackend, RunLifecycle};
    use test_case::test_case;

    const ALL: [RunLifecycle; 14] = [
        RunLifecycle::Queued,
        RunLifecycle::Starting,
        RunLifecycle::Running,
        RunLifecycle::WaitingInput,
        RunLifecycle::Blocked,
        RunLifecycle::Pausing,
        RunLifecycle::Paused,
        RunLifecycle::Cancelling,
        RunLifecycle::Succeeded,
        RunLifecycle::Failed,
        RunLifecycle::TimedOut,
        RunLifecycle::Cancelled,
        RunLifecycle::Interrupted,
        RunLifecycle::Lost,
    ];

    fn expected(from: RunLifecycle, to: RunLifecycle) -> bool {
        if from.is_terminal() || from == to {
            return false;
        }
        if matches!(to, RunLifecycle::Interrupted | RunLifecycle::Lost) {
            return true;
        }
        match from {
            RunLifecycle::Queued => matches!(
                to,
                RunLifecycle::Starting
                    | RunLifecycle::Cancelling
                    | RunLifecycle::Cancelled
                    | RunLifecycle::TimedOut
            ),
            RunLifecycle::Starting => matches!(
                to,
                RunLifecycle::Running
                    | RunLifecycle::Cancelling
                    | RunLifecycle::Failed
                    | RunLifecycle::TimedOut
            ),
            RunLifecycle::Running => matches!(
                to,
                RunLifecycle::WaitingInput
                    | RunLifecycle::Blocked
                    | RunLifecycle::Pausing
                    | RunLifecycle::Cancelling
                    | RunLifecycle::Succeeded
                    | RunLifecycle::Failed
                    | RunLifecycle::TimedOut
            ),
            RunLifecycle::WaitingInput | RunLifecycle::Blocked | RunLifecycle::Paused => {
                matches!(to, RunLifecycle::Running | RunLifecycle::Cancelling)
            }
            RunLifecycle::Pausing => matches!(
                to,
                RunLifecycle::Paused
                    | RunLifecycle::Running
                    | RunLifecycle::Failed
                    | RunLifecycle::Cancelling
            ),
            RunLifecycle::Cancelling => matches!(
                to,
                RunLifecycle::Cancelled | RunLifecycle::TimedOut | RunLifecycle::Failed
            ),
            RunLifecycle::Succeeded
            | RunLifecycle::Failed
            | RunLifecycle::TimedOut
            | RunLifecycle::Cancelled
            | RunLifecycle::Interrupted
            | RunLifecycle::Lost => false,
        }
    }

    #[test]
    fn every_legal_and_illegal_transition_matches_contract() {
        for from in ALL {
            for to in ALL {
                assert_eq!(
                    from.can_transition_to(to),
                    expected(from, to),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    #[test_case(RunLifecycle::Succeeded)]
    #[test_case(RunLifecycle::Failed)]
    #[test_case(RunLifecycle::TimedOut)]
    #[test_case(RunLifecycle::Cancelled)]
    #[test_case(RunLifecycle::Interrupted)]
    #[test_case(RunLifecycle::Lost)]
    fn terminal_states_are_immutable(terminal: RunLifecycle) {
        for target in ALL {
            assert!(!terminal.can_transition_to(target));
        }
    }

    #[test]
    fn workflow_is_a_run_kind_not_an_execution_backend() {
        assert!(serde_json::from_str::<ExecutionBackend>("\"workflow\"").is_err());
        assert_eq!(
            serde_json::from_str::<ExecutionBackend>("\"tui_session\"").unwrap(),
            ExecutionBackend::TuiSession
        );
        assert_eq!(
            serde_json::from_str::<ExecutionBackend>("\"worker_process\"").unwrap(),
            ExecutionBackend::WorkerProcess
        );
    }
}
