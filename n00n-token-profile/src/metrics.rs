use serde::{Deserialize, Serialize};

/// Named cold-start surfaces measured by the profiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceName {
    MainToolsSchemas,
    MainToolsPayload,
    SystemPrompt,
    CachePrefix,
}

impl std::fmt::Display for SurfaceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MainToolsSchemas => f.write_str("main_tools_schemas"),
            Self::MainToolsPayload => f.write_str("main_tools_payload"),
            Self::SystemPrompt => f.write_str("system_prompt"),
            Self::CachePrefix => f.write_str("cache_prefix"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceMetric {
    pub name: SurfaceName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<u32>,
    pub bytes: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileReport {
    pub model_id: String,
    /// MCP tools are intentionally excluded from cold-start fixtures.
    pub mcp_excluded: bool,
    pub surfaces: Vec<SurfaceMetric>,
}

impl ProfileReport {
    #[must_use]
    pub fn surface(&self, name: SurfaceName) -> Option<&SurfaceMetric> {
        self.surfaces.iter().find(|s| s.name == name)
    }
}
