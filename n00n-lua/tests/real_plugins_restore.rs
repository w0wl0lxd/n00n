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
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

use n00n_agent::AgentEvent;
use n00n_agent::CancelTrigger;
use n00n_agent::tools::ToolRegistry;
use n00n_config::{ToolOutputLines, providers::Tier};
use n00n_lua::PluginHost;
use serde_json::{Value, json};

const ARBOR_SRC: &str = include_str!("../../plugins/arbor/init.lua");
const BASH_SRC: &str = include_str!("../../plugins/bash/init.lua");
const BATCH_SRC: &str = include_str!("../../plugins/batch/init.lua");
const BLACKBOARD_SRC: &str = include_str!("../../plugins/blackboard/init.lua");
const CODEGRAPH_SRC: &str = include_str!("../../plugins/codegraph/init.lua");
const EXPLORE_SRC: &str = include_str!("../../plugins/explore/init.lua");
const FUSION_SRC: &str = include_str!("../../plugins/fusion/init.lua");
const GREP_SRC: &str = include_str!("../../plugins/grep/init.lua");
const SEMBLEM_SRC: &str = include_str!("../../plugins/semblem/init.lua");
const TASK_SRC: &str = include_str!("../../plugins/task/init.lua");
const TMUX_SRC: &str = include_str!("../../plugins/tmux/init.lua");
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
    host.load_source("arbor", ARBOR_SRC).unwrap();
    host.load_source("bash", BASH_SRC).unwrap();
    host.load_source("batch", BATCH_SRC).unwrap();
    host.load_source("codegraph", CODEGRAPH_SRC).unwrap();
    host.load_source("explore", EXPLORE_SRC).unwrap();
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

const FUSION_MODEL_MOCK: &str = r#"
    n00n.agent.resolve_model = function(ctx, opts)
        return { spec = "resolved/" .. tostring(opts.spec or opts.tier) }
    end
    n00n.agent.system_prompt = function() return "system" end
    n00n.agent.tools = function() return {} end
    n00n.agent.usage_cost = function() return 0, nil end
    n00n.agent.session = function(ctx, opts)
        local sess = {}
        function sess:prompt() return { text = opts.model_spec } end
        function sess:close() end
        return sess
    end
"#;

fn execute_fusion_result(
    input: Value,
    tier: Tier,
    enabled: bool,
    native_mock: &str,
) -> n00n_agent::tools::registry::ToolExecResult {
    let registry = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&registry)).unwrap();
    host.load_source("fusion", &format!("{native_mock}\n{FUSION_SRC}"))
        .unwrap();
    let invocation = registry
        .get("fusion_delegate")
        .unwrap()
        .tool
        .parse(&input)
        .unwrap();
    let mut ctx = n00n_agent::tools::test_support::stub_ctx(&n00n_agent::AgentMode::Build);
    let fusion = &mut Arc::make_mut(&mut ctx.config).fusion;
    fusion.enabled = enabled;
    fusion.sidekick_tier = tier;
    smol::block_on(invocation.execute(&ctx))
}

fn execute_fusion(
    input: Value,
    tier: Tier,
    enabled: bool,
    native_mock: &str,
) -> Result<String, String> {
    execute_fusion_result(input, tier, enabled, native_mock)
        .output
        .map(|output| match output {
            n00n_agent::ToolOutput::Plain(output) => output.text,
            other => panic!("unexpected output: {other:?}"),
        })
}

fn execute_plugin_with_native_mock(
    tool: &str,
    source: &str,
    native_mock: &str,
    input: Value,
) -> Result<String, String> {
    let registry = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&registry)).unwrap();
    let mocked_source = format!("{native_mock}\n{source}");
    host.load_source(tool, &mocked_source).unwrap();
    let invocation = registry.get(tool).unwrap().tool.parse(&input).unwrap();
    let ctx = n00n_agent::tools::test_support::stub_ctx(&n00n_agent::AgentMode::Build);
    smol::block_on(invocation.execute(&ctx))
        .output
        .map(|output| match output {
            n00n_agent::ToolOutput::Plain(output) => output.text,
            other => panic!("unexpected output: {other:?}"),
        })
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

#[test_case::test_case(
    json!({ "command": "callers", "symbol": "target", "project": "/fixture" }),
    "callers of target\n  caller (function) src/lib.rs:7";
    "callers"
)]
#[test_case::test_case(
    json!({ "command": "callers", "symbol": "orphan", "project": "/fixture" }),
    "No callers found for symbol 'orphan'";
    "empty_callers"
)]
#[test_case::test_case(
    json!({ "command": "callees", "symbol": "target", "project": "/fixture" }),
    "callees of target\n  callee (function) src/lib.rs:11";
    "callees"
)]
#[test_case::test_case(
    json!({
        "command": "trace_path",
        "from_symbol": "caller",
        "to_symbol": "target",
        "project": "/fixture"
    }),
    "trace_path caller -> target\n  target (function) src/lib.rs:9";
    "trace_path"
)]
fn arbor_native_graph_operations_do_not_require_cli(input: Value, expected: &str) {
    let output = execute_plugin_with_native_mock(
        "arbor",
        ARBOR_SRC,
        r#"
            n00n.arbor.available = function() return false end
            n00n.arbor.check_binary = function() error("CLI must not be called") end
            n00n.arbor.graph_index_available = function() return true end
            n00n.arbor.ensure_fresh_index = function() error("CLI must not be called") end
            n00n.arbor.callers = function() error("CLI must not be called") end
            n00n.arbor.callees = function() error("CLI must not be called") end
            n00n.arbor.graph_callers = function(symbol)
                if symbol == "orphan" then return {} end
                return { { name = "caller", kind = "function", path = "src/lib.rs", line = 7 } }
            end
            n00n.arbor.graph_callees = function()
                return { { name = "callee", kind = "function", path = "src/lib.rs", line = 11 } }
            end
            n00n.arbor.graph_trace_path = function()
                return { { name = "target", kind = "function", path = "src/lib.rs", line = 9 } }
            end
        "#,
        input,
    )
    .expect("native Arbor operation should succeed without the CLI");

    assert_eq!(output, expected);
}

#[test_case::test_case(())]
fn arbor_native_refresh_failure_does_not_fall_back_to_cli(_case: ()) {
    let error = execute_plugin_with_native_mock(
        "arbor",
        ARBOR_SRC,
        r#"
            n00n.arbor.available = function() return true end
            n00n.arbor.graph_index_available = function() return true end
            n00n.arbor.ensure_fresh_index = function() error("refresh failed") end
            n00n.arbor.callers = function() error("CLI must not be called") end
        "#,
        json!({ "command": "callers", "symbol": "target", "project": "/fixture" }),
    )
    .expect_err("native refresh failure must not query a stale CLI index");

    assert!(
        error.contains("failed to refresh graph index"),
        "unexpected error: {error}"
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
    "arbor",
    ARBOR_SRC,
    r"n00n.arbor.available = function() return false end",
    json!({ "command": "map", "project": "/fixture" }),
    "Arbor CLI not found";
    "arbor_map"
)]
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
    "arbor",
    json!({ "command": "callers", "symbol": "restore_item", "project": "/tmp/project" }),
    "callers restore_item",
    "/tmp/project";
    "arbor"
)]
#[test_case::test_case(
    "semblem",
    json!({ "command": "search", "query": "session restore", "repo": "/tmp/project" }),
    "session restore",
    "/tmp/project";
    "semblem"
)]
fn explore_restore_uses_shared_three_line_clickable_card(
    tool: &str,
    input: Value,
    header_text: &str,
    project: &str,
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
        collapsed.header.contains(project),
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
fn fusion_is_rejected_when_disabled() {
    let error = execute_fusion(
        json!({"description":"test brief", "goal":"do it", "definition_of_done":"it works"}),
        Tier::Weak,
        false,
        FUSION_MODEL_MOCK,
    )
    .unwrap_err();
    assert_eq!(error, "Fusion sidekick error: Fusion is disabled");
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

#[test]
fn fusion_rejects_compaction_sidekick_tier() {
    let error = execute_fusion(
        json!({"description":"test brief", "goal":"do it", "definition_of_done":"it works"}),
        Tier::Compaction,
        true,
        FUSION_MODEL_MOCK,
    )
    .unwrap_err();
    assert_eq!(error, "Fusion sidekick error: invalid sidekick tier");
}

#[test]
fn fusion_model_resolution_failure_is_sanitized() {
    let error = execute_fusion(
        json!({"description":"test brief", "goal":"do it", "definition_of_done":"it works"}),
        Tier::Weak,
        true,
        r#"n00n.agent.resolve_model = function() return nil, "model unavailable" end"#,
    )
    .unwrap_err();
    assert_eq!(error, "Fusion sidekick error: model resolution failed");
}
