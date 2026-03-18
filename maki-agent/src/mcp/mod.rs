//! MCP client: manages transports and routes tool calls to servers.
//!
//! Tool names are namespaced as `server__tool` (double underscore) to avoid collisions across servers.
//! Names are leaked into `&'static str` so they can be used in tool descriptors without lifetime friction.

pub mod config;
pub mod error;
pub mod http;
pub mod protocol;
pub mod stdio;
pub mod transport;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tracing::{info, warn};

use self::config::{McpConfig, McpServerInfo, ServerConfig, Transport, load_config, parse_servers};
use self::error::McpError;
use self::http::HttpTransport;
use self::stdio::StdioTransport;
use self::transport::McpTransport;

const SEPARATOR: &str = "__";

struct McpToolDef {
    qualified_name: &'static str,
    server_name: Arc<str>,
    raw_name: String,
    description: String,
    input_schema: Value,
}

pub struct McpManager {
    transports: HashMap<Arc<str>, Box<dyn McpTransport>>,
    tools: Vec<McpToolDef>,
    tool_index: HashMap<&'static str, usize>,
    origins: HashMap<String, PathBuf>,
    disabled_entries: Vec<(String, &'static str)>,
}

impl McpManager {
    pub async fn start(cwd: &Path) -> Option<Arc<Self>> {
        let config = load_config(cwd);
        Self::start_with_config(config).await
    }

    pub async fn start_with_config(config: McpConfig) -> Option<Arc<Self>> {
        let disabled_entries = config.disabled_entries();
        let origins = config.origins;
        let servers = match parse_servers(config.mcp) {
            Ok(s) if s.is_empty() && disabled_entries.is_empty() => return None,
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "invalid MCP config");
                return None;
            }
        };

        let mut transports: HashMap<Arc<str>, Box<dyn McpTransport>> = HashMap::new();
        let mut tools = Vec::new();
        let mut tool_index = HashMap::new();

        for server in &servers {
            match Self::start_server(server).await {
                Ok((t, server_tools)) => {
                    let server_name: Arc<str> = Arc::from(server.name.as_str());
                    for tool_info in server_tools {
                        let qualified = format!("{}{SEPARATOR}{}", server.name, tool_info.name);
                        let interned = intern(qualified);
                        let idx = tools.len();
                        tools.push(McpToolDef {
                            qualified_name: interned,
                            server_name: Arc::clone(&server_name),
                            raw_name: tool_info.name,
                            description: tool_info.description,
                            input_schema: tool_info.input_schema,
                        });
                        tool_index.insert(interned, idx);
                    }
                    transports.insert(server_name, t);
                }
                Err(e) => {
                    warn!(server = server.name, error = %e, "failed to start MCP server, skipping");
                }
            }
        }

        if transports.is_empty() && disabled_entries.is_empty() {
            return None;
        }

        info!(
            servers = transports.len(),
            tools = tools.len(),
            disabled = disabled_entries.len(),
            "MCP servers started"
        );

        Some(Arc::new(Self {
            transports,
            tools,
            tool_index,
            origins,
            disabled_entries,
        }))
    }

    async fn start_server(
        config: &ServerConfig,
    ) -> Result<(Box<dyn McpTransport>, Vec<protocol::ToolInfo>), McpError> {
        let t: Box<dyn McpTransport> = match &config.transport {
            Transport::Stdio {
                program,
                args,
                environment,
            } => Box::new(StdioTransport::spawn(
                &config.name,
                program,
                args,
                environment,
                config.timeout,
            )?),
            Transport::Http { url, headers } => Box::new(HttpTransport::new(
                &config.name,
                url,
                headers,
                config.timeout,
            )?),
        };
        transport::initialize(t.as_ref()).await?;
        let tools = transport::list_tools(t.as_ref()).await?;
        info!(
            server = config.name,
            tool_count = tools.len(),
            "MCP server initialized"
        );
        Ok((t, tools))
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tool_index.contains_key(name)
    }

    pub fn interned_name(&self, name: &str) -> &'static str {
        self.tool_index
            .get_key_value(name)
            .map(|(&k, _)| k)
            .unwrap_or("unknown_mcp")
    }

    pub async fn call_tool(&self, qualified_name: &str, args: &Value) -> Result<String, McpError> {
        let idx = self
            .tool_index
            .get(qualified_name)
            .ok_or_else(|| McpError::UnknownTool {
                name: qualified_name.into(),
            })?;
        let def = &self.tools[*idx];
        let t = self
            .transports
            .get(&def.server_name)
            .ok_or_else(|| McpError::ServerDied {
                server: (*def.server_name).into(),
            })?;
        transport::call_tool(t.as_ref(), &def.raw_name, args).await
    }

    pub fn extend_tools(
        &self,
        tool_names: &mut Vec<&'static str>,
        tools: &mut Value,
        disabled: &[String],
    ) {
        for t in self
            .tools
            .iter()
            .filter(|t| !disabled.contains(&t.server_name.to_string()))
        {
            if let Some(arr) = tools.as_array_mut() {
                arr.push(json!({
                    "name": t.qualified_name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                }));
            }
            tool_names.push(t.qualified_name);
        }
    }

    pub fn server_infos(&self, disabled: &[String]) -> Vec<McpServerInfo> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for tool in &self.tools {
            *counts.entry(&tool.server_name).or_default() += 1;
        }
        let active = self
            .transports
            .iter()
            .map(|(name, transport)| McpServerInfo {
                name: name.to_string(),
                transport_kind: transport.transport_kind(),
                tool_count: counts.get(name.as_ref()).copied().unwrap_or(0),
                enabled: !disabled.contains(&name.to_string()),
                config_path: self.origins.get(name.as_ref()).cloned().unwrap_or_default(),
            });
        let inactive = self
            .disabled_entries
            .iter()
            .map(|(name, kind)| McpServerInfo {
                name: name.clone(),
                transport_kind: kind,
                tool_count: 0,
                enabled: false,
                config_path: self.origins.get(name).cloned().unwrap_or_default(),
            });
        active.chain(inactive).collect()
    }

    pub async fn shutdown(self) {
        for (name, t) in self.transports {
            info!(server = &*name, "shutting down MCP server");
            t.shutdown().await;
        }
    }
}

fn intern(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_returns_none() {
        smol::block_on(async {
            let config = McpConfig::default();
            let result = McpManager::start_with_config(config).await;
            assert!(result.is_none());
        });
    }
}
