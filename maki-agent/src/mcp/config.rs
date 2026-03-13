use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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
    pub mcp: HashMap<String, McpServerConfig>,
}

#[derive(Deserialize, Clone)]
pub struct McpServerConfig {
    pub command: Vec<String>,
    #[serde(default)]
    pub environment: HashMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn is_valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(SEPARATOR)
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub fn validate_servers(servers: &HashMap<String, McpServerConfig>) -> Result<(), McpError> {
    for (name, server) in servers {
        if !is_valid_server_name(name) {
            return Err(McpError::Config(format!(
                "server name '{name}' must be ASCII alphanumeric + hyphens"
            )));
        }
        if BUILTIN_TOOL_NAMES.contains(&name.as_str()) {
            return Err(McpError::Config(format!(
                "server name '{name}' conflicts with built-in tool"
            )));
        }
        if server.command.is_empty() {
            return Err(McpError::Config(format!(
                "server '{name}' has empty command"
            )));
        }
        if server.timeout == 0 || server.timeout > MAX_TIMEOUT_MS {
            return Err(McpError::Config(format!(
                "server '{name}' timeout must be 1..={MAX_TIMEOUT_MS}"
            )));
        }
    }
    Ok(())
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

    fn server(cmd: &[&str]) -> McpServerConfig {
        McpServerConfig {
            command: cmd.iter().map(|s| s.to_string()).collect(),
            environment: HashMap::new(),
            enabled: true,
            timeout: DEFAULT_TIMEOUT_MS,
        }
    }

    #[test]
    fn empty_command_rejected() {
        let mut config = McpConfig::default();
        config.mcp.insert("srv".into(), server(&[]));
        let err = validate_servers(&config.mcp).unwrap_err();
        assert!(err.to_string().contains("empty command"));
    }

    #[test]
    fn builtin_name_collision_rejected() {
        let mut config = McpConfig::default();
        config.mcp.insert("bash".into(), server(&["echo"]));
        let err = validate_servers(&config.mcp).unwrap_err();
        assert!(err.to_string().contains("conflicts with built-in"));
    }

    #[test]
    fn invalid_server_name_rejected() {
        let mut config = McpConfig::default();
        config.mcp.insert("bad name!".into(), server(&["echo"]));
        let err = validate_servers(&config.mcp).unwrap_err();
        assert!(err.to_string().contains("ASCII alphanumeric"));
    }

    #[test_case(0               ; "zero")]
    #[test_case(MAX_TIMEOUT_MS + 1 ; "over_max")]
    fn invalid_timeout_rejected(timeout: u64) {
        let mut config = McpConfig::default();
        let mut srv = server(&["echo"]);
        srv.timeout = timeout;
        config.mcp.insert("srv".into(), srv);
        let err = validate_servers(&config.mcp).unwrap_err();
        assert!(err.to_string().contains("timeout"));
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
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mcp.len(), 2);
        let fs = &config.mcp["filesystem"];
        assert!(fs.enabled);
        assert_eq!(fs.timeout, DEFAULT_TIMEOUT_MS);
        let gh = &config.mcp["github"];
        assert!(!gh.enabled);
        assert_eq!(gh.timeout, 10000);
        assert_eq!(gh.environment["GITHUB_TOKEN"], "tok");
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

        assert_eq!(merged.mcp["srv"].command, vec!["project"]);
    }
}
