#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use n00n_agent::template::env_vars;
use n00n_agent::tools::{
    DescriptionContext, ToolAudience, ToolFilter, ToolRegistry, ToolSource, timeout_annotation,
};
use n00n_config::{AlwaysThinking, PluginsConfig, ToolOutputLines};
use n00n_lua::{PluginError, PluginHost, WARM_TOOL_CAP};
use n00n_providers::provider::{BoxFuture, Provider};
use n00n_providers::{
    AgentError, ContentBlock, Message, Model, ProviderEvent, RequestOptions, Role, StopReason,
    StreamResponse, TokenUsage,
};
use n00n_storage::id::SessionRef;

const TOOL_DEFINITIONS_BYTE_BUDGET: usize = 42_000;

fn fresh_registry() -> Arc<ToolRegistry> {
    Arc::new(ToolRegistry::new())
}

fn builtins_host() -> (Arc<ToolRegistry>, PluginHost) {
    let reg = fresh_registry();
    let mut host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_builtins(&PluginsConfig::from_plugins(&HashMap::new()))
        .unwrap();
    (reg, host)
}

#[test]
fn builtin_main_tool_definitions_stay_within_prompt_budget() {
    let (registry, _host) = builtins_host();
    let definitions = registry.definitions(
        &env_vars(),
        &DescriptionContext {
            filter: &ToolFilter::All,
            audience: ToolAudience::MAIN,
            workflow: false,
        },
        true,
    );
    let bytes = serde_json::to_vec_pretty(&definitions).unwrap().len() + 1;

    for required in ["task", "team", "workflow", "agent_control"] {
        assert!(
            registry.has(required),
            "required tool disappeared: {required}"
        );
    }
    assert!(
        bytes <= TOOL_DEFINITIONS_BYTE_BUDGET,
        "builtin main tool definitions use {bytes} bytes; budget is {TOOL_DEFINITIONS_BYTE_BUDGET}"
    );
}

fn exec_tool(reg: &ToolRegistry, name: &str, input: serde_json::Value) -> Result<String, String> {
    exec_tool_in(reg, name, input, None)
}

fn exec_tool_in(
    reg: &ToolRegistry,
    name: &str,
    input: serde_json::Value,
    registry_override: Option<Arc<ToolRegistry>>,
) -> Result<String, String> {
    exec_output_in(reg, name, input, registry_override).map(|out| match out {
        n00n_agent::ToolOutput::Plain(s) => s.text,
        other => panic!("unexpected output: {other:?}"),
    })
}

fn exec_tool_output(
    reg: &ToolRegistry,
    name: &str,
    input: serde_json::Value,
) -> Result<n00n_agent::ToolOutput, String> {
    exec_output_in(reg, name, input, None)
}

fn exec_output_in(
    reg: &ToolRegistry,
    name: &str,
    input: serde_json::Value,
    registry_override: Option<Arc<ToolRegistry>>,
) -> Result<n00n_agent::ToolOutput, String> {
    let entry = reg
        .get(name)
        .unwrap_or_else(|| panic!("tool {name} not registered"));
    let inv = entry.tool.parse(&input).expect("parse failed");
    let mut ctx = n00n_agent::tools::test_support::stub_ctx(&n00n_agent::AgentMode::Build);
    if let Some(r) = registry_override {
        ctx.registry = r;
    }
    smol::block_on(async { inv.execute(&ctx).await }).output
}

const ECHO_PLUGIN: &str = r#"
n00n.api.register_tool({
    name = "echo_",
    description = "echo",
    schema = {
        type = "object",
        properties = { msg = { type = "string" } },
        required = { "msg" }
    },
    audiences = { "main" },
    handler = function(input, ctx)
        return input.msg
    end
})
"#;

const MINIMAL_SCHEMA: &str =
    r#"{ type = "object", properties = {}, additionalProperties = false }"#;

const STRING_FIELD_SCHEMA: &str = r#"{
    type = "object",
    properties = { url = { type = "string" } },
    required = { "url" },
}"#;

const INVALID_PERMISSION_SCOPE_ERR: &str = "not in schema properties or not type 'string'";
const BAD_NAME_SRC: &str = r#"name = "bad name!", description = "test""#;
const EMPTY_DESC_SRC: &str = r#"name = "valid_name", description = """#;
const EMPTY_AUD_SRC: &str = r#"name = "no_aud", description = "test", audiences = {}"#;
const UNKNOWN_AUD_SRC: &str =
    r#"name = "bad_aud", description = "test", audiences = { "wurkflow" }"#;
const STRING_EXAMPLES_SRC: &str = r#"name = "ex_bad", description = "test", examples = "[]""#;
const TIMEOUT_FIELD_NOT_IN_SCHEMA_SRC: &str = r#"name = "to_bad", description = "test", start_annotation = { field = "timeout", kind = "timeout" }"#;
const SCOPE_MISSING_FIELD_SRC: &str =
    r#"name = "bad_scope", description = "test", permission_scopes = "nonexistent""#;
const SCOPE_NON_STRING_FIELD_SRC: &str =
    r#"name = "bad_scope", description = "test", permission_scopes = "count""#;
const OLD_SCOPE_KEY_SRC: &str =
    r#"name = "old_key", description = "test", permission_scope = "url""#;
const WRONG_TYPE_SCOPES_SRC: &str =
    r#"name = "num_scope", description = "test", permission_scopes = 42"#;
const NON_STRING_FIELD_SCHEMA: &str = r#"{
    type = "object",
    properties = { count = { type = "integer" } },
    required = { "count" },
}"#;

const CODE_SCHEMA: &str = r#"{
    type = "object",
    properties = { code = { type = "string" } },
    required = { "code" },
}"#;

const TIMEOUT_SCHEMA: &str = r#"{
    type = "object",
    properties = { timeout = { type = "integer" } },
    required = { "timeout" },
}"#;

const ARRAY_SCHEMA: &str = r#"{
    type = "object",
    properties = { edits = { type = "array", items = { type = "integer" } } },
    required = { "edits" },
}"#;

const START_ANNOTATION_COUNT_NON_ARRAY_SRC: &str =
    r#"name = "sa_bad", description = "test", start_annotation = "name""#;
const STRING_NAME_SCHEMA: &str = r#"{
    type = "object",
    properties = { name = { type = "string" } },
    required = { "name" },
}"#;
const JOB_BAD_CWD: &str = "~/definitely/not/a/dir";
const JOB_BAD_CWD_ERR_PREFIX: &str = "cwd is not a directory: ";
const NIL_WITHOUT_JOBS_ERR: &str =
    "handler returned nil without calling ctx:finish() or starting jobs";
const FINISH_CALLED_TWICE_ERR: &str = "ctx:finish() already called";
const DEADLINE_ALREADY_SET_ERR: &str = "ctx:set_deadline() already called";
const TIMED_OUT_SUBSTR: &str = "timed out";
const ALREADY_CALLED_ERR: &str = "already called";
const UNKNOWN_FIELD_ERR: &str = "unknown field";
const PERMISSION_DENIED_MSG: &str = "permission denied";

#[test]
fn stdlib_globals_accessible() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    for global in &["os", "debug", "string", "table", "math"] {
        let source =
            format!(r#"if {global} == nil then error("stdlib missing: {global} is nil") end"#);
        host.load_source(&format!("stdlib_check_{global}"), &source)
            .unwrap_or_else(|e| panic!("stdlib check for {global} failed: {e}"));
    }
}

#[test]
fn dangerous_globals_blocked() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    for global in &["io", "package"] {
        let source =
            format!(r#"if {global} ~= nil then error("sandbox leak: {global} is not nil") end"#);
        host.load_source(&format!("sandbox_check_{global}"), &source)
            .unwrap_or_else(|e| panic!("sandbox check for {global} failed: {e}"));
    }
}

#[test]
fn register_echo_tool() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source("echo_plugin", ECHO_PLUGIN).unwrap();

    let entry = reg.get("echo_").expect("echo_ tool not registered");
    assert_eq!(entry.tool.name(), "echo_");
    assert!(
        matches!(entry.source, ToolSource::Lua { ref plugin } if plugin.as_ref() == "echo_plugin"),
    );
    assert_eq!(entry.tool.tool_kind(), None);

    let out = exec_tool(&reg, "echo_", serde_json::json!({"msg": "hello"})).unwrap();
    assert_eq!(out, "hello");
}

#[test]
fn unload_round_trip() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    host.load_source("unload_test", ECHO_PLUGIN).unwrap();
    assert!(reg.has("echo_"));

    host.unload("unload_test").unwrap();
    assert!(!reg.has("echo_"));
}

#[test_case::test_case(BAD_NAME_SRC, MINIMAL_SCHEMA, "invalid name" ; "invalid_tool_name")]
#[test_case::test_case(EMPTY_DESC_SRC, MINIMAL_SCHEMA, "description must be non-empty" ; "empty_description")]
#[test_case::test_case(EMPTY_AUD_SRC, MINIMAL_SCHEMA, "audiences" ; "empty_audiences")]
#[test_case::test_case(UNKNOWN_AUD_SRC, MINIMAL_SCHEMA, "unknown audience" ; "unknown_audience")]
#[test_case::test_case(STRING_EXAMPLES_SRC, MINIMAL_SCHEMA, "'examples' must be a table" ; "string_examples")]
#[test_case::test_case(TIMEOUT_FIELD_NOT_IN_SCHEMA_SRC, MINIMAL_SCHEMA, "not type 'integer'" ; "timeout_field_not_in_schema")]
#[test_case::test_case(SCOPE_MISSING_FIELD_SRC, STRING_FIELD_SCHEMA, INVALID_PERMISSION_SCOPE_ERR ; "permission_scopes_missing_field")]
#[test_case::test_case(SCOPE_NON_STRING_FIELD_SRC, NON_STRING_FIELD_SCHEMA, INVALID_PERMISSION_SCOPE_ERR ; "permission_scopes_non_string_field")]
#[test_case::test_case(OLD_SCOPE_KEY_SRC, MINIMAL_SCHEMA, "'permission_scope' was removed" ; "old_permission_scope_key")]
#[test_case::test_case(WRONG_TYPE_SCOPES_SRC, MINIMAL_SCHEMA, "'permission_scopes' must be a string field name or a function" ; "permission_scopes_wrong_type")]
fn registration_validation_rejects(fields: &str, schema: &str, expected_err: &str) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            {fields},
            schema = {schema},
            handler = function(input, ctx) return "" end
        }})"#,
    );
    let err = host
        .load_source("validation_test", &src)
        .expect_err("expected validation error");
    assert!(matches!(err, PluginError::Lua { .. }));
    assert!(err.to_string().contains(expected_err), "got: {err}");
}

#[test]
fn permission_scopes_valid_string_field_accepted() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"n00n.api.register_tool({{
            name = "ok_scope",
            description = "test",
            schema = {STRING_FIELD_SCHEMA},
            permission_scopes = "url",
            handler = function() return "" end
        }})"#,
    );
    host.load_source("ok_scope_plugin", &src).unwrap();
    assert!(reg.has("ok_scope"));
}

#[test]
fn tool_kind_flows_to_trait() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"n00n.api.register_tool({{
            name = "my_fetcher",
            description = "fetches things",
            schema = {MINIMAL_SCHEMA},
            kind = "fetch",
            handler = function() return "" end
        }})"#,
    );
    host.load_source("kind_plugin", &src).unwrap();
    let entry = reg.get("my_fetcher").expect("tool not registered");
    assert_eq!(entry.tool.tool_kind(), Some("fetch"));
}

/// `get_tool` handles are the boundary between plugins: they never throw
/// (errors become nil) and their returns are normalized, so a composing
/// caller like batch needs no pcall of its own.
#[test]
fn get_tool_returns_normalized_header_and_restore_handles() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"
        n00n.api.register_tool({{
            name = "styled_tool",
            description = "t",
            schema = {STRING_FIELD_SCHEMA},
            handler = function() return "ok" end,
            header = function(input) return "H:" .. input.url end,
            restore = function(input)
                if input.with_body then
                    local b = n00n.ui.buf()
                    b:line("body")
                    return {{ body = b }}
                end
                return {{}}
            end,
        }})
        n00n.api.register_tool({{
            name = "throwing_tool",
            description = "t",
            schema = {MINIMAL_SCHEMA},
            handler = function() return "ok" end,
            header = function() error("kaboom") end,
            restore = function() error("kaboom") end,
        }})
        n00n.api.register_tool({{
            name = "handle_probe",
            description = "p",
            schema = {MINIMAL_SCHEMA},
            handler = function()
                local t = n00n.api.get_tool("styled_tool")
                if not t then return nil, "not found" end
                local thrower = n00n.api.get_tool("throwing_tool")
                local h = t.header({{ url = "abc" }})
                return table.concat({{
                    t.name,
                    h[1][1] .. "/" .. h[1][2],
                    type(t.restore({{}}, "", false, nil)),
                    type(t.restore({{ with_body = true }}, "", false, nil)),
                    tostring(thrower.header({{}}) == nil),
                    tostring(thrower.restore({{}}, "", false, nil) == nil),
                    tostring(n00n.api.get_tool("nope_tool") == nil),
                    type(n00n.api.get_tool("handle_probe").header),
                }}, "|")
            end
        }})
        "#,
    );
    host.load_source("get_tool_plugin", &src).unwrap();

    let out = exec_tool(&reg, "handle_probe", serde_json::json!({})).unwrap();
    assert_eq!(
        out,
        "styled_tool|H:abc/tool|nil|userdata|true|true|true|nil"
    );
}

#[test]
fn handler_state_flows_to_tool_output_and_serde() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "stateful",
            description = "t",
            schema = {MINIMAL_SCHEMA},
            handler = function()
                return {{ llm_output = "done", state = {{ n = 3, tag = "hi" }} }}
            end
        }})"#,
    );
    host.load_source("state_plugin", &src).unwrap();

    let entry = reg.get("stateful").unwrap();
    let inv = entry.tool.parse(&serde_json::json!({})).unwrap();
    let ctx = n00n_agent::tools::test_support::stub_ctx(&n00n_agent::AgentMode::Build);
    let out = smol::block_on(async { inv.execute(&ctx).await })
        .output
        .unwrap();
    let expected = serde_json::json!({ "n": 3, "tag": "hi" });
    assert_eq!(out.state(), Some(&expected));

    let json = serde_json::to_string(&out).unwrap();
    let parsed: n00n_agent::ToolOutput = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.state(), Some(&expected), "state must survive serde");
}

#[test]
fn handler_usage_metadata_flows_to_tool_output_without_private_fields() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "usage_metadata",
            description = "t",
            schema = {MINIMAL_SCHEMA},
            handler = function()
                return {{
                    llm_output = "done",
                    cost = 0.125,
                    usage = {{
                        fresh_input_tokens = 5,
                        cache_read_tokens = 7,
                        cache_write_tokens = 11,
                        input_tokens = 23,
                        output_tokens = 13,
                        raw_prompt = "PRIVATE_PROMPT",
                    }},
                    raw_payload = "PRIVATE_PAYLOAD",
                    state = {{ restore = "kept" }},
                }}
            end
        }})"#,
    );
    host.load_source("usage_metadata_plugin", &src).unwrap();

    let output = exec_tool_output(&reg, "usage_metadata", serde_json::json!({})).unwrap();
    let expected = serde_json::json!({
        "cost": 0.125,
        "usage": {
            "fresh_input_tokens": 5,
            "cache_read_tokens": 7,
            "cache_write_tokens": 11,
            "input_tokens": 23,
            "output_tokens": 13,
        },
    });
    assert_eq!(serde_json::to_value(output.telemetry()).unwrap(), expected);
    assert_eq!(
        output.state(),
        Some(&serde_json::json!({ "restore": "kept" })),
        "telemetry must not replace restore state"
    );

    let serialized = serde_json::to_string(&output).unwrap();
    let _: n00n_agent::ToolTelemetry = serde_json::from_value(expected.clone())
        .unwrap_or_else(|error| panic!("failed to restore telemetry {expected}: {error}"));
    let restored: n00n_agent::ToolOutput = serde_json::from_str(&serialized)
        .unwrap_or_else(|error| panic!("failed to restore {serialized}: {error}"));
    assert_eq!(
        serde_json::to_value(restored.telemetry()).unwrap(),
        expected,
        "telemetry must survive serde"
    );
    assert_eq!(
        restored.state(),
        Some(&serde_json::json!({ "restore": "kept" }))
    );
}

#[test]
fn image_and_diff_outputs_preserve_first_class_telemetry() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "telemetry_image",
            description = "t",
            schema = {MINIMAL_SCHEMA},
            handler = function()
                return {{
                    llm_output = "caption",
                    image = {{ media_type = "image/png", data = "aGVsbG8=" }},
                    cost = 0.25,
                }}
            end
        }})
        n00n.api.register_tool({{
            name = "telemetry_diff",
            description = "t",
            schema = {MINIMAL_SCHEMA},
            handler = function()
                return {{
                    llm_output = "changed",
                    diff_path = "src/lib.rs",
                    diff_before = "old",
                    diff_after = "new",
                    usage = {{ input_tokens = 9, output_tokens = 3 }},
                }}
            end
        }})"#,
    );
    host.load_source("telemetry_variants", &src).unwrap();

    let image = exec_tool_output(&reg, "telemetry_image", serde_json::json!({})).unwrap();
    assert!(matches!(image, n00n_agent::ToolOutput::Image { .. }));
    assert_eq!(image.telemetry().and_then(|value| value.cost), Some(0.25));

    let diff = exec_tool_output(&reg, "telemetry_diff", serde_json::json!({})).unwrap();
    assert!(matches!(diff, n00n_agent::ToolOutput::Diff { .. }));
    assert_eq!(
        diff.telemetry()
            .and_then(|value| value.usage.as_ref())
            .map(|usage| (usage.input_tokens, usage.output_tokens)),
        Some((9, 3))
    );

    for output in [image, diff] {
        let serialized = serde_json::to_string(&output).unwrap();
        let restored: n00n_agent::ToolOutput = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored.telemetry(), output.telemetry());
    }
}

/// Restores `tool` from `src` and returns the snapshot's concatenated text.
fn restore_snapshot_text(
    src: &str,
    tool: &str,
    clicks: Vec<usize>,
    state: Option<serde_json::Value>,
) -> String {
    let host = PluginHost::new(fresh_registry()).unwrap();
    host.load_source("restore_plugin", src).unwrap();
    let handle = host.event_handle().expect("event handle available");
    let (tx, rx) = flume::unbounded();

    handle.request_restore(
        n00n_lua::RestoreItem {
            tool: Arc::from(tool),
            tool_use_id: "restore_id".to_owned(),
            output: "ok".to_owned(),
            input: serde_json::json!({}),
            is_error: false,
            tool_output_lines: ToolOutputLines::default(),
            theme_gen: None,
            clicks,
            state,
        },
        n00n_agent::EventSender::new(tx, 0),
    );
    handle.wait_restore_complete_for_test();

    let mut text = String::new();
    for env in rx.drain() {
        if let n00n_agent::AgentEvent::ToolSnapshot { snapshot, .. } = env.event {
            for line in snapshot.lines.iter() {
                for span in &line.spans {
                    text.push_str(&span.text);
                }
            }
        }
    }
    text
}

#[test_case::test_case(true, "n=3 tag=hi" ; "state_present")]
#[test_case::test_case(false, "no state" ; "state_absent_falls_back")]
fn restore_reads_persisted_state(with_state: bool, expected: &str) {
    let state = with_state.then(|| serde_json::json!({ "n": 3, "tag": "hi" }));
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "state_restore",
            description = "t",
            schema = {MINIMAL_SCHEMA},
            handler = function() return "ok" end,
            restore = function(input, output, is_error, rctx)
                local buf = n00n.ui.buf()
                local s = rctx:state()
                if s == nil then
                    buf:line("no state")
                else
                    buf:line("n=" .. tostring(s.n) .. " tag=" .. s.tag)
                end
                return buf
            end
        }})"#,
    );
    let text = restore_snapshot_text(&src, "state_restore", Vec::new(), state);
    assert!(text.contains(expected), "expected {expected:?} in: {text}");
}

#[test]
fn restore_ctx_is_userdata_with_gated_capabilities() {
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "ctx_restore",
            description = "t",
            schema = {MINIMAL_SCHEMA},
            handler = function() return "ok" end,
            restore = function(input, output, is_error, rctx)
                local cfg, cfg_err = rctx:config()
                local _, fin_err = rctx:finish("x")
                local _, dl_err = rctx:set_deadline(5)
                local parts = {{
                    rctx:state().tag,
                    type(rctx:tool_output_lines()) == "table" and "tol_ok" or "tol_bad",
                    (cfg == nil and cfg_err ~= nil) and "config_err" or "config_ok",
                    fin_err ~= nil and "finish_err" or "finish_ok",
                    dl_err ~= nil and "deadline_err" or "deadline_ok",
                    rctx:cancelled() == false and "cancelled_ok" or "cancelled_bad",
                }}
                local buf = n00n.ui.buf()
                buf:line(table.concat(parts, " "))
                return buf
            end
        }})"#
    );
    let text = restore_snapshot_text(
        &src,
        "ctx_restore",
        Vec::new(),
        Some(serde_json::json!({ "tag": "hi" })),
    );
    assert!(
        text.contains("hi tol_ok config_err finish_err deadline_err cancelled_ok"),
        "restore ctx capability matrix mismatch: {text}"
    );
}

#[test]
fn get_tool_restore_accepts_table_or_userdata_ctx() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"local probe
n00n.api.register_tool({{
    name = "child_r",
    description = "t",
    schema = {MINIMAL_SCHEMA},
    handler = function() return "ok" end,
    restore = function(input, output, is_error, rctx)
        probe = {{ state = rctx:state(), tol = rctx:tool_output_lines() }}
        local buf = n00n.ui.buf()
        buf:line("body")
        return buf
    end
}})
n00n.api.register_tool({{
    name = "restore_driver",
    description = "t",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function(input, ctx)
        local t = n00n.api.get_tool("child_r")
        local parts = {{}}
        local buf = t.restore({{}}, "out", false, {{ tool_output_lines = {{ bash = 42 }}, state = {{ tag = "T" }} }})
        parts[1] = buf ~= nil and "buf_ok" or "buf_nil"
        parts[2] = (probe.state and probe.state.tag == "T") and "state_ok" or "state_bad"
        parts[3] = probe.tol.bash == 42 and "tol_ok" or "tol_bad"
        probe = nil
        local buf2 = t.restore({{}}, "out", false, ctx)
        parts[4] = buf2 ~= nil and "buf2_ok" or "buf2_nil"
        parts[5] = (probe.state == nil and type(probe.tol) == "table") and "ud_ok" or "ud_bad"
        probe = nil
        local buf3 = t.restore({{}}, "out", false)
        parts[6] = (buf3 ~= nil and type(probe.tol) == "table") and "default_ok" or "default_bad"
        return table.concat(parts, " ")
    end
}})"#
    );
    host.load_source("restore_compose_plugin", &src).unwrap();
    let out = exec_tool(&reg, "restore_driver", serde_json::json!({})).unwrap();
    assert_eq!(
        out, "buf_ok state_ok tol_ok buf2_ok ud_ok default_ok",
        "wrap_restore ctx normalization mismatch"
    );
}

#[test]
fn agent_api_value_failures_return_err_pairs() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "agent_pairs_probe",
            description = "t",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local function pair_err(v, e)
                    return v == nil and type(e) == "string"
                end
                local parts = {{}}
                parts[1] = pair_err(n00n.agent.system_prompt(ctx, {{ prompt_id = "nope" }})) and "prompt_err" or "prompt_ok"
                parts[2] = pair_err(n00n.agent.tools(ctx, {{ audience = "nope" }})) and "tools_err" or "tools_ok"
                parts[3] = pair_err(n00n.agent.resolve_model(ctx, {{ spec = "not-a-spec" }})) and "model_err" or "model_ok"
                return table.concat(parts, " ")
            end
        }})"#
    );
    host.load_source("agent_pairs_plugin", &src).unwrap();
    let out = exec_tool(&reg, "agent_pairs_probe", serde_json::json!({})).unwrap();
    assert_eq!(out, "prompt_err tools_err model_err");
}

/// Restore used to lose anything drawn via `n00n.async.run`: those tasks
/// landed in the global spawn queue, which runs after the snapshot is
/// taken. The runtime must run them inline, after the restore fn and after
/// each replayed click.
#[test_case::test_case(Vec::new(), "restore async line" ; "restore_async_task_runs_inline")]
#[test_case::test_case(vec![0], "click async line" ; "click_replay_async_task_runs_inline")]
fn restore_snapshot_contains_async_run_content(clicks: Vec<usize>, expected: &str) {
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "async_restore",
            description = "t",
            schema = {MINIMAL_SCHEMA},
            handler = function() return "ok" end,
            restore = function(input, output, is_error, rctx)
                local buf = n00n.ui.buf()
                buf:line("sync line")
                n00n.async.run(function()
                    buf:line("restore async line")
                end)
                buf:on("click", function()
                    n00n.async.run(function()
                        buf:line("click async line")
                    end)
                end)
                return buf
            end
        }})"#,
    );
    let text = restore_snapshot_text(&src, "async_restore", clicks, None);
    assert!(text.contains("sync line"), "sync content missing: {text}");
    assert!(
        text.contains(expected),
        "async content missing {expected:?}: {text}"
    );
}

#[test]
fn examples_table_flows_to_trait() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"n00n.api.register_tool({{
            name = "with_examples",
            description = "test",
            schema = {STRING_FIELD_SCHEMA},
            examples = {{ {{ url = "https://example.com" }} }},
            handler = function() return "" end
        }})"#,
    );
    host.load_source("examples_plugin", &src).unwrap();
    let entry = reg.get("with_examples").expect("tool not registered");
    assert_eq!(
        entry.tool.examples(),
        Some(serde_json::json!([{"url": "https://example.com"}]))
    );
}

#[test]
fn interrupt_kills_infinite_loop_and_vm_recovers() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"
n00n.api.register_tool({{
    name = "infinite_loop_",
    description = "loops forever",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function(input, ctx) while true do end end
}})
n00n.api.register_tool({{
    name = "noop_after_loop",
    description = "returns ok",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function(input, ctx) return "ok" end
}})
"#,
    );
    host.load_source("loop_plugin", &src).unwrap();

    let entry = reg.get("infinite_loop_").expect("loop tool not registered");
    let inv = entry.tool.parse(&serde_json::json!({})).unwrap();
    let mut ctx = n00n_agent::tools::test_support::stub_ctx(&n00n_agent::AgentMode::Build);
    ctx.deadline = n00n_agent::tools::Deadline::after(std::time::Duration::from_secs(5));

    let result = smol::block_on(async { inv.execute(&ctx).await });

    assert!(result.output.is_err(), "expected error from timed-out loop");

    let ok = exec_tool(&reg, "noop_after_loop", serde_json::json!({}));
    assert!(ok.is_ok(), "VM poisoned after interrupt: {ok:?}");
}

#[test]
fn failed_load_leaves_no_tools_or_commands() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"
n00n.api.register_tool({{
    name = "doomed",
    description = "never registered",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function() return "" end
}})
n00n.api.register_command({{
    name = "/doomed",
    handler = function() end,
}})
error("plugin blew up after register")
"#,
    );
    let err = host
        .load_source("broken", &src)
        .expect_err("expected lua error");
    assert!(matches!(err, PluginError::Lua { .. }));
    assert!(!reg.has("doomed"));
    assert_eq!(host.command_reader().load().commands.len(), 0);

    host.load_source("broken", ECHO_PLUGIN)
        .expect("retry with good source should succeed");
    assert!(reg.has("echo_"));
}

#[test]
fn is_error_propagated_as_error() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"n00n.api.register_tool({{
            name = "returns_error",
            description = "returns is_error=true",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                return {{ llm_output = "boom", is_error = true }}
            end
        }})"#,
    );
    host.load_source("err_plugin", &src).unwrap();

    let err = exec_tool(&reg, "returns_error", serde_json::json!({})).unwrap_err();
    assert_eq!(err, "boom");
}

#[test]
fn handler_bad_return_type_is_error() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "bad_ret_num",
            description = "bad return",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function() return 42 end
        }})"#,
    );
    host.load_source("bad_ret", &src).unwrap();

    let err = exec_tool(&reg, "bad_ret_num", serde_json::json!({})).unwrap_err();
    assert!(err.contains("must return string"), "got: {err}");
}

#[test]
fn handler_nil_without_jobs_is_error() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = r#"n00n.api.register_tool({
        name = "nil_no_jobs",
        description = "returns nil without starting jobs",
        schema = { type = "object", properties = {} },
        audiences = { "main" },
        handler = function() return nil end
    })"#;
    host.load_source("nil_no_jobs", src).unwrap();
    let err = exec_tool(&reg, "nil_no_jobs", serde_json::json!({})).unwrap_err();
    assert!(err.contains(NIL_WITHOUT_JOBS_ERR), "got: {err}");
}

#[test]
fn handler_nil_waits_for_owned_async_run() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = r#"n00n.api.register_tool({
        name = "async_finish",
        description = "finishes after delayed async work",
        schema = { type = "object", properties = {} },
        audiences = { "main" },
        handler = function(input, ctx)
            n00n.async.run(function()
                local id = n00n.fn.jobstart("sleep 0.2")
                n00n.fn.jobwait(id)
                return "finished"
            end, function(err, result)
                ctx:finish(result)
            end)
        end
    })"#;
    host.load_source("async_finish", src).unwrap();
    let first_registry = Arc::clone(&reg);
    let second_registry = Arc::clone(&reg);
    let first = std::thread::spawn(move || {
        exec_tool(&first_registry, "async_finish", serde_json::json!({}))
    });
    let second = std::thread::spawn(move || {
        exec_tool(&second_registry, "async_finish", serde_json::json!({}))
    });

    assert_eq!(first.join().unwrap().unwrap(), "finished");
    assert_eq!(second.join().unwrap().unwrap(), "finished");
}

#[test]
fn handler_lua_error_surfaces_as_tool_error() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"n00n.api.register_tool({{
            name = "thrower",
            description = "throws on call",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function() error("intentional kaboom") end
        }})"#,
    );
    host.load_source("thrower_plugin", &src).unwrap();

    let err = exec_tool(&reg, "thrower", serde_json::json!({})).unwrap_err();
    assert!(err.contains("intentional kaboom"), "got: {err}");
}

#[test]
fn lua_tool_schema_rejects_bad_input() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = r#"
n00n.api.register_tool({
    name = "needs_name",
    description = "requires a name field",
    schema = {
        type = "object",
        properties = { name = { type = "string" } },
        required = { "name" }
    },
    handler = function(input) return input.name end
})
"#;
    host.load_source("schema_test", src).unwrap();

    let entry = reg.get("needs_name").unwrap();
    let err = entry
        .tool
        .parse(&serde_json::json!({"count": 1}))
        .err()
        .expect("missing required field should fail");
    assert!(err.to_string().contains("name"));

    assert!(
        entry
            .tool
            .parse(&serde_json::json!({"name": "alice"}))
            .is_ok()
    );
}

#[test]
fn init_lua_with_require_registers_tools() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lua_dir = tmp.path().join("lua");
    std::fs::create_dir_all(lua_dir.join("tools")).unwrap();

    std::fs::write(
        lua_dir.join("tools/greet.lua"),
        r#"
local M = {}
function M.setup()
    n00n.api.register_tool({
        name = "greet",
        description = "says hi",
        schema = { type = "object", properties = {}, additionalProperties = false },
        handler = function() return "hi" end
    })
end
return M
"#,
    )
    .unwrap();

    std::fs::write(
        tmp.path().join("init.lua"),
        r#"
local greet = require("tools.greet")
greet.setup()
"#,
    )
    .unwrap();

    let init_path = tmp.path().join("init.lua");
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_plugin_file(&init_path).unwrap();

    assert!(reg.has("greet"));
    assert_eq!(reg.names().len(), 1);
}

#[test]
fn require_caches_modules() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lua_dir = tmp.path().join("lua");
    std::fs::create_dir_all(&lua_dir).unwrap();

    std::fs::write(lua_dir.join("counter.lua"), "return { value = 42 }\n").unwrap();

    std::fs::write(
        tmp.path().join("init.lua"),
        r#"
local a = require("counter")
local b = require("counter")
assert(a == b, "require should return cached module")
"#,
    )
    .unwrap();

    let init_path = tmp.path().join("init.lua");
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_plugin_file(&init_path).unwrap();
}

#[test]
fn require_sandbox_escape_blocked() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lua_dir = tmp.path().join("lua");
    std::fs::create_dir_all(&lua_dir).unwrap();

    std::fs::write(tmp.path().join("init.lua"), "require(\"../../escape\")\n").unwrap();

    let init_path = tmp.path().join("init.lua");
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let err = host
        .load_plugin_file(&init_path)
        .expect_err("expected sandbox error");
    assert!(matches!(err, PluginError::Lua { .. }));
    let msg = err.to_string();
    assert!(
        msg.contains("sandbox") || msg.contains("outside"),
        "got: {msg}"
    );
}

#[test]
fn require_circular_returns_sentinel_and_caches_real_value() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lua_dir = tmp.path().join("lua");
    std::fs::create_dir_all(&lua_dir).unwrap();

    std::fs::write(
        lua_dir.join("a.lua"),
        "local b = require(\"b\")\nreturn { name = \"a\" }\n",
    )
    .unwrap();
    std::fs::write(
        lua_dir.join("b.lua"),
        "local a = require(\"a\")\nassert(a == true, \"circular require should return sentinel\")\nreturn { name = \"b\" }\n",
    )
    .unwrap();

    std::fs::write(
        tmp.path().join("init.lua"),
        r#"
require("a")
local a2 = require("a")
assert(type(a2) == "table", "cached value should be table, got: " .. type(a2))
assert(a2.name == "a", "cached value should have name='a'")
local b2 = require("b")
assert(type(b2) == "table", "cached value should be table, got: " .. type(b2))
assert(b2.name == "b", "cached value should have name='b'")
"#,
    )
    .unwrap();

    let init_path = tmp.path().join("init.lua");
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_plugin_file(&init_path).unwrap();
}

#[test]
fn require_nonexistent_module_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lua_dir = tmp.path().join("lua");
    std::fs::create_dir_all(&lua_dir).unwrap();

    std::fs::write(tmp.path().join("init.lua"), "require(\"nonexistent\")\n").unwrap();

    let init_path = tmp.path().join("init.lua");
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let err = host
        .load_plugin_file(&init_path)
        .expect_err("expected error for missing module");
    assert!(matches!(err, PluginError::Lua { .. }));
    assert!(err.to_string().contains("nonexistent"), "got: {err}");
}

#[test]
fn require_error_cleans_loading_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lua_dir = tmp.path().join("lua");
    std::fs::create_dir_all(&lua_dir).unwrap();

    std::fs::write(lua_dir.join("bad.lua"), "error('deliberate')").unwrap();
    std::fs::write(lua_dir.join("good.lua"), "return { ok = true }").unwrap();

    std::fs::write(
        tmp.path().join("init.lua"),
        r#"
local ok, err = pcall(require, "bad")
assert(not ok, "bad module should fail")

-- second require of the same broken module must error again, not return a sentinel
local ok2, err2 = pcall(require, "bad")
assert(not ok2, "broken module should fail on retry too")

-- unrelated modules must still work
local g = require("good")
assert(type(g) == "table", "good module should load, got: " .. type(g))
assert(g.ok == true)
"#,
    )
    .unwrap();

    let init_path = tmp.path().join("init.lua");
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_plugin_file(&init_path).unwrap();
}

#[test]
fn multi_tool_plugin_registers_and_unloads_all() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"
n00n.api.register_tool({{
    name = "multi_alpha",
    description = "first tool",
    schema = {MINIMAL_SCHEMA},
    handler = function() return "alpha" end
}})
n00n.api.register_tool({{
    name = "multi_beta",
    description = "second tool",
    schema = {MINIMAL_SCHEMA},
    handler = function() return "beta" end
}})
"#,
    );
    host.load_source("multi", &src).unwrap();

    assert!(reg.has("multi_alpha"));
    assert!(reg.has("multi_beta"));

    host.unload("multi").unwrap();
    assert!(!reg.has("multi_alpha"));
    assert!(!reg.has("multi_beta"));
}

#[test]
fn conflict_from_different_plugin_preserves_original() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"n00n.api.register_tool({{
            name = "evolving",
            description = "version 1",
            schema = {MINIMAL_SCHEMA},
            handler = function() return "v1" end
        }})"#,
    );
    host.load_source("keeper", &src).unwrap();
    assert!(reg.has("evolving"));

    let err = host
        .load_source("intruder", &src)
        .expect_err("expected conflict");
    assert!(matches!(err, PluginError::NameConflict { .. }));

    let entry = reg.get("evolving").unwrap();
    assert!(matches!(entry.source, ToolSource::Lua { ref plugin } if plugin.as_ref() == "keeper"),);
}

#[test]
fn ctx_finish_called_twice_is_error() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "double_finish",
            description = "calls finish twice",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                ctx:finish("first")
                ctx:finish("second")
            end
        }})"#,
    );
    host.load_source("double_finish", &src).unwrap();
    let err = exec_tool(&reg, "double_finish", serde_json::json!({})).unwrap_err();
    assert!(err.contains(FINISH_CALLED_TWICE_ERR), "got: {err}");
}

#[test]
fn ctx_finish_with_is_error_propagates() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "finish_err",
            description = "finishes with error",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                ctx:finish({{ llm_output = "async boom", is_error = true }})
            end
        }})"#,
    );
    host.load_source("finish_err", &src).unwrap();
    let err = exec_tool(&reg, "finish_err", serde_json::json!({})).unwrap_err();
    assert_eq!(err, "async boom");
}

#[test]
fn async_job_on_exit_receives_exit_code() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "job_exit_code",
            description = "reports exit code",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                n00n.fn.jobstart("exit 42", {{
                    on_exit = function(job_id, code)
                        ctx:finish("code=" .. tostring(code))
                    end
                }})
            end
        }})"#,
    );
    host.load_source("job_exit_code", &src).unwrap();
    let out = exec_tool(&reg, "job_exit_code", serde_json::json!({})).unwrap();
    assert_eq!(out, "code=42");
}

#[test]
fn jobwait_fires_callbacks_while_waiting() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "job_stream",
            description = "streams lines during jobwait",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local seen = {{}}
                local exit_code
                local id = n00n.fn.jobstart("echo a; echo b; exit 7", {{
                    on_stdout = function(_, line) seen[#seen + 1] = line end,
                    on_exit = function(_, code) exit_code = code end,
                }})
                local res = n00n.fn.jobwait(id)
                return table.concat(seen, ",")
                    .. " exit=" .. tostring(exit_code)
                    .. " stdout=" .. (res.stdout:gsub("\n", ","))
            end
        }})"#,
    );
    host.load_source("job_stream", &src).unwrap();
    let out = exec_tool(&reg, "job_stream", serde_json::json!({})).unwrap();
    assert_eq!(out, "a,b exit=7 stdout=a,b");
}

#[test]
fn jobstart_invalid_cwd_errors_with_expanded_path() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "job_bad_cwd",
            description = "jobstart with missing tilde cwd",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local _, err = pcall(n00n.fn.jobstart, "pwd", {{ cwd = "{JOB_BAD_CWD}" }})
                return tostring(err)
            end
        }})"#,
    );
    host.load_source("job_bad_cwd", &src).unwrap();
    let out = exec_tool(&reg, "job_bad_cwd", serde_json::json!({})).unwrap();
    let expanded = n00n_storage::paths::home()
        .expect("home dir")
        .join(JOB_BAD_CWD.strip_prefix("~/").unwrap());
    let expected = format!("{JOB_BAD_CWD_ERR_PREFIX}{}", expanded.display());
    assert!(out.contains(&expected), "got: {out}");
}

#[test]
fn async_job_exits_without_finish_is_error() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "job_no_finish",
            description = "job exits but never calls finish",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                n00n.fn.jobstart("echo oops", {{
                    on_exit = function(job_id, code) end
                }})
            end
        }})"#,
    );
    host.load_source("job_no_finish", &src).unwrap();
    let err = exec_tool(&reg, "job_no_finish", serde_json::json!({})).unwrap_err();
    assert!(err.contains(NIL_WITHOUT_JOBS_ERR), "got: {err}");
}

#[test]
fn async_job_callback_error_surfaces() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "job_cb_err",
            description = "callback throws",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                n00n.fn.jobstart("echo trigger", {{
                    on_exit = function(job_id, code)
                        error("callback exploded")
                    end
                }})
            end
        }})"#,
    );
    host.load_source("job_cb_err", &src).unwrap();
    let err = exec_tool(&reg, "job_cb_err", serde_json::json!({})).unwrap_err();
    assert!(err.contains("callback exploded"), "got: {err}");
}

/// Runs `tool`, whose handler parks on `jobstart("sleep 30")` until a
/// click lands, while this thread keeps re-sending clicks until it
/// finishes. Clicks are fire-and-forget, so the loop self-corrects: only a
/// click delivered while the handler is registered can finish the tool.
fn click_until_finished(
    host: &PluginHost,
    reg: &ToolRegistry,
    tool: &str,
    click_id: &'static str,
) -> String {
    let eh = host.event_handle().expect("event handle available");
    let entry = reg.get(tool).expect("tool registered");
    let inv = entry.tool.parse(&serde_json::json!({})).expect("parse");
    let worker = std::thread::spawn(move || {
        let ctx = n00n_agent::tools::test_support::stub_ctx_with(
            &n00n_agent::AgentMode::Build,
            None,
            Some(click_id),
        );
        smol::block_on(inv.execute(&ctx)).output
    });
    for _ in 0..500 {
        if worker.is_finished() {
            break;
        }
        eh.request_click(click_id.to_owned(), 0);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = worker.join().expect("worker thread").expect("tool output");
    match out {
        n00n_agent::ToolOutput::Plain(s) => s.text,
        other => panic!("unexpected output: {other:?}"),
    }
}

#[test]
fn live_click_reaches_running_tool() {
    const LIVE_CLICK_ID: &str = "live-click-1";
    const CLICKED_MSG: &str = "clicked";
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "live_click",
            description = "finishes when clicked while running",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local buf = n00n.ui.buf()
                buf:on("click", function()
                    ctx:finish("{CLICKED_MSG}")
                end)
                n00n.fn.jobstart("sleep 30", {{}})
            end
        }})"#,
    );
    host.load_source("live_click", &src).unwrap();
    assert_eq!(
        click_until_finished(&host, &reg, "live_click", LIVE_CLICK_ID),
        CLICKED_MSG
    );
}

/// With several bufs holding click handlers, `request_click` must reach
/// the buf passed to `ctx:live_buf` (the root), not the first-created
/// fallback.
#[test]
fn live_click_routes_to_root_buf_among_many() {
    const ROOT_CLICK_ID: &str = "root-click-1";
    const ROOT_MSG: &str = "root_clicked";
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "root_click",
            description = "decoy buf registers a click first",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local decoy = n00n.ui.buf()
                decoy:on("click", function() ctx:finish("decoy_clicked") end)
                local root = n00n.ui.buf()
                root:on("click", function() ctx:finish("{ROOT_MSG}") end)
                ctx:live_buf(root)
                n00n.fn.jobstart("sleep 30", {{}})
            end
        }})"#,
    );
    host.load_source("root_click", &src).unwrap();
    assert_eq!(
        click_until_finished(&host, &reg, "root_click", ROOT_CLICK_ID),
        ROOT_MSG
    );
}

const WARM_TOOL_NAME: &str = "warm_probe";
const WARM_INITIAL_LINE: &str = "initial";
const WARM_CLICK_LINE: &str = "warm_clicked";
const WARM_ERROR_OUTPUT: &str = "boom";
const WARM_RESTORED_LINE: &str = "restored";
const WARM_RESTORE_CLICK_LINE: &str = "restore_clicked";

/// `live_click` wires the handler-side click; restore always wires its own.
fn warm_host(is_error: bool, live_click: bool) -> (Arc<ToolRegistry>, PluginHost) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let ret = if is_error {
        format!(r#"{{ llm_output = "{WARM_ERROR_OUTPUT}", is_error = true }}"#)
    } else {
        r#""done""#.to_owned()
    };
    let on_click = if live_click {
        format!(
            r#"buf:on("click", function()
                    buf:set_lines({{ "{WARM_CLICK_LINE}" }})
                end)"#
        )
    } else {
        String::new()
    };
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "{WARM_TOOL_NAME}",
            description = "warm click probe",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local buf = n00n.ui.buf()
                buf:set_lines({{ "{WARM_INITIAL_LINE}" }})
                {on_click}
                ctx:live_buf(buf)
                return {ret}
            end,
            restore = function(input, output, is_error, rctx)
                local buf = n00n.ui.buf()
                buf:set_lines({{ "{WARM_RESTORED_LINE}" }})
                buf:on("click", function()
                    buf:set_lines({{ "{WARM_RESTORE_CLICK_LINE}" }})
                end)
                return {{ body = buf }}
            end
        }})"#,
    );
    host.load_source("warm_probe_plugin", &src).unwrap();
    (reg, host)
}

/// `load_source` waits for the request channel and the inflight gate, so
/// once it returns every click sent before it has fully run, async jobs
/// included. No sleeps needed. It also clears the warm map, so click
/// before the barrier, never after.
fn barrier(host: &PluginHost) {
    host.load_source("barrier", "").unwrap();
}

fn warm_restore_item(id: &str, clicks: Vec<usize>) -> n00n_lua::RestoreItem {
    n00n_lua::RestoreItem {
        tool: Arc::from(WARM_TOOL_NAME),
        tool_use_id: id.to_owned(),
        output: "done".to_owned(),
        input: serde_json::json!({}),
        is_error: false,
        tool_output_lines: ToolOutputLines::default(),
        theme_gen: None,
        clicks,
        state: None,
    }
}

fn snapshot_texts(rx: &flume::Receiver<n00n_agent::Envelope>, id: &str) -> Vec<String> {
    rx.drain()
        .filter_map(|env| match env.event {
            n00n_agent::AgentEvent::ToolSnapshot {
                id: got, snapshot, ..
            } if got == id => Some(
                snapshot
                    .lines
                    .iter()
                    .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

fn warm_ctx(
    id: &str,
) -> (
    n00n_agent::tools::ToolContext,
    flume::Receiver<n00n_agent::Envelope>,
) {
    let (tx, rx) = flume::unbounded::<n00n_agent::Envelope>();
    let event_tx = n00n_agent::EventSender::new(tx, 0);
    let ctx = n00n_agent::tools::test_support::stub_ctx_with(
        &n00n_agent::AgentMode::Build,
        Some(&event_tx),
        Some(id),
    );
    (ctx, rx)
}

fn exec_warm_tool(
    reg: &ToolRegistry,
    tool: &str,
    ctx: &n00n_agent::tools::ToolContext,
) -> Result<n00n_agent::ToolOutput, String> {
    let inv = reg
        .get(tool)
        .expect("tool registered")
        .tool
        .parse(&serde_json::json!({}))
        .expect("parse failed");
    smol::block_on(inv.execute(ctx)).output
}

/// A click on a finished tool takes the warm path: it mutates the live
/// root buf and the fallback restore stays unused. Failed tools stay
/// warm too, since people click them to see what went wrong.
#[test_case::test_case(false ; "success")]
#[test_case::test_case(true ; "error_finish")]
fn warm_click_reaches_finished_tool(is_error: bool) {
    const WARM_ID: &str = "warm-click-1";
    let (reg, host) = warm_host(is_error, true);
    let (ctx, rx) = warm_ctx(WARM_ID);
    let res = exec_warm_tool(&reg, WARM_TOOL_NAME, &ctx);
    assert_eq!(res.err(), is_error.then(|| WARM_ERROR_OUTPUT.to_owned()));
    let body = recv_live_buf(&rx, WARM_ID).expect("live buf published");

    let (fb_tx, fb_rx) = flume::unbounded();
    let eh = host.event_handle().expect("event handle available");
    eh.request_click_with_fallback(
        WARM_ID.to_owned(),
        0,
        warm_restore_item(WARM_ID, vec![0]),
        n00n_agent::EventSender::new(fb_tx, 0),
    );
    barrier(&host);

    assert_eq!(body.read()[0].spans[0].text, WARM_CLICK_LINE);
    assert!(
        snapshot_texts(&fb_rx, WARM_ID).is_empty(),
        "warm hit must not trigger the fallback restore"
    );
}

/// A click that misses both the live and warm maps restores from the
/// fallback item (replaying its recorded clicks), so an evicted or
/// desynced warm cache costs latency, never a dropped click.
#[test]
fn click_fallback_restores_when_warm_missing() {
    const GONE_ID: &str = "warm-gone-1";
    let (_reg, host) = warm_host(false, true);
    let (tx, rx) = flume::unbounded();

    let eh = host.event_handle().expect("event handle available");
    eh.request_click_with_fallback(
        GONE_ID.to_owned(),
        0,
        warm_restore_item(GONE_ID, vec![0]),
        n00n_agent::EventSender::new(tx, 0),
    );
    barrier(&host);

    assert_eq!(
        snapshot_texts(&rx, GONE_ID),
        vec![WARM_RESTORE_CLICK_LINE.to_owned()],
        "fallback restore must replay the recorded clicks"
    );
}

/// A warm hit whose root buf has no click handler must still consume
/// the fallback: some plugins wire clicks only in `restore`.
#[test]
fn click_fallback_restores_when_warm_buf_has_no_handler() {
    const WARM_ID: &str = "warm-nohandler-1";
    let (reg, host) = warm_host(false, false);
    let (ctx, rx) = warm_ctx(WARM_ID);
    exec_warm_tool(&reg, WARM_TOOL_NAME, &ctx).expect("tool output");
    recv_live_buf(&rx, WARM_ID).expect("live buf published");

    let (fb_tx, fb_rx) = flume::unbounded();
    let eh = host.event_handle().expect("event handle available");
    eh.request_click_with_fallback(
        WARM_ID.to_owned(),
        0,
        warm_restore_item(WARM_ID, vec![0]),
        n00n_agent::EventSender::new(fb_tx, 0),
    );
    barrier(&host);

    assert_eq!(
        snapshot_texts(&fb_rx, WARM_ID),
        vec![WARM_RESTORE_CLICK_LINE.to_owned()],
        "warm hit without a click handler must fall back to restore"
    );
}

/// Any restore of a tool supersedes its warm handle: the entry is
/// evicted so the stale view can never serve later clicks (e.g. with
/// old-theme content after a rebake).
#[test]
fn restore_evicts_warm_handle() {
    const WARM_ID: &str = "warm-rebaked-1";
    let (reg, host) = warm_host(false, true);
    let (ctx, rx) = warm_ctx(WARM_ID);
    exec_warm_tool(&reg, WARM_TOOL_NAME, &ctx).expect("tool output");
    let body = recv_live_buf(&rx, WARM_ID).expect("live buf published");

    let (tx, _rx) = flume::unbounded();
    let eh = host.event_handle().expect("event handle available");
    eh.request_restore(
        warm_restore_item(WARM_ID, Vec::new()),
        n00n_agent::EventSender::new(tx, 0),
    );
    eh.request_click(WARM_ID.to_owned(), 0);
    barrier(&host);

    assert_eq!(
        body.read()[0].spans[0].text,
        WARM_INITIAL_LINE,
        "bare click after restore must be a no-op on the evicted warm buf"
    );
}

/// Overfilling the cache evicts the oldest entry. Bare clicks (no
/// fallback) make eviction observable: the evicted tool's click is
/// dropped while a still-warm one lands.
#[test]
fn warm_fifo_evicts_oldest_runtime_side() {
    let (reg, host) = warm_host(false, true);
    let mut bufs = Vec::with_capacity(WARM_TOOL_CAP + 1);
    for i in 0..=WARM_TOOL_CAP {
        let id = format!("t{i}");
        let (ctx, rx) = warm_ctx(&id);
        exec_warm_tool(&reg, WARM_TOOL_NAME, &ctx).expect("tool output");
        bufs.push(recv_live_buf(&rx, &id).expect("live buf published"));
    }

    let eh = host.event_handle().expect("event handle available");
    eh.request_click("t1".to_owned(), 0);
    eh.request_click("t0".to_owned(), 0);
    barrier(&host);

    assert_eq!(
        bufs[1].read()[0].spans[0].text,
        WARM_CLICK_LINE,
        "still-warm tool must take the warm click path"
    );
    assert_eq!(
        bufs[0].read()[0].spans[0].text,
        WARM_INITIAL_LINE,
        "evicted tool's click must be ignored"
    );
}

#[test]
fn explore_result_live_click_and_warm_eviction_fallback_preserve_card_contract() {
    const TOOL: &str = "explore_probe";
    const OUTPUT: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight";
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"
local ExploreResult = require("n00n.explore_result")
n00n.api.register_tool({{
    name = "{TOOL}",
    description = "shared explore card probe",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function(input, ctx)
        local card, err = ExploreResult.live(ctx)
        if not card then
            return {{ llm_output = tostring(err), is_error = true }}
        end
        card:update("one\ntwo\nthree\nfour\nfive\nsix\nseven\neight")
        return {{ llm_output = "done", body = card.buf }}
    end,
    restore = function(input, output)
        return ExploreResult.restore(output)
    end,
}})
"#,
    );
    host.load_source("explore_probe_plugin", &src).unwrap();

    let mut bodies = Vec::with_capacity(WARM_TOOL_CAP + 1);
    for i in 0..=WARM_TOOL_CAP {
        let id = format!("explore-{i}");
        let (ctx, rx) = warm_ctx(&id);
        exec_warm_tool(&reg, TOOL, &ctx).expect("tool output");
        bodies.push(recv_live_buf(&rx, &id).expect("live explore card"));
    }
    assert_eq!(bodies[0].read().len(), 6, "five rows plus expand hint");

    let evicted_id = "explore-0";
    let item = n00n_lua::RestoreItem {
        tool: Arc::from(TOOL),
        tool_use_id: evicted_id.to_owned(),
        output: OUTPUT.to_owned(),
        input: serde_json::json!({}),
        is_error: false,
        tool_output_lines: ToolOutputLines::default(),
        theme_gen: None,
        clicks: vec![0],
        state: None,
    };
    let (tx, rx) = flume::unbounded();
    let event_handle = host.event_handle().expect("event handle");
    event_handle.request_click_with_fallback(
        evicted_id.to_owned(),
        0,
        item,
        n00n_agent::EventSender::new(tx, 0),
    );
    event_handle.request_click(format!("explore-{WARM_TOOL_CAP}"), 0);
    barrier(&host);

    assert_eq!(
        bodies[0].read().len(),
        6,
        "evicted live card must not receive the click"
    );
    assert_eq!(
        bodies[WARM_TOOL_CAP].read().len(),
        8,
        "a still-warm live card must expand in place"
    );
    assert_eq!(
        snapshot_texts(&rx, evicted_id),
        vec![OUTPUT.replace('\n', "")],
        "fallback restore must replay the click and publish the expanded card"
    );
}

/// After a plugin (re)load the old handlers are gone, so stale warm
/// clicks must be dropped, never run.
#[test]
fn warm_map_cleared_by_load_source() {
    const WARM_ID: &str = "warm-cleared-1";
    let (reg, host) = warm_host(false, true);
    let (ctx, rx) = warm_ctx(WARM_ID);
    exec_warm_tool(&reg, WARM_TOOL_NAME, &ctx).expect("tool output");
    let body = recv_live_buf(&rx, WARM_ID).expect("live buf published");

    barrier(&host);
    let eh = host.event_handle().expect("event handle available");
    eh.request_click(WARM_ID.to_owned(), 0);
    barrier(&host);

    assert_eq!(body.read()[0].spans[0].text, WARM_INITIAL_LINE);
}

/// `LoadSource`'s drain barrier spawns and awaits queued async jobs, so
/// jobs a warm click enqueues land before the barrier returns.
#[test]
fn warm_click_runs_async_jobs() {
    const WARM_ID: &str = "warm-async-1";
    const ASYNC_LINE: &str = "async_appended";
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "warm_async",
            description = "appends a line from an async job on click",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local buf = n00n.ui.buf()
                buf:set_lines({{ "{WARM_INITIAL_LINE}" }})
                buf:on("click", function()
                    n00n.async.run(function()
                        buf:line("{ASYNC_LINE}")
                    end)
                end)
                ctx:live_buf(buf)
                return "done"
            end
        }})"#,
    );
    host.load_source("warm_async_plugin", &src).unwrap();

    let (ctx, rx) = warm_ctx(WARM_ID);
    exec_warm_tool(&reg, "warm_async", &ctx).expect("tool output");
    let body = recv_live_buf(&rx, WARM_ID).expect("live buf published");

    let eh = host.event_handle().expect("event handle available");
    eh.request_click(WARM_ID.to_owned(), 0);
    barrier(&host);

    let text = body.take().text();
    assert!(text.contains(ASYNC_LINE), "async job line missing: {text}");
}

/// The warm cell gets a fresh `CancelToken::none()`: cancelling the
/// original run after it finished must not kill warm clicks.
#[test]
fn warm_click_survives_post_completion_cancel() {
    const WARM_ID: &str = "warm-cancel-1";
    let (reg, host) = warm_host(false, true);
    let (mut ctx, rx) = warm_ctx(WARM_ID);
    let (trigger, token) = n00n_agent::CancelToken::new();
    ctx.cancel = token;
    exec_warm_tool(&reg, WARM_TOOL_NAME, &ctx).expect("tool output");
    let body = recv_live_buf(&rx, WARM_ID).expect("live buf published");
    trigger.cancel();

    let eh = host.event_handle().expect("event handle available");
    eh.request_click(WARM_ID.to_owned(), 0);
    barrier(&host);

    assert_eq!(body.read()[0].spans[0].text, WARM_CLICK_LINE);
}

/// `n00n.agent.call_tool` returns `(text, err)` and delivers live bufs and
/// annotations (live and completion alike) through the callbacks.
#[test]
fn call_tool_streams_live_buf_and_annotations() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"
n00n.api.register_tool({{
    name = "annotated_child",
    description = "returns an annotation",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function(input, ctx)
        return {{ llm_output = "child_done", annotation = "5 items" }}
    end
}})
n00n.api.register_tool({{
    name = "streaming_child",
    description = "publishes a live buf then finishes",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function(input, ctx)
        local buf = n00n.ui.buf()
        buf:line("streamed line")
        ctx:live_buf(buf)
        return "stream_done"
    end
}})
n00n.api.register_tool({{
    name = "failing_child",
    description = "always errors",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function(input, ctx)
        return {{ llm_output = "boom", is_error = true }}
    end
}})
n00n.api.register_tool({{
    name = "driver",
    description = "dispatches children via n00n.agent.call_tool",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function(input, ctx)
        local ann = "nil"
        local text, err = n00n.agent.call_tool(ctx, "annotated_child", {{}}, {{
            on_annotation = function(a) ann = a end,
        }})
        local live_text = "none"
        local ann2 = "nil"
        local text2 = n00n.agent.call_tool(ctx, "streaming_child", {{}}, {{
            on_live_buf = function(b)
                local lines = b:get_lines()
                live_text = lines[1] and lines[1][1] and lines[1][1][1] or "empty"
            end,
            on_annotation = function(a) ann2 = a end,
        }})
        local ann3 = "nil"
        local _, err3 = n00n.agent.call_tool(ctx, "failing_child", {{}}, {{
            on_annotation = function(a) ann3 = a end,
        }})
        return tostring(text) .. "/" .. ann
            .. " " .. tostring(text2) .. "/" .. live_text .. "/" .. ann2
            .. " " .. tostring(err3) .. "/" .. ann3
    end
}})
"#,
    );
    host.load_source("call_tool_live", &src).unwrap();
    let out = exec_tool_in(
        &reg,
        "driver",
        serde_json::json!({}),
        Some(Arc::clone(&reg)),
    )
    .expect("driver ok");
    assert_eq!(
        out,
        "child_done/5 items stream_done/streamed line/1 lines boom/nil"
    );
}

#[test]
fn jobstop_kills_running_job() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "job_stop",
            description = "starts and immediately stops a job",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local id = n00n.fn.jobstart("sleep 60", {{
                    on_exit = function(job_id, code)
                        ctx:finish("killed=" .. tostring(code ~= 0))
                    end
                }})
                n00n.fn.jobstop(id)
            end
        }})"#,
    );
    host.load_source("job_stop", &src).unwrap();
    let out = exec_tool(&reg, "job_stop", serde_json::json!({})).unwrap();
    assert_eq!(out, "killed=true");
}

#[test]
fn vm_recovers_after_async_job_tool() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"
n00n.api.register_tool({{
    name = "async_first",
    description = "async tool",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function(input, ctx)
        n00n.fn.jobstart("echo hi", {{
            on_exit = function(job_id, code) ctx:finish("ok1") end
        }})
    end
}})
n00n.api.register_tool({{
    name = "sync_after",
    description = "sync tool",
    schema = {MINIMAL_SCHEMA},
    audiences = {{ "main" }},
    handler = function() return "ok2" end
}})
"#,
    );
    host.load_source("recovery", &src).unwrap();
    let out1 = exec_tool(&reg, "async_first", serde_json::json!({})).unwrap();
    assert_eq!(out1, "ok1");
    let out2 = exec_tool(&reg, "sync_after", serde_json::json!({})).unwrap();
    assert_eq!(out2, "ok2");
}

#[test]
fn setup_happy_path() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let raw = host
        .send_run_init_lua(
            "n00n.setup({ agent = { max_output_lines = 3000 } })".to_owned(),
            "test_init.lua".to_owned(),
            None,
        )
        .unwrap();
    let raw = raw.expect("expected Some(RawConfig)");
    assert_eq!(raw.agent.max_output_lines, Some(3000));
}

#[test_case::test_case(
    r"n00n.setup({ agent = { compaction_buffer = 10000 } })",
    n00n_config::CompactionBuffer::Tokens(10_000)
    ; "compaction_buffer_tokens"
)]
#[test_case::test_case(
    r#"n00n.setup({ agent = { compaction_buffer = "15%" } })"#,
    n00n_config::CompactionBuffer::Percent(15)
    ; "compaction_buffer_percent"
)]
fn setup_compaction_buffer(lua_src: &str, expected: n00n_config::CompactionBuffer) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let raw = host
        .send_run_init_lua(lua_src.to_owned(), "test_init.lua".to_owned(), None)
        .unwrap()
        .expect("expected Some(RawConfig)");
    assert_eq!(raw.agent.compaction_buffer, Some(expected));
}

#[test_case::test_case(
    "n00n.setup({ ui = { splash_animaton = false } })",
    UNKNOWN_FIELD_ERR
    ; "unknown_field"
)]
#[test_case::test_case(
    r#"n00n.setup({ agent = { max_output_lines = "not a number" } })"#,
    ""
    ; "wrong_type"
)]
fn setup_rejects_bad_input(lua_src: &str, expected_substr: &str) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let err = host
        .send_run_init_lua(lua_src.to_owned(), "test_init.lua".to_owned(), None)
        .expect_err("expected error");
    assert!(matches!(err, PluginError::Lua { .. }), "got: {err}");
    if !expected_substr.is_empty() {
        assert!(err.to_string().contains(expected_substr), "got: {err}");
    }
}

#[test]
fn setup_double_call_error() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let err = host
        .send_run_init_lua(
            "n00n.setup({})\nn00n.setup({})".to_owned(),
            "test_init.lua".to_owned(),
            None,
        )
        .expect_err("expected error for double setup");
    assert!(err.to_string().contains(ALREADY_CALLED_ERR), "got: {err}");
}

#[test]
fn setup_not_called_returns_none() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let raw = host
        .send_run_init_lua(
            "-- no setup call".to_owned(),
            "test_init.lua".to_owned(),
            None,
        )
        .unwrap();
    assert!(raw.is_none());
}

#[test]
fn setup_all_sections_at_once() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let raw = host
        .send_run_init_lua(
            r#"n00n.setup({
                always_yolo = true,
                always_fast = true,
                always_thinking = "adaptive",
                ui = { splash_animation = false, mouse_scroll_lines = 5 },
                agent = { max_output_lines = 9000 },
                provider = { default_model = "anthropic/claude-opus-4-6" },
                storage = { max_log_files = 3 },
                plugins = { bash = { enabled = true, timeout_secs = 180 }, websearch = { enabled = false } },
            })"#
            .to_owned(),
            "test_init.lua".to_owned(),
            None,
        )
        .unwrap()
        .expect("expected Some(RawConfig)");
    assert_eq!(raw.always_yolo, Some(true));
    assert_eq!(raw.always_fast, Some(true));
    assert_eq!(
        raw.always_thinking,
        Some(AlwaysThinking::Mode("adaptive".into()))
    );
    assert_eq!(raw.ui.splash_animation, Some(false));
    assert_eq!(raw.ui.mouse_scroll_lines, Some(5));
    assert_eq!(raw.agent.max_output_lines, Some(9000));
    assert_eq!(
        raw.provider.default_model.as_deref(),
        Some("anthropic/claude-opus-4-6")
    );
    assert_eq!(raw.storage.max_log_files, Some(3));
    assert_eq!(raw.plugins["bash"].enabled, Some(true));
    assert_eq!(
        raw.plugins["bash"].opts["timeout_secs"],
        serde_json::json!(180)
    );
    assert_eq!(raw.plugins["websearch"].enabled, Some(false));
}

const OPTS_PROBE_PLUGIN: &str = r#"
local opts = n00n.api.register_options({
    timeout_secs = { default = 120, min = 5, desc = "Timeout." },
    label = { type = "string", desc = "Label." },
})
n00n.api.register_tool({
    name = "opts_probe",
    description = "returns merged opts",
    schema = { type = "object", properties = {}, additionalProperties = false },
    audiences = { "main" },
    handler = function(input, ctx)
        return (n00n.json.encode({
            timeout_secs = opts.timeout_secs,
            label = opts.label,
        }))
    end
})
"#;

const UNKNOWN_OPTION_ERR: &str =
    "unknown option \"typo\" for plugins.opts_plugin (valid options: label, timeout_secs)";
const OPTION_TYPE_ERR: &str =
    "invalid value for plugins.opts_plugin.timeout_secs: expected integer";
const OPTION_MIN_ERR: &str =
    "invalid value for plugins.opts_plugin.timeout_secs: 1 is below minimum (5)";
const OPTION_DESC_ERR: &str = "option \"timeout_secs\": desc is required";
const OPTION_NO_TYPE_ERR: &str = "option \"bare\": type is required when there is no default";
const OPTION_SPEC_KEY_ERR: &str = "option \"timeout_secs\": unknown spec key \"mins\"";
const OPTION_DEFAULT_TYPE_ERR: &str =
    "option \"timeout_secs\": default 120 does not match type string";
const OPTION_DEFAULT_MIN_ERR: &str = "option \"timeout_secs\": default 1 is below min (5)";
const OPTION_MIN_ON_STRING_ERR: &str = "option \"label\": min is not allowed for type string";
const OPTION_RESERVED_ERR: &str = "option \"enabled\": reserved name";
const OPTION_TWICE_ERR: &str = "register_options: called more than once";
const UNDECLARED_OPTS_ERR: &str = "unknown options in plugins.bare_plugin: timeout_secs \
(this plugin declares no options via n00n.api.register_options)";

fn probe_opts(reg: &ToolRegistry) -> serde_json::Value {
    let out = exec_tool(reg, "opts_probe", serde_json::json!({})).unwrap();
    serde_json::from_str(&out).unwrap()
}

fn json_obj(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    v.as_object().expect("test opts must be an object").clone()
}

#[test_case::test_case(
    serde_json::json!({}),
    serde_json::json!(120), serde_json::Value::Null
    ; "defaults_without_user_opts"
)]
#[test_case::test_case(
    serde_json::json!({ "timeout_secs": 30, "label": "x" }),
    serde_json::json!(30), serde_json::json!("x")
    ; "user_opts_win"
)]
fn register_options_merges(
    opts: serde_json::Value,
    timeout_secs: serde_json::Value,
    label: serde_json::Value,
) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source_with_opts("opts_plugin", OPTS_PROBE_PLUGIN, json_obj(opts))
        .unwrap();

    let snap = probe_opts(&reg);
    assert_eq!(snap["timeout_secs"], timeout_secs);
    assert_eq!(snap["label"], label);
}

#[test_case::test_case(serde_json::json!({ "typo": 1 }), UNKNOWN_OPTION_ERR ; "unknown_key")]
#[test_case::test_case(serde_json::json!({ "timeout_secs": "abc" }), OPTION_TYPE_ERR ; "wrong_type")]
#[test_case::test_case(serde_json::json!({ "timeout_secs": 12.5 }), OPTION_TYPE_ERR ; "float_for_integer")]
#[test_case::test_case(serde_json::json!({ "timeout_secs": 1 }), OPTION_MIN_ERR ; "below_min")]
fn register_options_rejects_bad_user_opts(opts: serde_json::Value, expected: &str) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let err = host
        .load_source_with_opts("opts_plugin", OPTS_PROBE_PLUGIN, json_obj(opts))
        .expect_err("plugin load should fail");
    assert!(err.to_string().contains(expected), "got: {err}");
}

#[test_case::test_case(
    r"n00n.api.register_options({ timeout_secs = { default = 120 } })",
    OPTION_DESC_ERR
    ; "missing_desc"
)]
#[test_case::test_case(
    r#"n00n.api.register_options({ bare = { desc = "no type or default" } })"#,
    OPTION_NO_TYPE_ERR
    ; "missing_type_and_default"
)]
#[test_case::test_case(
    r#"n00n.api.register_options({ timeout_secs = { default = 120, mins = 5, desc = "T." } })"#,
    OPTION_SPEC_KEY_ERR
    ; "unknown_spec_key"
)]
#[test_case::test_case(
    r#"n00n.api.register_options({ timeout_secs = { type = "string", default = 120, desc = "T." } })"#,
    OPTION_DEFAULT_TYPE_ERR
    ; "default_contradicts_type"
)]
#[test_case::test_case(
    r#"n00n.api.register_options({ timeout_secs = { default = 1, min = 5, desc = "T." } })"#,
    OPTION_DEFAULT_MIN_ERR
    ; "default_below_min"
)]
#[test_case::test_case(
    r#"n00n.api.register_options({ label = { type = "string", min = 1, desc = "L." } })"#,
    OPTION_MIN_ON_STRING_ERR
    ; "min_on_string"
)]
#[test_case::test_case(
    r#"n00n.api.register_options({ enabled = { default = true, desc = "E." } })"#,
    OPTION_RESERVED_ERR
    ; "reserved_enabled"
)]
#[test_case::test_case(
    r#"
    n00n.api.register_options({ a = { default = 1, desc = "A." } })
    n00n.api.register_options({ b = { default = 2, desc = "B." } })
    "#,
    OPTION_TWICE_ERR
    ; "called_twice"
)]
fn register_options_rejects_bad_spec(src: &str, expected: &str) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let err = host
        .load_source("opts_plugin", src)
        .expect_err("plugin load should fail");
    assert!(err.to_string().contains(expected), "got: {err}");
}

#[test]
fn builtin_opts_flow_from_setup_plugins() {
    let reg = fresh_registry();
    let mut host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let raw = host
        .send_run_init_lua(
            "n00n.setup({ plugins = { grep = { search_result_limit = 42 } } })".to_owned(),
            "test_init.lua".to_owned(),
            None,
        )
        .unwrap()
        .expect("expected Some(RawConfig)");
    host.load_builtins(&PluginsConfig::from_plugins(&raw.plugins))
        .unwrap();

    let options = host.plugin_options().unwrap();
    let grep = options.get("grep").expect("grep options registered");
    let limit = grep
        .iter()
        .find(|o| o.name == "search_result_limit")
        .expect("search_result_limit declared");
    assert!(limit.default.is_some(), "declared default surfaces");
    assert!(limit.min.is_some(), "declared min surfaces");
    assert!(!limit.desc.is_empty(), "declared desc surfaces");
}

#[test_case::test_case(
    serde_json::json!({}),
    &["edit", "multiedit"], &["edit_lines", "insert_lines"]
    ; "multiedit_on_others_opt_in"
)]
#[test_case::test_case(
    serde_json::json!({ "multiedit": false, "edit_lines": true }),
    &["edit", "edit_lines"], &["multiedit", "insert_lines"]
    ; "toggles_flip_sub_tools"
)]
fn edit_sub_tools_follow_edit_opts(opts: serde_json::Value, on: &[&str], off: &[&str]) {
    let reg = fresh_registry();
    let mut host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let config = PluginsConfig {
        enabled: true,
        names: vec!["edit".to_owned()],
        opts: HashMap::from([("edit".to_owned(), json_obj(opts))]),
    };
    host.load_builtins(&config).unwrap();
    for tool in on {
        assert!(reg.get(tool).is_some(), "{tool} should be registered");
    }
    for tool in off {
        assert!(reg.get(tool).is_none(), "{tool} should not be registered");
    }
}

#[test]
fn undeclared_opts_fail_the_load() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let err = host
        .load_source_with_opts(
            "bare_plugin",
            "local x = 1",
            json_obj(serde_json::json!({ "timeout_secs": 30 })),
        )
        .expect_err("plugin load should fail");
    assert!(err.to_string().contains(UNDECLARED_OPTS_ERR), "got: {err}");
}

#[test]
fn opts_for_unknown_plugin_fail_load_builtins() {
    let reg = fresh_registry();
    let mut host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let mut config = PluginsConfig::from_plugins(&HashMap::new());
    config.opts.insert(
        "bsah".to_owned(),
        json_obj(serde_json::json!({ "timeout_secs": 5 })),
    );
    let err = host
        .load_builtins(&config)
        .expect_err("load_builtins should fail");
    assert!(
        err.to_string()
            .contains("plugins.bsah sets options (timeout_secs)"),
        "got: {err}"
    );
}

#[test]
fn unknown_plugin_name_fails_load_builtins() {
    let reg = fresh_registry();
    let mut host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let mut config = PluginsConfig::from_plugins(&HashMap::new());
    config.names.push("gerp".to_string());
    let err = host
        .load_builtins(&config)
        .expect_err("load_builtins should fail");
    assert!(
        err.to_string().contains("no bundled plugin named \"gerp\""),
        "got: {err}"
    );
}

#[test]
fn disabled_plugin_opts_are_ignored_not_rejected() {
    let reg = fresh_registry();
    let mut host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let config = PluginsConfig {
        enabled: true,
        names: vec!["grep".to_owned()],
        opts: HashMap::from([(
            "bash".to_owned(),
            json_obj(serde_json::json!({ "timeout_secs": 180 })),
        )]),
    };
    host.load_builtins(&config).unwrap();
    assert!(reg.get("bash").is_none(), "bash stays disabled");
    assert!(reg.get("grep").is_some(), "enabled plugin still loads");
}

#[test_case::test_case("true", AlwaysThinking::Toggle(true) ; "bool")]
#[test_case::test_case("8192", AlwaysThinking::Budget(8192) ; "number")]
#[test_case::test_case("\"adaptive\"", AlwaysThinking::Mode("adaptive".into()) ; "string")]
fn setup_always_thinking_variants(lua_val: &str, expected: AlwaysThinking) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let raw = host
        .send_run_init_lua(
            format!("n00n.setup({{ always_thinking = {lua_val} }})"),
            "test_init.lua".to_owned(),
            None,
        )
        .unwrap()
        .expect("expected Some(RawConfig)");
    assert_eq!(raw.always_thinking, Some(expected));
}

#[test]
fn setup_no_tool_registration_in_init_env() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let err = host
        .send_run_init_lua(
            r#"n00n.register_tool({
                name = "sneaky",
                description = "should fail",
                audiences = { "main" },
                handler = function() return "nope" end
            })"#
            .to_owned(),
            "test_init.lua".to_owned(),
            None,
        )
        .expect_err("register_tool should not be available in init.lua env");
    assert!(
        matches!(err, PluginError::Lua { .. }),
        "expected Lua error, got: {err}"
    );
}

#[test]
fn register_command_happy_path() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source(
        "cmd_plugin",
        r#"
        n00n.api.register_command({
            name = "/hello",
            description = "says hello",
            handler = function(args) end,
        })
        "#,
    )
    .unwrap();

    let reader = host.command_reader();
    let snap = reader.load();
    assert_eq!(snap.commands.len(), 1);
    assert_eq!(snap.commands[0].name.as_ref(), "/hello");
    assert_eq!(snap.commands[0].description.as_ref(), "says hello");
    assert_eq!(snap.commands[0].plugin.as_ref(), "cmd_plugin");
}

#[test_case::test_case(
    r#"n00n.api.register_command({ name = "", handler = function() end })"#,
    "non-empty" ; "empty_name"
)]
#[test_case::test_case(
    r#"n00n.api.register_command({ name = "/test", description = "no handler" })"#,
    "handler" ; "missing_handler"
)]
fn register_command_validation_rejects(src: &str, expected_err: &str) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let err = host
        .load_source("bad_cmd", src)
        .expect_err("expected validation error");
    assert!(matches!(err, PluginError::Lua { .. }));
    assert!(err.to_string().contains(expected_err), "got: {err}");
}

#[test]
fn reload_replaces_commands() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source(
        "reload_cmd",
        r#"n00n.api.register_command({ name = "/v1", handler = function() end })"#,
    )
    .unwrap();

    host.load_source(
        "reload_cmd",
        r#"n00n.api.register_command({ name = "/v2", handler = function() end })"#,
    )
    .unwrap();
    let snap = host.command_reader().load();
    assert_eq!(snap.commands.len(), 1);
    assert_eq!(snap.commands[0].name.as_ref(), "/v2");
}

#[test]
fn unload_clears_commands() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source(
        "cmd_only",
        r#"n00n.api.register_command({ name = "/bye", handler = function() end })"#,
    )
    .unwrap();
    assert_eq!(host.command_reader().load().commands.len(), 1);

    host.unload("cmd_only").unwrap();
    assert_eq!(host.command_reader().load().commands.len(), 0);
}

#[test]
fn sessions_plugin_registers_commands() {
    let (_reg, host) = builtins_host();
    let snap = host.command_reader().load();
    let names: Vec<&str> = snap.commands.iter().map(|c| c.name.as_ref()).collect();
    assert!(
        names.contains(&"/sessions"),
        "missing /sessions in {names:?}"
    );
    assert!(names.contains(&"/rename"), "missing /rename in {names:?}");
}

#[test]
fn job_callback_finishes_after_handler_returns_nil() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "job_after_return",
            description = "on_exit finishes after handler returns nil",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                n00n.fn.jobstart("true", {{
                    on_exit = function(_, code)
                        ctx:finish("exit=" .. tostring(code))
                    end,
                }})
                return nil
            end
        }})"#,
    );
    host.load_source("job_after_return", &src).unwrap();
    let out = exec_tool(&reg, "job_after_return", serde_json::json!({})).unwrap();
    assert_eq!(out, "exit=0");
}

#[test]
fn ctx_set_deadline_times_out() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "deadline_test",
            description = "uses ctx:set_deadline",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                ctx:set_deadline(2)
                n00n.fn.jobstart("sleep 30", {{
                    on_exit = function(_, _) ctx:finish("should-not-reach") end,
                }})
                return nil
            end
        }})"#,
    );
    host.load_source("deadline_test", &src).unwrap();
    let err = exec_tool(&reg, "deadline_test", serde_json::json!({})).unwrap_err();
    assert!(err.contains(TIMED_OUT_SUBSTR), "got: {err}");
}

#[test]
fn ctx_set_deadline_twice_errors() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "deadline_twice",
            description = "calls set_deadline twice",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                ctx:set_deadline(5)
                ctx:set_deadline(5)
            end
        }})"#,
    );
    host.load_source("deadline_twice", &src).unwrap();
    let err = exec_tool(&reg, "deadline_twice", serde_json::json!({})).unwrap_err();
    assert!(err.contains(DEADLINE_ALREADY_SET_ERR), "got: {err}");
}

#[test]
fn restore_tool_async_ordering_and_delivery() {
    let (_reg, host) = builtins_host();

    let input = serde_json::json!({"command": "echo ok", "timeout": 1});

    let handle = host.event_handle().expect("event handle available");
    let (tx, rx) = flume::unbounded();
    let event_tx = n00n_agent::EventSender::new(tx, 0);

    let bash_item = |id: &str| n00n_lua::RestoreItem {
        tool: Arc::from("bash"),
        tool_use_id: id.to_owned(),
        output: "tool bash timed out after 1s".to_owned(),
        input: input.clone(),
        is_error: true,
        tool_output_lines: ToolOutputLines::default(),
        theme_gen: None,
        clicks: Vec::new(),
        state: None,
    };
    let unknown_item = n00n_lua::RestoreItem {
        tool: Arc::from("definitely_not_a_tool"),
        tool_use_id: "unknown_id".to_owned(),
        output: "ignored".to_owned(),
        input: serde_json::json!({}),
        is_error: false,
        tool_output_lines: ToolOutputLines::default(),
        theme_gen: None,
        clicks: Vec::new(),
        state: None,
    };

    handle.request_restore(unknown_item, event_tx.clone());
    handle.request_restore(bash_item("a"), event_tx.clone());
    handle.request_restore(bash_item("b"), event_tx);

    handle.wait_restore_complete_for_test();

    let snapshots: Vec<n00n_agent::Envelope> = rx.drain().collect();

    let tool_ids: Vec<&str> = snapshots
        .iter()
        .filter_map(|env| match &env.event {
            n00n_agent::AgentEvent::ToolSnapshot { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();

    assert!(
        !tool_ids.contains(&"unknown_id"),
        "unknown tool should emit no snapshots"
    );
    assert!(
        tool_ids.contains(&"a"),
        "known tool 'a' should emit snapshot"
    );
    assert!(
        tool_ids.contains(&"b"),
        "known tool 'b' should emit snapshot"
    );
}

#[test_case::test_case(
    "write",
    serde_json::json!({"path": "/tmp/x.md", "content": "alpha\nbeta"}),
    "wrote 10 bytes to /tmp/x.md",
    &["alpha", "beta"]
    ; "write_tool_restores_file_content"
)]
#[test_case::test_case(
    "memory",
    serde_json::json!({"command": "write", "path": "n.md", "content": "gamma"}),
    "wrote n.md (1 lines)",
    &["gamma"]
    ; "memory_write_restores_saved_content"
)]
fn restore_rebuilds_body_from_input_content(
    tool: &str,
    input: serde_json::Value,
    summary: &str,
    expected: &[&str],
) {
    let (_reg, host) = builtins_host();
    let handle = host.event_handle().expect("event handle available");
    let (tx, rx) = flume::unbounded();

    handle.request_restore(
        n00n_lua::RestoreItem {
            tool: Arc::from(tool),
            tool_use_id: "restore_id".to_owned(),
            output: summary.to_owned(),
            input,
            is_error: false,
            tool_output_lines: ToolOutputLines::default(),
            theme_gen: None,
            clicks: vec![0],
            state: None,
        },
        n00n_agent::EventSender::new(tx, 0),
    );
    handle.wait_restore_complete_for_test();

    let mut text = String::new();
    for env in rx.drain() {
        if let n00n_agent::AgentEvent::ToolSnapshot { snapshot, .. } = env.event {
            for line in snapshot.lines.iter() {
                for span in &line.spans {
                    text.push_str(&span.text);
                }
            }
        }
    }

    for needle in expected {
        assert!(
            text.contains(needle),
            "restored body missing '{needle}', got: {text}"
        );
    }
    assert!(
        !text.contains(summary),
        "restored body should show content, not the summary: {text}"
    );
}

/// Guards the stale-cancelled-handle bug: `permission_scopes` must call
/// the plugin callback and return parsed scopes, not fall back to raw JSON.
/// A leaked `{"command":...}` scope would break allow rules.
#[test_case::test_case("git status" ; "parseable command")]
#[test_case::test_case("echo 'unterminated" ; "unparseable command")]
fn bash_permission_scopes_never_falls_back_to_json(command: &str) {
    let (reg, _host) = builtins_host();

    let input = serde_json::json!({ "command": command });
    let entry = reg.get("bash").expect("bash registered");
    let inv = entry.tool.parse(&input).expect("parse failed");
    let scopes = smol::block_on(inv.permission_scopes())
        .expect("permission_scopes returned None (would fall back to raw JSON)");

    assert!(
        !scopes.scopes.iter().any(|s| s.contains("\"command\"")),
        "fell back to raw JSON scope: {:?}",
        scopes.scopes
    );
}

fn exec_tool_with_perms(
    perms: n00n_lua::PluginPermissions,
    src: &str,
    tool: &str,
    input: serde_json::Value,
) -> Result<String, String> {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source_with_permissions("perm_test", src, perms)
        .unwrap();
    exec_tool(&reg, tool, input)
}

fn perm_tool_src(name: &str, handler_body: &str) -> String {
    format!(
        r#"n00n.api.register_tool({{
            name = "{name}",
            description = "d",
            schema = {{ type = "object", properties = {{}}, additionalProperties = false }},
            handler = function(input, ctx)
                {handler_body}
            end,
        }})"#
    )
}

#[test_case::test_case(
    "read_deny",
    r#"local ok, err = pcall(function() n00n.fs.read("/etc/hostname") end)
                return tostring(err)"#,
    "fs_read"
    ; "fs_read_denied"
)]
#[test_case::test_case(
    "write_deny",
    r#"local ok, err = pcall(function() n00n.fs.write("/tmp/test", "x") end)
                return tostring(err)"#,
    "fs_write"
    ; "fs_write_denied"
)]
#[test_case::test_case(
    "run_deny",
    r#"local ok, err = pcall(function() n00n.fn.jobstart("echo hi") end)
                return tostring(err)"#,
    "run"
    ; "run_denied"
)]
fn denied_permission_blocks_api(tool_name: &str, handler_body: &str, expected_perm: &str) {
    let src = perm_tool_src(tool_name, handler_body);
    let result = exec_tool_with_perms(
        n00n_lua::PluginPermissions::denied(),
        &src,
        tool_name,
        serde_json::json!({}),
    )
    .unwrap();
    assert!(result.contains(PERMISSION_DENIED_MSG), "got: {result}");
    assert!(result.contains(expected_perm), "got: {result}");
}

#[test]
fn user_plugin_with_fs_read_can_read_but_not_write() {
    let src = perm_tool_src(
        "rw_test",
        r#"local read_ok = pcall(function() n00n.fs.read("/dev/null") end)
                local write_ok = pcall(function() n00n.fs.write("/tmp/test", "x") end)
                return "read=" .. tostring(read_ok) .. ",write=" .. tostring(write_ok)"#,
    );
    let mut perms = n00n_lua::PluginPermissions::denied();
    perms.set(n00n_lua::Permission::FsRead, true);
    let result = exec_tool_with_perms(perms, &src, "rw_test", serde_json::json!({})).unwrap();
    assert!(result.contains("read=true"), "got: {result}");
    assert!(result.contains("write=false"), "got: {result}");
}

#[test]
fn builtin_plugin_has_all_permissions() {
    let src = perm_tool_src(
        "trusted_test",
        r#"local cwd_ok = pcall(function() n00n.uv.cwd() end)
                local env_ok = pcall(function() n00n.env.state_dir() end)
                return "cwd=" .. tostring(cwd_ok) .. ",env=" .. tostring(env_ok)"#,
    );
    let result = exec_tool_with_perms(
        n00n_lua::PluginPermissions::trusted(),
        &src,
        "trusted_test",
        serde_json::json!({}),
    )
    .unwrap();
    assert!(result.contains("cwd=true"), "got: {result}");
    assert!(result.contains("env=true"), "got: {result}");
}

#[test]
fn env_permission_guards_uv_and_env() {
    let src = perm_tool_src(
        "env_guard_test",
        r#"local cwd_ok = pcall(function() n00n.uv.cwd() end)
                local home_ok = pcall(function() n00n.uv.os_homedir() end)
                local env_ok = pcall(function() n00n.env.state_dir() end)
                local exec_ok = pcall(function() n00n.fn.executable("ls") end)
                return "cwd=" .. tostring(cwd_ok) .. ",home=" .. tostring(home_ok) .. ",env=" .. tostring(env_ok) .. ",exec=" .. tostring(exec_ok)"#,
    );
    let result = exec_tool_with_perms(
        n00n_lua::PluginPermissions::denied(),
        &src,
        "env_guard_test",
        serde_json::json!({}),
    )
    .unwrap();
    assert!(result.contains("cwd=false"), "got: {result}");
    assert!(result.contains("home=false"), "got: {result}");
    assert!(result.contains("env=false"), "got: {result}");
    assert!(result.contains("exec=false"), "got: {result}");
}

const PATH_FIELD_SCHEMA: &str = r#"{
    type = "object",
    properties = { path = { type = "string" } },
    required = { "path" },
}"#;

#[test_case::test_case(STRING_FIELD_SCHEMA, "nonexistent" ; "missing_field")]
#[test_case::test_case(NON_STRING_FIELD_SCHEMA, "count" ; "non_string_field")]
fn mutable_path_invalid_rejected(schema: &str, scope_field: &str) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"n00n.api.register_tool({{
            name = "bad_mpath",
            description = "test",
            schema = {schema},
            mutable_path = "{scope_field}",
            handler = function() return "" end
        }})"#,
    );
    let err = host
        .load_source("bad_mpath_plugin", &src)
        .expect_err("expected error for invalid mutable_path");

    assert!(matches!(err, PluginError::Lua { .. }));
    assert!(
        err.to_string().contains("mutable_path")
            && err.to_string().contains(INVALID_PERMISSION_SCOPE_ERR),
        "got: {err}"
    );
}

#[test]
fn mutable_path_returns_path_from_input() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();

    let src = format!(
        r#"n00n.api.register_tool({{
            name = "mp_read",
            description = "test",
            schema = {PATH_FIELD_SCHEMA},
            mutable_path = "path",
            handler = function() return "" end
        }})"#,
    );
    host.load_source("mp_read_plugin", &src).unwrap();

    let entry = reg.get("mp_read").expect("tool not registered");
    let inv = entry
        .tool
        .parse(&serde_json::json!({ "path": "/tmp/foo.txt" }))
        .expect("parse failed");
    assert_eq!(inv.mutable_path(), Some(Path::new("/tmp/foo.txt")));
}

#[test]
fn pure_functions_not_guarded() {
    let src = perm_tool_src(
        "pure_test",
        r#"local dirname_ok = pcall(function() n00n.fs.dirname("/foo/bar") end)
                local basename_ok = pcall(function() n00n.fs.basename("/foo/bar") end)
                local json_ok = pcall(function() n00n.json.encode({a=1}) end)
                return "dirname=" .. tostring(dirname_ok) .. ",basename=" .. tostring(basename_ok) .. ",json=" .. tostring(json_ok)"#,
    );
    let result = exec_tool_with_perms(
        n00n_lua::PluginPermissions::denied(),
        &src,
        "pure_test",
        serde_json::json!({}),
    )
    .unwrap();
    assert!(result.contains("dirname=true"), "got: {result}");
    assert!(result.contains("basename=true"), "got: {result}");
    assert!(result.contains("json=true"), "got: {result}");
}

#[test]
fn runaway_allocation_hits_memory_limit_instead_of_oom() {
    const LIMITED: &str = "limited";
    let src = r#"
        local ok, err = pcall(function()
            local t = {}
            local chunk = string.rep("x", 1024 * 1024)
            while true do
                t[#t + 1] = chunk .. tostring(#t)
            end
        end)
        if ok then error("expected allocation to fail under the memory limit") end
        if not string.find(tostring(err), "memory") then
            error("expected an out-of-memory error, got: " .. tostring(err))
        end
    "#;
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source(LIMITED, src)
        .expect("plugin should hit the memory limit and recover, not crash the process");
}

#[test]
fn start_hook_publishes_live_buf_for_tool_use_id() {
    let (reg, _host) = start_hook_fixture();
    let rx = run_start(&reg, "st_tool", serde_json::json!({"code": "line1\nline2"}));
    let body = recv_live_buf(&rx, START_TOOL_USE_ID).expect("start must publish a LiveToolBuf");
    let text = body.take().text();
    assert!(text.contains("line1"), "preview must render input: {text}");
}

#[test]
fn start_hook_error_does_not_fail_tool() {
    let (reg, _host) = start_hook_fixture();
    let _rx = run_start(&reg, "st_boom", serde_json::json!({"code": "x"}));
    let out = exec_tool(&reg, "st_boom", serde_json::json!({"code": "x"})).expect("handler ok");
    assert_eq!(out, "handled");
}

#[test]
fn start_skipped_for_tool_without_start_fn() {
    let (reg, _host) = start_hook_fixture();
    let rx = run_start(&reg, "st_plain", serde_json::json!({"code": "x"}));
    assert!(
        recv_live_buf(&rx, START_TOOL_USE_ID).is_none(),
        "no start fn must mean no preview"
    );
}

/// `start` runs before permission checks, so its ctx can read and preview
/// but dispatch/finish/deadline must come back as `(nil, err)`.
#[test]
fn start_ctx_capabilities() {
    let (reg, _host) = start_hook_fixture();
    let rx = run_start(&reg, "st_probe", serde_json::json!({"code": "x"}));
    let body = recv_live_buf(&rx, START_TOOL_USE_ID).expect("probe publishes a buf");
    let text = body.take().text();
    assert_eq!(
        text,
        "call_tool_err finish_err deadline_err config_ok cancelled_ok workflow_ok audience_ok tol_ok",
        "start ctx capability matrix mismatch"
    );
}

const START_TOOL_USE_ID: &str = "start-tu-1";

fn start_hook_fixture() -> (Arc<ToolRegistry>, PluginHost) {
    let src = format!(
        r#"
local function preview(input, ctx)
    local buf = n00n.ui.buf()
    buf:set_lines({{ input.code }})
    ctx:live_buf(buf)
end
n00n.api.register_tool({{
    name = "st_tool",
    description = "test",
    schema = {CODE_SCHEMA},
    start = preview,
    handler = function(input, ctx) return "handled" end,
}})
n00n.api.register_tool({{
    name = "st_boom",
    description = "test",
    schema = {CODE_SCHEMA},
    start = function(input, ctx) error("boom") end,
    handler = function(input, ctx) return "handled" end,
}})
n00n.api.register_tool({{
    name = "st_plain",
    description = "test",
    schema = {CODE_SCHEMA},
    handler = function(input, ctx) return "handled" end,
}})
n00n.api.register_tool({{
    name = "st_probe",
    description = "test",
    schema = {CODE_SCHEMA},
    start = function(input, ctx)
        local parts = {{}}
        local function pair_err(v, e)
            return v == nil and type(e) == "string"
        end
        parts[1] = pair_err(n00n.agent.call_tool(ctx, "st_plain", {{ code = "x" }})) and "call_tool_err"
            or "call_tool_ok"
        parts[2] = pair_err(ctx:finish("x")) and "finish_err" or "finish_ok"
        parts[3] = pair_err(ctx:set_deadline(5)) and "deadline_err" or "deadline_ok"
        parts[4] = type(ctx:config()) == "table" and "config_ok" or "config_bad"
        parts[5] = ctx:cancelled() == false and "cancelled_ok" or "cancelled_bad"
        parts[6] = type(ctx:workflow()) == "boolean" and "workflow_ok" or "workflow_bad"
        parts[7] = type(ctx:audience()) == "string" and "audience_ok" or "audience_bad"
        parts[8] = type(ctx:tool_output_lines()) == "table" and "tol_ok" or "tol_bad"
        local buf = n00n.ui.buf()
        buf:set_lines({{ table.concat(parts, " ") }})
        ctx:live_buf(buf)
    end,
    handler = function(input, ctx) return "handled" end,
}})
"#
    );
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source("start_hooks", &src).unwrap();
    (reg, host)
}

/// `start` is awaited to completion, so the returned receiver already holds
/// everything the hook emitted.
fn run_start(
    reg: &ToolRegistry,
    name: &str,
    input: serde_json::Value,
) -> flume::Receiver<n00n_agent::Envelope> {
    let (tx, rx) = flume::unbounded::<n00n_agent::Envelope>();
    let event_tx = n00n_agent::EventSender::new(tx, 0);
    let ctx = n00n_agent::tools::test_support::stub_ctx_with(
        &n00n_agent::AgentMode::Build,
        Some(&event_tx),
        Some(START_TOOL_USE_ID),
    );
    let inv = reg
        .get(name)
        .unwrap_or_else(|| panic!("tool {name} not registered"))
        .tool
        .parse(&input)
        .expect("parse failed");
    smol::block_on(inv.start(&ctx));
    rx
}

fn recv_live_buf(
    rx: &flume::Receiver<n00n_agent::Envelope>,
    id: &str,
) -> Option<Arc<n00n_agent::SharedBuf>> {
    rx.drain().find_map(|env| match env.event {
        n00n_agent::AgentEvent::LiveToolBuf { id: got, body } if got == id => Some(body),
        _ => None,
    })
}

#[test]
fn start_annotation_timeout_happy_path() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "sa_to",
            description = "test",
            schema = {TIMEOUT_SCHEMA},
            start_annotation = {{ field = "timeout", kind = "timeout" }},
            handler = function(input, ctx) return "" end
        }})"#,
    );
    host.load_source("sa_to_plugin", &src).unwrap();
    let entry = reg.get("sa_to").expect("tool not registered");
    let inv = entry
        .tool
        .parse(&serde_json::json!({"timeout": 90}))
        .expect("parse failed");
    assert_eq!(inv.start_annotation(), Some(timeout_annotation(90)));
}

#[test]
fn start_annotation_count_happy_path() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "sa_ct",
            description = "test",
            schema = {ARRAY_SCHEMA},
            start_annotation = "edits",
            handler = function(input, ctx) return "" end
        }})"#,
    );
    host.load_source("sa_ct_plugin", &src).unwrap();
    let entry = reg.get("sa_ct").expect("tool not registered");
    let inv = entry
        .tool
        .parse(&serde_json::json!({"edits": [1, 2, 3]}))
        .expect("parse failed");
    assert_eq!(inv.start_annotation(), Some("3 edits".to_owned()));
}

#[test_case::test_case(START_ANNOTATION_COUNT_NON_ARRAY_SRC, STRING_NAME_SCHEMA, "not in schema properties or not type 'array'" ; "start_annotation_count_non_array")]
fn registration_with_schema_rejects(fields: &str, schema: &str, expected_err: &str) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            {fields},
            schema = {schema},
            handler = function(input, ctx) return "" end
        }})"#,
    );
    let err = host
        .load_source("schema_val_test", &src)
        .expect_err("expected validation error");
    assert!(matches!(err, PluginError::Lua { .. }));
    assert!(err.to_string().contains(expected_err), "got: {err}");
}

#[test]
fn interpreter_on_output_streams_lines() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "interp_stream",
            description = "streams interpreter output",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local lines = {{}}
                local result, err = n00n.interpreter.run("print('a')\nprint('b')", {{
                    timeout = 10,
                    max_memory_mb = 50,
                    on_output = function(line)
                        table.insert(lines, line)
                    end,
                }})
                if err then return "err: " .. err end
                return table.concat(lines, "|") .. ";stdout=" .. (result.stdout or "")
            end
        }})"#,
    );
    host.load_source("interp_stream_plugin", &src).unwrap();
    let out = exec_tool(&reg, "interp_stream", serde_json::json!({})).unwrap();
    assert_eq!(out, "a|b;stdout=a\nb");
}

const SESSION_CLOSED_ERR: &str = "session closed";

fn interp_tool_plugin(name: &str, python: &str, tools_lua: &str) -> String {
    format!(
        r#"n00n.api.register_tool({{
            name = "{name}",
            description = "test",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local lines = {{}}
                local result, err = n00n.interpreter.run("{python}", {{
                    timeout = 10,
                    max_memory_mb = 50,
                    on_output = function(line) table.insert(lines, line) end,
                    tools = {tools_lua},
                }})
                if err then return "err: " .. err end
                return table.concat(lines, "|")
            end
        }})"#
    )
}

#[test]
fn interpreter_tools_fn_map_kwargs_reach_lua_tool() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = interp_tool_plugin(
        "interp_tools",
        r"r = await greet(name='bob')\nprint(r)",
        "{ greet = function(input) return 'hi:' .. input.name end }",
    );
    host.load_source("interp_tools_plugin", &src).unwrap();
    let out = exec_tool(&reg, "interp_tools", serde_json::json!({})).unwrap();
    assert_eq!(out, "hi:bob");
}

#[test]
fn interpreter_tools_nil_err_pair_fails_call() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = interp_tool_plugin(
        "interp_err",
        r"await bad()",
        "{ bad = function(input) return nil, 'boom' end }",
    );
    host.load_source("interp_err_plugin", &src).unwrap();
    let out = exec_tool(&reg, "interp_err", serde_json::json!({})).unwrap();
    assert!(out.starts_with("err: "), "got: {out}");
    assert!(out.contains("boom"), "got: {out}");
}

#[test]
fn interpreter_tools_gather_resolves_parallel_batch() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = interp_tool_plugin(
        "interp_gather",
        r"import asyncio\nasync def main():\n    a, b = await asyncio.gather(t_a(), t_b())\n    print(a + '|' + b)\nawait main()",
        "{ t_a = function(input) return 'A' end, t_b = function(input) return 'B' end }",
    );
    host.load_source("interp_gather_plugin", &src).unwrap();
    let out = exec_tool(&reg, "interp_gather", serde_json::json!({})).unwrap();
    assert_eq!(out, "A|B");
}

#[test]
fn call_tool_resolves_lua_tool_and_reports_unknown() {
    let reg = Arc::clone(ToolRegistry::global_arc());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    host.load_source("echo_plugin", ECHO_PLUGIN).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "call_tool_probe",
            description = "test",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local out, err = n00n.agent.call_tool(ctx, "echo_", {{ msg = "hello" }})
                if err ~= nil then return "unexpected err: " .. err end
                local out2, err2 = n00n.agent.call_tool(ctx, "no_such_tool_xyz", {{}})
                if out2 ~= nil then return "unexpected output: " .. out2 end
                if err2 == nil then return "expected err for unknown tool" end
                return out
            end
        }})"#
    );
    host.load_source("call_tool_plugin", &src).unwrap();
    let out = exec_tool_in(
        &reg,
        "call_tool_probe",
        serde_json::json!({}),
        Some(Arc::clone(&reg)),
    )
    .unwrap();
    assert_eq!(out, "hello");
    host.unload("call_tool_plugin").unwrap();
    host.unload("echo_plugin").unwrap();
}

struct ScriptedSessionProvider {
    responses: Mutex<VecDeque<Result<StreamResponse, AgentError>>>,
}

impl ScriptedSessionProvider {
    fn new(responses: impl IntoIterator<Item = Result<StreamResponse, AgentError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }
}

impl Provider for ScriptedSessionProvider {
    fn stream_message<'a>(
        &'a self,
        _: &'a Model,
        _: &'a [Message],
        _: &'a str,
        _: &'a serde_json::Value,
        _: &'a flume::Sender<ProviderEvent>,
        _: RequestOptions,
        _: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted session provider exhausted")
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<n00n_providers::ModelInfo>, AgentError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

fn session_response(
    content: Vec<ContentBlock>,
    usage: TokenUsage,
    stop_reason: StopReason,
) -> StreamResponse {
    StreamResponse {
        message: Message {
            role: Role::Assistant,
            content,
            ..Message::default()
        },
        usage,
        stop_reason: Some(stop_reason),
    }
}

fn run_session_usage_probe(provider: ScriptedSessionProvider, fast: bool) -> serde_json::Value {
    let registry = fresh_registry();
    let host = PluginHost::new(Arc::clone(&registry)).unwrap();
    let source = format!(
        r#"n00n.api.register_tool({{
            name = "session_usage_probe",
            description = "test",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local sess, session_err = n00n.agent.session(ctx, {{ fast = {fast} }})
                if session_err then return session_err end
                local result, prompt_err = sess:prompt("measure this")
                sess:close()
                local encoded, encode_err = n00n.json.encode({{ result = result, error = prompt_err }})
                if encode_err then return encode_err end
                return encoded
            end
        }})"#,
    );
    host.load_source("session_usage_plugin", &source).unwrap();

    let entry = registry.get("session_usage_probe").unwrap();
    let invocation = entry.tool.parse(&serde_json::json!({})).unwrap();
    let (event_tx, event_rx) = flume::unbounded();
    let event_tx = n00n_agent::EventSender::new(event_tx, 0);
    let mut ctx = n00n_agent::tools::test_support::stub_ctx_with(
        &n00n_agent::AgentMode::Build,
        Some(&event_tx),
        None,
    );
    ctx.provider = Arc::new(provider);
    ctx.model = Arc::new(Model::from_spec("anthropic/claude-opus-4-8").unwrap());
    ctx.registry = Arc::clone(&registry);

    let output = smol::block_on(invocation.execute(&ctx))
        .output
        .expect("session usage probe failed");
    drop(event_rx);
    serde_json::from_str(&output.as_text()).expect("session usage probe returned invalid JSON")
}

#[test]
fn session_prompt_returns_current_usage_on_normal_completion() {
    let usage = TokenUsage {
        input: 2,
        output: 7,
        cache_creation: 5,
        cache_read: 3,
    };
    let output = run_session_usage_probe(
        ScriptedSessionProvider::new([Ok(session_response(
            vec![ContentBlock::Text {
                text: "finished".to_owned(),
            }],
            usage,
            StopReason::EndTurn,
        ))]),
        true,
    );

    assert_eq!(output["error"], serde_json::Value::Null);
    assert_eq!(output["result"]["text"], "finished");
    assert_eq!(output["result"]["fresh_input_tokens"], usage.input);
    assert_eq!(output["result"]["cache_read_tokens"], usage.cache_read);
    assert_eq!(output["result"]["cache_write_tokens"], usage.cache_creation);
    assert_eq!(output["result"]["input_tokens"], usage.total_input());
    assert_eq!(output["result"]["output_tokens"], usage.output);
    assert_eq!(output["result"]["fast"], true);
    let model = Model::from_spec("anthropic/claude-opus-4-8").unwrap();
    assert_eq!(output["result"]["cost"], usage.cost(&model.pricing, true));
}

#[test]
fn session_prompt_returns_charged_usage_with_later_error() {
    let usage = TokenUsage {
        input: 17,
        output: 29,
        cache_creation: 23,
        cache_read: 19,
    };
    let output = run_session_usage_probe(
        ScriptedSessionProvider::new([
            Ok(session_response(
                vec![ContentBlock::ToolUse {
                    id: "charged-call".to_owned(),
                    name: "missing_tool".to_owned(),
                    input: serde_json::json!({}),
                }],
                usage,
                StopReason::ToolUse,
            )),
            Err(AgentError::Config {
                message: "charged failure".to_owned(),
            }),
        ]),
        false,
    );

    assert_eq!(output["error"], "charged failure");
    assert_eq!(output["result"]["fresh_input_tokens"], usage.input);
    assert_eq!(output["result"]["cache_read_tokens"], usage.cache_read);
    assert_eq!(output["result"]["cache_write_tokens"], usage.cache_creation);
    assert_eq!(output["result"]["input_tokens"], usage.total_input());
    assert_eq!(output["result"]["output_tokens"], usage.output);
    assert_eq!(output["result"]["fast"], false);
    let model = Model::from_spec("anthropic/claude-opus-4-8").unwrap();
    assert_eq!(output["result"]["cost"], usage.cost(&model.pricing, false));
}

#[test]
fn session_close_idempotent_and_prompt_after_close_errors() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "session_probe",
            description = "test",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local sess = n00n.agent.session(ctx, {{}})
                sess:close()
                sess:close()
                local result, err = sess:prompt("x")
                if result ~= nil then return "unexpected result" end
                return err or "no error"
            end
        }})"#
    );
    host.load_source("session_plugin", &src).unwrap();
    let out = exec_tool(&reg, "session_probe", serde_json::json!({})).unwrap();
    assert_eq!(out, SESSION_CLOSED_ERR);
}

#[test_case::test_case("{ audience = 'wurkflow' }", "unknown audience: wurkflow" ; "unknown_audience")]
#[test_case::test_case("{ local_tools = { foo = { handler = function() return '' end } } }", "local_tools.foo: 'description' is required" ; "local_tool_missing_description")]
#[test_case::test_case("{ local_tools = { foo = { description = 'd' } } }", "local_tools.foo: 'handler' is required" ; "local_tool_missing_handler")]
fn session_opts_validation_rejects(opts: &str, expected: &str) {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "session_opts_probe",
            description = "test",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local sess, err = n00n.agent.session(ctx, {opts})
                if sess ~= nil then return "unexpected session" end
                return err or "no error"
            end
        }})"#
    );
    host.load_source("session_opts_plugin", &src).unwrap();
    let out = exec_tool(&reg, "session_opts_probe", serde_json::json!({})).unwrap();
    assert!(out.contains(expected), "got: {out}");
}

fn load_img_tool(host: &PluginHost) {
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "img_probe",
            description = "test",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                return {{
                    llm_output = "[image: test 1x1]",
                    image = {{ media_type = "image/png", data = "aGVsbG8=" }},
                }}
            end
        }})"#
    );
    host.load_source("img_plugin", &src).unwrap();
}

#[test]
fn lua_tool_image_reply_maps_to_image_output() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    load_img_tool(&host);
    let out = exec_tool_output(&reg, "img_probe", serde_json::json!({})).unwrap();
    let n00n_agent::ToolOutput::Image { source, text, .. } = out else {
        panic!("expected Image output, got {out:?}");
    };
    assert_eq!(source.media_type, n00n_agent::ImageMediaType::Png);
    assert_eq!(&*source.data, "aGVsbG8=");
    assert_eq!(text, "[image: test 1x1]");
}

#[test]
fn call_tool_flattens_image_output_with_not_visible_note() {
    use n00n_agent::tools::interpreter_bridge::IMAGE_NOT_VISIBLE_NOTE;

    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    load_img_tool(&host);
    let src = format!(
        r#"n00n.api.register_tool({{
            name = "img_caller",
            description = "test",
            schema = {MINIMAL_SCHEMA},
            audiences = {{ "main" }},
            handler = function(input, ctx)
                local out, err = n00n.agent.call_tool(ctx, "img_probe", {{}})
                return err or out
            end
        }})"#
    );
    host.load_source("img_caller_plugin", &src).unwrap();
    let out = exec_tool_in(
        &reg,
        "img_caller",
        serde_json::json!({}),
        Some(Arc::clone(&reg)),
    )
    .unwrap();
    assert_eq!(out, format!("[image: test 1x1] ({IMAGE_NOT_VISIBLE_NOTE})"));
}

#[test]
fn view_image_tool_returns_image_output() {
    use base64::Engine as _;

    let (reg, _host) = builtins_host();

    // The code_execution bridge flattens output to text, so view_image is
    // pointless from the interpreter.
    let audience = reg.get("view_image").unwrap().tool.audience();
    assert!(audience.contains(n00n_agent::tools::ToolAudience::MAIN));
    assert!(!audience.contains(n00n_agent::tools::ToolAudience::INTERPRETER));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiny.png");
    let img = image::DynamicImage::new_rgb8(4, 2);
    img.save_with_format(&path, image::ImageFormat::Png)
        .unwrap();

    let out = exec_tool_output(
        &reg,
        "view_image",
        serde_json::json!({"path": path.to_str().unwrap()}),
    )
    .unwrap();
    let n00n_agent::ToolOutput::Image { source, text, .. } = out else {
        panic!("expected Image output, got {out:?}");
    };
    assert_eq!(source.media_type, n00n_agent::ImageMediaType::Png);
    assert!(text.contains("tiny.png"), "caption: {text}");
    assert!(text.contains("4x2"), "caption: {text}");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&*source.data)
        .unwrap();
    assert_eq!(decoded, std::fs::read(&path).unwrap());
}

#[test]
fn view_image_tool_rejects_non_image() {
    let (reg, _host) = builtins_host();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "plain text").unwrap();
    let err = exec_tool_output(
        &reg,
        "view_image",
        serde_json::json!({"path": path.to_str().unwrap()}),
    )
    .unwrap_err();
    assert!(err.contains("not an image"), "got: {err}");
}

fn probe_output(data: &str) -> (image::ImageFormat, u32, u32) {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap();
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .unwrap();
    let format = reader.format().unwrap();
    let (w, h) = reader.into_dimensions().unwrap();
    (format, w, h)
}

#[test]
fn view_image_downscales_oversized_png_with_honest_caption() {
    let (reg, _host) = builtins_host();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wide.png");
    image::DynamicImage::new_rgb8(2000, 100)
        .save_with_format(&path, image::ImageFormat::Png)
        .unwrap();

    let out = exec_tool_output(
        &reg,
        "view_image",
        serde_json::json!({"path": path.to_str().unwrap()}),
    )
    .unwrap();
    let n00n_agent::ToolOutput::Image { source, text, .. } = out else {
        panic!("expected Image output, got {out:?}");
    };
    assert_eq!(source.media_type, n00n_agent::ImageMediaType::Png);
    assert!(text.contains("downscaled from 2000x100"), "caption: {text}");

    let (format, w, h) = probe_output(&source.data);
    assert_eq!(format, image::ImageFormat::Png);
    assert_eq!(w, 1568, "long edge must land exactly on the API limit");
    assert!(h <= 79, "aspect ratio broken: {w}x{h}");
    // Caption must report the dimensions actually shipped, not the original.
    assert!(text.contains(&format!("{w}x{h}")), "caption: {text}");
}

#[test]
fn view_image_oversized_gif_reencodes_to_png_first_frame() {
    let (reg, _host) = builtins_host();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("banner.gif");
    image::DynamicImage::new_rgb8(2000, 8)
        .save_with_format(&path, image::ImageFormat::Gif)
        .unwrap();

    let out = exec_tool_output(
        &reg,
        "view_image",
        serde_json::json!({"path": path.to_str().unwrap()}),
    )
    .unwrap();
    let n00n_agent::ToolOutput::Image { source, text, .. } = out else {
        panic!("expected Image output, got {out:?}");
    };
    // gif encoding is unsupported, so downscaling forces png; the caption
    // must confess the downscale and the lost animation.
    assert_eq!(source.media_type, n00n_agent::ImageMediaType::Png);
    assert!(text.contains("downscaled from 2000x8"), "caption: {text}");
    assert!(text.contains("first frame only"), "caption: {text}");
    assert_eq!(probe_output(&source.data).0, image::ImageFormat::Png);
}

#[test]
fn view_image_small_gif_passes_through_unchanged() {
    use base64::Engine as _;

    let (reg, _host) = builtins_host();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiny.gif");
    image::DynamicImage::new_rgb8(4, 2)
        .save_with_format(&path, image::ImageFormat::Gif)
        .unwrap();

    let out = exec_tool_output(
        &reg,
        "view_image",
        serde_json::json!({"path": path.to_str().unwrap()}),
    )
    .unwrap();
    let n00n_agent::ToolOutput::Image { source, text, .. } = out else {
        panic!("expected Image output, got {out:?}");
    };
    assert_eq!(source.media_type, n00n_agent::ImageMediaType::Gif);
    assert!(
        !text.contains("first frame only"),
        "pass-through keeps animation, caption must not claim otherwise: {text}"
    );
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&*source.data)
        .unwrap();
    assert_eq!(
        decoded,
        std::fs::read(&path).unwrap(),
        "under-limit gif must ship byte-identical, not re-encoded"
    );
}

#[test]
fn interpreter_bridge_flattens_image_with_visibility_note() {
    let reg = fresh_registry();
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    load_img_tool(&host);

    let mut ctx = n00n_agent::tools::test_support::stub_ctx(&n00n_agent::AgentMode::Build);
    ctx.registry = Arc::clone(&reg);
    let out = smol::block_on(n00n_agent::tools::interpreter_bridge::dispatch(
        &ctx,
        "img_probe",
        &serde_json::json!({}),
    ))
    .unwrap();
    assert!(out.starts_with("[image: test 1x1]"), "got: {out}");
    assert!(
        out.contains(n00n_agent::tools::interpreter_bridge::IMAGE_NOT_VISIBLE_NOTE),
        "got: {out}"
    );
}

#[test]
fn bundled_todo_panel_keeps_current_todo_stable_in_hint() {
    let (reg, host) = builtins_host();
    let ui_rx = host.ui_action_rx().unwrap();
    exec_tool(
        &reg,
        "todo_write",
        serde_json::json!({
            "todos": [
                { "content": "Run tests", "status": "in_progress", "priority": "high" }
            ]
        }),
    )
    .unwrap();
    let open = ui_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let n00n_lua::UiAction::OpenWin { .. } = open else {
        panic!("todo tool did not open its panel");
    };
    let handle = host.event_handle().unwrap();
    let toggle_id = host
        .keymap_reader()
        .load()
        .entries
        .iter()
        .find(|entry| entry.desc == "Toggle todo panel")
        .unwrap()
        .id;
    assert!(handle.run_keybind_callback(toggle_id));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while host.hint_reader().load().entries.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "todo panel did not collapse"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.fire_autocmd(
        "ToolStart",
        serde_json::json!({
            "id": "cmd-1",
            "tool": "bash",
            "summary": "cargo test --workspace",
        }),
    );

    let hints = host.hint_reader().load();
    let text = hints
        .entries
        .iter()
        .flat_map(|(_, spans)| spans.iter().map(|(text, _)| text.as_str()))
        .collect::<String>();
    assert!(
        text.contains("Run tests"),
        "current todo disappeared: {text}"
    );
    assert!(
        !text.contains("cargo test --workspace"),
        "transient tool activity replaced the current todo: {text}"
    );

    handle.fire_autocmd(
        "ToolDone",
        serde_json::json!({ "id": "cmd-1", "tool": "bash", "is_error": false }),
    );
}

#[test]
fn bundled_todo_running_click_toggles_and_final_done_resets_collapsed() {
    let (reg, host) = builtins_host();
    let ui_rx = host.ui_action_rx().unwrap();
    let handle = host.event_handle().unwrap();
    handle.fire_autocmd(
        "ToolStart",
        serde_json::json!({ "id": "cmd-1", "tool": "bash", "summary": "cargo test" }),
    );
    barrier(&host);
    exec_tool(
        &reg,
        "todo_write",
        serde_json::json!({
            "todos": [
                { "content": "Run tests", "status": "in_progress", "priority": "high" }
            ]
        }),
    )
    .unwrap();
    let n00n_lua::UiAction::OpenWin { buf, cmd_rx, .. } =
        ui_rx.recv_timeout(Duration::from_secs(2)).unwrap()
    else {
        panic!("todo tool did not open its panel");
    };
    let text = || {
        buf.read()
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
            .collect::<String>()
    };
    let wait_for = |needle: &str| {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let got = text();
            if got.contains(needle) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "missing {needle:?}: {got}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    wait_for("Running ▸");
    assert!(
        buf.click().is_some(),
        "panel click handler must remain registered"
    );
    handle.request_buf_click(Arc::clone(&buf), 1);
    assert!(
        cmd_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
        "click handler did not reconfigure panel"
    );
    wait_for("Running ▾");
    handle.fire_autocmd(
        "ToolDone",
        serde_json::json!({ "id": "cmd-1", "tool": "bash", "is_error": false }),
    );
    handle.fire_autocmd(
        "ToolStart",
        serde_json::json!({ "id": "cmd-2", "tool": "bash", "summary": "cargo clippy" }),
    );
    barrier(&host);
    wait_for("Running ▸");
    assert!(
        !text().contains("Running ▾"),
        "new activity must start collapsed"
    );
}

#[test]
fn bundled_todo_ctrl_t_keybind_dispatches() {
    let (_reg, host) = builtins_host();
    let snap = host.keymap_reader().load();
    let entry = snap
        .entries
        .iter()
        .find(|entry| entry.desc == "Toggle todo panel")
        .expect("todo plugin must publish its Ctrl+T keybind");
    assert_eq!(entry.key, crossterm::event::KeyCode::Char('t'));
    assert_eq!(entry.modifiers, crossterm::event::KeyModifiers::CONTROL);
    assert!(
        host.event_handle().unwrap().run_keybind_callback(entry.id),
        "live plugin host must accept the Ctrl+T callback"
    );
}

#[test]
fn team_launcher_uses_native_model_picker_and_amp_labels() {
    let (_reg, host) = builtins_host();
    let rx = host.ui_action_rx().unwrap();
    let handle = host.event_handle().unwrap();
    handle.run_command(
        Arc::from("team"),
        Arc::from("/team"),
        "fix the parser".into(),
    );

    let action = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("Team launcher did not open");
    let n00n_lua::UiAction::OpenWin { buf, event_tx, .. } = action else {
        panic!("expected Team launcher window");
    };
    let rendered = || {
        buf.read()
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.text.as_str()))
            .collect::<String>()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let initial = loop {
        let text = rendered();
        if text.contains("Model: Default (tier routing)") {
            break text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "team launcher did not render: {text}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(initial.contains("Start team"), "{initial}");
    assert!(!initial.contains("Exact model"), "{initial}");

    for key in ["down", "down", "enter"] {
        event_tx
            .send(n00n_lua::WinEvent::Key { key: key.into() })
            .unwrap();
    }
    let action = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("Model picker did not open");
    let n00n_lua::UiAction::PickModel { current, reply_tx } = action else {
        panic!("expected native model picker request");
    };
    assert_eq!(current, None);
    reply_tx
        .send(Some("anthropic/claude-sonnet-4-6".into()))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !rendered().contains("anthropic/claude-sonnet-4-6") {
        assert!(
            std::time::Instant::now() < deadline,
            "selected model was not rendered: {}",
            rendered()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    event_tx
        .send(n00n_lua::WinEvent::Key {
            key: "ctrl+enter".into(),
        })
        .unwrap();
    let action = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("Team launcher did not submit a session prompt");
    let n00n_lua::UiAction::Session { req, reply_tx } = action else {
        panic!("expected Team session prompt");
    };
    let n00n_lua::SessionRequest::Prompt { text, .. } = req else {
        panic!("expected a prompt request");
    };
    assert!(
        text.contains("model: anthropic/claude-sonnet-4-6"),
        "submitted prompt: {text}"
    );
    assert!(
        text.contains("model_tier: strong"),
        "tier routing default was not retained: {text}"
    );
    reply_tx.send(Ok(serde_json::json!("started"))).unwrap();
}

#[test]
fn team_launcher_collects_goal_and_submits_configured_prompt() {
    let (_reg, host) = builtins_host();
    let rx = host.ui_action_rx().unwrap();
    let handle = host.event_handle().unwrap();
    handle.run_command(Arc::from("team"), Arc::from("/team"), String::new());

    let action = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("Team launcher did not open");
    let n00n_lua::UiAction::OpenWin { event_tx, .. } = action else {
        panic!("expected Team launcher window");
    };
    event_tx
        .send(n00n_lua::WinEvent::Paste {
            text: "fix the parser".into(),
        })
        .unwrap();
    event_tx
        .send(n00n_lua::WinEvent::Key {
            key: "ctrl+enter".into(),
        })
        .unwrap();

    let action = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("Team launcher did not submit a session prompt");
    let n00n_lua::UiAction::Session { req, reply_tx } = action else {
        panic!("expected Team session prompt");
    };
    let n00n_lua::SessionRequest::Prompt { id, text } = req else {
        panic!("expected a prompt request");
    };
    assert!(id.is_none());
    assert!(
        text.contains("Goal:\nfix the parser"),
        "submitted prompt: {text}"
    );
    assert!(
        text.contains("mode: supervised"),
        "submitted prompt: {text}"
    );
    assert!(
        text.contains("Use the team tool now"),
        "submitted prompt: {text}"
    );
    assert!(text.contains("thinking: max"), "submitted prompt: {text}");
    assert!(text.contains("auto_tier: true"), "submitted prompt: {text}");
    reply_tx.send(Ok(serde_json::json!("started"))).unwrap();
}

/// The sessions picker parks its command handler in a `win:recv` loop while a
/// `n00n.async.run` task fetches the stored-session list. Queued async tasks
/// must run while the spawning handler is still parked, not wait for the next
/// unrelated lua-thread event.
#[test]
fn async_run_from_parked_command_handler_runs_promptly() {
    let host = PluginHost::new(fresh_registry()).unwrap();
    host.load_source(
        "p",
        r#"
        n00n.api.register_command({
            name = "/park",
            description = "parks forever",
            handler = function()
                n00n.async.run(function()
                    n00n.ui.flash("task-ran")
                end)
                n00n.async.await(1, function(_cb) end)
            end,
        })
        "#,
    )
    .unwrap();
    let rx = host.ui_action_rx().unwrap();
    let handle = host.event_handle().unwrap();
    handle.run_command(Arc::from("p"), Arc::from("/park"), String::new());

    let action = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("async.run task starved while its command handler was parked");
    assert!(matches!(action, n00n_lua::UiAction::Flash(msg) if msg == "task-ran"));
}

/// Job callbacks must fire while a detached command handler is parked
/// (the homepage `/standup` example: jobstart, then a `win:recv` loop).
#[test]
fn job_callbacks_fire_while_command_handler_parked() {
    let host = PluginHost::new(fresh_registry()).unwrap();
    host.load_source(
        "p",
        r#"
        n00n.api.register_command({
            name = "/stream",
            description = "streams job output while parked",
            handler = function()
                n00n.fn.jobstart("echo hi", {
                    on_stdout = function(_, line) n00n.ui.flash("job:" .. line) end,
                })
                n00n.async.await(1, function(_cb) end)
            end,
        })
        "#,
    )
    .unwrap();
    let rx = host.ui_action_rx().unwrap();
    let handle = host.event_handle().unwrap();
    handle.run_command(Arc::from("p"), Arc::from("/stream"), String::new());

    let action = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("job callbacks starved while command handler was parked");
    assert!(matches!(action, n00n_lua::UiAction::Flash(msg) if msg == "job:hi"));
}
