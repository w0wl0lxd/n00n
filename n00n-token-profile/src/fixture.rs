//! Cold-start fixture matching production agent startup (minus MCP and live AGENTS.md).

use std::sync::Arc;

use n00n_agent::AgentConfig;
use n00n_agent::AgentMode;
use n00n_agent::agent::build_system_prompt;
use n00n_agent::prompt::ResolvedSlots;
use n00n_agent::template::Vars;
use n00n_agent::tokenize::{count_json_for_model, count_tokens_for_model};
use n00n_agent::tools::{DescriptionContext, ToolAudience, ToolFilter, ToolRegistry};
use n00n_providers::{CacheControl, Model, System};
use serde_json::{Value, json};

use crate::error::ProfileError;
use crate::metrics::{ProfileReport, SurfaceMetric, SurfaceName};

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
    let model = Model::from_spec(FIXTURE_MODEL_ID)
        .map_err(|e| ProfileError::Model(format!("{FIXTURE_MODEL_ID}: {e}")))?;

    let registry = Arc::new(ToolRegistry::new());
    let _host = n00n_lua::PluginHost::with_all_builtins(Arc::clone(&registry))
        .map_err(|e| ProfileError::PluginHost(e.to_string()))?;

    let vars = pinned_vars();
    let filter = ToolFilter::from_config(&AgentConfig::default(), &model, &[]);
    let active = n00n_agent::tools::default_active_tools(&AgentConfig::default());
    let ctx = DescriptionContext {
        filter: &filter,
        audience: ToolAudience::MAIN,
        workflow: false,
    };
    let payload = registry.definitions_active(&vars, &ctx, model.supports_tool_examples(), &active);
    let schemas = schemas_only(&payload);

    let system = build_system_prompt(
        &vars,
        &AgentMode::Build,
        "",
        &ResolvedSlots::default(),
        &model,
    );
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
    let system_metric = measure_system(SurfaceName::SystemPrompt, &system, &model.id);
    let cache_metric = measure_cache_prefix(&sealed, &schemas, &model.id)?;

    Ok(ProfileReport {
        // Keep the fixture pin (provider/id), not the resolved short model id.
        model_id: FIXTURE_MODEL_ID.to_owned(),
        mcp_excluded: true,
        surfaces: vec![schemas_metric, payload_metric, system_metric, cache_metric],
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
    schemas: &Value,
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
    let schema_metric = measure_json(SurfaceName::MainToolsSchemas, schemas, None, model_id)?;
    Ok(SurfaceMetric {
        name: SurfaceName::CachePrefix,
        tool_count: schema_metric.tool_count,
        bytes: bytes.saturating_add(schema_metric.bytes),
        tokens: tokens.saturating_add(schema_metric.tokens),
    })
}

fn usize_as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or_else(|_| u64::MAX)
}
