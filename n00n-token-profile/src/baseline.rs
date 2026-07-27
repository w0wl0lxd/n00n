use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ProfileError, RegressionError};
use crate::metrics::{ProfileReport, SurfaceMetric, SurfaceName};

/// Absolute deltas allowed above a committed baseline for hard-gated surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceLimit {
    #[serde(default)]
    pub tool_count_exact: bool,
    pub max_token_delta: Option<u64>,
    pub max_byte_delta: Option<u64>,
    /// When set without hard max_*, breaches are warnings only (returned as Ok with note).
    #[serde(default)]
    pub warn_token_delta: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub model_id: String,
    pub mcp_excluded: bool,
    pub surfaces: BTreeMap<String, SurfaceMetric>,
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
/// gate. Soft surfaces with only `warn_token_delta` never fail.
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
    }
    Ok(())
}

fn parse_surface_name(name: &str) -> Result<SurfaceName, RegressionError> {
    match name {
        "main_tools_schemas" => Ok(SurfaceName::MainToolsSchemas),
        "main_tools_payload" => Ok(SurfaceName::MainToolsPayload),
        "system_prompt" => Ok(SurfaceName::SystemPrompt),
        "cache_prefix" => Ok(SurfaceName::CachePrefix),
        other => Err(RegressionError::Breach(format!(
            "unknown surface name in baseline: {other}"
        ))),
    }
}
