#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

//! Tests the task plugin's structured-output policy end-to-end: real plugin
//! source, real `n00n.json` / `n00n.async`, with model I/O replaced by
//! scriptable Lua stubs.

use std::sync::Arc;

use n00n_agent::agent::tool_dispatch::{self, Emit};
use n00n_agent::tools::test_support::stub_ctx;
use n00n_agent::tools::{ToolExecResult, ToolRegistry};
use n00n_agent::{AgentMode, ToolDoneEvent, ToolOutput};
use n00n_lua::PluginHost;
use serde_json::{Value, json};

const TASK_PLUGIN_SRC: &str = include_str!("../../plugins/task/init.lua");

// Mirrors of the plugin's error contracts and policy numbers.
const STRUCTURED_OUTPUT_TOOL: &str = "structured_output";
const MAX_SCHEMA_ERRORS: usize = 3;
const OVERSIZED_SCHEMA_DESCRIPTION_BYTES: usize = 33_000;
const OVERDEEP_SCHEMA_LEVELS: usize = 20;
const SCHEMA_COMPILE_ERROR: &str = "invalid output_schema";
const SCHEMA_ROOT_ERROR: &str = "output_schema must have type object";
const SCHEMA_SIZE_ERROR: &str = "output_schema exceeds 32768-byte limit";
const SCHEMA_DEPTH_ERROR: &str = "output_schema exceeds maximum depth of 16";
const STRUCTURED_MISSING_ERROR: &str = "subagent finished without calling structured_output";
const STRUCTURED_INVALID_ERROR: &str = "subagent result does not match output_schema";
const UNKNOWN_SUBAGENT_ERR: &str = "unknown subagent type: bogus";
const SUB_AGENT_ERROR_PREFIX: &str = "sub-agent error: ";

const TASK_TOOL: &str = "task";
const PROBE_TOOL: &str = "probe";
const TASK_PROMPT: &str = "do the thing";
const PLAIN_TEXT: &str = "plain text result";
const PROMPT_ERR_MSG: &str = "model exploded";
const RAISE_MSG: &str = "stub prompt kaboom";
/// Mirrors the task plugin's `max_concurrent` default.
const TASK_DEFAULT_MAX_CONCURRENT: u64 = 4;

const SCENARIO_PLAIN: &str = "plain";
const SCENARIO_HAPPY: &str = "happy";
const SCENARIO_INVALID_THEN_VALID: &str = "invalid_then_valid";
const SCENARIO_NEVER_STRUCTURED: &str = "never_structured";
const SCENARIO_INVALID_ONLY: &str = "invalid_only";
const SCENARIO_PROMPT_ERROR: &str = "prompt_error";
const SCENARIO_RAISE: &str = "raise";
const SCENARIO_SLOW: &str = "slow";

/// Stubs keyed by `opts.name` (the task's `description`). `n00n.json` and
/// `n00n.async` stay real so schema validation and semaphore behavior are tested.
const STUB_PRELUDE: &str = r#"
recorder = { prompts = {}, closed = 0, sessions = 0, acquired = 0, released = 0 }

-- Spy wrapper: the real semaphore does the work, counters track that every
-- permit is explicitly released (gc would silently hide a leak).
local real_semaphore = n00n.async.semaphore
n00n.async.semaphore = function(n)
  recorder.sem_size = n
  local sem = real_semaphore(n)
  return {
    acquire = function(self)
      local permit = sem:acquire()
      recorder.acquired = recorder.acquired + 1
      return {
        release = function(p)
          recorder.released = recorder.released + 1
          return permit:release()
        end,
      }
    end,
  }
end

n00n.agent.resolve_model = function(ctx, opts)
  return { spec = "test/model" }
end

n00n.agent.system_prompt = function(ctx, opts)
  return "sys"
end

n00n.agent.usage_cost = function(spec, input_tokens, output_tokens, breakdown)
  recorder.usage_fast = breakdown and breakdown.fast
  return 0.01, nil
end

n00n.agent.tools = function(ctx, opts)
  return {}
end

local behaviors = {}

behaviors.plain = function(sess, msg)
  return {
    text = "@PLAIN_TEXT@",
    input_tokens = 100,
    output_tokens = 20,
    fresh_input_tokens = 50,
    cache_read_tokens = 30,
    cache_write_tokens = 20,
    fast = true,
    raw_prompt = "PRIVATE_TASK_PROMPT",
  }
end

behaviors.happy = function(sess, msg)
  local h = sess.opts.local_tools.structured_output.handler
  recorder.first_ack, recorder.first_err = h({ answer = "42" })
  return { text = "" }
end

behaviors.invalid_then_valid = function(sess, msg)
  local h = sess.opts.local_tools.structured_output.handler
  recorder.first_ack, recorder.first_err = h({ answer = 42 })
  recorder.second_ack, recorder.second_err = h({ answer = "42" })
  return { text = "raw text ignored" }
end

behaviors.never_structured = function(sess, msg)
  return { text = "no structured call" }
end

behaviors.invalid_only = function(sess, msg)
  local h = sess.opts.local_tools.structured_output.handler
  recorder.first_ack, recorder.first_err = h({ a = 1, b = 2, c = 3, d = 4 })
  return { text = "still invalid" }
end

behaviors.prompt_error = function(sess, msg)
  return {
    input_tokens = 30,
    output_tokens = 5,
    fresh_input_tokens = 20,
    cache_read_tokens = 7,
    cache_write_tokens = 3,
    raw_prompt = "PRIVATE_ERROR_PROMPT",
  }, "@PROMPT_ERR@"
end

behaviors.raise = function(sess, msg)
  error("@RAISE_MSG@")
end

n00n.api.register_tool({
  name = "slow_tool",
  description = "simulated slow model",
  schema = { type = "object", properties = {}, additionalProperties = false },
  audiences = { "main" },
  handler = function(input, ctx)
    n00n.fn.jobstart("@SLOW_CMD@", { on_exit = function() ctx:finish("ok") end })
  end,
})

behaviors.slow = function(sess, msg)
  local out, err = n00n.agent.call_tool(sess.ctx, "slow_tool", {})
  if err then error(err) end
  return { text = "@PLAIN_TEXT@" }
end

n00n.agent.session = function(ctx, opts)
  recorder.sessions = recorder.sessions + 1
  recorder.has_local_tools = opts.local_tools ~= nil
  recorder.structured_output_schema = opts.local_tools and opts.local_tools.structured_output and opts.local_tools.structured_output.input_schema
  local sess = { opts = opts, ctx = ctx, turn = 0 }
  function sess:prompt(msg)
    self.turn = self.turn + 1
    recorder.prompts[#recorder.prompts + 1] = msg
    return behaviors[opts.name](self, msg)
  end
  function sess:get_progress()
    return { turn_id = self.turn, completed_count = 0, recent_tools = {}, activities = {}, elapsed_ms = 0, done = true }
  end
  function sess:close()
    recorder.closed = recorder.closed + 1
  end
  function sess:done(answer)
    recorder.done_answer = answer
  end
  return sess
end

n00n.api.register_tool({
  name = "probe",
  description = "recorder snapshot",
  schema = { type = "object", properties = {}, additionalProperties = false },
  audiences = { "main" },
  handler = function(input, ctx)
    local snap = {
      sessions = recorder.sessions,
      closed = recorder.closed,
      prompt_count = #recorder.prompts,
      has_local_tools = recorder.has_local_tools,
      structured_output_schema = recorder.structured_output_schema,
      first_ack = recorder.first_ack,
      first_err = recorder.first_err,
      second_ack = recorder.second_ack,
      second_err = recorder.second_err,
      acquired = recorder.acquired,
      released = recorder.released,
      sem_size = recorder.sem_size,
      usage_fast = recorder.usage_fast,
    }
    if #recorder.prompts > 0 then
      snap.prompts = recorder.prompts
    end
    return (n00n.json.encode(snap))
  end,
})
"#;

fn load_task_host() -> (Arc<ToolRegistry>, PluginHost) {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let prelude = STUB_PRELUDE
        .replace("@PLAIN_TEXT@", PLAIN_TEXT)
        .replace("@PROMPT_ERR@", PROMPT_ERR_MSG)
        .replace("@RAISE_MSG@", RAISE_MSG)
        .replace(
            "@SLOW_CMD@",
            if cfg!(windows) {
                "ping -n 2 127.0.0.1 > nul"
            } else {
                "sleep 0.2"
            },
        );
    host.load_source("task_policy", &format!("{prelude}\n{TASK_PLUGIN_SRC}"))
        .unwrap();
    (reg, host)
}

fn exec_tool_result(reg: &Arc<ToolRegistry>, name: &str, input: Value) -> ToolExecResult {
    let entry = reg
        .get(name)
        .unwrap_or_else(|| panic!("tool {name} not registered"));
    let inv = entry.tool.parse(&input).expect("parse failed");
    let mut ctx = stub_ctx(&AgentMode::Build);
    ctx.registry = Arc::clone(reg);
    smol::block_on(async { inv.execute(&ctx).await })
}

fn exec_tool_output(
    reg: &Arc<ToolRegistry>,
    name: &str,
    input: Value,
) -> Result<ToolOutput, String> {
    exec_tool_result(reg, name, input).output
}

fn exec_tool(reg: &Arc<ToolRegistry>, name: &str, input: Value) -> Result<String, String> {
    exec_tool_output(reg, name, input).map(|out| match out {
        ToolOutput::Plain(s) | ToolOutput::Markdown(s) => s.text,
        other => panic!("unexpected output: {other:?}"),
    })
}

fn dispatch_tool(reg: &Arc<ToolRegistry>, name: &str, input: Value) -> ToolDoneEvent {
    let mut ctx = stub_ctx(&AgentMode::Build);
    ctx.registry = Arc::clone(reg);
    smol::block_on(tool_dispatch::run(
        reg,
        None,
        "test-call".to_owned(),
        name,
        &input,
        &ctx,
        Emit::Silent,
    ))
}

fn probe(reg: &Arc<ToolRegistry>) -> Value {
    let out = exec_tool(reg, PROBE_TOOL, json!({})).expect("probe failed");
    serde_json::from_str(&out).expect("probe returned invalid json")
}

fn task_input(scenario: &str, output_schema: Option<Value>) -> Value {
    let mut input = json!({ "description": scenario, "prompt": TASK_PROMPT });
    if let Some(schema) = output_schema {
        input["output_schema"] = schema;
    }
    input
}

fn answer_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "answer": { "type": "string" } },
        "required": ["answer"],
        "additionalProperties": false,
    })
}

/// Four wrong-typed properties, one more than `MAX_SCHEMA_ERRORS`, so
/// truncation in `bounded_errors` is observable.
fn multi_error_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "a": { "type": "string" },
            "b": { "type": "string" },
            "c": { "type": "string" },
            "d": { "type": "string" },
        },
        "required": ["a", "b", "c", "d"],
    })
}

#[test_case::test_case(json!({"subagent_type": "bogus"}), UNKNOWN_SUBAGENT_ERR ; "unknown_subagent_type")]
#[test_case::test_case(json!({"output_schema": {"type": 42}}), SCHEMA_ROOT_ERROR ; "invalid_output_schema_type")]
#[test_case::test_case(json!({"output_schema": {"type": "string"}}), SCHEMA_ROOT_ERROR ; "primitive_output_schema")]
#[test_case::test_case(json!({"output_schema": {"type": "object", "properties": 42}}), SCHEMA_COMPILE_ERROR ; "invalid_object_output_schema")]
fn bad_input_errors_before_any_session(extra: Value, expected_prefix: &str) {
    let (reg, _host) = load_task_host();
    let mut input = task_input(SCENARIO_PLAIN, None);
    for (k, v) in extra.as_object().unwrap() {
        input[k.as_str()] = v.clone();
    }
    let err = exec_tool(&reg, TASK_TOOL, input).unwrap_err();
    assert!(err.starts_with(expected_prefix), "got: {err}");
    let snap = probe(&reg);
    assert_eq!(snap["sessions"], json!(0));
    assert_eq!(snap["prompt_count"], json!(0));
}

#[test]
fn oversized_schema_errors_before_any_session() {
    let (reg, _host) = load_task_host();
    let schema = json!({
        "type": "object",
        "description": "x".repeat(OVERSIZED_SCHEMA_DESCRIPTION_BYTES),
    });
    let err = exec_tool(&reg, TASK_TOOL, task_input(SCENARIO_PLAIN, Some(schema))).unwrap_err();

    assert_eq!(err, SCHEMA_SIZE_ERROR);
    let snap = probe(&reg);
    assert_eq!(snap["sessions"], json!(0));
    assert_eq!(snap["prompt_count"], json!(0));
}

#[test]
fn deeply_nested_schema_errors_before_any_session() {
    let (reg, _host) = load_task_host();
    let mut schema = json!({ "type": "string" });
    for _ in 0..OVERDEEP_SCHEMA_LEVELS {
        schema = json!({
            "type": "object",
            "properties": { "nested": schema },
        });
    }
    let err = exec_tool(&reg, TASK_TOOL, task_input(SCENARIO_PLAIN, Some(schema))).unwrap_err();

    assert_eq!(err, SCHEMA_DEPTH_ERROR);
    let snap = probe(&reg);
    assert_eq!(snap["sessions"], json!(0));
}

#[test]
fn structured_happy_path_returns_validated_json() {
    let (reg, _host) = load_task_host();
    let out = exec_tool(
        &reg,
        TASK_TOOL,
        task_input(SCENARIO_HAPPY, Some(answer_schema())),
    )
    .expect("structured task failed");
    let parsed: Value = serde_json::from_str(&out).expect("result is not json");
    assert_eq!(parsed, json!({ "answer": "42" }));

    let snap = probe(&reg);
    assert_eq!(snap["sessions"], json!(1));
    assert_eq!(snap["closed"], json!(1));
    assert_eq!(snap["prompt_count"], json!(1));
    assert_eq!(snap["has_local_tools"], json!(true));
    assert!(snap["first_ack"].is_string(), "valid input must be acked");
    assert!(snap.get("first_err").is_none_or(Value::is_null));
    let prompt = snap["prompts"][0].as_str().expect("prompt missing");
    assert!(prompt.starts_with(TASK_PROMPT), "got: {prompt}");
    assert!(
        prompt.contains(STRUCTURED_OUTPUT_TOOL),
        "prompt must point at the structured_output tool: {prompt}"
    );
}

#[test]
fn invalid_then_valid_recovers_within_one_prompt() {
    let (reg, _host) = load_task_host();
    let out = exec_tool(
        &reg,
        TASK_TOOL,
        task_input(SCENARIO_INVALID_THEN_VALID, Some(answer_schema())),
    )
    .expect("task should succeed after inline retry");
    let parsed: Value = serde_json::from_str(&out).expect("result is not json");
    assert_eq!(parsed, json!({ "answer": "42" }));

    let snap = probe(&reg);
    assert!(snap.get("first_ack").is_none_or(Value::is_null));
    let first_err = snap["first_err"].as_str().expect("first_err missing");
    assert!(
        first_err.contains("/answer"),
        "inline error should point at the failing path: {first_err}"
    );
    assert!(snap["second_ack"].is_string(), "valid retry must be acked");
    assert!(snap.get("second_err").is_none_or(Value::is_null));
    assert_eq!(snap["prompt_count"], json!(1));
    assert_eq!(snap["closed"], json!(1));
}

#[test]
fn missing_structured_output_errors_after_bounded_nudges() {
    let (reg, _host) = load_task_host();
    let err = exec_tool(
        &reg,
        TASK_TOOL,
        task_input(SCENARIO_NEVER_STRUCTURED, Some(answer_schema())),
    )
    .unwrap_err();
    assert_eq!(err, STRUCTURED_MISSING_ERROR);

    let snap = probe(&reg);
    assert_eq!(snap["prompt_count"], json!(2));
    assert_eq!(snap["closed"], json!(1));
}

#[test]
fn invalid_only_errors_with_bounded_schema_errors() {
    let (reg, _host) = load_task_host();
    let err = exec_tool(
        &reg,
        TASK_TOOL,
        task_input(SCENARIO_INVALID_ONLY, Some(multi_error_schema())),
    )
    .unwrap_err();
    assert!(err.starts_with(STRUCTURED_INVALID_ERROR), "got: {err}");
    assert_eq!(err.lines().count(), 1 + MAX_SCHEMA_ERRORS, "got: {err}");

    let snap = probe(&reg);
    assert_eq!(snap["prompt_count"], json!(2));
    let first_err = snap["first_err"].as_str().expect("first_err missing");
    assert_eq!(
        first_err.lines().count(),
        1 + MAX_SCHEMA_ERRORS,
        "inline error must carry at most MAX_SCHEMA_ERRORS validation lines: {first_err}"
    );
}

#[test]
fn prompt_error_maps_to_sub_agent_error_with_charged_telemetry() {
    let (reg, _host) = load_task_host();
    let result = dispatch_tool(&reg, TASK_TOOL, task_input(SCENARIO_PROMPT_ERROR, None));
    assert!(result.is_error);
    assert_eq!(
        result.output.as_text(),
        format!("{SUB_AGENT_ERROR_PREFIX}{PROMPT_ERR_MSG}")
    );
    let telemetry = result
        .output
        .telemetry()
        .expect("charged telemetry missing");
    let usage = telemetry.usage.as_ref().expect("charged usage missing");
    assert_eq!(usage.fresh_input_tokens, 20);
    assert_eq!(usage.cache_read_tokens, 7);
    assert_eq!(usage.cache_write_tokens, 3);
    assert_eq!(usage.input_tokens, 30);
    assert_eq!(usage.output_tokens, 5);
    assert!(
        !serde_json::to_string(telemetry)
            .unwrap()
            .contains("PRIVATE_")
    );
    let snap = probe(&reg);
    assert_eq!(snap["closed"], json!(1));
}

#[test]
fn plain_path_returns_text_and_sanitized_usage_without_local_tools() {
    let (reg, _host) = load_task_host();
    let output = exec_tool_output(&reg, TASK_TOOL, task_input(SCENARIO_PLAIN, None)).unwrap();
    assert_eq!(output.as_text(), PLAIN_TEXT);
    assert_eq!(
        serde_json::to_value(
            output
                .telemetry()
                .and_then(|telemetry| telemetry.usage.as_ref())
        )
        .unwrap(),
        json!({
            "fresh_input_tokens": 50,
            "cache_read_tokens": 30,
            "cache_write_tokens": 20,
            "input_tokens": 100,
            "output_tokens": 20,
        })
    );
    assert_eq!(output.state(), None);
    assert!(
        !serde_json::to_string(output.telemetry().expect("telemetry missing"))
            .unwrap()
            .contains("PRIVATE_")
    );

    let snap = probe(&reg);
    assert_eq!(snap["has_local_tools"], json!(false));
    assert_eq!(snap["structured_output_schema"], Value::Null);
    assert_eq!(snap["usage_fast"], json!(true));
    assert_eq!(snap["prompt_count"], json!(1));
    let prompt = snap["prompts"][0].as_str().expect("prompt missing");
    assert_eq!(prompt, TASK_PROMPT, "got: {prompt}");
    assert_eq!(snap["closed"], json!(1));
}

/// Spy counters catch a leaked permit even when gc would silently reclaim it.
#[test]
fn raising_prompt_does_not_leak_semaphore_permit() {
    let (reg, _host) = load_task_host();
    let err = exec_tool(&reg, TASK_TOOL, task_input(SCENARIO_RAISE, None)).unwrap_err();
    assert!(err.contains(RAISE_MSG), "got: {err}");

    let snap = probe(&reg);
    assert_eq!(
        snap["sem_size"],
        json!(TASK_DEFAULT_MAX_CONCURRENT),
        "semaphore not sized from the default max_concurrent option"
    );
    assert_eq!(snap["acquired"], json!(1));
    assert_eq!(snap["released"], json!(1), "permit not explicitly released");

    // Pool is full again (released == acquired), so this cannot block.
    let out = exec_tool(&reg, TASK_TOOL, task_input(SCENARIO_PLAIN, None)).unwrap();
    assert_eq!(out, PLAIN_TEXT);
}

#[test]
fn slow_nested_tool_finishes() {
    let (reg, _host) = load_task_host();
    let out = exec_tool(&reg, TASK_TOOL, task_input(SCENARIO_SLOW, None))
        .expect("slow async callback should finish");
    assert_eq!(out, PLAIN_TEXT);

    let snap = probe(&reg);
    assert_eq!(snap["prompt_count"], json!(1));
    assert_eq!(snap["closed"], json!(1));
}

#[test]
fn three_concurrent_tasks_return_results_without_nil_lifecycle_errors() {
    let (reg, _host) = load_task_host();
    let calls: Vec<_> = (0..3)
        .map(|_| {
            let reg = Arc::clone(&reg);
            std::thread::spawn(move || exec_tool(&reg, TASK_TOOL, task_input(SCENARIO_PLAIN, None)))
        })
        .collect();

    for call in calls {
        let out = call
            .join()
            .expect("task thread panicked")
            .expect("concurrent task failed");
        assert_eq!(out, PLAIN_TEXT);
    }

    let snap = probe(&reg);
    assert_eq!(snap["sessions"], json!(3));
    assert_eq!(snap["closed"], json!(3));
}
