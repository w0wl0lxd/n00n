pub mod client;
pub mod config;
pub mod error;
pub mod protocol;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use tracing::{info, warn};

use self::client::McpClient;
use self::config::{McpConfig, McpServerConfig, load_config, validate_servers};
use self::error::McpError;

const SEPARATOR: &str = "__";

struct McpToolDef {
    qualified_name: &'static str,
    server_name: Arc<str>,
    raw_name: String,
    description: String,
    input_schema: Value,
}

pub struct McpManager {
    clients: HashMap<Arc<str>, McpClient>,
    tools: Vec<McpToolDef>,
    tool_index: HashMap<&'static str, usize>,
}

impl McpManager {
    pub async fn start(cwd: &Path) -> Option<Arc<Self>> {
        let config = load_config(cwd);
        Self::start_with_config(config).await
    }

    pub async fn start_with_config(config: McpConfig) -> Option<Arc<Self>> {
        let enabled: HashMap<String, McpServerConfig> =
            config.mcp.into_iter().filter(|(_, v)| v.enabled).collect();

        if enabled.is_empty() {
            return None;
        }

        if let Err(e) = validate_servers(&enabled) {
            warn!(error = %e, "invalid MCP config");
            return None;
        }

        let mut clients = HashMap::new();
        let mut tools = Vec::new();
        let mut tool_index = HashMap::new();

        for (name, server_config) in &enabled {
            match Self::start_server(name, server_config).await {
                Ok((client, server_tools)) => {
                    let server_name: Arc<str> = Arc::from(name.as_str());
                    for tool_info in server_tools {
                        let qualified = format!("{name}{SEPARATOR}{}", tool_info.name);
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
                    clients.insert(server_name, client);
                }
                Err(e) => {
                    warn!(server = name, error = %e, "failed to start MCP server, skipping");
                }
            }
        }

        if clients.is_empty() {
            return None;
        }

        info!(
            servers = clients.len(),
            tools = tools.len(),
            "MCP servers started"
        );

        Some(Arc::new(Self {
            clients,
            tools,
            tool_index,
        }))
    }

    async fn start_server(
        name: &str,
        config: &McpServerConfig,
    ) -> Result<(McpClient, Vec<protocol::ToolInfo>), McpError> {
        let client = McpClient::spawn(name, config)?;
        client.initialize().await?;
        let tools = client.list_tools().await?;
        info!(
            server = name,
            tool_count = tools.len(),
            "MCP server initialized"
        );
        Ok((client, tools))
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
        let client = self
            .clients
            .get(&def.server_name)
            .ok_or_else(|| McpError::ServerDied {
                server: (*def.server_name).into(),
            })?;
        client.call_tool(&def.raw_name, args).await
    }

    pub fn extend_tools(&self, tool_names: &mut Vec<&'static str>, tools: &mut Value) {
        let defs: Vec<Value> = self
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.qualified_name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        if let Some(arr) = tools.as_array_mut() {
            arr.extend(defs);
        }
        tool_names.extend(self.tools.iter().map(|t| t.qualified_name));
    }

    pub async fn shutdown(self) {
        for (name, client) in self.clients {
            info!(server = &*name, "shutting down MCP server");
            client.shutdown().await;
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

    #[test]
    fn disabled_servers_ignored() {
        smol::block_on(async {
            let mut config = McpConfig::default();
            config.mcp.insert(
                "srv".into(),
                McpServerConfig {
                    command: vec!["echo".into()],
                    environment: HashMap::new(),
                    enabled: false,
                    timeout: 30_000,
                },
            );
            let result = McpManager::start_with_config(config).await;
            assert!(result.is_none());
        });
    }
}
