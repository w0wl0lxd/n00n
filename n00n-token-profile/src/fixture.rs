//! Cold-start fixture matching production agent startup (minus MCP and live AGENTS.md).

use std::collections::BTreeSet;
use std::sync::Arc;

use n00n_agent::AgentConfig;
use n00n_agent::AgentMode;
use n00n_agent::agent::build_system_prompt;
use n00n_agent::prompt::ResolvedSlots;
use n00n_agent::template::Vars;
use n00n_agent::tokenize::{count_json_for_model, count_tokens_for_model};
use n00n_agent::tools::{DescriptionContext, ToolAudience, ToolRegistry, runtime_tool_definitions};
use n00n_providers::{CacheControl, Model, System};
use serde_json::{Value, json};

use crate::error::ProfileError;
use crate::metrics::{ProfileReport, SurfaceMetric, SurfaceName, ToolAttribution};

pub const FIXTURE_MODEL_ID: &str = "anthropic/claude-sonnet-4-6";
const FIXTURE_CWD: &str = "/tmp/n00n-token-profile";
const FIXTURE_PLATFORM: &str = "linux";
const FIXTURE_DATE: &str = "2026-07-27";

/// Build the offline cold-start profile used by CI regression tests.
///
/// # Errors
/// Returns when the pinned model cannot be resolved, plugins fail to load, or a
/// surface cannot be serialized for measurement.
pub fn profile_cold_start() -> Result<ProfileReport, ProfileError> {
    let model = Model::from_spec(
        &n00n_providers::model_registry::test_registry(),
        FIXTURE_MODEL_ID,
    )
    .map_err(|e| ProfileError::Model(format!("{FIXTURE_MODEL_ID}: {e}")))?;

    let registry = Arc::new(ToolRegistry::new());
    let host = n00n_lua::PluginHost::with_all_builtins(Arc::clone(&registry))
        .map_err(|e| ProfileError::PluginHost(e.to_string()))?;

    let vars = pinned_vars();
    let (payload, filter) = runtime_tool_definitions(
        &registry,
        &vars,
        &AgentConfig::default(),
        &model,
        &[],
        false,
    );
    let schemas = schemas_only(&payload);
    let ctx = DescriptionContext {
        filter: &filter,
        audience: ToolAudience::MAIN,
        workflow: false,
    };
    let full_payload = registry.definitions(&vars, &ctx, model.supports_tool_examples());
    let full_schemas = schemas_only(&full_payload);
    let deferred_names: BTreeSet<String> = registry
        .snapshot()
        .iter()
        .filter(|entry| entry.defer_loading)
        .filter(|entry| entry.tool.audience().contains(ToolAudience::MAIN))
        .filter(|entry| filter.matches(entry.name()))
        .map(|entry| entry.name().to_owned())
        .collect();
    let deferred_payload = select_definitions(&full_payload, &deferred_names);
    let deferred_schemas = schemas_only(&deferred_payload);
    let tools = tool_attributions(&registry, &full_payload, &payload, &model.id)?;
    let mcp_below_threshold = representative_mcp_definitions(n00n_agent::mcp::DEFAULT_DEFER_TOOLS);
    let mcp_deferred_catalog =
        representative_mcp_definitions(n00n_agent::mcp::DEFAULT_DEFER_TOOLS.saturating_add(1));
    let mcp_above_threshold = Value::Array(Vec::new());

    let prompt_slots = host
        .event_handle()
        .map_or_else(ResolvedSlots::default, |handle| {
            handle.collect_prompt_slots()
        });
    let system = build_system_prompt(&vars, &AgentMode::Build, "", &prompt_slots, &model);
    let mut sealed = system.clone();
    sealed.seal();

    let payload_metric = measure_json(
        SurfaceName::MainToolsPayload,
        &payload,
        Some(tool_count(&payload)),
        &model.id,
    )?;
    let schemas_metric = measure_json(
        SurfaceName::MainToolsSchemas,
        &schemas,
        Some(tool_count(&schemas)),
        &model.id,
    )?;
    let full_payload_metric = measure_json(
        SurfaceName::FullCatalogPayload,
        &full_payload,
        Some(tool_count(&full_payload)),
        &model.id,
    )?;
    let full_schemas_metric = measure_json(
        SurfaceName::FullCatalogSchemas,
        &full_schemas,
        Some(tool_count(&full_schemas)),
        &model.id,
    )?;
    let deferred_payload_metric = measure_json(
        SurfaceName::DeferredCatalogPayload,
        &deferred_payload,
        Some(tool_count(&deferred_payload)),
        &model.id,
    )?;
    let deferred_schemas_metric = measure_json(
        SurfaceName::DeferredCatalogSchemas,
        &deferred_schemas,
        Some(tool_count(&deferred_schemas)),
        &model.id,
    )?;
    let mcp_below_metric = measure_json(
        SurfaceName::McpBelowThresholdPayload,
        &mcp_below_threshold,
        Some(tool_count(&mcp_below_threshold)),
        &model.id,
    )?;
    let mcp_above_metric = measure_json(
        SurfaceName::McpAboveThresholdPayload,
        &mcp_above_threshold,
        Some(tool_count(&mcp_above_threshold)),
        &model.id,
    )?;
    let mcp_catalog_metric = measure_json(
        SurfaceName::McpDeferredCatalogPayload,
        &mcp_deferred_catalog,
        Some(tool_count(&mcp_deferred_catalog)),
        &model.id,
    )?;
    let system_metric = measure_system(SurfaceName::SystemPrompt, &system, &model.id);
    let cache_metric = measure_cache_prefix(&sealed, &payload, &model.id)?;

    Ok(ProfileReport {
        // Keep the fixture pin (provider/id), not the resolved short model id.
        model_id: FIXTURE_MODEL_ID.to_owned(),
        mcp_excluded: true,
        surfaces: vec![
            schemas_metric,
            payload_metric,
            full_schemas_metric,
            full_payload_metric,
            deferred_schemas_metric,
            deferred_payload_metric,
            mcp_below_metric,
            mcp_above_metric,
            mcp_catalog_metric,
            system_metric,
            cache_metric,
        ],
        tools,
    })
}

fn pinned_vars() -> Vars {
    Vars::new()
        .set("{cwd}", FIXTURE_CWD)
        .set("{platform}", FIXTURE_PLATFORM)
        .set("{date}", FIXTURE_DATE)
}

fn tool_count(defs: &Value) -> u32 {
    match defs.as_array() {
        Some(a) => u32::try_from(a.len()).unwrap_or_else(|_| u32::MAX),
        None => 0,
    }
}

fn representative_mcp_definitions(count: usize) -> Value {
    Value::Array(
        (1..=count)
            .map(|index| {
                json!({
                    "name": format!("fixture__tool_{index}"),
                    "description": format!("Representative MCP tool {index} for deterministic profiling"),
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "Representative search query"
                            }
                        },
                        "required": ["query"],
                        "additionalProperties": false
                    }
                })
            })
            .collect(),
    )
}
fn schemas_only(defs: &Value) -> Value {
    let Some(arr) = defs.as_array() else {
        return Value::Array(vec![]);
    };
    let mut items: Vec<Value> = arr
        .iter()
        .map(|def| {
            let name = def.get("name").cloned().unwrap_or_else(|| Value::Null);
            let schema = def
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| Value::Null);
            json!({
                "name": name,
                "input_schema": schema,
            })
        })
        .collect();
    items.sort_by(|a, b| {
        let an = a.get("name").and_then(Value::as_str).unwrap_or_else(|| "");
        let bn = b.get("name").and_then(Value::as_str).unwrap_or_else(|| "");
        an.cmp(bn)
    });
    Value::Array(items)
}

fn select_definitions(definitions: &Value, names: &BTreeSet<String>) -> Value {
    let selected = definitions
        .as_array()
        .into_iter()
        .flatten()
        .filter(|definition| {
            definition
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| names.contains(name))
        })
        .cloned()
        .collect();
    Value::Array(selected)
}

fn tool_attributions(
    registry: &ToolRegistry,
    full_payload: &Value,
    initial_payload: &Value,
    model_id: &str,
) -> Result<Vec<ToolAttribution>, ProfileError> {
    let initial_names: BTreeSet<&str> = initial_payload
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|definition| definition.get("name").and_then(Value::as_str))
        .collect();
    let snapshot = registry.snapshot();
    let mut tools = Vec::new();
    for definition in full_payload.as_array().into_iter().flatten() {
        let Some(name) = definition.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(entry) = snapshot.iter().find(|entry| entry.name() == name) else {
            continue;
        };
        let encoded = serde_json::to_vec(definition).map_err(|source| ProfileError::Serialize {
            surface: SurfaceName::FullCatalogPayload,
            source,
        })?;
        tools.push(ToolAttribution {
            name: name.to_owned(),
            namespace: entry.namespace.as_deref().map(str::to_owned),
            deferred: entry.defer_loading,
            initially_active: initial_names.contains(name),
            bytes: usize_as_u64(encoded.len()),
            tokens: usize_as_u64(count_json_for_model(model_id, definition)),
        });
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(tools)
}

fn measure_json(
    name: SurfaceName,
    value: &Value,
    tool_count: Option<u32>,
    model_id: &str,
) -> Result<SurfaceMetric, ProfileError> {
    let encoded = serde_json::to_vec(value).map_err(|source| ProfileError::Serialize {
        surface: name,
        source,
    })?;
    let bytes = usize_as_u64(encoded.len());
    let tokens = usize_as_u64(count_json_for_model(model_id, value));
    Ok(SurfaceMetric {
        name,
        tool_count,
        bytes,
        tokens,
    })
}

fn measure_system(name: SurfaceName, system: &System, model_id: &str) -> SurfaceMetric {
    let mut bytes: u64 = 0;
    let mut tokens: u64 = 0;
    for block in system.blocks() {
        bytes = bytes.saturating_add(usize_as_u64(block.text.len()));
        tokens = tokens.saturating_add(usize_as_u64(count_tokens_for_model(model_id, &block.text)));
    }
    SurfaceMetric {
        name,
        tool_count: None,
        bytes,
        tokens,
    }
}

fn measure_cache_prefix(
    system: &System,
    tool_definitions: &Value,
    model_id: &str,
) -> Result<SurfaceMetric, ProfileError> {
    let mut bytes: u64 = 0;
    let mut tokens: u64 = 0;
    for block in system.blocks() {
        if block.cache == CacheControl::Dynamic {
            continue;
        }
        bytes = bytes.saturating_add(usize_as_u64(block.text.len()));
        tokens = tokens.saturating_add(usize_as_u64(count_tokens_for_model(model_id, &block.text)));
    }
    let tools_metric = measure_json(
        SurfaceName::MainToolsPayload,
        tool_definitions,
        Some(tool_count(tool_definitions)),
        model_id,
    )?;
    Ok(SurfaceMetric {
        name: SurfaceName::CachePrefix,
        tool_count: tools_metric.tool_count,
        bytes: bytes.saturating_add(tools_metric.bytes),
        tokens: tokens.saturating_add(tools_metric.tokens),
    })
}

fn usize_as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or_else(|_| u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use n00n_agent::tools::{DescriptionContext, ToolAudience, ToolFilter, default_active_tools};
    #[test]
    fn profile_uses_runtime_default_active_tools() {
        let model = Model::from_spec(
            &n00n_providers::model_registry::test_registry(),
            FIXTURE_MODEL_ID,
        )
        .expect("fixture model");
        let registry = Arc::new(ToolRegistry::new());
        let _host = n00n_lua::PluginHost::with_all_builtins(Arc::clone(&registry))
            .expect("built-in plugins");
        let filter = ToolFilter::from_config(&AgentConfig::default(), &model, &[]);
        let ctx = DescriptionContext {
            filter: &filter,
            audience: ToolAudience::MAIN,
            workflow: false,
        };
        let runtime_payload = registry.definitions_active(
            &pinned_vars(),
            &ctx,
            model.supports_tool_examples(),
            &default_active_tools(),
        );
        let report = profile_cold_start().expect("cold-start profile");
        let profiled = report
            .surface(SurfaceName::MainToolsPayload)
            .expect("main tools payload");

        assert_eq!(profiled.tool_count, Some(tool_count(&runtime_payload)));
    }

    #[test]
    fn runtime_descriptions_use_pinned_fixture_variables() {
        let model = Model::from_spec(
            &n00n_providers::model_registry::test_registry(),
            FIXTURE_MODEL_ID,
        )
        .expect("fixture model");
        let registry = Arc::new(ToolRegistry::new());
        let _host = n00n_lua::PluginHost::with_all_builtins(Arc::clone(&registry))
            .expect("built-in plugins");
        let (payload, _) = runtime_tool_definitions(
            &registry,
            &pinned_vars(),
            &AgentConfig::default(),
            &model,
            &[],
            false,
        );
        let shell_description = payload
            .as_array()
            .expect("tool definitions")
            .iter()
            .find(|definition| definition["name"] == "run_shell")
            .and_then(|definition| definition["description"].as_str())
            .expect("run_shell description");

        assert!(shell_description.contains(FIXTURE_CWD));
    }

    #[test]
    fn cache_prefix_includes_complete_tool_definitions() {
        let model = Model::from_spec(
            &n00n_providers::model_registry::test_registry(),
            FIXTURE_MODEL_ID,
        )
        .expect("fixture model");
        let registry = Arc::new(ToolRegistry::new());
        let host = n00n_lua::PluginHost::with_all_builtins(Arc::clone(&registry))
            .expect("built-in plugins");
        let filter = ToolFilter::from_config(&AgentConfig::default(), &model, &[]);
        let ctx = DescriptionContext {
            filter: &filter,
            audience: ToolAudience::MAIN,
            workflow: false,
        };
        let payload = registry.definitions_active(
            &pinned_vars(),
            &ctx,
            model.supports_tool_examples(),
            &default_active_tools(),
        );
        let prompt_slots = host
            .event_handle()
            .map_or_else(ResolvedSlots::default, |handle| {
                handle.collect_prompt_slots()
            });
        let mut system =
            build_system_prompt(&pinned_vars(), &AgentMode::Build, "", &prompt_slots, &model);
        system.seal();
        let expected =
            measure_cache_prefix(&system, &payload, &model.id).expect("complete cache prefix");
        let report = profile_cold_start().expect("cold-start profile");
        let actual = report
            .surface(SurfaceName::CachePrefix)
            .expect("cache prefix");

        assert_eq!(actual.bytes, expected.bytes);
        assert_eq!(actual.tokens, expected.tokens);
    }

    #[test]
    fn profile_attributes_full_and_deferred_catalogs() {
        let report = profile_cold_start().expect("cold-start profile");
        let full = report
            .surface(SurfaceName::FullCatalogPayload)
            .expect("full catalog payload");
        let deferred = report
            .surface(SurfaceName::DeferredCatalogPayload)
            .expect("deferred catalog payload");
        let deferred_count = report.tools.iter().filter(|tool| tool.deferred).count();

        assert_eq!(full.tool_count, u32::try_from(report.tools.len()).ok());
        assert_eq!(deferred.tool_count, u32::try_from(deferred_count).ok());
        assert!(
            report
                .tools
                .iter()
                .all(|tool| tool.bytes > 0 && tool.tokens > 0)
        );
        assert!(
            report
                .tools
                .windows(2)
                .all(|tools| tools[0].name < tools[1].name)
        );
    }

    #[test]
    fn profile_models_mcp_deferral_threshold() {
        let report = profile_cold_start().expect("cold-start profile");
        let below = report
            .surface(SurfaceName::McpBelowThresholdPayload)
            .expect("MCP below threshold");
        let above = report
            .surface(SurfaceName::McpAboveThresholdPayload)
            .expect("MCP above threshold");
        let catalog = report
            .surface(SurfaceName::McpDeferredCatalogPayload)
            .expect("MCP deferred catalog");

        assert_eq!(
            below.tool_count,
            u32::try_from(n00n_agent::mcp::DEFAULT_DEFER_TOOLS).ok()
        );
        assert_eq!(above.tool_count, Some(0));
        assert_eq!(
            catalog.tool_count,
            u32::try_from(n00n_agent::mcp::DEFAULT_DEFER_TOOLS.saturating_add(1)).ok()
        );
    }
}
