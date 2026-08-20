use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ProfileError, RegressionError};
use crate::metrics::{ProfileReport, SurfaceMetric, SurfaceName, ToolAttribution};

/// Absolute deltas allowed above a committed baseline for hard-gated surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceLimit {
    #[serde(default)]
    pub tool_count_exact: bool,
    pub max_token_delta: Option<u64>,
    pub max_byte_delta: Option<u64>,
    /// When set without hard max_*, growth beyond this emits a stderr warning only.
    #[serde(default)]
    pub warn_token_delta: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub model_id: String,
    pub mcp_excluded: bool,
    pub surfaces: BTreeMap<String, SurfaceMetric>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolAttribution>,
    pub limits: BTreeMap<String, SurfaceLimit>,
}

impl Baseline {
    /// # Errors
    /// Returns when the baseline file cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self, RegressionError> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }
}

/// Compare a live profile to a committed baseline.
///
/// Hard surfaces (`max_token_delta` / `max_byte_delta` / `tool_count_exact`) fail the
/// gate. Soft surfaces with only `warn_token_delta` log to stderr and still succeed.
///
/// # Errors
/// Returns [`RegressionError::Breach`] when a hard limit is exceeded, or when a
/// required hard surface is missing from the report.
pub fn assert_within_baseline(
    report: &ProfileReport,
    baseline: &Baseline,
) -> Result<(), RegressionError> {
    if report.model_id != baseline.model_id {
        return Err(RegressionError::Breach(format!(
            "model_id mismatch: report={} baseline={}",
            report.model_id, baseline.model_id
        )));
    }

    for (name, limit) in &baseline.limits {
        let Some(expected) = baseline.surfaces.get(name) else {
            return Err(RegressionError::Breach(format!(
                "baseline limit '{name}' has no surface metrics"
            )));
        };
        let surface_name = parse_surface_name(name)?;
        let actual = report
            .surface(surface_name)
            .ok_or(ProfileError::MissingSurface(surface_name))?;

        if limit.tool_count_exact && actual.tool_count != expected.tool_count {
            return Err(RegressionError::Breach(format!(
                "{name}: tool_count changed {:?} -> {:?} (exact match required; update baseline intentionally)",
                expected.tool_count, actual.tool_count
            )));
        }

        if let Some(max) = limit.max_token_delta {
            let delta = actual.tokens.saturating_sub(expected.tokens);
            if delta > max {
                return Err(RegressionError::Breach(format!(
                    "{name}: tokens grew by {delta} ({} -> {}), max allowed {max}",
                    expected.tokens, actual.tokens
                )));
            }
        }

        if let Some(max) = limit.max_byte_delta {
            let delta = actual.bytes.saturating_sub(expected.bytes);
            if delta > max {
                return Err(RegressionError::Breach(format!(
                    "{name}: bytes grew by {delta} ({} -> {}), max allowed {max}",
                    expected.bytes, actual.bytes
                )));
            }
        }

        if limit.max_token_delta.is_none()
            && limit.max_byte_delta.is_none()
            && let Some(warn_at) = limit.warn_token_delta
        {
            let delta = actual.tokens.saturating_sub(expected.tokens);
            if delta > warn_at {
                eprintln!(
                    "token-profile soft warning: {name} tokens grew by {delta} ({} -> {}), warn at {warn_at}",
                    expected.tokens, actual.tokens
                );
            }
        }
    }
    Ok(())
}

fn parse_surface_name(name: &str) -> Result<SurfaceName, RegressionError> {
    match name {
        "main_tools_schemas" => Ok(SurfaceName::MainToolsSchemas),
        "main_tools_payload" => Ok(SurfaceName::MainToolsPayload),
        "full_catalog_schemas" => Ok(SurfaceName::FullCatalogSchemas),
        "full_catalog_payload" => Ok(SurfaceName::FullCatalogPayload),
        "deferred_catalog_schemas" => Ok(SurfaceName::DeferredCatalogSchemas),
        "deferred_catalog_payload" => Ok(SurfaceName::DeferredCatalogPayload),
        "mcp_below_threshold_payload" => Ok(SurfaceName::McpBelowThresholdPayload),
        "mcp_above_threshold_payload" => Ok(SurfaceName::McpAboveThresholdPayload),
        "mcp_deferred_catalog_payload" => Ok(SurfaceName::McpDeferredCatalogPayload),
        "system_prompt" => Ok(SurfaceName::SystemPrompt),
        "cache_prefix" => Ok(SurfaceName::CachePrefix),
        other => Err(RegressionError::Breach(format!(
            "unknown surface name in baseline: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::{Baseline, RegressionError};

    #[test]
    fn load_succeeds_for_valid_baseline_file() {
        let json_data = r#"{
            "model_id": "test-model",
            "mcp_excluded": true,
            "surfaces": {
                "system_prompt": {
                    "name": "system_prompt",
                    "bytes": 100,
                    "tokens": 20
                }
            },
            "tools": [
                {
                    "name": "test_tool",
                    "deferred": false,
                    "initially_active": true,
                    "bytes": 50,
                    "tokens": 10
                }
            ],
            "limits": {
                "system_prompt": {
                    "tool_count_exact": false,
                    "max_token_delta": 5,
                    "max_byte_delta": 10,
                    "warn_token_delta": null
                }
            }
        }"#;

        let mut file = NamedTempFile::new().expect("failed to create temp file");
        file.write_all(json_data.as_bytes())
            .expect("failed to write json");

        let baseline = Baseline::load(file.path()).expect("baseline should load successfully");
        assert_eq!(baseline.model_id, "test-model");
        assert!(baseline.mcp_excluded);
        assert_eq!(baseline.surfaces.len(), 1);
        assert_eq!(baseline.surfaces["system_prompt"].bytes, 100);
        assert_eq!(baseline.surfaces["system_prompt"].tokens, 20);
        assert_eq!(baseline.tools.len(), 1);
        assert_eq!(baseline.tools[0].name, "test_tool");
        assert_eq!(baseline.limits.len(), 1);
        assert_eq!(baseline.limits["system_prompt"].max_token_delta, Some(5));
    }

    #[test]
    fn load_fails_when_file_does_not_exist() {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let non_existent_path = temp_dir.path().join("missing_baseline.json");

        let result = Baseline::load(&non_existent_path);
        match result {
            Err(RegressionError::BaselineIo(_)) => {}
            res => panic!("expected RegressionError::BaselineIo, got {res:?}"),
        }
    }

    #[test]
    fn load_fails_when_file_has_invalid_json() {
        let invalid_json = r#"{ "model_id": "test-model", "mcp_excluded": "#;

        let mut file = NamedTempFile::new().expect("failed to create temp file");
        file.write_all(invalid_json.as_bytes())
            .expect("failed to write json");

        let result = Baseline::load(file.path());
        match result {
            Err(RegressionError::BaselineParse(_)) => {}
            res => panic!("expected RegressionError::BaselineParse, got {res:?}"),
        }
    }
}
