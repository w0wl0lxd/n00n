//! Cancellation-safe admission for work that can fan out or hold process resources.

use async_lock::{Semaphore, SemaphoreGuardArc};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use n00n_config::canonical_tool_name;

use crate::cancel::CancelToken;

pub const DEFAULT_MAX_CONCURRENT_TOOLS: usize = 8;
pub const DEFAULT_MAX_CONCURRENT_CHEAP_TOOLS: usize = 32;
pub const DEFAULT_MAX_CONCURRENT_AGENT_TOOLS: usize = 4;

const ORCHESTRATOR_TOOLS: &[&str] = &[
    "control_agent",
    "run_batch",
    "run_task",
    "run_team",
    "run_workflow",
];
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
const CHEAP_TOOL_KINDS: &[&str] = &["cheap", "read", "metadata", "search"];
const ORCHESTRATOR_TOOL_KINDS: &[&str] = &["orchestrator", "fanout"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAdmissionClass {
    /// A bounded, low-cost read. Cheap calls have their own wider lane and do
    /// not consume a slot from the expensive work budget.
    Cheap,
    /// Work that may invoke a process, network request, interpreter, or MCP.
    Standard,
    /// A wrapper that is expected to acquire admission for its children.
    /// Wrappers themselves do not hold a slot, which avoids parent-child
    /// deadlocks when the process budget is full.
    Orchestrator,
}

impl ToolAdmissionClass {
    #[must_use]
    pub fn from_workload(value: &str) -> Option<Self> {
        match value {
            "cheap" => Some(Self::Cheap),
            "standard" | "expensive" => Some(Self::Standard),
            "orchestrator" | "fanout" => Some(Self::Orchestrator),
            _ => None,
        }
    }

    #[must_use]
    pub fn for_tool(name: &str, kind: Option<&str>) -> Self {
        let canonical = canonical_tool_name(name);
        if ORCHESTRATOR_TOOLS.contains(&canonical)
            || kind.is_some_and(|k| ORCHESTRATOR_TOOL_KINDS.contains(&k))
        {
            return Self::Orchestrator;
        }
        if matches!(
            canonical,
            "read_file" | "search_files" | "search_code" | "view_image" | "search_tools"
        ) || kind.is_some_and(|k| CHEAP_TOOL_KINDS.contains(&k))
        {
            return Self::Cheap;
        }
        Self::Standard
    }

    #[must_use]
    pub const fn is_orchestrator(self) -> bool {
        matches!(self, Self::Orchestrator)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AdmissionError {
    #[error("tool admission cancelled")]
    Cancelled,
}

struct AgentSlot {
    semaphore: Arc<Semaphore>,
    users: AtomicUsize,
}

struct AdmissionState {
    agents: Mutex<HashMap<String, Arc<AgentSlot>>>,
}

/// A registry-owned admission controller. The process and per-agent lanes are
/// independent from Lua's user-facing semaphores, so every dispatch path is
/// covered even when a plugin forgets to opt into a Lua semaphore.
pub struct ToolAdmission {
    process: Arc<Semaphore>,
    cheap: Arc<Semaphore>,
    agent_limit: usize,
    state: AdmissionState,
    process_active: AtomicUsize,
    cheap_active: AtomicUsize,
}

impl fmt::Debug for ToolAdmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolAdmission")
            .field("agent_limit", &self.agent_limit)
            .field(
                "process_active",
                &self.process_active.load(Ordering::Relaxed),
            )
            .field("cheap_active", &self.cheap_active.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Default for ToolAdmission {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolAdmission {
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(
            DEFAULT_MAX_CONCURRENT_TOOLS,
            DEFAULT_MAX_CONCURRENT_AGENT_TOOLS,
            DEFAULT_MAX_CONCURRENT_CHEAP_TOOLS,
        )
    }

    #[must_use]
    pub fn new_scope() -> Arc<str> {
        Arc::from(format!(
            "agent-{}",
            NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[must_use]
    pub fn with_limits(process_limit: usize, agent_limit: usize, cheap_limit: usize) -> Self {
        Self {
            process: Arc::new(Semaphore::new(process_limit.max(1))),
            cheap: Arc::new(Semaphore::new(cheap_limit.max(1))),
            agent_limit: agent_limit.max(1),
            state: AdmissionState {
                agents: Mutex::new(HashMap::new()),
            },
            process_active: AtomicUsize::new(0),
            cheap_active: AtomicUsize::new(0),
        }
    }

    /// Wait for admission while allowing cancellation to remove the waiter.
    /// All guards release on drop, including when the tool future panics.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError::Cancelled`] when cancellation wins the race
    /// while waiting for a permit.
    pub async fn acquire(
        &self,
        scope: &str,
        class: ToolAdmissionClass,
        cancel: &CancelToken,
    ) -> Result<ToolAdmissionGuard<'_>, AdmissionError> {
        if class.is_orchestrator() {
            return Ok(ToolAdmissionGuard::empty());
        }

        if matches!(class, ToolAdmissionClass::Cheap) {
            let permit = cancel
                .race(self.cheap.acquire_arc())
                .await
                .map_err(|_| AdmissionError::Cancelled)?;
            self.cheap_active.fetch_add(1, Ordering::Relaxed);
            return Ok(ToolAdmissionGuard {
                _process: None,
                _cheap: Some(ActivePermit {
                    guard: permit,
                    active: &self.cheap_active,
                }),
                agent: None,
                state: None,
                scope: None,
            });
        }

        let process = ActivePermit {
            guard: cancel
                .race(self.process.acquire_arc())
                .await
                .map_err(|_| AdmissionError::Cancelled)?,
            active: &self.process_active,
        };
        self.process_active.fetch_add(1, Ordering::Relaxed);

        let scope = scope.to_owned();
        let agent = {
            let mut agents = self
                .state
                .agents
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let slot = Arc::clone(agents.entry(scope.clone()).or_insert_with(|| {
                Arc::new(AgentSlot {
                    semaphore: Arc::new(Semaphore::new(self.agent_limit)),
                    users: AtomicUsize::new(0),
                })
            }));
            slot.users.fetch_add(1, Ordering::Relaxed);
            slot
        };

        let Ok(agent_guard) = cancel.race(agent.semaphore.acquire_arc()).await else {
            self.release_agent(&scope, &agent);
            return Err(AdmissionError::Cancelled);
        };

        Ok(ToolAdmissionGuard {
            _process: Some(process),
            _cheap: None,
            agent: Some(agent_guard),
            state: Some(&self.state),
            scope: Some(scope),
        })
    }

    fn release_agent(&self, scope: &str, slot: &Arc<AgentSlot>) {
        let mut agents = self
            .state
            .agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !agents
            .get(scope)
            .is_some_and(|current| Arc::ptr_eq(current, slot))
        {
            return;
        }
        if slot.users.fetch_sub(1, Ordering::Relaxed) == 1 {
            agents.remove(scope);
        }
    }

    #[must_use]
    pub fn process_active(&self) -> usize {
        self.process_active.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn cheap_active(&self) -> usize {
        self.cheap_active.load(Ordering::Relaxed)
    }
}

struct ActivePermit<'a> {
    guard: SemaphoreGuardArc,
    active: &'a AtomicUsize,
}

impl Drop for ActivePermit<'_> {
    fn drop(&mut self) {
        let _ = &self.guard;
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct ToolAdmissionGuard<'a> {
    _process: Option<ActivePermit<'a>>,
    _cheap: Option<ActivePermit<'a>>,
    agent: Option<SemaphoreGuardArc>,
    state: Option<&'a AdmissionState>,
    scope: Option<String>,
}

impl ToolAdmissionGuard<'_> {
    fn empty() -> Self {
        Self {
            _process: None,
            _cheap: None,
            agent: None,
            state: None,
            scope: None,
        }
    }
}

impl Drop for ToolAdmissionGuard<'_> {
    fn drop(&mut self) {
        self.agent.take();
        let (Some(state), Some(scope)) = (self.state, self.scope.take()) else {
            return;
        };
        let mut agents = state
            .agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(slot) = agents.get(&scope).cloned() else {
            return;
        };
        if slot.users.fetch_sub(1, Ordering::Relaxed) == 1
            && agents
                .get(&scope)
                .is_some_and(|current| Arc::ptr_eq(current, &slot))
        {
            agents.remove(&scope);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_wrappers_without_consuming_expensive_lane() {
        assert_eq!(
            ToolAdmissionClass::for_tool("batch", Some("execute")),
            ToolAdmissionClass::Orchestrator
        );
        assert_eq!(
            ToolAdmissionClass::for_tool("read", Some("filesystem")),
            ToolAdmissionClass::Cheap
        );
        assert_eq!(
            ToolAdmissionClass::for_tool("webfetch", Some("network")),
            ToolAdmissionClass::Standard
        );
    }

    #[test]
    fn cancelled_wait_does_not_leak_process_or_agent_slot() {
        smol::block_on(async {
            let admission = Arc::new(ToolAdmission::with_limits(1, 1, 1));
            let cancel = CancelToken::none();
            let first = admission
                .acquire("agent", ToolAdmissionClass::Standard, &cancel)
                .await
                .expect("first permit");
            let (trigger, waiter_cancel) = CancelToken::new();
            let (started_tx, started_rx) = flume::bounded(1);
            let admission_for_waiter = Arc::clone(&admission);
            let waiter = smol::spawn(async move {
                started_tx
                    .send_async(())
                    .await
                    .expect("test receiver remains available");
                assert!(matches!(
                    admission_for_waiter
                        .acquire("agent", ToolAdmissionClass::Standard, &waiter_cancel)
                        .await,
                    Err(AdmissionError::Cancelled)
                ));
            });
            started_rx
                .recv_async()
                .await
                .expect("waiter reached admission");
            trigger.cancel();
            waiter.await;
            drop(first);
            assert_eq!(admission.process_active(), 0);
        });
    }

    #[test]
    fn cancelled_agent_wait_releases_process_permit() {
        smol::block_on(async {
            let admission = Arc::new(ToolAdmission::with_limits(2, 1, 1));
            let first = admission
                .acquire("agent", ToolAdmissionClass::Standard, &CancelToken::none())
                .await
                .expect("first permit");
            let (trigger, waiter_cancel) = CancelToken::new();
            let (started_tx, started_rx) = flume::bounded(1);
            let admission_for_waiter = Arc::clone(&admission);
            let waiter = smol::spawn(async move {
                started_tx
                    .send_async(())
                    .await
                    .expect("test receiver remains available");
                assert!(matches!(
                    admission_for_waiter
                        .acquire("agent", ToolAdmissionClass::Standard, &waiter_cancel)
                        .await,
                    Err(AdmissionError::Cancelled)
                ));
            });
            started_rx
                .recv_async()
                .await
                .expect("waiter reached admission");
            trigger.cancel();
            waiter.await;
            assert_eq!(admission.process_active(), 1);
            drop(first);
            assert_eq!(admission.process_active(), 0);
        });
    }

    #[test]
    fn permit_releases_when_work_returns_error() {
        smol::block_on(async {
            let admission = ToolAdmission::with_limits(1, 1, 1);
            let result: Result<(), AdmissionError> = async {
                let _permit = admission
                    .acquire("agent", ToolAdmissionClass::Standard, &CancelToken::none())
                    .await?;
                Err(AdmissionError::Cancelled)
            }
            .await;
            assert_eq!(result, Err(AdmissionError::Cancelled));
            assert_eq!(admission.process_active(), 0);
        });
    }

    #[test]
    fn permit_releases_when_work_panics() {
        let admission = Arc::new(ToolAdmission::with_limits(1, 1, 1));
        let admission_for_work = Arc::clone(&admission);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            smol::block_on(async move {
                let _permit = admission_for_work
                    .acquire("agent", ToolAdmissionClass::Standard, &CancelToken::none())
                    .await
                    .expect("permit");
                panic!("simulated tool panic");
            });
        }));
        assert!(result.is_err());
        assert_eq!(admission.process_active(), 0);
    }

    #[test]
    fn orchestrator_is_a_noop_guard() {
        smol::block_on(async {
            let admission = ToolAdmission::with_limits(1, 1, 1);
            let permit = admission
                .acquire(
                    "agent",
                    ToolAdmissionClass::Orchestrator,
                    &CancelToken::none(),
                )
                .await
                .expect("orchestrator bypass");
            assert_eq!(admission.process_active(), 0);
            drop(permit);
        });
    }
}
