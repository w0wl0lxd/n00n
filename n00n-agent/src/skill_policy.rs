//! Active skill tool-policy enforcement for the agent dispatch path.

use serde_json::Value;

pub const SKILL_TOOL_NAME: &str = "skill";
pub const SKILL_POLICY_DENIED_PREFIX: &str = "tool blocked by active skill policy";

fn canonical_tool_name(name: &str) -> std::borrow::Cow<'_, str> {
    let normalized = name.replace('-', "_").to_ascii_lowercase();
    std::borrow::Cow::Owned(crate::tools::canonical_tool_name(&normalized).to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSkillPolicy {
    pub name: String,
    pub allowed_tools: Option<Vec<String>>,
    pub disallowed_tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPolicyDecision {
    pub allowed: bool,
    pub reason: Option<String>,
}

impl ActiveSkillPolicy {
    fn matches_policy(entry: &str, tool_name: &str) -> bool {
        let canonical_tool = canonical_tool_name(tool_name);
        let canonical_entry = canonical_tool_name(entry);
        tool_names_match(canonical_entry.as_ref(), canonical_tool.as_ref())
            || tool_names_match(entry, tool_name)
    }

    #[must_use]
    pub fn evaluate(&self, tool_name: &str) -> SkillPolicyDecision {
        let canonical_tool = canonical_tool_name(tool_name);
        if canonical_tool.as_ref() == canonical_tool_name(SKILL_TOOL_NAME).as_ref() {
            return SkillPolicyDecision {
                allowed: true,
                reason: None,
            };
        }

        if let Some(disallowed) = &self.disallowed_tools {
            for denied in disallowed {
                if Self::matches_policy(denied, tool_name) {
                    return SkillPolicyDecision {
                        allowed: false,
                        reason: Some(format!(
                            "{SKILL_POLICY_DENIED_PREFIX}: tool {tool_name} is disallowed by active skill '{}'",
                            self.name
                        )),
                    };
                }
            }
        }

        if let Some(allowed) = &self.allowed_tools
            && !allowed.is_empty()
        {
            let permitted = allowed
                .iter()
                .any(|entry| Self::matches_policy(entry, tool_name));
            if !permitted {
                return SkillPolicyDecision {
                    allowed: false,
                    reason: Some(format!(
                        "{SKILL_POLICY_DENIED_PREFIX}: tool {tool_name} is not allowed by active skill '{}'",
                        self.name
                    )),
                };
            }
        }

        SkillPolicyDecision {
            allowed: true,
            reason: None,
        }
    }

    #[must_use]
    pub fn from_state_active_skill(state: &Value) -> Option<Self> {
        let active = state.get("active_skill")?;
        Self::from_value(active)
    }

    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        let name = value.get("name")?.as_str()?.to_owned();
        let allowed_tools = value.get("allowed_tools").and_then(string_array);
        let disallowed_tools = value.get("disallowed_tools").and_then(string_array);
        if allowed_tools.is_none() && disallowed_tools.is_none() {
            return None;
        }
        Some(Self {
            name,
            allowed_tools,
            disallowed_tools,
        })
    }

    pub fn apply_from_skill_tool_result(
        policy: &mut Option<Self>,
        tool_name: &str,
        is_error: bool,
        state: Option<&Value>,
    ) {
        if tool_name != SKILL_TOOL_NAME || is_error {
            return;
        }
        let Some(state) = state else {
            return;
        };
        match state.get("active_skill") {
            // List/discovery omit the key and must not clear an existing policy.
            None => {}
            // Explicit null, or a present object without tool lists (unrestricted load).
            Some(serde_json::Value::Null) => *policy = None,
            Some(active) => {
                *policy = Self::from_value(active);
            }
        }
    }
}

#[must_use]
pub fn normalize_tool_name(tool_name: &str) -> String {
    // MCP providers send `server__tool`; skills author native / bare names.
    let as_internal = if tool_name.contains("__") {
        crate::mcp::internal_tool_name(tool_name)
    } else {
        tool_name.to_owned()
    };
    as_internal.replace('-', "_").to_ascii_lowercase()
}

fn bare_tool_name(normalized: &str) -> Option<&str> {
    normalized
        .split_once("__")
        .or_else(|| normalized.split_once('.'))
        .map(|(_, rest)| rest)
        .filter(|rest| !rest.is_empty())
}

fn tool_names_match(policy_entry: &str, tool_name: &str) -> bool {
    let entry = normalize_tool_name(policy_entry);
    let tool = normalize_tool_name(tool_name);
    entry == tool || bare_tool_name(&tool).is_some_and(|bare| bare == entry)
}

fn string_array(value: &Value) -> Option<Vec<String>> {
    let array = value.as_array()?;
    let mut out = Vec::with_capacity(array.len());
    for entry in array {
        let text = entry.as_str()?;
        out.push(text.to_owned());
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evaluate_blocks_disallowed_tool() {
        let policy = ActiveSkillPolicy {
            name: "safe".into(),
            allowed_tools: None,
            disallowed_tools: Some(vec!["bash".into()]),
        };
        let decision = policy.evaluate("bash");
        assert!(!decision.allowed);
        assert!(decision.reason.is_some_and(|r| r.contains("disallowed")));
    }

    #[test]
    fn evaluate_allows_allowlisted_tool() {
        let policy = ActiveSkillPolicy {
            name: "safe".into(),
            allowed_tools: Some(vec!["read".into(), "grep".into()]),
            disallowed_tools: None,
        };
        assert!(policy.evaluate("grep").allowed);
    }

    #[test]
    fn evaluate_rejects_tool_outside_allowlist() {
        let policy = ActiveSkillPolicy {
            name: "safe".into(),
            allowed_tools: Some(vec!["read".into()]),
            disallowed_tools: None,
        };
        assert!(!policy.evaluate("bash").allowed);
    }

    #[test]
    fn evaluate_normalizes_dashed_tool_names() {
        let policy = ActiveSkillPolicy {
            name: "safe".into(),
            allowed_tools: Some(vec!["code-execution".into()]),
            disallowed_tools: None,
        };
        assert!(policy.evaluate("code_execution").allowed);
    }

    #[test]
    fn evaluate_always_allows_skill_tool() {
        let policy = ActiveSkillPolicy {
            name: "safe".into(),
            allowed_tools: Some(vec!["read".into()]),
            disallowed_tools: None,
        };
        assert!(policy.evaluate("skill").allowed);
    }

    #[test]
    fn from_state_parses_active_skill_envelope() {
        let state = json!({
            "active_skill": {
                "name": "safe",
                "allowed_tools": ["read", "grep"]
            }
        });
        let policy = ActiveSkillPolicy::from_state_active_skill(&state).expect("policy");
        assert_eq!(policy.name, "safe");
        assert_eq!(
            policy.allowed_tools,
            Some(vec!["read".into(), "grep".into()])
        );
    }

    #[test]
    fn apply_from_skill_tool_result_sets_and_preserves_policy() {
        let mut policy = None;
        ActiveSkillPolicy::apply_from_skill_tool_result(
            &mut policy,
            "skill",
            false,
            Some(&json!({
                "active_skill": {
                    "name": "safe",
                    "allowed_tools": ["read"]
                }
            })),
        );
        assert!(policy.is_some());

        // Discovery-only state (no active_skill) must not clear an existing policy.
        ActiveSkillPolicy::apply_from_skill_tool_result(
            &mut policy,
            "skill",
            false,
            Some(&json!({ "discovery_cache_hit": true })),
        );
        assert!(policy.is_some());

        // Explicit null active_skill clears the policy.
        ActiveSkillPolicy::apply_from_skill_tool_result(
            &mut policy,
            "skill",
            false,
            Some(&json!({ "active_skill": null })),
        );
        assert!(policy.is_none());
    }

    #[test]
    fn apply_from_skill_tool_result_clears_on_name_only_active_skill() {
        let mut policy = Some(ActiveSkillPolicy {
            name: "safe".into(),
            allowed_tools: Some(vec!["read".into()]),
            disallowed_tools: None,
        });
        // Successful load without tool policy emits name-only active_skill.
        ActiveSkillPolicy::apply_from_skill_tool_result(
            &mut policy,
            "skill",
            false,
            Some(&json!({
                "active_skill": { "name": "ungated" }
            })),
        );
        assert!(policy.is_none());
    }

    #[test]
    fn evaluate_matches_mcp_wire_name_to_bare_allowlist_entry() {
        let policy = ActiveSkillPolicy {
            name: "safe".into(),
            allowed_tools: Some(vec!["read".into(), "grep".into()]),
            disallowed_tools: None,
        };
        assert!(policy.evaluate("docs__read").allowed);
        assert!(!policy.evaluate("docs__bash").allowed);
    }

    #[test]
    fn evaluate_matches_mcp_wire_name_on_denylist() {
        let policy = ActiveSkillPolicy {
            name: "safe".into(),
            allowed_tools: None,
            disallowed_tools: Some(vec!["bash".into()]),
        };
        assert!(!policy.evaluate("shell__bash").allowed);
        assert!(policy.evaluate("shell__read").allowed);
    }
}
