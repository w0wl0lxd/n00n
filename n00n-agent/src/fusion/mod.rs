//! Beta Fusion orchestration: the lead plans and reviews while an isolated,
//! cheaper sidekick executes one bounded mechanical brief.

use n00n_providers::TokenUsage;
use serde::Serialize;

pub const FUSION_DELEGATE_TOOL: &str = "fusion_delegate";
const TRIVIAL_REQUEST_MAX_WORDS: usize = 4;
const RECENT_ERROR_ESCALATE_THRESHOLD: u32 = 2;
const SIDEKICK_FAILURE_ESCALATE_THRESHOLD: u32 = 2;
const MAX_DELEGATIONS_BEFORE_LEAD_LOCK: u32 = 8;

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
pub enum FusionRequestDecision {
    Bypass,
    LeadOnly,
    Delegate,
}

/// Legacy classification result retained for source compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationKind {
    LeadOnly,
    Delegate,
}

/// Legacy compaction routing result. The beta runtime does not use model switching.
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
}

impl FusionState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Legacy constructor. New orchestration uses [`Self::new`].
    #[must_use]
    pub fn new_lead() -> Self {
        Self::new()
    }

    #[must_use]
    pub const fn phase(&self) -> FusionPhase {
        self.phase
    }

    #[must_use]
    pub const fn needs_continuation(&self) -> bool {
        matches!(
            self.phase,
            FusionPhase::Reviewing | FusionPhase::LeadFallback
        )
    }

    pub fn begin_continuation(&mut self, max_attempts: u32) -> bool {
        if !self.needs_continuation() || self.continuation_attempts >= max_attempts.max(1) {
            return false;
        }
        self.continuation_attempts = self.continuation_attempts.saturating_add(1);
        true
    }

    pub fn start_request(&mut self, decision: FusionRequestDecision) -> Option<FusionPhase> {
        if !matches!(self.phase, FusionPhase::Idle | FusionPhase::Complete) {
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

    pub fn observe_tool_results(
        &mut self,
        results: &[crate::ToolDoneEvent],
    ) -> Option<FusionContinuation> {
        if self.phase != FusionPhase::Executing {
            return None;
        }
        let done = results
            .iter()
            .find(|done| done.tool.as_ref() == FUSION_DELEGATE_TOOL)?;

        self.delegation_count = self.delegation_count.saturating_add(1);
        if done.is_error {
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

        let continuation = if done.is_error {
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

    pub fn record_compact(&mut self) {
        self.compact_count = self.compact_count.saturating_add(1);
    }

    /// Legacy error counter reset.
    pub fn clear_recent_tool_errors(&mut self) {
        self.recent_tool_errors = 0;
    }

    #[must_use]
    pub const fn recent_tool_errors(&self) -> u32 {
        self.recent_tool_errors
    }

    #[must_use]
    pub const fn should_escalate_for_tool_errors(&self) -> bool {
        self.recent_tool_errors >= RECENT_ERROR_ESCALATE_THRESHOLD
    }

    /// Legacy bookkeeping helper. The beta runtime records delegations itself.
    pub fn record_delegation(&mut self) {
        self.delegation_count = self.delegation_count.saturating_add(1);
        self.lane = FusionLane::Sidekick;
    }

    /// Legacy bookkeeping helper. The beta runtime records failures itself.
    pub fn record_sidekick_failure(&mut self) {
        self.sidekick_failures = self.sidekick_failures.saturating_add(1);
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
    let prompt = prompt.to_ascii_lowercase();
    let lead_hits = LEAD
        .iter()
        .filter(|signal| prompt.contains(**signal))
        .count();
    let delegate_hits = DELEGATE
        .iter()
        .filter(|signal| prompt.contains(**signal))
        .count();
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
    match (state.lane, classify_delegation(compact_summary)) {
        (FusionLane::Lead, DelegationKind::Delegate)
            if state.delegation_count < MAX_DELEGATIONS_BEFORE_LEAD_LOCK =>
        {
            FusionRoute::Switch(FusionLane::Sidekick)
        }
        (FusionLane::Sidekick, DelegationKind::LeadOnly) => FusionRoute::Switch(FusionLane::Lead),
        (lane, _) => FusionRoute::Stay(lane),
    }
}

/// Decides whether a Fusion-enabled request should delegate at all. Lead-only
/// signals always win over mechanical signals, and short requests bypass Fusion.
#[must_use]
pub fn decide_request(prompt: &str) -> FusionRequestDecision {
    const LEAD_ONLY: &[&str] = &[
        "ambiguous",
        "unclear",
        "architect",
        "design",
        "security",
        "sensitive",
        "credential",
        "secret",
        "password",
        "token",
        "api key",
        "private key",
        "authorization",
        "authentication",
        "cookie",
        ".env",
        "environment variable",
        "personal data",
        "customer data",
        "pii",
        "production",
        "delete",
        "destroy",
        "destructive",
        "drop database",
        "commit",
        "merge",
        "rebase",
        "serial debug",
        "serial-debug",
        "debug chain",
        "root cause",
        "review",
        "approve",
        "decide",
    ];
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
    if LEAD_ONLY.iter().any(|signal| prompt.contains(signal)) {
        return FusionRequestDecision::LeadOnly;
    }
    if prompt.split_whitespace().count() <= TRIVIAL_REQUEST_MAX_WORDS {
        return FusionRequestDecision::Bypass;
    }
    if DELEGATE.iter().any(|signal| prompt.contains(signal)) {
        FusionRequestDecision::Delegate
    } else {
        FusionRequestDecision::LeadOnly
    }
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

    #[test_case("implement credential rotation and add tests", FusionRequestDecision::LeadOnly ; "credentials override mechanics")]
    #[test_case("delete the production database after review", FusionRequestDecision::LeadOnly ; "destructive overrides review")]
    #[test_case("format files and commit the result", FusionRequestDecision::LeadOnly ; "commit overrides mechanics")]
    #[test_case("debug the serial authentication failure", FusionRequestDecision::LeadOnly ; "serial debugging")]
    #[test_case("search .env for API keys", FusionRequestDecision::LeadOnly ; "environment secrets")]
    #[test_case("grep customer data for duplicate records", FusionRequestDecision::LeadOnly ; "customer data")]
    #[test_case("implement cookie authentication and add tests", FusionRequestDecision::LeadOnly ; "authentication")]
    #[test_case("fix typo", FusionRequestDecision::Bypass ; "trivial bypass")]
    #[test_case("implement the parser and add focused tests", FusionRequestDecision::Delegate ; "mechanical delegation")]
    fn request_policy_is_conservative(prompt: &str, expected: FusionRequestDecision) {
        assert_eq!(decide_request(prompt), expected);
    }

    #[test_case(FusionRequestDecision::Bypass ; "bypass")]
    #[test_case(FusionRequestDecision::LeadOnly ; "lead_only")]
    fn non_delegated_requests_complete_without_a_user_phase(decision: FusionRequestDecision) {
        let mut state = FusionState::new();
        assert_eq!(state.start_request(decision), None);
        assert_eq!(state.phase(), FusionPhase::Complete);
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

    #[test_case("sidekick failed", FusionContinuation::Fallback ; "failure")]
    #[test_case("sidekick cancelled", FusionContinuation::Fallback ; "cancel")]
    fn failed_or_cancelled_delegate_schedules_one_fallback(
        message: &str,
        expected: FusionContinuation,
    ) {
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

        assert_eq!(state.observe_tool_results(&[done]), Some(expected));
        assert_eq!(state.phase(), FusionPhase::LeadFallback);
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

    #[test]
    fn lane_as_str_matches_storage_labels() {
        assert_eq!(FusionLane::Lead.as_str(), "lead");
        assert_eq!(FusionLane::Sidekick.as_str(), "sidekick");
    }
}
