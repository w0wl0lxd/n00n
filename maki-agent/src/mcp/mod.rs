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

use self::config::{
    McpConfig, McpServerInfo, McpServerStatus, ServerConfig, Transport, load_config, parse_server,
    transport_kind,
};
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

struct ServerEntry {
    name: String,
    transport_kind: &'static str,
    origin: PathBuf,
    status: McpServerStatus,
}

pub struct McpManager {
    transports: HashMap<Arc<str>, Box<dyn McpTransport>>,
    tools: Vec<McpToolDef>,
    tool_index: HashMap<&'static str, usize>,
    entries: Vec<ServerEntry>,
}

impl McpManager {
    pub async fn start(cwd: &Path) -> Option<Arc<Self>> {
        let config = load_config(cwd);
        Self::start_with_config(config).await
    }

    pub async fn start_with_config(config: McpConfig) -> Option<Arc<Self>> {
        if config.is_empty() {
            return None;
        }

        let origins = config.origins;
        let mut transports: HashMap<Arc<str>, Box<dyn McpTransport>> = HashMap::new();
        let mut tools = Vec::new();
        let mut tool_index = HashMap::new();
        let mut entries = Vec::new();

        for (name, raw) in config.mcp {
            let kind = transport_kind(&raw.transport);
            let origin = origins.get(&name).cloned().unwrap_or_default();

            if !raw.enabled {
                entries.push(ServerEntry {
                    name,
                    transport_kind: kind,
                    origin,
                    status: McpServerStatus::Disabled,
                });
                continue;
            }

            let server_config = match parse_server(name.clone(), raw) {
                Ok(sc) => sc,
                Err(e) => {
                    warn!(server = %name, error = %e, "invalid MCP server config");
                    entries.push(ServerEntry {
                        name,
                        transport_kind: kind,
                        origin,
                        status: McpServerStatus::Failed(e.to_string()),
                    });
                    continue;
                }
            };

            match Self::start_server(&server_config).await {
                Ok((t, server_tools)) => {
                    let server_name: Arc<str> = Arc::from(server_config.name.as_str());
                    for tool_info in server_tools {
                        let qualified =
                            format!("{}{SEPARATOR}{}", server_config.name, tool_info.name);
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
                    transports.insert(Arc::clone(&server_name), t);
                    entries.push(ServerEntry {
                        name: server_config.name,
                        transport_kind: kind,
                        origin,
                        status: McpServerStatus::Running,
                    });
                }
                Err(e) => {
                    warn!(server = %server_config.name, error = %e, "failed to start MCP server");
                    entries.push(ServerEntry {
                        name: server_config.name,
                        transport_kind: kind,
                        origin,
                        status: McpServerStatus::Failed(e.to_string()),
                    });
                }
            }
        }

        info!(
            running = transports.len(),
            tools = tools.len(),
            total = entries.len(),
            "MCP servers initialized"
        );

        Some(Arc::new(Self {
            transports,
            tools,
            tool_index,
            entries,
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

    pub fn extend_tools(&self, tools: &mut Value, disabled: &[String]) {
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
        }
    }

    pub fn server_infos(&self, disabled: &[String]) -> Vec<McpServerInfo> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for tool in &self.tools {
            *counts.entry(&tool.server_name).or_default() += 1;
        }
        self.entries
            .iter()
            .map(|entry| {
                let status = if disabled.contains(&entry.name) {
                    McpServerStatus::Disabled
                } else {
                    entry.status.clone()
                };
                McpServerInfo {
                    name: entry.name.clone(),
                    transport_kind: entry.transport_kind,
                    tool_count: match &status {
                        McpServerStatus::Running => {
                            counts.get(entry.name.as_str()).copied().unwrap_or(0)
                        }
                        _ => 0,
                    },
                    status,
                    config_path: entry.origin.clone(),
                }
            })
            .collect()
    }

    pub async fn shutdown(self) {
        for (name, t) in self.transports {
            info!(server = &*name, "shutting down MCP server");
            t.shutdown().await;
        }
    }

    pub fn child_pids(&self) -> Vec<u32> {
        self.transports
            .values()
            .flat_map(|t| t.child_pids())
            .collect()
    }
}

#[cfg(unix)]
pub fn kill_process_groups(pids: &[u32]) {
    for &pid in pids {
        unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
    }
}

#[cfg(not(unix))]
pub fn kill_process_groups(_pids: &[u32]) {}

fn intern(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::{McpServerStatus, RawServerConfig, RawStdioFields, RawTransport};
    use std::collections::HashMap;
    use std::path::PathBuf;

    const DEFAULT_TIMEOUT_MS: u64 = 30_000;

    fn stdio_raw(cmd: &[&str]) -> RawServerConfig {
        RawServerConfig {
            enabled: true,
            timeout: DEFAULT_TIMEOUT_MS,
            transport: RawTransport::Stdio(RawStdioFields {
                command: cmd.iter().map(|s| s.to_string()).collect(),
                environment: HashMap::new(),
            }),
        }
    }

    fn make_config(entries: Vec<(&str, RawServerConfig)>) -> McpConfig {
        let mut mcp = HashMap::new();
        let mut origins = HashMap::new();
        for (name, cfg) in entries {
            origins.insert(name.to_string(), PathBuf::from("/test/config.toml"));
            mcp.insert(name.to_string(), cfg);
        }
        McpConfig { mcp, origins }
    }

    #[test]
    fn empty_config_returns_none() {
        smol::block_on(async {
            let config = McpConfig::default();
            let result = McpManager::start_with_config(config).await;
            assert!(result.is_none());
        });
    }

    #[test]
    fn invalid_config_creates_failed_entry() {
        smol::block_on(async {
            let config = make_config(vec![("srv", stdio_raw(&[]))]);
            let mgr = McpManager::start_with_config(config).await.unwrap();
            let infos = mgr.server_infos(&[]);
            assert_eq!(infos.len(), 1);
            assert_eq!(infos[0].name, "srv");
            assert!(matches!(infos[0].status, McpServerStatus::Failed(_)));
            assert_eq!(infos[0].tool_count, 0);
        });
    }

    #[test]
    fn disabled_config_creates_disabled_entry() {
        smol::block_on(async {
            let mut raw = stdio_raw(&["echo"]);
            raw.enabled = false;
            let config = make_config(vec![("srv", raw)]);
            let mgr = McpManager::start_with_config(config).await.unwrap();
            let infos = mgr.server_infos(&[]);
            assert_eq!(infos.len(), 1);
            assert_eq!(infos[0].name, "srv");
            assert_eq!(infos[0].status, McpServerStatus::Disabled);
            assert_eq!(infos[0].config_path, PathBuf::from("/test/config.toml"));
        });
    }

    #[test]
    fn failed_server_does_not_block_others() {
        smol::block_on(async {
            let config = make_config(vec![("bad", stdio_raw(&[])), ("also-bad", stdio_raw(&[]))]);
            let mgr = McpManager::start_with_config(config).await.unwrap();
            let infos = mgr.server_infos(&[]);
            assert_eq!(infos.len(), 2);
            assert!(
                infos
                    .iter()
                    .all(|i| matches!(i.status, McpServerStatus::Failed(_)))
            );
        });
    }
}
