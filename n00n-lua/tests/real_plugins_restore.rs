#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

//! Exercises real plugins (bash, grep, batch) through `request_restore`.
//! A broken restore silently falls back to raw LLM output, so we assert
//! things only the real views produce (gutters, command headers, truncation).

use std::{
    collections::HashMap,
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

use n00n_agent::AgentEvent;
use n00n_agent::CancelTrigger;
use n00n_agent::tools::ToolRegistry;
use n00n_config::{PluginsConfig, ToolOutputLines};
use n00n_lua::PluginHost;
use serde_json::{Map, Value, json};

const BASH_SRC: &str = include_str!("../../plugins/bash/init.lua");
const BATCH_SRC: &str = include_str!("../../plugins/batch/init.lua");
const BLACKBOARD_SRC: &str = include_str!("../../plugins/blackboard/init.lua");
const CODEGRAPH_SRC: &str = include_str!("../../plugins/codegraph/init.lua");
const EXPLORE_SRC: &str = include_str!("../../plugins/explore/init.lua");
const FUSION_SRC: &str = include_str!("../../plugins/fusion/init.lua");
const GIT_SRC: &str = include_str!("../../plugins/git/init.lua");
const GITHUB_SRC: &str = include_str!("../../plugins/github/init.lua");
const GREP_SRC: &str = include_str!("../../plugins/grep/init.lua");
const SEMBLEM_SRC: &str = include_str!("../../plugins/semblem/init.lua");
const TASK_SRC: &str = include_str!("../../plugins/task/init.lua");
const TMUX_SRC: &str = include_str!("../../plugins/tmux/init.lua");
const WEBFETCH_SRC: &str = include_str!("../../plugins/webfetch/init.lua");
const WEBSEARCH_SRC: &str = include_str!("../../plugins/websearch/init.lua");
const WORKFLOW_SRC: &str = include_str!("../../plugins/workflow/init.lua");

/// Only the real `ToolView` emits this when collapsed.
const EXPAND_HINT: &str = "click to expand";
/// Fixed cap so truncation tests don't depend on the product default.
const VIEW_CAP: usize = 3;

const GREP_OUT: &str =
    "src/a.rs:\n  1: fn main() {}\n  2: fn helper() {}\n\nsrc/b.rs:\n  10: fn other() {}";

const BATCH_INPUT_GREP_BASH: &str = r#"{ "tool_calls": [
    { "tool": "grep", "parameters": { "pattern": "fn" } },
    { "tool": "bash", "parameters": { "command": "echo hello-from-bash" } }
]}"#;
const WORKFLOW_TOOL: &str = "workflow";
const LIVE_PREVIEW_ID: &str = "live-preview";
const LIVE_PREVIEW_EVENT_SEQUENCE: u64 = 0;
const LIVE_PREVIEW_RECEIVE_TIMEOUT: Duration = Duration::from_secs(5);
const LIVE_PREVIEW_TIMEOUT_MSG: &str = "plugin did not publish a live preview";

/// Cancels and joins the spawned tool on every exit path, including panics.
struct LivePreviewGuard<T> {
    cancel: Option<CancelTrigger>,
    execution: Option<JoinHandle<T>>,
}

impl<T> LivePreviewGuard<T> {
    fn new(cancel: CancelTrigger, execution: JoinHandle<T>) -> Self {
        Self {
            cancel: Some(cancel),
            execution: Some(execution),
        }
    }
}

impl<T> Drop for LivePreviewGuard<T> {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        if let Some(execution) = self.execution.take() {
            let _ = execution.join();
        }
    }
}

fn load_host() -> PluginHost {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source("bash", BASH_SRC).unwrap();
    host.load_source("batch", BATCH_SRC).unwrap();
    host.load_source("codegraph", CODEGRAPH_SRC).unwrap();
    host.load_source("explore", EXPLORE_SRC).unwrap();
    host.load_source("git", GIT_SRC).unwrap();
    host.load_source("github", GITHUB_SRC).unwrap();
    host.load_source("grep", GREP_SRC).unwrap();
    host.load_source("semblem", SEMBLEM_SRC).unwrap();
    host.load_source("task", TASK_SRC).unwrap();
    host
}

fn assert_publishes_live_buf(tool: &str, source: &str, input: Value, expected: &str) {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    if tool == WORKFLOW_TOOL {
        host.load_source(
            "mock_workflow_state",
            r#"
                n00n.env.state_dir = function() return "/state" end
                n00n.fs.metadata = function() return nil, nil end
                n00n.fs.mkdir = function() return true, nil end
                n00n.fs.write = function() return true, nil end
            "#,
        )
        .unwrap();
    }
    host.load_source("live_preview", source).unwrap();

    let (tx, rx) = flume::unbounded();
    let event_tx = n00n_agent::EventSender::new(tx, LIVE_PREVIEW_EVENT_SEQUENCE);
    let mut ctx = n00n_agent::tools::test_support::stub_ctx_with(
        &n00n_agent::AgentMode::Build,
        Some(&event_tx),
        Some(LIVE_PREVIEW_ID),
    );
    ctx.registry = Arc::clone(&reg);
    let (cancel, token) = n00n_agent::CancelToken::new();
    ctx.cancel = token;
    let inv = reg.get(tool).unwrap().tool.parse(&input).unwrap();
    let execution = std::thread::spawn(move || smol::block_on(inv.execute(&ctx)));
    let _guard = LivePreviewGuard::new(cancel, execution);

    let deadline = Instant::now() + LIVE_PREVIEW_RECEIVE_TIMEOUT;
    let body = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "{LIVE_PREVIEW_TIMEOUT_MSG}");
        let env = rx
            .recv_timeout(remaining)
            .unwrap_or_else(|_| panic!("{LIVE_PREVIEW_TIMEOUT_MSG}"));
        if let AgentEvent::LiveToolBuf { id, body } = env.event
            && id == LIVE_PREVIEW_ID
        {
            break body;
        }
    };
    assert!(
        body.read()
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.text.contains(expected))
    );
}

#[test_case::test_case(
    "bash",
    BASH_SRC,
    json!({ "command": "printf live-preview" }),
    "live-preview";
    "bash"
)]
#[test_case::test_case(
    WORKFLOW_TOOL,
    WORKFLOW_SRC,
    json!({ "script": "meta({ name = 'preview' }) return 'done'" }),
    "workflow";
    "workflow"
)]
fn running_plugin_publishes_live_preview(tool: &str, source: &str, input: Value, expected: &str) {
    assert_publishes_live_buf(tool, source, input, expected);
}

fn batch_state() -> Value {
    json!({ "children": [
        { "tool": "grep", "status": "success", "output": GREP_OUT },
        { "tool": "bash", "status": "success", "output": "hello-from-bash" },
    ]})
}

fn execute_plugin_with_native_mock(
    tool: &str,
    source: &str,
    native_mock: &str,
    input: Value,
) -> Result<String, String> {
    execute_plugin_with_native_mock_and_opts(tool, source, native_mock, input, Map::new())
}

fn execute_plugin_with_native_mock_and_opts(
    tool: &str,
    source: &str,
    native_mock: &str,
    input: Value,
    opts: Map<String, Value>,
) -> Result<String, String> {
    let registry = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&registry)).unwrap();
    let mocked_source = format!("{native_mock}\n{source}");
    host.load_source_with_opts(tool, &mocked_source, opts)
        .unwrap();
    let invocation = registry.get(tool).unwrap().tool.parse(&input).unwrap();
    let ctx = n00n_agent::tools::test_support::stub_ctx(&n00n_agent::AgentMode::Build);
    smol::block_on(invocation.execute(&ctx))
        .output
        .map(|output| match output {
            n00n_agent::ToolOutput::Plain(output) => output.text,
            other => panic!("unexpected output: {other:?}"),
        })
}

fn firecrawl_plugin_opts(max_response_bytes: usize) -> Map<String, Value> {
    let mut opts = Map::new();
    opts.insert("backend".to_string(), json!("firecrawl"));
    opts.insert("max_response_bytes".to_string(), json!(max_response_bytes));
    opts
}

#[test]
fn webfetch_forwards_configured_response_limit_to_firecrawl() {
    let output = execute_plugin_with_native_mock_and_opts(
        "webfetch",
        WEBFETCH_SRC,
        r#"
            n00n.firecrawl = {
                configured = function() return true, nil end,
                scrape = function(url, _format, _timeout, max_response_bytes)
                    if max_response_bytes ~= 2048 then
                        return nil, "wrong response limit: " .. tostring(max_response_bytes)
                    end
                    return { content = "bounded fetch", requested_url = url }, nil
                end,
            }
        "#,
        json!({ "url": "https://example.com" }),
        firecrawl_plugin_opts(2_048),
    )
    .unwrap();

    assert!(
        output.contains("bounded fetch"),
        "unexpected output: {output}"
    );
}

#[test]
fn websearch_accepts_ten_and_forwards_response_limit_to_firecrawl() {
    let output = execute_plugin_with_native_mock_and_opts(
        "websearch",
        WEBSEARCH_SRC,
        r#"
            n00n.firecrawl = {
                configured = function() return true, nil end,
                search = function(_query, limit, max_response_bytes)
                    if limit ~= 10 then
                        return nil, "wrong result limit: " .. tostring(limit)
                    end
                    if max_response_bytes ~= 3072 then
                        return nil, "wrong response limit: " .. tostring(max_response_bytes)
                    end
                    return {{ title = "Bounded search", url = "https://example.com", description = "result" }}, nil
                end,
            }
        "#,
        json!({ "query": "rust", "num_results": 10 }),
        firecrawl_plugin_opts(3_072),
    )
    .unwrap();

    assert!(
        output.contains("Bounded search"),
        "unexpected output: {output}"
    );
}

#[test]
fn webfetch_firecrawl_text_keeps_clean_content_at_small_line_limit() {
    let mut opts = firecrawl_plugin_opts(2_048);
    opts.insert("max_output_lines".to_string(), json!(2));
    let output = execute_plugin_with_native_mock_and_opts(
        "webfetch",
        WEBFETCH_SRC,
        r#"
            n00n.firecrawl = {
                configured = function() return true, nil end,
                scrape = function(url)
                    return {
                        content = "<main>Actual Firecrawl page</main><script>hostile()</script>",
                        requested_url = url,
                    }, nil
                end,
            }
        "#,
        json!({ "url": "https://example.com", "format": "text" }),
        opts,
    )
    .unwrap();

    assert!(
        output.contains("Actual Firecrawl page"),
        "unexpected output: {output}"
    );
    assert!(
        output.contains("Firecrawl scrape API"),
        "unexpected output: {output}"
    );
    assert!(!output.contains("hostile"), "unexpected output: {output}");
}

#[test]
fn websearch_firecrawl_keeps_result_at_small_line_limit() {
    let mut opts = firecrawl_plugin_opts(2_048);
    opts.insert("max_output_lines".to_string(), json!(2));
    let output = execute_plugin_with_native_mock_and_opts(
        "websearch",
        WEBSEARCH_SRC,
        r#"
            n00n.firecrawl = {
                configured = function() return true, nil end,
                search = function()
                    return {{
                        title = "Actual Firecrawl result",
                        url = "https://example.com/result",
                        description = "Useful description",
                    }}, nil
                end,
            }
        "#,
        json!({ "query": "rust" }),
        opts,
    )
    .unwrap();

    assert!(
        output.contains("Actual Firecrawl result"),
        "unexpected output: {output}"
    );
    assert!(
        output.contains("Source: Firecrawl search API"),
        "unexpected output: {output}"
    );
}

#[test]
fn webfetch_direct_html_reports_requested_url_and_keeps_content() {
    let mut opts = Map::new();
    opts.insert("backend".to_string(), json!("direct"));
    opts.insert("max_output_lines".to_string(), json!(2));
    let output = execute_plugin_with_native_mock_and_opts(
        "webfetch",
        WEBFETCH_SRC,
        r#"
            n00n.firecrawl = { configured = function() return false, nil end }
            n00n.net.request = function()
                return {
                    status = 200,
                    content_type = "text/html",
                    body = "<article>Actual direct page</article><style>hidden</style>",
                }, nil
            end
        "#,
        json!({ "url": "https://example.com/start", "format": "text" }),
        opts,
    )
    .unwrap();

    assert!(
        output.contains("Actual direct page"),
        "unexpected output: {output}"
    );
    assert!(
        output.contains("Requested URL: https://example.com/start"),
        "unexpected output: {output}"
    );
    assert!(
        !output.contains("Final URL:"),
        "unexpected output: {output}"
    );
    assert!(!output.contains("hidden"), "unexpected output: {output}");
}

#[test_case::test_case(false, "direct" ; "before_direct_backend")]
#[test_case::test_case(true, "firecrawl" ; "before_firecrawl_backend")]
fn webfetch_rejects_requested_url_credentials_before_backend(
    firecrawl_configured: bool,
    backend: &str,
) {
    const USER: &str = "requested-user";
    const SECRET: &str = "requested-secret";
    let native_mock = format!(
        r#"
            n00n.firecrawl = {{
                configured = function() return {firecrawl_configured}, nil end,
                scrape = function() error("Firecrawl backend must not run") end,
            }}
            n00n.net.request = function() error("direct backend must not run") end
        "#
    );
    let output = execute_plugin_with_native_mock_and_opts(
        "webfetch",
        WEBFETCH_SRC,
        &native_mock,
        json!({ "url": format!("https://{USER}:{SECRET}@example.com/private") }),
        Map::from_iter([("backend".to_string(), json!(backend))]),
    )
    .unwrap_err();

    assert!(
        output.contains("URL must not contain credentials"),
        "unexpected output: {output}"
    );
    assert!(!output.contains(USER), "credential leaked: {output}");
    assert!(!output.contains(SECRET), "credential leaked: {output}");
}

#[test]
fn webfetch_start_header_strips_url_credentials() {
    const USER: &str = "header-user";
    const SECRET: &str = "header-secret";
    let registry = Arc::new(ToolRegistry::new());
    let mut host = PluginHost::new(Arc::clone(&registry)).unwrap();
    host.load_builtins(&PluginsConfig {
        enabled: true,
        names: vec!["webfetch".to_string()],
        opts: HashMap::new(),
    })
    .unwrap();

    let webfetch = registry.get("webfetch").unwrap();
    let invocation = webfetch
        .tool
        .parse(&json!({ "url": format!("https://{USER}:{SECRET}@example.com/private"), "format": "text" }))
        .unwrap();
    let header = smol::block_on(invocation.start_header()).text();

    assert_eq!(header, "https://example.com/private [text]");
    assert!(!header.contains(USER), "credential leaked: {header}");
    assert!(!header.contains(SECRET), "credential leaked: {header}");
}

#[test]
fn webfetch_direct_http_error_does_not_expose_response_body() {
    let output = execute_plugin_with_native_mock_and_opts(
        "webfetch",
        WEBFETCH_SRC,
        r#"
            n00n.firecrawl = { configured = function() return false, nil end }
            n00n.net.request = function()
                return { status = 503, content_type = "text/plain", body = "secret hostile body" }, nil
            end
        "#,
        json!({ "url": "https://example.com" }),
        Map::from_iter([("backend".to_string(), json!("direct"))]),
    )
    .unwrap_err();

    assert!(output.contains("HTTP 503"), "unexpected output: {output}");
    assert!(
        !output.contains("secret hostile body"),
        "unexpected output: {output}"
    );
}

#[test]
fn websearch_exa_allows_more_than_ten_results() {
    let output = execute_plugin_with_native_mock_and_opts(
        "websearch",
        WEBSEARCH_SRC,
        r#"
            n00n.firecrawl = { configured = function() return false, nil end }
            n00n.net.request = function(_url, request)
                if not request.body:find('"numResults":12', 1, true) then
                    return nil, "missing Exa result limit"
                end
                return {
                    status = 200,
                    content_type = "text/event-stream",
                    body = 'data: {"jsonrpc":"2.0","result":{"content":[{"type":"text","text":"Actual Exa result"}]}}',
                }, nil
            end
        "#,
        json!({ "query": "rust", "num_results": 12 }),
        Map::from_iter([("backend".to_string(), json!("exa"))]),
    )
    .unwrap();

    assert!(
        output.contains("Actual Exa result"),
        "unexpected output: {output}"
    );
}

#[test]
fn websearch_exa_accepts_one_hundred_json_results() {
    let output = execute_plugin_with_native_mock_and_opts(
        "websearch",
        WEBSEARCH_SRC,
        r#"
            n00n.firecrawl = { configured = function() return false, nil end }
            n00n.net.request = function(_url, request)
                if not request.body:find('"numResults":100', 1, true) then
                    return nil, "missing Exa result limit"
                end
                return {
                    status = 200,
                    content_type = "application/json; charset=utf-8",
                    body = '{"jsonrpc":"2.0","result":{"content":[{"type":"text","text":"One hundred accepted"}]}}',
                }, nil
            end
        "#,
        json!({ "query": "rust", "num_results": 100 }),
        Map::from_iter([("backend".to_string(), json!("exa"))]),
    )
    .unwrap();

    assert!(
        output.contains("One hundred accepted"),
        "unexpected output: {output}"
    );
}

#[test]
fn websearch_exa_rejects_above_one_hundred_results() {
    let output = execute_plugin_with_native_mock_and_opts(
        "websearch",
        WEBSEARCH_SRC,
        r#"
            n00n.firecrawl = { configured = function() return false, nil end }
            n00n.net.request = function() error("Exa backend must not run") end
        "#,
        json!({ "query": "rust", "num_results": 101 }),
        Map::from_iter([("backend".to_string(), json!("exa"))]),
    )
    .unwrap_err();

    assert!(
        output.contains("Exa num_results must be between 1 and 100"),
        "unexpected output: {output}"
    );
}

#[test_case::test_case(0, "num_results must be at least 1" ; "zero_rejected_globally")]
#[test_case::test_case(11, "Firecrawl num_results must be between 1 and 10" ; "firecrawl_above_ten_rejected")]
fn websearch_validates_result_limit_per_backend(num_results: usize, expected: &str) {
    let output = execute_plugin_with_native_mock_and_opts(
        "websearch",
        WEBSEARCH_SRC,
        r#"
            n00n.firecrawl = {
                configured = function() return true, nil end,
                search = function() error("search should not run") end,
            }
        "#,
        json!({ "query": "rust", "num_results": num_results }),
        firecrawl_plugin_opts(2_048),
    )
    .unwrap_err();

    assert!(output.contains(expected), "unexpected output: {output}");
}

#[test_case::test_case(
    r#"
        local run_dir = n00n.fs.joinpath("/state", "workflows", "aabbccdd")
        local meta_path = n00n.fs.joinpath(run_dir, "meta.json")
        n00n.fs.metadata = function(path)
            if path == run_dir then return { is_dir = true }, nil end
            if path == meta_path then return { is_file = true }, nil end
            return nil, "permission denied"
        end
        n00n.fs.read = function()
            return '{"run_id":"aabbccdd","script_hash":"script-id"}', nil
        end
    "#,
    "permission denied";
    "metadata_failure"
)]
#[test_case::test_case(
    r#"
        local run_dir = n00n.fs.joinpath("/state", "workflows", "aabbccdd")
        local meta_path = n00n.fs.joinpath(run_dir, "meta.json")
        n00n.fs.metadata = function(path)
            if path == run_dir then return { is_dir = true }, nil end
            return { is_file = true }, nil
        end
        n00n.fs.read = function(path)
            if path == meta_path then
                return '{"run_id":"aabbccdd","script_hash":"script-id"}', nil
            end
            return nil, "input/output error"
        end
    "#,
    "input/output error";
    "read_failure"
)]
fn workflow_journal_io_failure_prevents_replay(fs_mock: &str, expected: &str) {
    let native_mock = format!(
        r#"
            n00n.env.state_dir = function() return "/state" end
            n00n.workflow.hash = function() return "script-id" end
            n00n.agent.session = function() error("paid agent call must not start") end
            {fs_mock}
        "#
    );
    let error = execute_plugin_with_native_mock(
        "workflow",
        WORKFLOW_SRC,
        &native_mock,
        json!({
            "script": "meta({ name = 'resume' }); return agent({ prompt = 'paid' })",
            "resume": "aabbccdd",
        }),
    )
    .expect_err("journal I/O failure must reject resume before replay");

    assert!(error.contains(expected), "unexpected error: {error}");
    assert!(
        !error.contains("paid agent call"),
        "paid call was replayed: {error}"
    );
}

#[test]
fn workflow_missing_resume_id_starts_zero_agents() {
    let error = execute_plugin_with_native_mock(
        WORKFLOW_TOOL,
        WORKFLOW_SRC,
        r#"
            n00n.env.state_dir = function() return "/state" end
            n00n.fs.metadata = function() return nil, nil end
            n00n.agent.session = function() error("paid agent call must not start") end
        "#,
        json!({
            "script": "meta({ name = 'resume' }); return agent({ prompt = 'paid' })",
            "resume": "aabbccdd",
        }),
    )
    .expect_err("a missing explicit resume must be rejected");

    assert!(
        error.contains("resume run_id not found"),
        "unexpected error: {error}"
    );
    assert!(
        !error.contains("paid agent call"),
        "paid call started: {error}"
    );
}

#[test]
fn workflow_script_mismatch_starts_zero_agents() {
    let error = execute_plugin_with_native_mock(
        WORKFLOW_TOOL,
        WORKFLOW_SRC,
        r#"
            n00n.env.state_dir = function() return "/state" end
            local run_dir = n00n.fs.joinpath("/state", "workflows", "aabbccdd")
            local meta_path = n00n.fs.joinpath(run_dir, "meta.json")
            n00n.fs.metadata = function(path)
                if path == run_dir then return { is_dir = true }, nil end
                return { is_file = true }, nil
            end
            n00n.fs.read = function(path)
                if path == meta_path then
                    return '{"run_id":"aabbccdd","script_hash":"wrong"}', nil
                end
                return "", nil
            end
            n00n.agent.session = function() error("paid agent call must not start") end
        "#,
        json!({
            "script": "meta({ name = 'resume' }); return agent({ prompt = 'paid' })",
            "resume": "aabbccdd",
        }),
    )
    .expect_err("resume with a different script must be rejected");

    assert!(
        error.contains("script mismatch"),
        "unexpected error: {error}"
    );
    assert!(
        !error.contains("paid agent call"),
        "paid call started: {error}"
    );
}

#[test]
fn codegraph_native_database_does_not_require_cli() {
    let output = execute_plugin_with_native_mock(
        "codegraph",
        CODEGRAPH_SRC,
        r#"
            n00n.codegraph.available = function() return false end
            n00n.codegraph.has_index = function() return true end
            n00n.codegraph.has_database = function() return true end
            n00n.codegraph.explore = function() return "native result", nil end
        "#,
        json!({ "command": "explore", "query": "target", "projectPath": "/fixture" }),
    )
    .expect("native Codegraph operation should succeed without the CLI");

    assert_eq!(output, "native result");
}

#[test_case::test_case(
    "codegraph",
    CODEGRAPH_SRC,
    r"
        n00n.codegraph.available = function() return false end
        n00n.codegraph.has_index = function() return true end
        n00n.codegraph.has_database = function() return false end
    ",
    json!({ "command": "explore", "query": "target", "projectPath": "/fixture" }),
    "codegraph CLI not found";
    "codegraph_without_database"
)]
fn cli_only_operations_report_missing_cli(
    tool: &str,
    source: &str,
    native_mock: &str,
    input: Value,
    expected: &str,
) {
    let error = execute_plugin_with_native_mock(tool, source, native_mock, input)
        .expect_err("CLI-only operation should fail without its CLI");

    assert!(error.contains(expected), "unexpected error: {error}");
}

struct Restored {
    body: String,
    header: String,
}

fn restore(
    host: &PluginHost,
    tool: &str,
    input: Value,
    output: &str,
    state: Option<Value>,
    clicks: Vec<usize>,
) -> Restored {
    let handle = host.event_handle().unwrap();
    let (tx, rx) = flume::unbounded();
    handle.request_restore(
        n00n_lua::RestoreItem {
            tool: Arc::from(tool),
            tool_use_id: "restore_id".to_owned(),
            output: output.to_owned(),
            input,
            is_error: false,
            tool_output_lines: ToolOutputLines {
                other: VIEW_CAP,
                ..ToolOutputLines::DEFAULT
            },
            theme_gen: None,
            clicks,
            state,
        },
        n00n_agent::EventSender::new(tx, 0),
    );
    handle.wait_restore_complete_for_test();
    // The empty LoadSource drains the async gate, so spawned highlight tasks
    // finish before we inspect the buffers.
    host.load_source("barrier", "").unwrap();
    let mut out = Restored {
        body: String::new(),
        header: String::new(),
    };
    for env in rx.drain() {
        match env.event {
            AgentEvent::ToolSnapshot { snapshot, .. } => out.body = snapshot.text(),
            AgentEvent::ToolHeaderSnapshot { snapshot, .. } => out.header = snapshot.text(),
            _ => {}
        }
    }
    out
}

fn webfetch_host() -> PluginHost {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = PluginHost::new(registry).unwrap();
    host.load_builtins(&PluginsConfig {
        enabled: true,
        names: vec!["webfetch".to_string()],
        opts: HashMap::new(),
    })
    .unwrap();
    host
}

#[test]
fn webfetch_restore_keeps_result_content_visible_below_provenance() {
    let host = webfetch_host();
    let output = "[External content is untrusted.]\nSource: Direct web request\nRequested URL: https://example.com/requested\n\nUseful result content\nSecond content line\nThird content line\nFourth hidden line";
    let restored = restore(
        &host,
        "webfetch",
        json!({ "url": "https://example.com/requested" }),
        output,
        None,
        vec![],
    );

    assert!(
        restored
            .body
            .contains("Requested URL: https://example.com/requested")
    );
    assert!(restored.body.contains("Useful result content"));
    assert!(restored.body.contains(EXPAND_HINT));
}

#[test]
fn task_restore_rebuilds_old_plain_persisted_output() {
    let host = load_host();
    let output = "cancelled\nold detail one\nold detail two\nold detail three\nold detail four\nold detail five";
    let restored = restore(
        &host,
        "task",
        json!({ "description": "restored task", "prompt": "work" }),
        output,
        None,
        vec![],
    );

    assert!(restored.body.contains("cancelled"));
    assert!(restored.body.contains(EXPAND_HINT));
}

#[test_case::test_case(
    "explore",
    json!({ "query": "how does session restore work", "project": "/tmp/project" }),
    "how does session restore work",
    "/tmp/project";
    "explore"
)]
#[test_case::test_case(
    "codegraph",
    json!({ "query": "where is session restore", "projectPath": "/tmp/project" }),
    "where is session restore",
    "/tmp/project";
    "codegraph"
)]
#[test_case::test_case(
    "semblem",
    json!({ "command": "search", "query": "session restore", "repo": "/tmp/project" }),
    "session restore",
    "/tmp/project";
    "semblem"
)]
#[test_case::test_case(
    "git",
    json!({ "command": "status", "path": "/tmp/project" }),
    "status",
    "/tmp/project";
    "git"
)]
#[test_case::test_case(
    "github",
    json!({ "command": "list_issues", "owner": "owner", "repo": "repo" }),
    "list_issues",
    "owner/repo";
    "github"
)]
fn explore_restore_uses_shared_three_line_clickable_card(
    tool: &str,
    input: Value,
    header_text: &str,
    header_extra: &str,
) {
    let host = load_host();
    let output = (1..=8)
        .map(|line| format!("result {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let collapsed = restore(&host, tool, input.clone(), &output, None, Vec::new());

    assert!(
        collapsed.header.contains(header_text),
        "header: {}",
        collapsed.header
    );
    assert!(
        collapsed.header.contains(header_extra),
        "header: {}",
        collapsed.header
    );
    assert!(
        collapsed.body.contains("result 3"),
        "body: {}",
        collapsed.body
    );
    assert!(
        !collapsed.body.contains("result 4"),
        "body: {}",
        collapsed.body
    );
    assert!(
        collapsed.body.contains(EXPAND_HINT),
        "body: {}",
        collapsed.body
    );

    let expanded = restore(&host, tool, input, &output, None, vec![0]);
    assert!(
        expanded.body.contains("result 8"),
        "body: {}",
        expanded.body
    );
    assert!(
        !expanded.body.contains(EXPAND_HINT),
        "body: {}",
        expanded.body
    );
}

#[test]
fn bash_restore_renders_real_view() {
    let host = load_host();
    let r = restore(
        &host,
        "bash",
        json!({ "command": "echo hi", "description": "print hi" }),
        "hi",
        None,
        Vec::new(),
    );
    assert!(
        r.body.contains("echo hi"),
        "real view renders the command header; the fallback body is raw output only: {}",
        r.body
    );
    assert!(r.header.contains("print hi"), "header: {}", r.header);
}

/// Phase 1: children render through their own real views (grep gutter,
/// bash command header), not the raw-llm fallback. Phase 2: a replayed
/// click inside grep's range reaches its real toggle and expands only it.
#[test]
fn batch_restore_renders_real_children_and_click_expands_grep() {
    let host = load_host();
    let input: Value = serde_json::from_str(BATCH_INPUT_GREP_BASH).unwrap();
    let collapsed = restore(
        &host,
        "batch",
        input.clone(),
        "whatever",
        Some(batch_state()),
        Vec::new(),
    );
    let text = &collapsed.body;
    assert!(text.contains("grep> "), "grep child header: {text}");
    assert!(text.contains("bash> "), "bash child header: {text}");
    // grep's real view reformats `nr:` into gutter lines.
    assert!(text.contains(" 1 fn main() {}"), "grep gutter: {text}");
    assert!(
        !text.contains("1: fn main"),
        "raw llm text means the child restore degraded to fallback: {text}"
    );
    assert!(
        text.contains(EXPAND_HINT),
        "grep view collapsed past its cap: {text}"
    );
    assert!(
        text.contains("echo hello-from-bash"),
        "bash child rendered its real view (command header): {text}"
    );
    assert!(
        text.lines().any(|l| l.trim() == "hello-from-bash"),
        "bash output line: {text}"
    );

    // Rows are 1-based (row 0 = header), so snapshot line i = row i+1.
    let notice_row = 1 + collapsed
        .body
        .lines()
        .position(|l| l.contains(EXPAND_HINT))
        .expect("grep truncation notice in collapsed render");
    let clicked = restore(
        &host,
        "batch",
        input,
        "whatever",
        Some(batch_state()),
        vec![notice_row],
    );
    let text = &clicked.body;
    assert!(
        text.contains("10 fn other() {}"),
        "expanded grep tail visible: {text}"
    );
    assert!(
        !text.contains(EXPAND_HINT),
        "grep no longer collapsed: {text}"
    );
    assert!(
        text.contains("hello-from-bash"),
        "bash child untouched: {text}"
    );
}

/// Header fn that yields (e.g. highlight) must work, not fall back.
#[test]
fn restore_header_fn_may_await_async_apis() {
    let host = PluginHost::new(Arc::new(ToolRegistry::new())).unwrap();
    host.load_source(
        "hdr",
        r#"n00n.api.register_tool({
            name = "hdr_await",
            description = "t",
            schema = { type = "object", properties = {} },
            handler = function() return "ok" end,
            header = function(input)
                local hl = n00n.ui.highlight("echo marker", "bash") or { { { "echo marker" } } }
                local buf = n00n.ui.buf()
                buf:set_lines(hl)
                return buf
            end,
            restore = function(input, output)
                local buf = n00n.ui.buf()
                buf:line("body")
                return buf
            end,
        })"#,
    )
    .unwrap();
    let r = restore(&host, "hdr_await", json!({}), "ok", None, Vec::new());
    assert_eq!(r.body.trim(), "body");
    assert!(
        r.header.contains("echo marker"),
        "awaiting header fn must survive: {}",
        r.header
    );
}

/// Standalone edit diffs never truncate (Rust hardcodes it), so batch
/// children must match: whole diff, `-` lines numbered by finding the new
/// text in the edited file, `+` lines with a blank gutter.
#[test]
fn multiedit_batch_child_shows_full_numbered_diff() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.rs");
    std::fs::write(&path, "top\nzzz\nn1\nn2\nn3\nn4\nn5\nbottom\n").unwrap();

    let host = PluginHost::with_all_builtins(Arc::new(ToolRegistry::new())).unwrap();
    let input = json!({ "tool_calls": [{ "tool": "multiedit", "parameters": {
        "path": path.to_str().unwrap(),
        "edits": [{ "old_string": "old1\nold2\nold3\nold4\nold5", "new_string": "n1\nn2\nn3\nn4\nn5" }],
    }}]});
    let state = json!({ "children": [
        { "tool": "multiedit", "status": "success", "output": "applied 1 edit" },
    ]});
    let r = restore(&host, "batch", input, "whatever", Some(state), Vec::new());

    let text = &r.body;
    // keep = "head" truncation would cut the tail, so the last added line
    // present plus no collapse notice proves the 10-line diff is whole.
    assert!(
        text.contains("+ n5") && !text.contains(EXPAND_HINT),
        "edit diffs must never truncate: {text}"
    );
    assert!(
        text.contains("3 - old1") && text.contains("7 - old5"),
        "removed lines numbered from the new text's file position: {text}"
    );
    assert!(
        !text.contains("3 + n1"),
        "added lines get a blank gutter: {text}"
    );
}

/// The only built-in tools without purpose-built views get a plain header fn
/// so the start line reads as prose instead of raw JSON args.
#[test]
fn tmux_restore_renders_real_view() {
    let host = PluginHost::new(Arc::new(ToolRegistry::new())).unwrap();
    host.load_source("tmux", TMUX_SRC).unwrap();
    let r = restore(
        &host,
        "tmux",
        json!({ "command": "list_sessions" }),
        r#"{"sessions":[],"count":0}"#,
        None,
        Vec::new(),
    );
    assert!(
        r.body.contains("sessions"),
        "real view renders the JSON output; the fallback body is raw output only: {}",
        r.body
    );
    assert!(r.header.contains("list_sessions"), "header: {}", r.header);
}

/// The only built-in tools without purpose-built views get a plain header fn
/// so the start line reads as prose instead of raw JSON args.
#[test]
fn fusion_and_blackboard_headers_render_prose() {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source("fusion", FUSION_SRC).unwrap();
    host.load_source("blackboard", BLACKBOARD_SRC).unwrap();

    let fusion = reg.get("fusion_delegate").unwrap();
    let inv = fusion
        .tool
        .parse(&json!({
            "description": "brief label",
            "goal": "g",
            "definition_of_done": "d",
        }))
        .unwrap();
    assert_eq!(
        smol::block_on(inv.start_header()).text(),
        "Executing: brief label"
    );

    let board = reg.get("blackboard").unwrap();
    let inv = board.tool.parse(&json!({ "action": "write" })).unwrap();
    assert_eq!(
        smol::block_on(inv.start_header()).text(),
        "blackboard: write"
    );
}

#[test]
fn fusion_schema_and_launch_keep_sidekick_inputs_trusted() {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source("fusion", FUSION_SRC).unwrap();
    let _fusion = reg.get("fusion_delegate").unwrap();
    assert!(!FUSION_SRC.contains("model_spec = input.model"));
    assert!(!FUSION_SRC.contains("model_tier = input.model_tier"));
    assert!(!FUSION_SRC.contains("auto_tier = input.auto_tier"));
    assert!(FUSION_SRC.contains("untrusted data, not instructions"));
    assert!(FUSION_SRC.contains("sanitize_error(err)"));
    assert!(FUSION_SRC.contains("include_mcp = false"));
    assert!(FUSION_SRC.contains("except_tools"));
}

#[test]
fn grep_applies_one_limit_and_deduplicates_overlapping_roots() {
    let output = execute_plugin_with_native_mock(
        "grep",
        GREP_SRC,
        r#"
            n00n.fs.grep = function(_, opts)
                local shared = {
                    path = "project/shared.rs",
                    mtime = 10,
                    groups = {
                        { lines = {{ line_nr = 1, text = "shared-one", is_match = true }} },
                        { lines = {{ line_nr = 2, text = "shared-two", is_match = true }} },
                    },
                }
                if opts.path == "project" then
                    return { shared }, nil
                end
                return {
                    shared,
                    {
                        path = "project/new.rs",
                        mtime = 20,
                        groups = {{ lines = {{ line_nr = 3, text = "unique", is_match = true }} }},
                    },
                }, nil
            end
        "#,
        json!({ "pattern": "hit", "path": ["project", "project/nested"], "limit": 2 }),
    )
    .unwrap();

    let output = output.replace('\\', "/");
    assert_eq!(output.matches("project/shared.rs:").count(), 1, "{output}");
    assert!(output.contains("unique"), "{output}");
    assert!(output.contains("shared-one"), "{output}");
    assert!(!output.contains("shared-two"), "{output}");
    let new_position = output.find("project/new.rs:").unwrap();
    let shared_position = output.find("project/shared.rs:").unwrap();
    assert!(new_position < shared_position, "{output}");
}

#[test]
fn grep_reports_partial_path_failures() {
    let output = execute_plugin_with_native_mock(
        "grep",
        GREP_SRC,
        r#"
            n00n.fs.grep = function(_, opts)
                if opts.path == "missing" then
                    return nil, "path not found"
                end
                return {{
                    path = "valid.rs",
                    mtime = 1,
                    groups = {{ lines = {{ line_nr = 1, text = "found", is_match = true }} }},
                }}, nil
            end
        "#,
        json!({ "pattern": "found", "path": ["valid", "missing"] }),
    )
    .unwrap();

    assert!(output.contains("found"), "{output}");
    assert!(
        output.contains("Warning: some paths could not be searched: missing: path not found"),
        "{output}"
    );
}

#[test]
fn grep_reports_partial_path_failures_when_successful_path_is_empty() {
    let output = execute_plugin_with_native_mock(
        "grep",
        GREP_SRC,
        r#"
            n00n.fs.grep = function(_, opts)
                if opts.path == "missing" then
                    return nil, "path not found"
                end
                return {}, nil
            end
        "#,
        json!({ "pattern": "found", "path": ["empty", "missing"] }),
    )
    .unwrap();

    assert!(output.contains("No files found"), "{output}");
    assert!(
        output.contains("Warning: some paths could not be searched: missing: path not found"),
        "{output}"
    );
}

#[test]
fn grep_searches_default_root_when_path_is_omitted() {
    let output = execute_plugin_with_native_mock(
        "grep",
        GREP_SRC,
        r#"
            n00n.fs.grep = function(_, opts)
                if opts.path ~= nil then
                    return nil, "expected nil path"
                end
                return {{
                    path = "default.rs",
                    mtime = 1,
                    groups = {{ lines = {{ line_nr = 1, text = "default-hit", is_match = true }} }},
                }}, nil
            end
        "#,
        json!({ "pattern": "default" }),
    )
    .unwrap();

    assert!(output.contains("default-hit"), "{output}");
}

#[test]
fn grep_header_accepts_multiple_paths() {
    let host = load_host();
    let restored = restore(
        &host,
        "grep",
        json!({ "pattern": "fn", "path": ["src", "tests"] }),
        "src/main.rs:\n  1: fn main",
        None,
        vec![],
    );

    assert!(
        restored.header.contains("src"),
        "header: {}",
        restored.header
    );
    assert!(
        restored.header.contains("tests"),
        "header: {}",
        restored.header
    );
}
