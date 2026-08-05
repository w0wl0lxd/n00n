-- Structured-output story: the subagent gets a session-local structured_output
-- tool whose handler validates and captures the result as closure upvalues.
-- Invalid input is an inline tool error the model can fix in the same run.
-- This plugin owns structured output and subagent concurrency; Rust exposes
-- primitives only (`n00n.agent.session`, `n00n.json.schema_validator`,
-- `n00n.async.semaphore`).

local ActivityPreview = require("n00n.activity_preview")
local ToolView = require("n00n.tool_view")
local output_limits = require("n00n.output_limits")
local route_tier = require("n00n.route_tier").route_tier
local structured_output = require("n00n.structured_output")
local subagent = require("n00n.subagent")

local DONE_NAME = "done"
local DONE_DESCRIPTION = "Call when the task is complete with your final answer."
local DONE_PROMPT_SUFFIX = "\n\nWhen finished, call the done tool with your final answer."
local BODY_INDENT_COLS = 4
local MIN_MD_WIDTH = 20
local DEFAULT_OUTPUT_LINES = 5
local DEFAULT_MAX_LINE_BYTES = 500

local description =
  [[Launch isolated agent; combine independent calls with batch. research (default) = read-only; general = can edit. Each call starts fresh; include context and ask for concise file:line results. Summarize returned results. auto_tier opt-in. background returns agent_id.]]

local schema = {
  type = "object",
  required = { "description", "prompt" },
  additionalProperties = false,
  properties = {
    description = {
      type = "string",
      description = "Task summary (3-5 words).",
    },
    prompt = {
      type = "string",
      description = "Task prompt.",
    },
    subagent_type = {
      type = "string",
      description = "research (default) or general.",
    },
    model = {
      type = "string",
      description = "Exact model override.",
    },
    model_tier = {
      type = "string",
      description = "Tier: weak/medium/strong.",
    },
    thinking = {
      type = { "string", "integer" },
      description = "Thinking mode. Omit to inherit.",
    },
    auto_tier = {
      type = "boolean",
      description = "Auto-route tier from prompt.",
    },
    background = {
      type = "boolean",
      description = "Start in background; return agent_id immediately.",
    },
    output_schema = {
      description = "Output JSON schema. Result returned as validated JSON string.",
    },
  },
}

local opts = n00n.api.register_options({
  max_concurrent = { default = 4, min = 1, desc = "Concurrent subagents (hard max 8)." },
  auto_tier = { default = false, desc = "Route each subagent's model tier from its prompt (opt-in, off by default)." },
})

-- Process-wide cap on concurrent subagents.
local semaphore = n00n.async.semaphore(math.min(opts.max_concurrent, 8))

local function handler(input, ctx)
  if input.background then
    local forwarded = {}
    for key, value in pairs(input) do
      forwarded[key] = value
    end
    forwarded.background = false
    local forwarded_json, encode_err = n00n.json.encode(forwarded)
    if encode_err then
      return { llm_output = "failed to encode task input: " .. tostring(encode_err), is_error = true }
    end
    local prompt = "Use the task tool now with background=false. Do not only describe this request.\n\n"
      .. forwarded_json
    local title = (input.description or input.prompt or "background task"):sub(1, 80)
    local id, err = n00n.session.new({ prompt = prompt, title = title, focus = false })
    if not id then
      return { llm_output = err, is_error = true }
    end
    local output, output_err = n00n.json.encode({ agent_id = id, status = "started" })
    if output_err then
      return { llm_output = "failed to encode task status: " .. tostring(output_err), is_error = true }
    end
    return { llm_output = output }
  end

  local subagent_type = input.subagent_type or "research"
  if subagent_type ~= "research" and subagent_type ~= "general" then
    return { llm_output = "unknown subagent type: " .. subagent_type, is_error = true }
  end

  -- Compile early: a bad schema costs zero tokens.
  local validator
  if input.output_schema then
    local compile_err
    validator, compile_err = structured_output.compile_validator(input.output_schema)
    if compile_err then
      return { llm_output = compile_err, is_error = true }
    end
  end

  local model_tier = input.model_tier
  if not input.model and (input.auto_tier == true or (input.auto_tier == nil and opts.auto_tier)) then
    model_tier = route_tier(input.prompt)
  end

  local preview, preview_err = ActivityPreview.new(ctx, input.description or "task", {})
  if not preview then
    return { llm_output = "failed to create task preview: " .. tostring(preview_err), is_error = true }
  end

  -- Build local tools: either structured_output (with schema) or done tool
  local local_tools
  if not input.output_schema then
    local_tools = {
      [DONE_NAME] = {
        description = DONE_DESCRIPTION,
        input_schema = {
          type = "object",
          properties = {
            answer = { type = "string", description = "Final answer to return to the parent agent." },
          },
          required = { "answer" },
        },
        handler = function(value)
          return "Done."
        end,
      },
    }
  end

  local function on_finish(err, result)
    if err then
      ctx:finish({ llm_output = "task failed: " .. tostring(err), is_error = true, body = preview.view.buf })
    else
      ctx:finish({
        llm_output = result.llm_output,
        body = preview.view.buf,
        is_error = result.is_error,
        format = result.format,
        usage = result.usage,
        cost = result.cost,
      })
    end
  end

  n00n.async.run(function()
    local permit = semaphore:acquire()
    local ok, out = pcall(function()
      if input.output_schema then
        -- Use subagent.launch for structured output
        local captured, err = subagent.launch(ctx, {
          description = input.description or "task",
          prompt = input.prompt,
          subagent_type = subagent_type,
          model_spec = input.model,
          model_tier = model_tier,
          auto_tier = input.auto_tier,
          thinking = input.thinking,
          output_schema = input.output_schema,
          preview = preview,
          activity_label = input.description or "task",
        })
        if err then
          return { llm_output = err, is_error = true }
        end
        if type(captured) == "string" then
          return { llm_output = captured, format = "markdown" }
        end
        local encoded, encode_err = n00n.json.encode(captured)
        if encode_err then
          return { llm_output = "failed to encode structured output: " .. tostring(encode_err), is_error = true }
        end
        return {
          llm_output = encoded,
          format = "markdown",
        }
      else
        -- Manual session for done tool (legacy path)
        local model, model_err = n00n.agent.resolve_model(ctx, {
          spec = input.model,
          tier = not input.model and model_tier or nil,
        })
        if model_err then
          return { llm_output = model_err, is_error = true }
        end

        local audience = subagent_type == "research" and "research_sub" or "general_sub"
        local prompt_id = subagent_type == "research" and "research" or "general"
        local system, system_err = n00n.agent.system_prompt(ctx, {
          prompt_id = prompt_id,
          instructions = true,
        })
        if system_err then
          return { llm_output = system_err, is_error = true }
        end

        local tool_defs, tools_err = n00n.agent.tools(ctx, {
          audience = audience,
          spec = model.spec,
        })
        if tools_err then
          return { llm_output = tools_err, is_error = true }
        end

        local captured
        local done_tool = {
          [DONE_NAME] = {
            description = DONE_DESCRIPTION,
            input_schema = {
              type = "object",
              properties = {
                answer = { type = "string", description = "Final answer to return to the parent agent." },
              },
              required = { "answer" },
            },
            handler = function(value)
              captured = value.answer
              return "Done."
            end,
          },
        }

        local sess, sess_err = n00n.agent.session(ctx, {
          model_spec = model.spec,
          system = system,
          tools = tool_defs,
          local_tools = done_tool,
          audience = audience,
          name = input.description,
          thinking = input.thinking,
          mode = subagent_type,
        })
        if sess_err then
          return { llm_output = sess_err, is_error = true }
        end

        local function attach_cost(r)
          if r and not r.cost and r.input_tokens and r.output_tokens then
            local cost, _ = n00n.agent.usage_cost(model.spec, r.input_tokens, r.output_tokens, r)
            r.cost = cost
          end
        end

        local function do_prompt()
          local message = input.prompt .. DONE_PROMPT_SUFFIX
          local result, err = sess:prompt(message)
          attach_cost(result)
          if err then
            return {
              llm_output = "sub-agent error: " .. err,
              is_error = true,
              usage = result,
              cost = result and result.cost,
            }
          end
          if captured then
            return { llm_output = captured, format = "markdown", usage = result, cost = result and result.cost }
          end
          return { llm_output = result.text, format = "markdown", usage = result, cost = result and result.cost }
        end

        local function do_poll()
          while true do
            local progress = sess:get_progress()
            if not progress then
              return
            end
            preview:update(progress)
            if progress.done then
              return
            end
          end
        end

        local results = n00n.async.gather({ do_prompt, do_poll })
        sess:close()
        local prompt_res = results[1]
        if not prompt_res.ok then
          error(prompt_res.err, 0)
        end
        return prompt_res.value
      end
    end)
    permit:release()
    if not ok then
      error(out, 0)
    end
    return out
  end, on_finish)

  return nil
end

local function header(input)
  return input.description
end

-- Standalone runs render markdown on the Rust side (format = "markdown");
-- this mirrors that for restore and batch children, which build the body here.
local function restore(_input, output, is_error, ctx)
  local tol = ctx:tool_output_lines()
  local opts = {
    max_lines = (tol and tol.task) or DEFAULT_OUTPUT_LINES,
    keep = "head",
    max_line_bytes = output_limits.DEFAULT_MAX_LINE_BYTES,
  }
  if not is_error then
    local width = math.max(n00n.ui.terminal_size().cols - BODY_INDENT_COLS, MIN_MD_WIDTH)
    local ok, md_lines = pcall(n00n.ui.markdown, output, width)
    if ok then
      return ToolView.restore_lines(md_lines, opts)
    end
  end
  return ToolView.restore(output, opts)
end

n00n.api.register_tool({
  name = "task",
  description = description,
  kind = "execute",
  workload = "orchestrator",
  audiences = { "main", "workflow" },
  schema = schema,
  handler = handler,
  header = header,
  restore = restore,
})
