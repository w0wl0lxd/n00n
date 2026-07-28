//! Devin Fusion-style dual-lane routing: a frontier lead agent delegates mechanical
//! work to a cheaper sidekick while both keep separate cached contexts. Model switches
//! happen at compaction boundaries when a cache miss is already unavoidable.

use n00n_providers::TokenUsage;
use serde::Serialize;

const FUSION_DELEGATE_TOOL: &str = "fusion_delegate";
const RECENT_ERROR_ESCALATE_THRESHOLD: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FusionLane {
    #[default]
    Lead,
    Sidekick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationKind {
    /// Ambiguity, design, review, or serial debugging — keep on lead.
    LeadOnly,
    /// Mechanical exploration, edits, tests, lint — delegate to sidekick.
    Delegate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionRoute {
    Stay(FusionLane),
    Switch(FusionLane),
    EscalateToLead,
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
    pub lane: FusionLane,
    pub delegation_count: u32,
    pub sidekick_failures: u32,
    pub compact_count: u32,
    pub recent_tool_errors: u32,
    pub lead_usage: TokenUsage,
    pub sidekick_usage: TokenUsage,
    pub lead_cost: f64,
    pub sidekick_cost: f64,
}

impl FusionState {
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
        }
    }

    pub fn record_lane_usage(&mut self, lane: FusionLane, usage: TokenUsage, cost: f64) {
        match lane {
            FusionLane::Lead => {
                self.lead_usage += usage;
                self.lead_cost += cost;
            }
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
            if !done.is_error {
                continue;
            }
            self.recent_tool_errors = self.recent_tool_errors.saturating_add(1);
            if done.tool.as_ref() == FUSION_DELEGATE_TOOL {
                self.record_sidekick_failure();
            }
        }
    }

    pub fn clear_recent_tool_errors(&mut self) {
        self.recent_tool_errors = 0;
    }

    #[must_use]
    pub fn recent_tool_errors(&self) -> u32 {
        self.recent_tool_errors
    }

    #[must_use]
    pub fn should_escalate_for_tool_errors(&self) -> bool {
        self.recent_tool_errors >= RECENT_ERROR_ESCALATE_THRESHOLD
    }

    pub fn record_delegation(&mut self) {
        self.delegation_count = self.delegation_count.saturating_add(1);
        self.lane = FusionLane::Sidekick;
    }

    pub fn record_sidekick_failure(&mut self) {
        self.sidekick_failures = self.sidekick_failures.saturating_add(1);
    }

    pub fn record_compact(&mut self) {
        self.compact_count = self.compact_count.saturating_add(1);
    }
}

/// Lexical classifier aligned with Cognition's delegation guidance: lead owns plan,
/// ambiguity, and final review; sidekick handles exploration, broad edits, tests, lint.
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

    let p = prompt.to_ascii_lowercase();
    if p.is_empty() {
        return DelegationKind::LeadOnly;
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
        return DelegationKind::Delegate;
    }
    DelegationKind::LeadOnly
}

/// Route after compaction using summary text and recent sidekick failure pressure.
#[must_use]
pub fn route_after_compact(
    state: &mut FusionState,
    compact_summary: &str,
    recent_tool_errors: u32,
) -> FusionRoute {
    state.record_compact();

    if state.sidekick_failures >= 2 || recent_tool_errors >= RECENT_ERROR_ESCALATE_THRESHOLD {
        return FusionRoute::EscalateToLead;
    }

    let summary_kind = classify_delegation(compact_summary);

    match (state.lane, summary_kind) {
        (FusionLane::Lead, DelegationKind::Delegate) if state.delegation_count < 8 => {
            FusionRoute::Switch(FusionLane::Sidekick)
        }
        (FusionLane::Sidekick, DelegationKind::LeadOnly) => FusionRoute::Switch(FusionLane::Lead),
        (lane, _) => FusionRoute::Stay(lane),
    }
}

/// System prompt appendix for lead lane when Fusion is enabled.
#[must_use]
pub fn fusion_lead_system_append() -> &'static str {
    "Fusion mode is on. You are the lead agent: plan, resolve ambiguity, and do final review. \
     Delegate mechanical work early via fusion_delegate with a spec-quality brief (goal, constraints, \
     definition of done) — do not dictate full file contents. Monitor sidekick results and escalate \
     when judgment is needed."
}

/// System prompt appendix for sidekick lane.
#[must_use]
pub fn fusion_sidekick_system_append() -> &'static str {
    "Fusion sidekick lane: execute the delegated brief efficiently. Prefer index/codegraph/arbor \
     before broad reads. Return concise file:line evidence, test results, and a short summary — \
     not full file dumps."
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use test_case::test_case;

    #[test_case("grep the repo for TODO and list matches", DelegationKind::Delegate ; "mechanical grep")]
    #[test_case("design the auth architecture and trade-offs", DelegationKind::LeadOnly ; "architecture")]
    #[test_case("run cargo test and fix lint issues", DelegationKind::Delegate ; "tests lint")]
    #[test_case("review the PR and decide if we should merge", DelegationKind::LeadOnly ; "review merge")]
    #[test_case("explore the codebase for handler registration", DelegationKind::Delegate ; "explore")]
    fn classify_delegation_cases(prompt: &str, expected: DelegationKind) {
        assert_eq!(classify_delegation(prompt), expected);
    }

    #[test]
    fn route_escalates_after_sidekick_failures() {
        let mut state = FusionState::new_lead();
        state.lane = FusionLane::Sidekick;
        state.sidekick_failures = 2;
        let route = route_after_compact(&mut state, "grep for foo", 0);
        assert_eq!(route, FusionRoute::EscalateToLead);
    }

    #[test]
    fn route_switches_lead_to_sidekick_on_mechanical_summary() {
        let mut state = FusionState::new_lead();
        let route = route_after_compact(&mut state, "implement tests and fix lint", 0);
        assert_eq!(route, FusionRoute::Switch(FusionLane::Sidekick));
    }

    #[test]
    fn observe_tool_results_counts_errors_and_sidekick_failures() {
        let mut state = FusionState::new_lead();
        state.observe_tool_results(&[
            crate::ToolDoneEvent::error("1".into(), "fail"),
            crate::ToolDoneEvent {
                id: "2".into(),
                tool: Arc::from(FUSION_DELEGATE_TOOL),
                output: crate::ToolOutput::Plain("sidekick fail".into()),
                is_error: true,
                annotation: None,
                written_path: None,
            },
        ]);
        assert_eq!(state.recent_tool_errors(), 2);
        assert_eq!(state.sidekick_failures, 1);
        assert!(state.should_escalate_for_tool_errors());
    }
}
