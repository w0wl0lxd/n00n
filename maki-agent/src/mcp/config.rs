use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::SEPARATOR;
use super::error::McpError;
use crate::tools::{
    BASH_TOOL_NAME, BATCH_TOOL_NAME, CODE_EXECUTION_TOOL_NAME, EDIT_TOOL_NAME, GLOB_TOOL_NAME,
    GREP_TOOL_NAME, MULTIEDIT_TOOL_NAME, QUESTION_TOOL_NAME, READ_TOOL_NAME, SKILL_TOOL_NAME,
    TASK_TOOL_NAME, TODOWRITE_TOOL_NAME, WEBFETCH_TOOL_NAME, WEBSEARCH_TOOL_NAME, WRITE_TOOL_NAME,
};

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 300_000;
const GLOBAL_CONFIG_PATH: &str = ".config/maki/config.toml";
const PROJECT_CONFIG_FILE: &str = "maki.toml";

const BUILTIN_TOOL_NAMES: &[&str] = &[
    BASH_TOOL_NAME,
    READ_TOOL_NAME,
    WRITE_TOOL_NAME,
    EDIT_TOOL_NAME,
    MULTIEDIT_TOOL_NAME,
    GLOB_TOOL_NAME,
    GREP_TOOL_NAME,
    QUESTION_TOOL_NAME,
    TODOWRITE_TOOL_NAME,
    WEBFETCH_TOOL_NAME,
    WEBSEARCH_TOOL_NAME,
    SKILL_TOOL_NAME,
    TASK_TOOL_NAME,
    BATCH_TOOL_NAME,
    CODE_EXECUTION_TOOL_NAME,
];

fn default_true() -> bool {
    true
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}

#[derive(Deserialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub mcp: HashMap<String, RawServerConfig>,
}

#[derive(Deserialize, Clone)]
pub struct RawServerConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(flatten)]
    pub transport: RawTransport,
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
pub enum RawTransport {
    Stdio(RawStdioFields),
    Http(RawHttpFields),
}

#[derive(Deserialize, Clone)]
pub struct RawStdioFields {
    pub command: Vec<String>,
    #[serde(default)]
    pub environment: HashMap<String, String>,
}

#[derive(Deserialize, Clone)]
pub struct RawHttpFields {
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(Debug)]
pub struct ServerConfig {
    pub name: String,
    pub timeout: Duration,
    pub transport: Transport,
}

#[derive(Debug)]
pub enum Transport {
    Stdio {
        program: String,
        args: Vec<String>,
        environment: HashMap<String, String>,
    },
    Http {
        url: String,
        headers: HashMap<String, String>,
    },
}

fn is_valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(SEPARATOR)
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub fn parse_servers(raw: HashMap<String, RawServerConfig>) -> Result<Vec<ServerConfig>, McpError> {
    raw.into_iter()
        .filter(|(_, v)| v.enabled)
        .map(|(name, server)| {
            if !is_valid_server_name(&name) {
                return Err(McpError::Config(format!(
                    "server name '{name}' must be ASCII alphanumeric + hyphens"
                )));
            }
            if BUILTIN_TOOL_NAMES.contains(&name.as_str()) {
                return Err(McpError::Config(format!(
                    "server name '{name}' conflicts with built-in tool"
                )));
            }
            if server.timeout == 0 || server.timeout > MAX_TIMEOUT_MS {
                return Err(McpError::Config(format!(
                    "server '{name}' timeout must be 1..={MAX_TIMEOUT_MS}"
                )));
            }
            let transport = match server.transport {
                RawTransport::Stdio(cfg) => {
                    let mut cmd = cfg.command.into_iter();
                    let program = cmd.next().ok_or_else(|| {
                        McpError::Config(format!("server '{name}' has empty command"))
                    })?;
                    Transport::Stdio {
                        program,
                        args: cmd.collect(),
                        environment: cfg.environment,
                    }
                }
                RawTransport::Http(cfg) => {
                    if !cfg.url.starts_with("http://") && !cfg.url.starts_with("https://") {
                        return Err(McpError::Config(format!(
                            "server '{name}' url must start with http:// or https://"
                        )));
                    }
                    Transport::Http {
                        url: cfg.url,
                        headers: cfg.headers,
                    }
                }
            };
            Ok(ServerConfig {
                name,
                timeout: Duration::from_millis(server.timeout),
                transport,
            })
        })
        .collect()
}

pub fn load_config(cwd: &Path) -> McpConfig {
    let mut merged = McpConfig::default();

    if let Some(home) = home_dir() {
        let global_path = home.join(GLOBAL_CONFIG_PATH);
        if let Some(cfg) = read_config(&global_path) {
            merged.mcp.extend(cfg.mcp);
        }
    }

    let project_path = cwd.join(PROJECT_CONFIG_FILE);
    if let Some(cfg) = read_config(&project_path) {
        merged.mcp.extend(cfg.mcp);
    }

    merged
}

fn read_config(path: &Path) -> Option<McpConfig> {
    let content = fs::read_to_string(path).ok()?;
    match toml::from_str(&content) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to parse MCP config");
            None
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

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

    fn http_raw(url: &str) -> RawServerConfig {
        RawServerConfig {
            enabled: true,
            timeout: DEFAULT_TIMEOUT_MS,
            transport: RawTransport::Http(RawHttpFields {
                url: url.to_string(),
                headers: HashMap::new(),
            }),
        }
    }

    fn servers(entries: Vec<(&str, RawServerConfig)>) -> HashMap<String, RawServerConfig> {
        entries.into_iter().map(|(k, v)| (k.into(), v)).collect()
    }

    #[test]
    fn empty_command_rejected() {
        let err = parse_servers(servers(vec![("srv", stdio_raw(&[]))])).unwrap_err();
        assert!(err.to_string().contains("empty command"));
    }

    #[test]
    fn builtin_name_collision_rejected() {
        let err = parse_servers(servers(vec![("bash", stdio_raw(&["echo"]))])).unwrap_err();
        assert!(err.to_string().contains("conflicts with built-in"));
    }

    #[test]
    fn invalid_server_name_rejected() {
        let err = parse_servers(servers(vec![("bad name!", stdio_raw(&["echo"]))])).unwrap_err();
        assert!(err.to_string().contains("ASCII alphanumeric"));
    }

    #[test_case(0               ; "zero")]
    #[test_case(MAX_TIMEOUT_MS + 1 ; "over_max")]
    fn invalid_timeout_rejected(timeout: u64) {
        let mut cfg = stdio_raw(&["echo"]);
        cfg.timeout = timeout;
        let err = parse_servers(servers(vec![("srv", cfg)])).unwrap_err();
        assert!(err.to_string().contains("timeout"));
    }

    #[test]
    fn invalid_http_url_rejected() {
        let err = parse_servers(servers(vec![("srv", http_raw("ftp://bad.com"))])).unwrap_err();
        assert!(err.to_string().contains("http://"));
    }

    #[test]
    fn parse_splits_command_into_program_and_args() {
        let result =
            parse_servers(servers(vec![("srv", stdio_raw(&["npx", "-y", "server"]))])).unwrap();
        assert_eq!(result.len(), 1);
        match &result[0].transport {
            Transport::Stdio { program, args, .. } => {
                assert_eq!(program, "npx");
                assert_eq!(args, &["-y", "server"]);
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn disabled_servers_filtered() {
        let mut cfg = stdio_raw(&["echo"]);
        cfg.enabled = false;
        let result = parse_servers(servers(vec![("srv", cfg)])).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn toml_deserialization() {
        let toml_str = r#"
[mcp.filesystem]
command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[mcp.github]
command = ["gh", "mcp-server"]
environment = { GITHUB_TOKEN = "tok" }
timeout = 10000
enabled = false

[mcp.remote]
url = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer tok123" }
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mcp.len(), 3);

        assert!(matches!(
            config.mcp["filesystem"].transport,
            RawTransport::Stdio(_)
        ));

        let gh_cfg = &config.mcp["github"];
        assert!(!gh_cfg.enabled);
        assert_eq!(gh_cfg.timeout, 10000);
        match &gh_cfg.transport {
            RawTransport::Stdio(s) => assert_eq!(s.environment["GITHUB_TOKEN"], "tok"),
            _ => panic!("expected Stdio"),
        }

        match &config.mcp["remote"].transport {
            RawTransport::Http(h) => {
                assert_eq!(h.url, "https://mcp.example.com/mcp");
                assert_eq!(h.headers["Authorization"], "Bearer tok123");
            }
            _ => panic!("expected Http"),
        }
    }

    #[test]
    fn project_config_overrides_global() {
        let dir = tempfile::tempdir().unwrap();
        let global_dir = dir.path().join("global");
        fs::create_dir_all(&global_dir).unwrap();
        fs::write(
            global_dir.join("config.toml"),
            r#"[mcp.srv]
command = ["global"]
timeout = 5000
"#,
        )
        .unwrap();

        let project_dir = dir.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("maki.toml"),
            r#"[mcp.srv]
command = ["project"]
"#,
        )
        .unwrap();

        let project_cfg = read_config(&project_dir.join("maki.toml")).unwrap();
        let global_cfg = read_config(&global_dir.join("config.toml")).unwrap();

        let mut merged = McpConfig::default();
        merged.mcp.extend(global_cfg.mcp);
        merged.mcp.extend(project_cfg.mcp);

        let parsed = parse_servers(merged.mcp).unwrap();
        match &parsed[0].transport {
            Transport::Stdio { program, .. } => assert_eq!(program, "project"),
            _ => panic!("expected Stdio"),
        }
    }
}
