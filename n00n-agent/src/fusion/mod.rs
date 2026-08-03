//! Beta Fusion orchestration: the lead plans and reviews while an isolated,
//! cheaper sidekick executes one bounded mechanical brief.

use n00n_providers::TokenUsage;
use serde::Serialize;
use thiserror::Error;

use crate::tools::ToolAudience;

pub(crate) const FUSION_DELEGATE_TOOL: &str = "fusion_delegate";
const RECENT_ERROR_ESCALATE_THRESHOLD: u32 = 2;
const SIDEKICK_FAILURE_ESCALATE_THRESHOLD: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionLane {
    #[default]
    Lead,
    Sidekick,
}

impl FusionLane {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Sidekick => "sidekick",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationKind {
    /// Trivial conversational input that does not need delegation policy.
    Bypass,
    /// Ambiguity, design, review, or serial debugging — keep on lead.
    LeadOnly,
    Delegate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionPhase {
    #[default]
    Planning,
    Executing,
    Reviewing,
    LeadFallback,
    Complete,
    Cancelled,
    Failed,
}

impl FusionPhase {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionFailure {
    ToolError,
    Timeout,
    ModelUnavailable,
    DelegateCancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionInvocationOrigin {
    Direct,
    Interpreter,
    Batch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FusionDispatchError {
    #[error("Fusion delegation is disabled")]
    Disabled,
    #[error("prompt is not eligible for Fusion delegation")]
    Ineligible,
    #[error("Fusion delegation requires the main-agent audience")]
    InvalidAudience,
    #[error("indirect Fusion delegation is not allowed")]
    IndirectInvocation,
    #[error("Fusion delegation has already been dispatched")]
    AlreadyDispatched,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FusionDispatchGuard {
    enabled: bool,
    classification: DelegationKind,
    audience: ToolAudience,
    dispatched: bool,
}

impl FusionDispatchGuard {
    #[must_use]
    pub const fn new(
        enabled: bool,
        classification: DelegationKind,
        audience: ToolAudience,
    ) -> Self {
        Self {
            enabled,
            classification,
            audience,
            dispatched: false,
        }
    }

    /// Authorize one direct delegation from the main agent.
    ///
    /// # Errors
    ///
    /// Returns an error when Fusion is disabled, policy does not delegate, the
    /// caller is not the main agent, invocation is indirect, or the guard was consumed.
    pub fn authorize(&mut self, origin: FusionInvocationOrigin) -> Result<(), FusionDispatchError> {
        if !self.enabled {
            return Err(FusionDispatchError::Disabled);
        }
        if self.classification != DelegationKind::Delegate {
            return Err(FusionDispatchError::Ineligible);
        }
        if self.audience != ToolAudience::MAIN {
            return Err(FusionDispatchError::InvalidAudience);
        }
        if origin != FusionInvocationOrigin::Direct {
            return Err(FusionDispatchError::IndirectInvocation);
        }
        if self.dispatched {
            return Err(FusionDispatchError::AlreadyDispatched);
        }
        self.dispatched = true;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("invalid Fusion transition from {from:?} to {to:?}")]
pub struct FusionTransitionError {
    from: FusionPhase,
    to: FusionPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionRoute {
    Stay(FusionLane),
    Switch(FusionLane),
    EscalateToLead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionPhase {
    #[default]
    Idle,
    Planning,
    Executing,
    Reviewing,
    LeadFallback,
    Complete,
    Cancelled,
    Failed,
}

impl FusionPhase {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionContinuation {
    Review,
    Fallback,
}

impl FusionContinuation {
    #[must_use]
    pub const fn prompt(self) -> &'static str {
        match self {
            Self::Review => {
                "Review the completed Fusion sidekick work now. Verify the result against the user request, inspect or test anything necessary, fix any issue yourself, then give the final answer. Do not delegate again."
            }
            Self::Fallback => {
                "The Fusion sidekick failed or was cancelled. Continue on the lead model now, complete the remaining work yourself, then give the final answer. Do not delegate again."
            }
        }
    }

    #[must_use]
    pub const fn phase(self) -> FusionPhase {
        match self {
            Self::Review => FusionPhase::Reviewing,
            Self::Fallback => FusionPhase::LeadFallback,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FusionUsageStats {
    pub lead_cost: f64,
    pub sidekick_cost: f64,
    pub lead_usage: TokenUsage,
    pub sidekick_usage: TokenUsage,
    pub delegation_count: u32,
    pub compact_count: u32,
    pub final_lane: FusionLane,
}

#[derive(Debug, Clone, Default)]
pub struct FusionState {
    phase: FusionPhase,
    /// Legacy lane state. The beta runtime remains lead-owned and does not consult it.
    pub lane: FusionLane,
    pub delegation_count: u32,
    pub sidekick_failures: u32,
    /// Legacy error counter retained for callers that implemented escalation policy.
    pub recent_tool_errors: u32,
    pub continuation_attempts: u32,
    pub compact_count: u32,
    pub lead_usage: TokenUsage,
    pub sidekick_usage: TokenUsage,
    pub lead_cost: f64,
    pub sidekick_cost: f64,
    phase: FusionPhase,
    review_count: u32,
    fallback_count: u32,
}

impl FusionState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Legacy constructor. New orchestration uses [`Self::new`].
    #[must_use]
    pub fn new_lead() -> Self {
        Self {
            lane: FusionLane::Lead,
            delegation_count: 0,
            sidekick_failures: 0,
            compact_count: 0,
            recent_tool_errors: 0,
            lead_usage: TokenUsage::default(),
            sidekick_usage: TokenUsage::default(),
            lead_cost: 0.0,
            sidekick_cost: 0.0,
            phase: FusionPhase::Planning,
            review_count: 0,
            fallback_count: 0,
        }
    }

    pub fn start_request(&mut self, decision: FusionRequestDecision) -> Option<FusionPhase> {
        if self.phase != FusionPhase::Idle {
            return None;
        }
        if decision != FusionRequestDecision::Delegate {
            self.phase = FusionPhase::Complete;
            return None;
        }
        self.phase = FusionPhase::Planning;
        Some(self.phase)
    }

    pub fn start_delegate(&mut self) -> Option<FusionPhase> {
        if self.phase != FusionPhase::Planning {
            return None;
        }
        self.phase = FusionPhase::Executing;
        Some(self.phase)
    }

    pub fn record_lead_usage(&mut self, usage: TokenUsage, cost: f64) {
        self.lead_usage += usage;
        self.lead_cost += cost;
    }

    /// Legacy lane-aware usage accounting, not used by the beta runtime.
    pub fn record_lane_usage(&mut self, lane: FusionLane, usage: TokenUsage, cost: f64) {
        match lane {
            FusionLane::Lead => self.record_lead_usage(usage, cost),
            FusionLane::Sidekick => {
                self.sidekick_usage += usage;
                self.sidekick_cost += cost;
            }
        }
    }

    #[must_use]
    pub fn usage_stats(&self) -> FusionUsageStats {
        FusionUsageStats {
            lead_cost: self.lead_cost,
            sidekick_cost: self.sidekick_cost,
            lead_usage: self.lead_usage,
            sidekick_usage: self.sidekick_usage,
            delegation_count: self.delegation_count,
            compact_count: self.compact_count,
            final_lane: self.lane,
        }
    }

    pub fn observe_tool_results(&mut self, results: &[crate::ToolDoneEvent]) {
        for done in results {
            if done.tool.as_ref() == FUSION_DELEGATE_TOOL {
                if let Some(telemetry) = done.output.telemetry() {
                    #[allow(clippy::manual_unwrap_or)]
                    let cost = match telemetry.cost {
                        Some(cost) => cost,
                        None => 0.0,
                    };
                    let usage = match telemetry.usage.as_ref() {
                        Some(usage) => tool_usage_to_token_usage(usage),
                        None => TokenUsage::default(),
                    };
                    self.record_lane_usage(FusionLane::Sidekick, usage, cost);
                }
                if done.is_error {
                    self.recent_tool_errors = self.recent_tool_errors.saturating_add(1);
                    self.record_sidekick_failure();
                } else {
                    self.delegation_count = self.delegation_count.saturating_add(1);
                }
                continue;
            }
            if done.is_error {
                has_error = true;
                self.recent_tool_errors = self.recent_tool_errors.saturating_add(1);
                self.sidekick_failures = self.sidekick_failures.saturating_add(1);
            }
            if let Some(telemetry) = done.output.telemetry() {
                if let Some(cost) = telemetry.cost {
                    self.sidekick_cost += cost;
                }
                if let Some(usage) = telemetry.usage.as_ref() {
                    self.sidekick_usage += tool_usage_to_token_usage(usage);
                }
            }
        }

        if !found_delegate {
            return None;
        }

        let continuation = if has_error {
            FusionContinuation::Fallback
        } else {
            FusionContinuation::Review
        };
        self.phase = continuation.phase();
        Some(continuation)
    }

    pub fn finish_continuation(&mut self) -> Option<FusionPhase> {
        if !matches!(
            self.phase,
            FusionPhase::Planning
                | FusionPhase::Executing
                | FusionPhase::Reviewing
                | FusionPhase::LeadFallback
        ) {
            return None;
        }
        self.phase = FusionPhase::Complete;
        Some(self.phase)
    }

    pub fn cancel(&mut self) -> Option<FusionPhase> {
        self.finish_with(FusionPhase::Cancelled)
    }

    pub fn fail(&mut self) -> Option<FusionPhase> {
        self.finish_with(FusionPhase::Failed)
    }

    pub fn record_delegation(&mut self) {
        self.delegation_count = self.delegation_count.saturating_add(1);
        self.lane = FusionLane::Lead;
    }

    pub fn record_sidekick_failure(&mut self) {
        self.sidekick_failures = self.sidekick_failures.saturating_add(1);
    }

    pub fn record_compact(&mut self) {
        self.compact_count = self.compact_count.saturating_add(1);
    }

    #[must_use]
    pub const fn phase(&self) -> FusionPhase {
        self.phase
    }

    #[must_use]
    pub const fn review_count(&self) -> u32 {
        self.review_count
    }

    #[must_use]
    pub const fn fallback_count(&self) -> u32 {
        self.fallback_count
    }

    /// Advance the one-way Fusion lifecycle.
    ///
    /// # Errors
    ///
    /// Returns an error for cycles, back edges, terminal-state transitions, or
    /// delegation attempts from a sidekick context.
    pub fn transition(&mut self, next: FusionPhase) -> Result<(), FusionTransitionError> {
        let allowed = matches!(
            (self.phase, next),
            (
                FusionPhase::Planning,
                FusionPhase::Executing | FusionPhase::Complete
            ) | (FusionPhase::Executing, FusionPhase::Reviewing)
                | (
                    FusionPhase::Reviewing | FusionPhase::LeadFallback,
                    FusionPhase::Complete
                )
        );
        if !allowed || (next == FusionPhase::Executing && self.lane != FusionLane::Lead) {
            return Err(FusionTransitionError {
                from: self.phase,
                to: next,
            });
        }

        if next == FusionPhase::Reviewing {
            self.review_count = self.review_count.saturating_add(1);
        }
        if matches!(next, FusionPhase::Reviewing | FusionPhase::Complete) {
            self.lane = FusionLane::Lead;
        }
        self.phase = next;
        Ok(())
    }

    /// Move a failed delegate into the single lead fallback turn.
    ///
    /// # Errors
    ///
    /// Returns an error unless a delegate is currently executing.
    pub fn delegate_failed(
        &mut self,
        _failure: FusionFailure,
    ) -> Result<(), FusionTransitionError> {
        if self.phase != FusionPhase::Executing {
            return Err(FusionTransitionError {
                from: self.phase,
                to: FusionPhase::LeadFallback,
            });
        }
        self.phase = FusionPhase::LeadFallback;
        self.lane = FusionLane::Lead;
        self.fallback_count = self.fallback_count.saturating_add(1);
        Ok(())
    }

    /// Mark the entire Fusion run cancelled.
    ///
    /// # Errors
    ///
    /// Returns an error when the lifecycle is already terminal.
    pub fn cancel(&mut self) -> Result<(), FusionTransitionError> {
        self.enter_terminal(FusionPhase::Cancelled)
    }

    /// Mark the Fusion run failed due to an unrecoverable lead error.
    ///
    /// # Errors
    ///
    /// Returns an error when the lifecycle is already terminal.
    pub fn fail(&mut self) -> Result<(), FusionTransitionError> {
        self.enter_terminal(FusionPhase::Failed)
    }

    fn enter_terminal(&mut self, terminal: FusionPhase) -> Result<(), FusionTransitionError> {
        if matches!(
            self.phase,
            FusionPhase::Complete | FusionPhase::Cancelled | FusionPhase::Failed
        ) {
            return Err(FusionTransitionError {
                from: self.phase,
                to: terminal,
            });
        }
        self.phase = terminal;
        self.lane = FusionLane::Lead;
        Ok(())
    }
}

/// Legacy lexical classifier retained for callers using the pre-beta policy.
#[must_use]
pub fn classify_delegation(prompt: &str) -> DelegationKind {
    const LEAD: &[&str] = &[
        "ambiguous",
        "unclear",
        "trade-off",
        "tradeoff",
        "architect",
        "design",
        "plan",
        "review",
        "approve",
        "decide",
        "judgment",
        "security",
        "vulnerab",
        "credential",
        "permission",
        "production",
        "delete",
        "database",
        "root cause",
        "serial debug",
        "debug chain",
        "commit",
        "merge",
        "definition of done",
        "constraints",
        "edge case",
        "interpret",
    ];
    const DELEGATE: &[&str] = &[
        "explore",
        "search",
        "grep",
        "find",
        "list",
        "read file",
        "implement",
        "write test",
        "add test",
        "fix lint",
        "format",
        "rename",
        "boilerplate",
        "run test",
        "cargo test",
        "cargo clippy",
        "update doc",
        "mechanical",
        "apply patch",
    ];

    let p = prompt.trim().to_ascii_lowercase();
    if p.is_empty() {
        return DelegationKind::LeadOnly;
    }
    if matches!(
        p.as_str(),
        "hello" | "hi" | "hey" | "what is the current status?" | "what is the current status"
    ) {
        return DelegationKind::Bypass;
    }

    let lead_hits = LEAD.iter().filter(|sig| p.contains(**sig)).count();
    let delegate_hits = DELEGATE.iter().filter(|sig| p.contains(**sig)).count();

    if lead_hits > 0 && delegate_hits == 0 {
        return DelegationKind::LeadOnly;
    }
    if delegate_hits > 0 && lead_hits == 0 {
        return DelegationKind::Delegate;
    }
    if delegate_hits > lead_hits {
        DelegationKind::Delegate
    } else {
        DelegationKind::LeadOnly
    }
}

/// Legacy compaction policy. The beta runtime intentionally never calls this.
#[must_use]
pub fn route_after_compact(
    state: &mut FusionState,
    compact_summary: &str,
    recent_tool_errors: u32,
) -> FusionRoute {
    state.record_compact();
    if state.sidekick_failures >= SIDEKICK_FAILURE_ESCALATE_THRESHOLD
        || recent_tool_errors >= RECENT_ERROR_ESCALATE_THRESHOLD
    {
        return FusionRoute::EscalateToLead;
    }

    let summary_kind = classify_delegation(compact_summary);
    let summary = compact_summary.to_ascii_lowercase();
    let requests_mutation = [
        "implement",
        "write test",
        "add test",
        "fix lint",
        "format",
        "rename",
        "boilerplate",
        "update doc",
        "apply patch",
    ]
    .iter()
    .any(|signal| summary.contains(signal));

    match (state.lane, summary_kind) {
        (FusionLane::Lead, DelegationKind::Delegate) if requests_mutation => {
            FusionRoute::Switch(FusionLane::Sidekick)
        }
        (FusionLane::Sidekick, DelegationKind::LeadOnly | DelegationKind::Bypass) => {
            FusionRoute::Switch(FusionLane::Lead)
        }
        (lane, _) => FusionRoute::Stay(lane),
    }
}

/// Decides whether a Fusion-enabled request should delegate at all. Lead-only
/// signals always win over mechanical signals, and short requests bypass Fusion.
#[must_use]
pub fn decide_request(prompt: &str) -> FusionRequestDecision {
    const DELEGATE: &[&str] = &[
        "explore",
        "search",
        "grep",
        "implement",
        "write test",
        "add test",
        "fix lint",
        "format",
        "rename",
        "boilerplate",
        "run test",
        "cargo test",
        "cargo clippy",
        "mechanical",
        "apply patch",
    ];

    let prompt = prompt.trim().to_ascii_lowercase();
    if prompt.is_empty() {
        return FusionRequestDecision::Bypass;
    }
    if contains_lead_only_signal(&prompt) {
        return FusionRequestDecision::LeadOnly;
    }
    if prompt.split_whitespace().count() <= TRIVIAL_REQUEST_MAX_WORDS {
        return FusionRequestDecision::Bypass;
    }
    if DELEGATE
        .iter()
        .any(|signal| contains_signal(&prompt, signal))
    {
        FusionRequestDecision::Delegate
    } else {
        FusionRequestDecision::LeadOnly
    }
}

pub(crate) fn contains_lead_only_signal(prompt: &str) -> bool {
    let prompt = prompt.to_ascii_lowercase();
    let normalized_prompt = normalize_word_separators(&prompt);
    contains_destructive_git_command(&prompt)
        || LEAD_ONLY_SIGNALS.iter().any(|signal| {
            contains_signal(&prompt, signal)
                || signal_has_multiple_words(signal)
                    && contains_signal(&normalized_prompt, &normalize_word_separators(signal))
        })
}

fn contains_destructive_git_command(prompt: &str) -> bool {
    let Ok(shell_words) = shell_words::split(prompt) else {
        warn!("fusion: shell-word parsing failed; keeping request on lead");
        return true;
    };
    let mut found_git = false;
    for shell_word in shell_words {
        for word in normalize_word_separators(&shell_word).split_whitespace() {
            if word == GIT_COMMAND {
                found_git = true;
            }
            if found_git && DESTRUCTIVE_GIT_SUBCOMMANDS.contains(&word) {
                return true;
            }
        }
    }
    false
}

fn signal_has_multiple_words(signal: &str) -> bool {
    signal
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .nth(1)
        .is_some()
}

fn normalize_word_separators(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut pending_separator = false;
    for character in value.chars() {
        if character.is_alphanumeric() {
            if pending_separator && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character);
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    normalized
}

fn contains_signal(prompt: &str, signal: &str) -> bool {
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    prompt.match_indices(signal).any(|(start, _)| {
        let end = start + signal.len();
        !prompt[..start].chars().next_back().is_some_and(is_word)
            && !prompt[end..].chars().next().is_some_and(is_word)
    })
}

fn tool_usage_to_token_usage(usage: &crate::ToolUsage) -> TokenUsage {
    TokenUsage {
        input: u64_to_u32_saturating(usage.fresh_input_tokens),
        output: u64_to_u32_saturating(usage.output_tokens),
        cache_creation: u64_to_u32_saturating(usage.cache_write_tokens),
        cache_read: u64_to_u32_saturating(usage.cache_read_tokens),
    }
}

fn u64_to_u32_saturating(value: u64) -> u32 {
    u32::try_from(value).unwrap_or_else(|_| u32::MAX)
}

#[must_use]
pub fn fusion_lead_system_append() -> &'static str {
    "Fusion mode is on. Stay on the lead model. Delegate one bounded mechanical task early via fusion_delegate with a spec-quality brief (goal, constraints, definition of done). Keep architecture, ambiguity, sensitive work, credentials, destructive actions, serial debugging, commits, merges, and final review on the lead."
}

/// Legacy sidekick prompt appendix retained for clients constructing prompts.
#[must_use]
pub fn fusion_sidekick_system_append() -> &'static str {
    "Fusion sidekick lane: execute the delegated brief efficiently. Prefer index/codegraph/arbor before broad reads. Return concise file:line evidence, test results, and a short summary — not full file dumps."
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use test_case::test_case;

    #[test_case("hello", "Bypass" ; "trivial greeting")]
    #[test_case("what is the current status?", "Bypass" ; "trivial status")]
    #[test_case("", "LeadOnly" ; "empty")]
    #[test_case("please handle this", "LeadOnly" ; "unknown")]
    #[test_case("grep the repo for TODO and list matches", "Delegate" ; "mechanical grep")]
    #[test_case("add boilerplate getters", "Delegate" ; "boilerplate")]
    #[test_case("run cargo test", "Delegate" ; "narrow tests")]
    #[test_case("fix lint in this module", "Delegate" ; "narrow lint")]
    #[test_case("design the auth architecture and trade-offs", "LeadOnly" ; "architecture")]
    #[test_case("plan the implementation", "LeadOnly" ; "planning")]
    #[test_case("review the PR", "LeadOnly" ; "review")]
    #[test_case("commit and merge this change", "LeadOnly" ; "commit merge")]
    #[test_case("perform a security audit", "LeadOnly" ; "security")]
    #[test_case("rotate these credentials", "LeadOnly" ; "credentials")]
    #[test_case("change production permissions", "LeadOnly" ; "permissions")]
    #[test_case("delete the customer database", "LeadOnly" ; "destructive")]
    #[test_case("debug this serial failure chain", "LeadOnly" ; "serial debug")]
    #[test_case("grep for credentials and rotate them", "LeadOnly" ; "mandatory lead signal wins")]
    fn classify_delegation_contract(prompt: &str, expected: &str) {
        assert_eq!(format!("{:?}", classify_delegation(prompt)), expected);
    }

    #[test]
    fn successful_delegate_schedules_one_review_without_looping() {
        let mut state = FusionState::new();
        assert_eq!(
            state.start_request(FusionRequestDecision::Delegate),
            Some(FusionPhase::Planning)
        );
        assert_eq!(state.start_delegate(), Some(FusionPhase::Executing));
        let done = crate::ToolDoneEvent {
            id: "1".into(),
            tool: Arc::from(FUSION_DELEGATE_TOOL),
            output: crate::ToolOutput::Plain("ok".into()),
            is_error: false,
            annotation: None,
            written_path: None,
        };

        assert_eq!(
            state.observe_tool_results(std::slice::from_ref(&done)),
            Some(FusionContinuation::Review)
        );
        assert_eq!(state.phase(), FusionPhase::Reviewing);
        assert!(state.needs_continuation());
        assert!(!state.begin_continuation(0));
        assert!(state.begin_continuation(2));
        assert_eq!(state.continuation_attempts, 1);
        assert!(state.begin_continuation(2));
        assert_eq!(state.continuation_attempts, 2);
        assert!(!state.begin_continuation(2));
        assert_eq!(state.observe_tool_results(&[done]), None);
        assert_eq!(state.finish_continuation(), Some(FusionPhase::Complete));
        assert!(!state.needs_continuation());
        assert_eq!(state.finish_continuation(), None);
    }

    #[test_case("sidekick failed", FusionContinuation::Fallback ; "generic")]
    #[test_case("sidekick timed out", FusionContinuation::Fallback ; "timeout")]
    #[test_case("model unavailable", FusionContinuation::Fallback ; "model unavailable")]
    #[test_case("sidekick cancelled", FusionContinuation::Fallback ; "delegate cancel")]
    fn delegate_errors_schedule_one_fallback(message: &str, expected: FusionContinuation) {
        let mut state = FusionState::new();
        state.start_request(FusionRequestDecision::Delegate);
        state.start_delegate();
        let done = crate::ToolDoneEvent {
            id: "1".into(),
            tool: Arc::from(FUSION_DELEGATE_TOOL),
            output: crate::ToolOutput::Plain(message.into()),
            is_error: true,
            annotation: None,
            written_path: None,
        };

        assert_eq!(
            state.observe_tool_results(std::slice::from_ref(&done)),
            Some(expected)
        );
        assert_eq!(state.observe_tool_results(&[done]), None);
        assert_eq!(state.phase(), FusionPhase::LeadFallback);
    }

    #[test]
    fn observe_tool_results_counts_errors_and_sidekick_failures() {
        let mut state = FusionState::new_lead();
        state.observe_tool_results(&[
            crate::ToolDoneEvent::error("1".into(), "fail"),
            crate::ToolDoneEvent {
                id: "2".into(),
                tool: Arc::from(FUSION_DELEGATE_TOOL),
                output: telemetry(0.25, 8),
                is_error: true,
                annotation: None,
                written_path: None,
            },
        ];
        assert_eq!(
            state.observe_tool_results(&results),
            Some(FusionContinuation::Fallback)
        );
        assert_eq!(state.delegation_count, 2);
        assert_eq!(state.sidekick_failures, 1);
        assert!((state.sidekick_cost - 0.37).abs() < f64::EPSILON);
        assert_eq!(state.sidekick_usage.input, 18);
    }

    #[test]
    fn observe_successful_fusion_delegate_updates_sidekick_stats() {
        let mut state = FusionState::new();
        state.start_request(FusionRequestDecision::Delegate);
        state.start_delegate();
        let telemetry = crate::ToolTelemetry::try_new(
            Some(0.12),
            Some(crate::ToolUsage::try_new(10, 2, 1, 13, 5).expect("conserving tool usage")),
        )
        .expect("valid telemetry")
        .expect("some telemetry");
        state.observe_tool_results(&[crate::ToolDoneEvent {
            id: "1".into(),
            tool: Arc::from(FUSION_DELEGATE_TOOL),
            output: crate::ToolOutput::Plain("ok".into()).with_telemetry(Some(telemetry)),
            is_error: false,
            annotation: None,
            written_path: None,
        }]);
        assert_eq!(state.delegation_count, 1);
        assert_eq!(state.phase(), FusionPhase::Reviewing);
        assert!((state.sidekick_cost - 0.12).abs() < f64::EPSILON);
        assert_eq!(state.sidekick_usage.input, 10);
        assert_eq!(state.sidekick_usage.output, 5);
        assert_eq!(state.sidekick_usage.cache_read, 2);
        assert_eq!(state.sidekick_usage.cache_creation, 1);
    }

    #[test_case(FusionPhase::Cancelled ; "cancelled")]
    #[test_case(FusionPhase::Failed ; "failed")]
    fn terminal_lifecycle_is_one_way(terminal: FusionPhase) {
        let mut state = FusionState::new();
        state.start_request(FusionRequestDecision::Delegate);
        let changed = match terminal {
            FusionPhase::Cancelled => state.cancel(),
            FusionPhase::Failed => state.fail(),
            _ => unreachable!(),
        };
        assert_eq!(changed, Some(terminal));
        assert!(state.phase().is_terminal());
        assert_eq!(state.start_delegate(), None);
        assert_eq!(state.finish_continuation(), None);
    }

    #[test]
    fn failed_delegate_telemetry_is_charged_once_and_missing_telemetry_is_zero() {
        let mut state = FusionState::new();
        state.start_request(FusionRequestDecision::Delegate);
        state.start_delegate();
        let telemetry = crate::ToolTelemetry::try_new(
            Some(0.25),
            Some(crate::ToolUsage::try_new(8, 3, 2, 13, 4).expect("conserving usage")),
        )
        .expect("valid telemetry")
        .expect("some telemetry");
        let failed = crate::ToolDoneEvent {
            id: "failed".into(),
            tool: Arc::from(FUSION_DELEGATE_TOOL),
            output: crate::ToolOutput::Plain("model unavailable".into())
                .with_telemetry(Some(telemetry)),
            is_error: true,
            annotation: None,
            written_path: None,
        };
        assert_eq!(
            state.observe_tool_results(std::slice::from_ref(&failed)),
            Some(FusionContinuation::Fallback)
        );
        assert_eq!(state.observe_tool_results(&[failed]), None);
        assert!((state.sidekick_cost - 0.25).abs() < f64::EPSILON);
        assert_eq!(state.sidekick_usage.input, 8);
        assert_eq!(state.sidekick_usage.output, 4);

        let mut missing = FusionState::new();
        missing.start_request(FusionRequestDecision::Delegate);
        missing.start_delegate();
        missing.observe_tool_results(&[crate::ToolDoneEvent::error("missing".into(), "timed out")]);
        assert!(missing.sidekick_cost.abs() < f64::EPSILON);
        assert_eq!(missing.sidekick_usage, TokenUsage::default());
    }

    #[test]
    fn lane_as_str_matches_storage_labels() {
        assert_eq!(FusionLane::Lead.as_str(), "lead");
        assert_eq!(FusionLane::Sidekick.as_str(), "sidekick");
    }

    fn assert_transition(state: &mut FusionState, next: FusionPhase) {
        state
            .transition(next)
            .unwrap_or_else(|error| panic!("expected transition to {next:?}: {error}"));
    }

    #[test]
    fn lead_only_lifecycle_completes_without_delegation() {
        let mut state = FusionState::new_lead();
        assert_eq!(state.phase(), FusionPhase::Planning);
        assert_transition(&mut state, FusionPhase::Complete);
        assert_eq!(state.phase(), FusionPhase::Complete);
        assert_eq!(state.delegation_count, 0);
    }

    #[test]
    fn successful_delegation_has_exactly_one_review_turn() {
        let mut state = FusionState::new_lead();
        assert_transition(&mut state, FusionPhase::Executing);
        assert_transition(&mut state, FusionPhase::Reviewing);
        assert_transition(&mut state, FusionPhase::Complete);

        assert_eq!(state.review_count(), 1);
        assert_eq!(state.fallback_count(), 0);
        assert_eq!(state.usage_stats().final_lane, FusionLane::Lead);
        assert!(state.transition(FusionPhase::Reviewing).is_err());
    }

    #[test_case(FusionFailure::ToolError ; "tool error")]
    #[test_case(FusionFailure::Timeout ; "timeout")]
    #[test_case(FusionFailure::ModelUnavailable ; "model unavailable")]
    #[test_case(FusionFailure::DelegateCancelled ; "delegate local cancellation")]
    fn delegate_failures_share_one_fallback_transition(failure: FusionFailure) {
        let mut state = FusionState::new_lead();
        assert_transition(&mut state, FusionPhase::Executing);
        state.delegate_failed(failure).unwrap();
        assert_eq!(state.phase(), FusionPhase::LeadFallback);
        assert_eq!(state.fallback_count(), 1);
        assert!(
            state.delegate_failed(failure).is_err(),
            "fallback is one-shot"
        );
        assert!(
            state.transition(FusionPhase::Executing).is_err(),
            "no retry"
        );
        assert_transition(&mut state, FusionPhase::Complete);
        assert_eq!(state.usage_stats().final_lane, FusionLane::Lead);
    }

    #[test]
    fn whole_run_cancellation_is_terminal_without_fallback() {
        let mut state = FusionState::new_lead();
        assert_transition(&mut state, FusionPhase::Executing);
        state.cancel().unwrap();
        assert_eq!(state.phase(), FusionPhase::Cancelled);
        assert_eq!(state.fallback_count(), 0);
        assert!(state.transition(FusionPhase::LeadFallback).is_err());
    }

    #[test]
    fn unrecoverable_lead_error_is_terminal_failed() {
        let mut state = FusionState::new_lead();
        state.fail().unwrap();
        assert_eq!(state.phase(), FusionPhase::Failed);
        assert!(state.transition(FusionPhase::Planning).is_err());
    }

    #[test]
    fn lifecycle_rejects_cycles_and_recursive_sidekick_delegation() {
        let mut state = FusionState::new_lead();
        assert_transition(&mut state, FusionPhase::Executing);
        assert!(
            state.transition(FusionPhase::Executing).is_err(),
            "second delegation"
        );
        assert_transition(&mut state, FusionPhase::Reviewing);
        assert!(
            state.transition(FusionPhase::Executing).is_err(),
            "delegation from review"
        );
        assert!(
            state.transition(FusionPhase::Planning).is_err(),
            "back edge"
        );

        let mut child = FusionState::new_lead();
        child.lane = FusionLane::Sidekick;
        assert!(
            child.transition(FusionPhase::Executing).is_err(),
            "recursive delegation"
        );
    }

    #[test]
    fn compaction_never_routes_the_main_agent_away_from_lead() {
        let mut state = FusionState::new_lead();
        let route = route_after_compact(&mut state, "grep files and run tests", 0);
        assert_eq!(route, FusionRoute::Stay(FusionLane::Lead));
        assert_eq!(state.lane, FusionLane::Lead);
        assert_eq!(state.compact_count, 1);
    }

    #[test]
    fn lane_costs_are_stable_and_total_is_their_sum() {
        let mut state = FusionState::new_lead();
        let lead_usage = TokenUsage {
            input: 10,
            output: 2,
            ..Default::default()
        };
        let sidekick_usage = TokenUsage {
            input: 4,
            output: 1,
            ..Default::default()
        };
        state.record_lane_usage(FusionLane::Lead, lead_usage, 0.20);
        state.record_lane_usage(FusionLane::Sidekick, sidekick_usage, 0.03);
        state.record_delegation();

        let totals = state.usage_stats();
        assert_eq!(totals.lead_usage, lead_usage);
        assert_eq!(totals.sidekick_usage, sidekick_usage);
        assert!((totals.lead_cost - 0.20).abs() < f64::EPSILON);
        assert!((totals.sidekick_cost - 0.03).abs() < f64::EPSILON);
        assert!((totals.lead_cost + totals.sidekick_cost - 0.23).abs() < f64::EPSILON);
        assert_eq!(totals.final_lane, FusionLane::Lead);
    }

    #[test]
    fn failed_delegate_telemetry_is_charged_once() {
        let mut state = FusionState::new_lead();
        let telemetry = crate::ToolTelemetry::try_new(
            Some(0.07),
            Some(crate::ToolUsage::try_new(8, 2, 1, 11, 3).unwrap()),
        )
        .unwrap()
        .unwrap();
        state.observe_tool_results(&[crate::ToolDoneEvent {
            id: "failed".into(),
            tool: Arc::from(FUSION_DELEGATE_TOOL),
            output: crate::ToolOutput::Plain("model unavailable".into())
                .with_telemetry(Some(telemetry)),
            is_error: true,
            annotation: None,
            written_path: None,
        }]);

        assert!((state.sidekick_cost - 0.07).abs() < f64::EPSILON);
        assert_eq!(state.sidekick_usage.input, 8);
        assert_eq!(state.sidekick_usage.output, 3);
    }

    #[test]
    fn missing_delegate_telemetry_adds_zero_without_repricing() {
        let mut state = FusionState::new_lead();
        state.observe_tool_results(&[crate::ToolDoneEvent {
            id: "missing".into(),
            tool: Arc::from(FUSION_DELEGATE_TOOL),
            output: crate::ToolOutput::Plain("ok".into()),
            is_error: false,
            annotation: None,
            written_path: None,
        }]);
        assert!(state.sidekick_cost.abs() < f64::EPSILON);
        assert_eq!(state.sidekick_usage, TokenUsage::default());
    }
}
