use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::AgentError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTrigger {
    Auto,
    Manual,
}

impl CompactionTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookEntry {
    #[serde(default)]
    matcher: Option<String>,
    hooks: Vec<Handler>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Handler {
    #[serde(default = "default_handler_type")]
    r#type: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout: u64,
    #[serde(rename = "if", default)]
    if_condition: Option<String>,
}

fn default_handler_type() -> String {
    "command".to_string()
}

fn default_timeout() -> u64 {
    // 30 seconds is a reasonable default for compaction hooks; they should
    // be fast, and a stuck hook blocks the conversation.
    30
}

#[derive(Debug, Deserialize)]
struct Settings {
    #[serde(default)]
    hooks: Hooks,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Hooks {
    #[serde(default, rename = "PreCompact")]
    pre_compact: Vec<HookEntry>,
    #[serde(default, rename = "PostCompact")]
    post_compact: Vec<HookEntry>,
}

fn strip_jsonc_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let len = chars.len();
    let mut in_string = false;
    let mut escape_next = false;

    while i < len {
        let c = chars[i];

        if escape_next {
            result.push(c);
            escape_next = false;
            i += 1;
            continue;
        }

        if c == '\\' {
            result.push(c);
            escape_next = true;
            i += 1;
            continue;
        }

        if c == '"' {
            in_string = !in_string;
            result.push(c);
            i += 1;
            continue;
        }

        if in_string {
            result.push(c);
            i += 1;
            continue;
        }

        if c == '/' && i + 1 < len {
            if chars[i + 1] == '/' {
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
            } else if chars[i + 1] == '*' {
                i += 2;
                while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            } else {
                result.push(c);
                i += 1;
            }
        } else {
            result.push(c);
            i += 1;
        }
    }

    result
}

fn expand_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_var = false;
    let mut var_name = String::new();
    let mut in_brace = false;

    while let Some(c) = chars.next() {
        if c == '$' && !in_var {
            in_var = true;
            if let Some(&'{') = chars.peek() {
                chars.next();
                in_brace = true;
            }
        } else if in_var {
            if in_brace {
                if c == '}' {
                    in_var = false;
                    in_brace = false;
                    if let Ok(val) = std::env::var(&var_name) {
                        result.push_str(&val);
                    }
                    var_name.clear();
                } else {
                    var_name.push(c);
                }
            } else if c.is_alphanumeric() || c == '_' {
                var_name.push(c);
            } else {
                in_var = false;
                if let Ok(val) = std::env::var(&var_name) {
                    result.push_str(&val);
                }
                var_name.clear();
                result.push(c);
            }
        } else {
            result.push(c);
        }
    }

    if in_var
        && !var_name.is_empty()
        && let Ok(val) = std::env::var(&var_name)
    {
        result.push_str(&val);
    }

    result
}

fn expand_path(input: &str) -> String {
    let expanded = expand_env_vars(input);
    if let Some(stripped) = expanded.strip_prefix('~')
        && let Some(home) = std::env::var_os("HOME")
    {
        let home_str = home.to_string_lossy();
        if stripped.is_empty() {
            return home_str.to_string();
        } else if stripped.starts_with('/') {
            return format!("{home_str}{stripped}");
        }
    }
    expanded
}

fn settings_path() -> Option<PathBuf> {
    n00n_storage::paths::home().map(|h| h.join(".claude").join("settings.json"))
}

async fn load_settings() -> Result<Option<Settings>, AgentError> {
    let Some(path) = settings_path() else {
        return Ok(None);
    };

    if !path.exists() {
        return Ok(None);
    }

    let content = smol::unblock(move || std::fs::read_to_string(&path))
        .await
        .map_err(|e| AgentError::Config {
            message: format!("failed to read settings.json: {e}"),
        })?;

    let stripped = strip_jsonc_comments(&content);

    let settings: Settings = serde_json::from_str(&stripped).map_err(|e| AgentError::Config {
        message: format!("failed to parse settings.json: {e}"),
    })?;

    Ok(Some(settings))
}

#[allow(clippy::ref_option)]
fn should_run_hook(matcher: &Option<String>, trigger: CompactionTrigger) -> bool {
    match matcher {
        Some(m) if !m.is_empty() => m == trigger.as_str(),
        _ => true,
    }
}

fn compaction_hook_has_if(handler: &Handler) -> bool {
    handler.if_condition.as_ref().is_some_and(|c| !c.is_empty())
}

#[allow(clippy::redundant_closure_for_method_calls)]
fn build_precompact_input(
    trigger: CompactionTrigger,
    session_id: Option<&n00n_storage::id::SessionRef>,
    cwd: &Path,
    transcript_path: Option<&Path>,
) -> Value {
    let mut obj = json!({
        "session_id": session_id.map(|s| s.to_string()),
        "cwd": cwd.to_string_lossy().to_string(),
        "transcript_path": transcript_path.map(|p| p.to_string_lossy().to_string()),
        "hook_event_name": "PreCompact",
        "trigger": trigger.as_str(),
        "compaction_reason": trigger.as_str(),
        "custom_instructions": "",
    });
    if let Some(sid) = session_id {
        obj["session_id"] = Value::String(sid.to_string());
    }
    obj
}

#[allow(clippy::redundant_closure_for_method_calls)]
fn build_postcompact_input(
    trigger: CompactionTrigger,
    session_id: Option<&n00n_storage::id::SessionRef>,
    cwd: &Path,
    transcript_path: Option<&Path>,
    compact_summary: &str,
) -> Value {
    let mut obj = json!({
        "session_id": session_id.map(|s| s.to_string()),
        "cwd": cwd.to_string_lossy().to_string(),
        "transcript_path": transcript_path.map(|p| p.to_string_lossy().to_string()),
        "hook_event_name": "PostCompact",
        "trigger": trigger.as_str(),
        "compact_summary": compact_summary,
    });
    if let Some(sid) = session_id {
        obj["session_id"] = Value::String(sid.to_string());
    }
    obj
}

async fn run_command_hook(
    cwd: &Path,
    command: &str,
    args: &[String],
    timeout_secs: u64,
    input: &Value,
) -> Result<(String, String, i32), AgentError> {
    use futures_lite::io::{AsyncReadExt, AsyncWriteExt};

    let expanded_cmd = expand_path(command);
    let expanded_args: Vec<String> = args.iter().map(|a| expand_path(a)).collect();

    let mut cmd = async_process::Command::new(&expanded_cmd);
    cmd.args(&expanded_args).current_dir(cwd).kill_on_drop(true);

    let input_json = serde_json::to_string(input).map_err(|e| AgentError::Config {
        message: format!("failed to serialize hook input: {e}"),
    })?;

    let mut child = cmd
        .stdin(async_process::Stdio::piped())
        .stdout(async_process::Stdio::piped())
        .stderr(async_process::Stdio::piped())
        .spawn()
        .map_err(|e| AgentError::Config {
            message: format!("failed to spawn hook command: {e}"),
        })?;

    let mut stdin = child.stdin.take().ok_or_else(|| AgentError::Config {
        message: "failed to get stdin".to_string(),
    })?;

    let stdout = child.stdout.take().ok_or_else(|| AgentError::Config {
        message: "failed to get stdout".to_string(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| AgentError::Config {
        message: "failed to get stderr".to_string(),
    })?;

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();

    let write_stdin = async {
        let _ = stdin.write_all(input_json.as_bytes()).await;
        let _ = stdin.close().await;
    };

    let (mut stdout, mut stderr) = (stdout, stderr);
    let read_output = async {
        write_stdin.await;
        let read_stdout = async {
            let _ = stdout.read_to_end(&mut stdout_buf).await;
        };
        let read_stderr = async {
            let _ = stderr.read_to_end(&mut stderr_buf).await;
        };
        futures_lite::future::zip(read_stdout, read_stderr).await;
    };

    let timeout = async {
        async_io::Timer::after(Duration::from_secs(timeout_secs)).await;
    };

    let completed = futures_lite::future::race(
        async {
            read_output.await;
            true
        },
        async {
            timeout.await;
            false
        },
    )
    .await;

    if !completed {
        let _ = child.kill();
        return Err(AgentError::Config {
            message: format!("hook command timed out after {timeout_secs}s: {expanded_cmd}"),
        });
    }

    let status = child.status().await.map_err(|e| AgentError::Config {
        message: format!("failed to wait for hook command: {e}"),
    })?;

    let code = status.code().unwrap_or_else(|| -1);
    Ok((
        String::from_utf8_lossy(&stdout_buf).to_string(),
        String::from_utf8_lossy(&stderr_buf).to_string(),
        code,
    ))
}

pub async fn run_precompact_hooks(
    trigger: CompactionTrigger,
    session_id: Option<&n00n_storage::id::SessionRef>,
    cwd: &Path,
    transcript_path: Option<&Path>,
) -> Result<(), AgentError> {
    let settings = match load_settings().await {
        Ok(Some(s)) => s,
        Ok(None) => return Ok(()),
        Err(e) => {
            warn!(error = %e, "failed to load settings for PreCompact hooks");
            return Ok(());
        }
    };

    let input = build_precompact_input(trigger, session_id, cwd, transcript_path);

    for entry in &settings.hooks.pre_compact {
        if !should_run_hook(&entry.matcher, trigger) {
            continue;
        }

        for handler in &entry.hooks {
            if handler.r#type != "command" {
                continue;
            }
            if compaction_hook_has_if(handler) {
                debug!(command = %handler.command, "skipping compaction hook: `if` is only evaluated for tool events");
                continue;
            }

            match run_command_hook(
                cwd,
                &handler.command,
                &handler.args,
                handler.timeout,
                &input,
            )
            .await
            {
                Ok((stdout, stderr, code)) => {
                    if code == 2 {
                        return Err(AgentError::Config {
                            message: format!(
                                "compaction blocked by PreCompact hook (exit code 2): {}",
                                stderr.trim()
                            ),
                        });
                    }

                    if let Ok(obj) = serde_json::from_str::<Value>(&stdout)
                        && let Some(decision) = obj.get("decision").and_then(|d| d.as_str())
                        && decision == "block"
                    {
                        let reason = obj
                            .get("reason")
                            .and_then(|r| r.as_str())
                            .unwrap_or_else(|| "no reason provided");
                        return Err(AgentError::Config {
                            message: format!("compaction blocked by PreCompact hook: {reason}"),
                        });
                    }

                    if !stderr.is_empty() && code != 0 {
                        warn!(
                            hook = %handler.command,
                            stderr = %stderr.trim(),
                            "PreCompact hook produced stderr"
                        );
                    }
                }
                Err(e) => {
                    warn!(hook = %handler.command, error = %e, "PreCompact hook failed");
                }
            }
        }
    }

    Ok(())
}

pub async fn run_postcompact_hooks(
    trigger: CompactionTrigger,
    session_id: Option<&n00n_storage::id::SessionRef>,
    cwd: &Path,
    transcript_path: Option<&Path>,
    compact_summary: &str,
) {
    let settings = match load_settings().await {
        Ok(Some(s)) => s,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, "failed to load settings for PostCompact hooks");
            return;
        }
    };

    let input = build_postcompact_input(trigger, session_id, cwd, transcript_path, compact_summary);

    for entry in &settings.hooks.post_compact {
        if !should_run_hook(&entry.matcher, trigger) {
            continue;
        }

        for handler in &entry.hooks {
            if handler.r#type != "command" {
                continue;
            }
            if compaction_hook_has_if(handler) {
                debug!(command = %handler.command, "skipping compaction hook: `if` is only evaluated for tool events");
                continue;
            }

            match run_command_hook(
                cwd,
                &handler.command,
                &handler.args,
                handler.timeout,
                &input,
            )
            .await
            {
                Ok((_stdout, stderr, code)) => {
                    if !stderr.is_empty() && code != 0 {
                        warn!(
                            hook = %handler.command,
                            stderr = %stderr.trim(),
                            "PostCompact hook produced stderr"
                        );
                    }
                }
                Err(e) => {
                    warn!(hook = %handler.command, error = %e, "PostCompact hook failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_jsonc_comments_removes_single_line() {
        let input = r#"{"a": 1, // comment
"b": 2}"#;
        let output = strip_jsonc_comments(input);
        assert_eq!(
            output,
            r#"{"a": 1, 
"b": 2}"#
        );
    }

    #[test]
    fn strip_jsonc_comments_removes_multi_line() {
        let input = r#"{"a": 1, /* comment */ "b": 2}"#;
        let output = strip_jsonc_comments(input);
        assert_eq!(output, r#"{"a": 1,  "b": 2}"#);
    }

    #[test]
    fn strip_jsonc_comments_preserves_strings() {
        let input = r#"{"url": "http://example.com", "comment": "// not a comment"}"#;
        let output = strip_jsonc_comments(input);
        assert!(output.contains("http://example.com"));
        assert!(output.contains("// not a comment"));
    }

    #[test]
    #[allow(unsafe_code)]
    fn expand_env_vars_expands_simple() {
        unsafe { std::env::set_var("TEST_VAR", "value") };
        let input = "prefix $TEST_VAR suffix";
        let output = expand_env_vars(input);
        assert_eq!(output, "prefix value suffix");
        unsafe { std::env::remove_var("TEST_VAR") };
    }

    #[test]
    #[allow(unsafe_code)]
    fn expand_env_vars_expands_braced() {
        unsafe { std::env::set_var("TEST_VAR", "value") };
        let input = "prefix ${TEST_VAR} suffix";
        let output = expand_env_vars(input);
        assert_eq!(output, "prefix value suffix");
        unsafe { std::env::remove_var("TEST_VAR") };
    }

    #[test]
    fn expand_env_vars_missing_var_leaves_empty() {
        let input = "prefix $NONEXISTENT_VAR suffix";
        let output = expand_env_vars(input);
        assert_eq!(output, "prefix  suffix");
    }

    #[test]
    #[allow(unsafe_code)]
    fn expand_path_expands_tilde() {
        unsafe { std::env::set_var("HOME", "/home/user") };
        let input = "~/file.txt";
        let output = expand_path(input);
        assert_eq!(output, "/home/user/file.txt");
        unsafe { std::env::remove_var("HOME") };
    }

    #[test]
    #[allow(unsafe_code)]
    fn expand_path_expands_tilde_alone() {
        unsafe { std::env::set_var("HOME", "/home/user") };
        let input = "~";
        let output = expand_path(input);
        assert_eq!(output, "/home/user");
        unsafe { std::env::remove_var("HOME") };
    }

    #[test]
    fn should_run_hook_matcher_auto() {
        assert!(should_run_hook(
            &Some("auto".to_string()),
            CompactionTrigger::Auto
        ));
        assert!(!should_run_hook(
            &Some("auto".to_string()),
            CompactionTrigger::Manual
        ));
    }

    #[test]
    fn should_run_hook_matcher_manual() {
        assert!(!should_run_hook(
            &Some("manual".to_string()),
            CompactionTrigger::Auto
        ));
        assert!(should_run_hook(
            &Some("manual".to_string()),
            CompactionTrigger::Manual
        ));
    }

    #[test]
    fn should_run_hook_no_matcher_always_runs() {
        assert!(should_run_hook(&None, CompactionTrigger::Auto));
        assert!(should_run_hook(&None, CompactionTrigger::Manual));
    }

    #[test]
    fn should_run_hook_empty_matcher_always_runs() {
        assert!(should_run_hook(
            &Some(String::new()),
            CompactionTrigger::Auto
        ));
        assert!(should_run_hook(
            &Some(String::new()),
            CompactionTrigger::Manual
        ));
    }

    #[test]
    fn build_precompact_input_includes_both_trigger_and_reason() {
        let input = build_precompact_input(
            CompactionTrigger::Auto,
            None,
            Path::new("/cwd"),
            Some(Path::new("/transcript")),
        );
        assert_eq!(input["trigger"], "auto");
        assert_eq!(input["compaction_reason"], "auto");
        assert_eq!(input["cwd"], "/cwd");
        assert_eq!(input["transcript_path"], "/transcript");
        assert_eq!(input["hook_event_name"], "PreCompact");
        assert_eq!(input["custom_instructions"], "");
    }

    #[test]
    fn build_postcompact_input_includes_summary() {
        let input = build_postcompact_input(
            CompactionTrigger::Manual,
            None,
            Path::new("/cwd"),
            None,
            "summary text",
        );
        assert_eq!(input["trigger"], "manual");
        assert_eq!(input["compact_summary"], "summary text");
        assert_eq!(input["hook_event_name"], "PostCompact");
    }

    #[test]
    fn default_timeout_is_30_seconds() {
        assert_eq!(default_timeout(), 30);
    }

    #[test]
    fn compaction_hook_with_if_is_skipped() {
        let with_if = Handler {
            r#type: "command".to_string(),
            command: "true".to_string(),
            args: Vec::new(),
            timeout: default_timeout(),
            if_condition: Some("Bash(*)".to_string()),
        };
        assert!(compaction_hook_has_if(&with_if));
    }

    #[test]
    fn compaction_hook_without_if_runs() {
        let without_if = Handler {
            r#type: "command".to_string(),
            command: "true".to_string(),
            args: Vec::new(),
            timeout: default_timeout(),
            if_condition: None,
        };
        assert!(!compaction_hook_has_if(&without_if));

        let empty_if = Handler {
            r#type: "command".to_string(),
            command: "true".to_string(),
            args: Vec::new(),
            timeout: default_timeout(),
            if_condition: Some(String::new()),
        };
        assert!(!compaction_hook_has_if(&empty_if));
    }
}
