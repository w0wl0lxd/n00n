use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use toml_edit::DocumentMut;

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

#[derive(Clone, Debug)]
pub struct McpServerInfo {
    pub name: String,
    pub transport_kind: &'static str,
    pub tool_count: usize,
    pub enabled: bool,
    pub config_path: PathBuf,
}

#[derive(Deserialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub mcp: HashMap<String, RawServerConfig>,
    #[serde(skip)]
    pub origins: HashMap<String, PathBuf>,
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

impl McpConfig {
    pub fn disabled_entries(&self) -> Vec<(String, &'static str)> {
        self.mcp
            .iter()
            .filter(|(_, v)| !v.enabled)
            .map(|(name, v)| {
                let kind = match &v.transport {
                    RawTransport::Stdio(_) => "stdio",
                    RawTransport::Http(_) => "http",
                };
                (name.clone(), kind)
            })
            .collect()
    }
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
            for name in cfg.mcp.keys() {
                merged.origins.insert(name.clone(), global_path.clone());
            }
            merged.mcp.extend(cfg.mcp);
        }
    }

    let project_path = cwd.join(PROJECT_CONFIG_FILE);
    if let Some(cfg) = read_config(&project_path) {
        for name in cfg.mcp.keys() {
            merged.origins.insert(name.clone(), project_path.clone());
        }
        merged.mcp.extend(cfg.mcp);
    }

    merged
}

pub fn persist_enabled(
    config_path: &Path,
    server_name: &str,
    enabled: bool,
) -> Result<(), McpError> {
    let content = fs::read_to_string(config_path).unwrap_or_default();
    let mut doc: DocumentMut = content
        .parse()
        .map_err(|e| McpError::Config(format!("failed to parse {}: {e}", config_path.display())))?;

    let mcp = doc
        .entry("mcp")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let server = mcp
        .as_table_like_mut()
        .ok_or_else(|| McpError::Config("[mcp] is not a table".into()))?
        .entry(server_name)
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    server
        .as_table_like_mut()
        .ok_or_else(|| McpError::Config(format!("[mcp.{server_name}] is not a table")))?;
    server["enabled"] = toml_edit::value(enabled);

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| McpError::Config(format!("cannot create dir: {e}")))?;
    }
    fs::write(config_path, doc.to_string())
        .map_err(|e| McpError::Config(format!("cannot write {}: {e}", config_path.display())))?;
    Ok(())
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

    #[test]
    fn persist_enabled_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        persist_enabled(&path, "myserver", false).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let doc: toml_edit::DocumentMut = content.parse().unwrap();
        assert_eq!(doc["mcp"]["myserver"]["enabled"].as_bool(), Some(false));
    }

    #[test]
    fn persist_enabled_preserves_existing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"[mcp.myserver]
command = ["echo"]
timeout = 5000
"#,
        )
        .unwrap();
        persist_enabled(&path, "myserver", false).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let doc: toml_edit::DocumentMut = content.parse().unwrap();
        assert_eq!(doc["mcp"]["myserver"]["enabled"].as_bool(), Some(false));
        assert!(doc["mcp"]["myserver"]["command"].is_array());
        assert_eq!(doc["mcp"]["myserver"]["timeout"].as_integer(), Some(5000));
    }

    #[test]
    fn persist_enabled_updates_existing_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"[mcp.srv]
command = ["x"]
enabled = true
"#,
        )
        .unwrap();
        persist_enabled(&path, "srv", false).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let doc: toml_edit::DocumentMut = content.parse().unwrap();
        assert_eq!(doc["mcp"]["srv"]["enabled"].as_bool(), Some(false));
    }
}
