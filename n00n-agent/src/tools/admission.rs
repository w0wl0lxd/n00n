use std::sync::Arc;
use std::time::Instant;

use async_lock::{Semaphore, SemaphoreGuardArc};
use thiserror::Error;
use tracing::debug;

use crate::cancel::CancelToken;

pub const DEFAULT_PROCESS_SLOTS: usize = 4;
pub const DEFAULT_AGENT_SLOTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolWorkload {
    Cheap,
    Process,
    Agent,
    Orchestrator,
}

impl ToolWorkload {
    #[must_use]
    pub fn parse_name(name: &str) -> Option<Self> {
        match name {
            "cheap" => Some(Self::Cheap),
            "process" => Some(Self::Process),
            "agent" => Some(Self::Agent),
            "orchestrator" => Some(Self::Orchestrator),
            _ => None,
        }
    }

    #[must_use]
    pub fn from_kind(kind: Option<&str>) -> Self {
        match kind {
            Some("execute" | "process") => Self::Process,
            Some("agent") => Self::Agent,
            Some("orchestrator") => Self::Orchestrator,
            _ => Self::Cheap,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cheap => "cheap",
            Self::Process => "process",
            Self::Agent => "agent",
            Self::Orchestrator => "orchestrator",
        }
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    #[error("cancelled")]
    Cancelled,
}

/// Shared admission control for all calls that use one tool registry.
///
/// Cheap calls and orchestration wrappers do not consume a permit. Their child
/// calls acquire the process or agent budget at the shared dispatch boundary.
pub struct ToolAdmission {
    process: Arc<Semaphore>,
    agents: Arc<Semaphore>,
}

pub struct AdmissionGuard {
    workload: ToolWorkload,
    _permit: Option<SemaphoreGuardArc>,
}

impl ToolAdmission {
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_PROCESS_SLOTS, DEFAULT_AGENT_SLOTS)
    }

    #[must_use]
    pub fn with_limits(process_slots: usize, agent_slots: usize) -> Self {
        Self {
            process: Arc::new(Semaphore::new(process_slots.max(1))),
            agents: Arc::new(Semaphore::new(agent_slots.max(1))),
        }
    }

    /// # Errors
    ///
    /// Returns [`AdmissionError::Cancelled`] when cancellation wins the permit race.
    pub async fn acquire(
        &self,
        workload: ToolWorkload,
        cancel: &CancelToken,
    ) -> Result<AdmissionGuard, AdmissionError> {
        let semaphore = match workload {
            ToolWorkload::Cheap | ToolWorkload::Orchestrator => {
                return Ok(AdmissionGuard::none(workload));
            }
            ToolWorkload::Process => Arc::clone(&self.process),
            ToolWorkload::Agent => Arc::clone(&self.agents),
        };
        let started = Instant::now();
        let permit = cancel
            .race(semaphore.acquire_arc())
            .await
            .map_err(|_| AdmissionError::Cancelled)?;
        debug!(
            workload = workload.as_str(),
            wait_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or_else(|_| u64::MAX),
            "tool admission granted"
        );
        Ok(AdmissionGuard {
            workload,
            _permit: Some(permit),
        })
    }

    /// # Errors
    ///
    /// Returns [`AdmissionError::Cancelled`] when cancellation wins the permit race.
    pub async fn acquire_agent(
        &self,
        cancel: &CancelToken,
    ) -> Result<AdmissionGuard, AdmissionError> {
        self.acquire(ToolWorkload::Agent, cancel).await
    }
}

impl Default for ToolAdmission {
    fn default() -> Self {
        Self::new()
    }
}

impl AdmissionGuard {
    fn none(workload: ToolWorkload) -> Self {
        Self {
            workload,
            _permit: None,
        }
    }

    #[must_use]
    pub const fn workload(&self) -> ToolWorkload {
        self.workload
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_defaults_to_process_only_for_execute_tools() {
        assert_eq!(
            ToolWorkload::from_kind(Some("execute")),
            ToolWorkload::Process
        );
        assert_eq!(ToolWorkload::from_kind(Some("read")), ToolWorkload::Cheap);
        assert_eq!(ToolWorkload::from_kind(None), ToolWorkload::Cheap);
    }

    #[test]
    fn explicit_names_are_strict() {
        assert_eq!(
            ToolWorkload::parse_name("process"),
            Some(ToolWorkload::Process)
        );
        assert_eq!(ToolWorkload::parse_name("PROCESS"), None);
        assert_eq!(ToolWorkload::parse_name("unknown"), None);
    }

    #[test]
    fn process_permit_blocks_until_release() {
        smol::block_on(async {
            let admission = ToolAdmission::with_limits(1, 1);
            let first = admission
                .acquire(ToolWorkload::Process, &CancelToken::none())
                .await
                .expect("first permit");
            let (trigger, cancel) = CancelToken::new();
            trigger.cancel();
            assert!(matches!(
                admission.acquire(ToolWorkload::Process, &cancel).await,
                Err(AdmissionError::Cancelled)
            ));
            drop(first);
            assert!(
                admission
                    .acquire(ToolWorkload::Process, &CancelToken::none())
                    .await
                    .is_ok()
            );
        });
    }

    #[test]
    fn agent_permit_blocks_until_release() {
        smol::block_on(async {
            let admission = Arc::new(ToolAdmission::with_limits(1, 1));
            let first = admission
                .acquire(ToolWorkload::Agent, &CancelToken::none())
                .await
                .expect("first agent permit");
            let waiter_admission = Arc::clone(&admission);
            let waiter = smol::spawn(async move {
                waiter_admission
                    .acquire(ToolWorkload::Agent, &CancelToken::none())
                    .await
            });
            smol::future::yield_now().await;
            assert!(!waiter.is_finished());
            drop(first);
            assert!(waiter.await.is_ok());
        });
    }

    #[test]
    fn cheap_and_orchestrator_calls_bypass_other_budgets() {
        smol::block_on(async {
            let admission = ToolAdmission::with_limits(1, 1);
            let process = admission
                .acquire(ToolWorkload::Process, &CancelToken::none())
                .await
                .expect("process permit");

            let cheap = admission
                .acquire(ToolWorkload::Cheap, &CancelToken::none())
                .await
                .expect("cheap call");
            let orchestrator = admission
                .acquire(ToolWorkload::Orchestrator, &CancelToken::none())
                .await
                .expect("orchestrator call");
            assert_eq!(cheap.workload(), ToolWorkload::Cheap);
            assert_eq!(orchestrator.workload(), ToolWorkload::Orchestrator);
            drop((cheap, orchestrator, process));
        });
    }

    #[test]
    fn agent_and_process_budgets_are_independent() {
        smol::block_on(async {
            let admission = ToolAdmission::with_limits(1, 1);
            let process = admission
                .acquire(ToolWorkload::Process, &CancelToken::none())
                .await
                .expect("process permit");
            let agent = admission
                .acquire(ToolWorkload::Agent, &CancelToken::none())
                .await
                .expect("agent permit");
            assert_eq!(agent.workload(), ToolWorkload::Agent);
            drop((process, agent));
        });
    }

    #[test]
    fn a_dropped_guard_releases_after_panic_unwind() {
        let admission = ToolAdmission::with_limits(1, 1);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard =
                smol::block_on(admission.acquire(ToolWorkload::Process, &CancelToken::none()))
                    .expect("process permit");
            panic!("test unwind");
        }));
        assert!(result.is_err());
        smol::block_on(async {
            assert!(
                admission
                    .acquire(ToolWorkload::Process, &CancelToken::none())
                    .await
                    .is_ok()
            );
        });
    }
}
