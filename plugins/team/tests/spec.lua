local roles = require("roles")
local route_tier = require("n00n.route_tier")
local usage_metrics = require("n00n.usage")

local failures = {}
local function case(name, fn)
  local ok, err = pcall(fn)
  if not ok then
    table.insert(failures, name .. ": " .. tostring(err))
  end
end
local function assert(c, m)
  if not c then
    error(m or "assertion failed")
  end
end

local USAGE_FIELDS = {
  fresh_input_tokens = true,
  cache_read_tokens = true,
  cache_write_tokens = true,
  input_tokens = true,
  output_tokens = true,
}

local function assert_usage(usage, expected, label)
  label = label or "usage"
  assert(type(usage) == "table", label .. " must be a table")
  for key, value in pairs(usage) do
    assert(USAGE_FIELDS[key], label .. " exposed unexpected field " .. tostring(key))
    assert(type(value) == "number", label .. "." .. key .. " must be numeric")
    assert(value >= 0, label .. "." .. key .. " must be non-negative")
  end
  for key, value in pairs(expected) do
    assert(usage[key] == value, label .. "." .. key .. " expected " .. value .. ", got " .. tostring(usage[key]))
  end
  assert(
    usage.input_tokens == usage.fresh_input_tokens + usage.cache_read_tokens + usage.cache_write_tokens,
    label .. " must conserve legacy input_tokens"
  )
end

case("usage_rejects_malformed_and_nonconserving_counts", function()
  local malformed, malformed_err = usage_metrics.normalize({ input_tokens = "100" })
  assert(malformed == nil, "malformed usage must not become zero")
  assert(malformed_err:find("input_tokens"), "malformed usage error must name the field")

  local nonconserving, conservation_err = usage_metrics.normalize({
    input_tokens = 10,
    fresh_input_tokens = 5,
    cache_read_tokens = 4,
    cache_write_tokens = 2,
    output_tokens = 1,
  })
  assert(nonconserving == nil, "nonconserving usage must be rejected")
  assert(conservation_err:find("conserve"), "nonconserving usage error must explain conservation")

  local overflow, overflow_err = usage_metrics.normalize({
    cache_read_tokens = 9007199254740991,
    cache_write_tokens = 1,
  })
  assert(overflow == nil, "derived input totals outside the safe integer range must be rejected")
  assert(overflow_err:find("safe integer range"), "overflow error must explain the safe range")
end)

case("usage_price_preserves_pricing_errors_instead_of_zeroing", function()
  local old_cost = n00n.agent.usage_cost
  n00n.agent.usage_cost = function()
    return nil, "pricing unavailable"
  end

  local measured, cost, err = usage_metrics.price("mock-model", {
    input_tokens = 10,
    output_tokens = 2,
  })
  n00n.agent.usage_cost = old_cost

  assert_usage(measured, {
    fresh_input_tokens = 10,
    cache_read_tokens = 0,
    cache_write_tokens = 0,
    input_tokens = 10,
    output_tokens = 2,
  }, "pricing failure usage")
  assert(cost == nil, "pricing failures must not become zero cost")
  assert(err == "pricing unavailable", "pricing error must be preserved")
end)

case("usage_price_rejects_missing_cost_without_error", function()
  local old_cost = n00n.agent.usage_cost
  n00n.agent.usage_cost = function()
    return nil, nil
  end

  local measured, cost, err = usage_metrics.price("mock-model", {
    input_tokens = 10,
    output_tokens = 2,
  })
  n00n.agent.usage_cost = old_cost

  assert(measured ~= nil, "usage must remain available when pricing violates its contract")
  assert(cost == nil, "missing cost must not become zero")
  assert(err == "usage pricing returned no cost", "missing cost must become an explicit error")
end)

case("usage_rejects_nonfinite_negative_and_fractional_counts", function()
  local invalid = { -1, 1.5, math.huge, 0 / 0, 9007199254740992 }
  for _, value in ipairs(invalid) do
    local measured, err = usage_metrics.normalize({ input_tokens = value })
    assert(measured == nil, "invalid token count must be rejected")
    assert(err:find("finite non%-negative integer"), "strict count error must explain the contract")
  end
end)

case("usage_price_prefers_session_aggregate_cost", function()
  local old_cost = n00n.agent.usage_cost
  n00n.agent.usage_cost = function()
    error("per-model aggregate cost must not be repriced")
  end

  local measured, cost, err = usage_metrics.price("main-model", {
    input_tokens = 10,
    output_tokens = 2,
    cost = 0.75,
    fast = true,
  })
  n00n.agent.usage_cost = old_cost

  assert(err == nil, "valid session aggregate cost must be accepted")
  assert(cost == 0.75, "session aggregate cost must be preserved exactly")
  assert_usage(measured, {
    fresh_input_tokens = 10,
    cache_read_tokens = 0,
    cache_write_tokens = 0,
    input_tokens = 10,
    output_tokens = 2,
  }, "aggregate-cost usage")
end)

case("roles_catalogue_has_six", function()
  local n = 0
  for _ in pairs(roles.ROLES) do
    n = n + 1
  end
  assert(n == 6, "expected 6 roles, got " .. n)
  assert(roles.ROLES.developer.tier == "strong", "developer must be strong")
  assert(roles.ROLES.product_manager.tier == "weak", "product_manager must be weak")
  assert(roles.ROLES.sprint.tier == "weak", "sprint must be weak")
end)

case("route_tier_available", function()
  assert(type(route_tier.route_tier) == "function")
end)

case("retrieve_lexical_finds_context", function()
  local retrieve = require("retrieve")

  -- Mock n00n.agent.call_tool
  local old_call = n00n.agent.call_tool
  local grep_pattern = nil
  n00n.agent.call_tool = function(ctx, name, args)
    if name == "grep" then
      grep_pattern = args.pattern
      return "class RetryHelper {\n  // some implementation\n}"
    end
    return nil
  end

  local dummy_ctx = {}
  local block = retrieve.retrieve(dummy_ctx, "add retry helper", "developer", 2)

  -- Restore original
  n00n.agent.call_tool = old_call

  assert(block ~= nil, "retrieved block should not be nil")
  assert(block:find("RetryHelper") ~= nil, "retrieved block should contain mock results")
  assert(grep_pattern == "retry" or grep_pattern == "helper", "should grep for goal keywords")
end)

case("retrieve_vector_fallback", function()
  local retrieve = require("retrieve")

  -- Mock n00n.agent.call_tool to return empty for first call, then a match
  local old_call = n00n.agent.call_tool
  local calls = 0
  n00n.agent.call_tool = function(ctx, name, args)
    if name == "grep" then
      calls = calls + 1
      if calls == 1 then
        return ""
      else
        return "some match with vector similarity"
      end
    end
    return nil
  end

  local dummy_ctx = {}
  local block = retrieve.retrieve(dummy_ctx, "add retry helper", "developer", 2)

  -- Restore original
  n00n.agent.call_tool = old_call

  assert(block ~= nil, "retrieved block should fallback to vector and not be nil")
  assert(block:find("vector similarity") ~= nil, "retrieved block should contain mock results")
end)

case("roles_run_with_custom_opts", function()
  -- Mock n00n.agent.resolve_model, n00n.agent.tools, and n00n.agent.session
  local old_resolve = n00n.agent.resolve_model
  local old_tools = n00n.agent.tools
  local old_session = n00n.agent.session
  local old_cost = n00n.agent.usage_cost

  local resolved_tier = nil
  local cost_args = nil
  n00n.agent.resolve_model = function(ctx, opts)
    resolved_tier = opts.tier
    return { spec = "mock-model" }, nil
  end
  n00n.agent.tools = function()
    return {}, nil
  end

  local session_name, activity_label
  n00n.agent.session = function(ctx, opts)
    session_name = opts.name
    return {
      prompt = function()
        return {
          text = "implemented!",
          input_tokens = 100,
          output_tokens = 20,
          fresh_input_tokens = 50,
          cache_read_tokens = 30,
          cache_write_tokens = 20,
          fast = true,
        }
      end,
      close = function() end,
    },
      nil
  end
  n00n.agent.usage_cost = function(spec, input_tokens, output_tokens, breakdown)
    cost_args = {
      spec = spec,
      input_tokens = input_tokens,
      output_tokens = output_tokens,
      breakdown = breakdown,
    }
    return 0.05, nil
  end

  local preview = {
    prompt = function(_, sess, prompt, label)
      activity_label = label
      return sess:prompt(prompt)
    end,
  }
  local dummy_ctx = {}
  local res = roles.run(dummy_ctx, "developer", "implement helper", { model_tier = "medium", preview = preview })

  n00n.agent.resolve_model = old_resolve
  n00n.agent.tools = old_tools
  n00n.agent.session = old_session
  n00n.agent.usage_cost = old_cost

  assert(res.ok == true, "roles.run should succeed")
  assert(resolved_tier == "medium", "should use custom model tier")
  assert(session_name == "developer", "session name should match role")
  assert(activity_label == "developer", "role session must publish through the shared preview")
  assert(res.text == "implemented!", "returned text should match mock")
  assert(res.cost == 0.05, "cost should match mock")
  assert(cost_args.input_tokens == 100, "legacy aggregate input must reach usage_cost")
  assert(cost_args.output_tokens == 20, "output must reach usage_cost")
  assert(type(cost_args.breakdown) == "table", "cache breakdown must reach usage_cost")
  assert(cost_args.breakdown.fresh_input_tokens == 50, "fresh-input tokens must be forwarded")
  assert(cost_args.breakdown.cache_read_tokens == 30, "cache-read tokens must be forwarded")
  assert(cost_args.breakdown.cache_write_tokens == 20, "cache-write tokens must be forwarded")
  assert(cost_args.breakdown.fast == true, "actual fast session state must be forwarded")
  assert_usage(res.usage, {
    fresh_input_tokens = 50,
    cache_read_tokens = 30,
    cache_write_tokens = 20,
    input_tokens = 100,
    output_tokens = 20,
  }, "role usage")
end)

case("roles_run_with_exact_model_and_thinking", function()
  local old_resolve = n00n.agent.resolve_model
  local old_tools = n00n.agent.tools
  local old_session = n00n.agent.session
  local old_cost = n00n.agent.usage_cost
  local resolved, session_opts, legacy_breakdown
  n00n.agent.resolve_model = function(_, opts)
    resolved = opts
    return { spec = opts.spec }, nil
  end
  n00n.agent.tools = function()
    return {}, nil
  end
  n00n.agent.session = function(_, opts)
    session_opts = opts
    return {
      prompt = function()
        return { text = "done", input_tokens = 12, output_tokens = 3 }
      end,
      close = function() end,
    },
      nil
  end
  n00n.agent.usage_cost = function(_, input_tokens, output_tokens, breakdown)
    assert(input_tokens == 12, "legacy input_tokens must remain accepted")
    assert(output_tokens == 3, "legacy output_tokens must remain accepted")
    legacy_breakdown = breakdown
    return 0, nil
  end

  local out = roles.run({}, "developer", "fix it", {
    model = "openai/gpt-5.6-luna",
    model_tier = "weak",
    thinking = "max",
  })

  n00n.agent.resolve_model = old_resolve
  n00n.agent.tools = old_tools
  n00n.agent.session = old_session
  n00n.agent.usage_cost = old_cost
  assert(out.ok, "exact model role should succeed")
  assert(resolved.spec == "openai/gpt-5.6-luna", "exact model must reach resolver")
  assert(resolved.tier == nil, "exact model must override tier routing")
  assert(session_opts.thinking == "max", "thinking tier must reach session")
  assert(type(legacy_breakdown) == "table", "legacy session results must receive a zero cache breakdown")
  assert(legacy_breakdown.fresh_input_tokens == 12, "legacy input must map to fresh-input tokens")
  assert(legacy_breakdown.cache_read_tokens == 0, "missing cache-read tokens must default to zero")
  assert(legacy_breakdown.cache_write_tokens == 0, "missing cache-write tokens must default to zero")
  assert(legacy_breakdown.fast == false, "missing fast state must default to false")
  assert_usage(out.usage, {
    fresh_input_tokens = 12,
    cache_read_tokens = 0,
    cache_write_tokens = 0,
    input_tokens = 12,
    output_tokens = 3,
  }, "legacy role usage")
end)

case("ibn_exact_model_uses_resolved_tier", function()
  local ibn = require("ibn")
  local old_resolve = n00n.agent.resolve_model
  local resolved_opts
  n00n.agent.resolve_model = function(_, opts)
    resolved_opts = opts
    return { tier = "weak" }, nil
  end
  local tier, err = ibn.resolve_tier({}, "openai/gpt-5.6-luna", "strong")
  n00n.agent.resolve_model = old_resolve
  assert(err == nil, "exact model tier resolution should succeed")
  assert(tier == "weak", "IBN must use the exact model's resolved tier")
  assert(resolved_opts.spec == "openai/gpt-5.6-luna", "IBN must resolve the exact model spec")
end)

case("ibn_weak_fans_out", function()
  local ibn = require("ibn")
  local d = ibn.decide({}, "refactor the parser", "weak")
  assert(d.fan_out == true, "weak model should fan out")
  assert(type(d.relay_k) == "number" and d.relay_k > 0, "relay_k must be positive")
end)

case("ibn_strong_single_step_does_not_fan_out", function()
  local ibn = require("ibn")
  local d = ibn.decide({}, "fix the typo", "strong")
  assert(d.fan_out == false, "strong model + single-step goal must not fan out")
end)

case("ibn_relay_k_monotonic_in_tier", function()
  local ibn = require("ibn")
  local weak = ibn.decide({}, "task", "weak").relay_k
  local med = ibn.decide({}, "task", "medium").relay_k
  local strong = ibn.decide({}, "task", "strong").relay_k
  assert(weak < med and med < strong, "relay_k must grow with tier")
end)

local function stub_agent(approved)
  local old_resolve = n00n.agent.resolve_model
  local old_tools = n00n.agent.tools
  local old_session = n00n.agent.session
  local old_cost = n00n.agent.usage_cost
  local old_call = n00n.agent.call_tool
  local stats = { sessions = 0, cost_calls = 0 }

  n00n.agent.resolve_model = function()
    return { spec = "mock-model" }, nil
  end
  n00n.agent.tools = function()
    return {}, nil
  end
  n00n.agent.session = function(_, opts)
    stats.sessions = stats.sessions + 1
    local session_approved = approved
    if type(approved) == "function" then
      session_approved = approved(opts, stats.sessions)
    end
    return {
      prompt = function()
        return {
          text = session_approved and "APPROVED" or "issues: bad",
          input_tokens = 10,
          output_tokens = 20,
          fresh_input_tokens = 5,
          cache_read_tokens = 3,
          cache_write_tokens = 2,
        }
      end,
      close = function() end,
    },
      nil
  end
  n00n.agent.usage_cost = function()
    stats.cost_calls = stats.cost_calls + 1
    return 0.01, nil
  end
  n00n.agent.call_tool = function()
    return ""
  end

  return function()
    n00n.agent.resolve_model = old_resolve
    n00n.agent.tools = old_tools
    n00n.agent.session = old_session
    n00n.agent.usage_cost = old_cost
    n00n.agent.call_tool = old_call
  end,
    stats
end

case("quorum_sums_every_validator_call", function()
  local quorum = require("quorum")
  local restore, stats = stub_agent(true)
  local budget_calls = 0
  local budget = {
    consume = function()
      budget_calls = budget_calls + 1
      return true
    end,
  }
  local ok, v = pcall(function()
    return quorum.validate({}, "artifact", { n = 3, budget = budget })
  end)
  restore()
  assert(ok, "quorum.validate should not error: " .. tostring(v))
  assert(v.accepted == true, "3 distinct-tier approvers must be accepted")
  assert(v.diverse == true, "weak/medium/strong tiers are diverse")
  assert(v.confidence == 1.0, "all approved -> confidence 1.0")
  assert(budget_calls == 3, "each validator must consume one agent-call budget slot")
  assert(stats.sessions == 3, "quorum must open exactly one session per validator")
  assert(stats.cost_calls == 3, "quorum must price exactly one result per validator")
  assert(v.cost == 0.03, "quorum cost must sum all three validators")
  assert_usage(v.usage, {
    fresh_input_tokens = 15,
    cache_read_tokens = 9,
    cache_write_tokens = 6,
    input_tokens = 30,
    output_tokens = 60,
  }, "quorum usage")
end)

case("quorum_validators_receive_no_tools", function()
  local quorum = require("quorum")
  local old_resolve = n00n.agent.resolve_model
  local old_tools = n00n.agent.tools
  local old_session = n00n.agent.session
  n00n.agent.resolve_model = function()
    return { spec = "mock-model" }, nil
  end
  n00n.agent.tools = function()
    error("validator must not build tool definitions")
  end
  n00n.agent.session = function(_, opts)
    assert(next(opts.tools) == nil, "validator tools must be empty")
    return {
      prompt = function()
        return { text = "APPROVED", cost = 0 }
      end,
      close = function() end,
    },
      nil
  end

  local ok, result = pcall(function()
    return quorum.validate({}, "artifact", { n = 1 })
  end)
  n00n.agent.resolve_model = old_resolve
  n00n.agent.tools = old_tools
  n00n.agent.session = old_session

  assert(ok, "tool-less quorum should succeed: " .. tostring(result))
  assert(result.accepted, "tool-less validator should approve")
end)

case("quorum_routes_each_validator_through_preview", function()
  local quorum = require("quorum")
  local restore = stub_agent(true)
  local labels = {}
  local preview = {
    prompt = function(_, sess, prompt, label)
      labels[#labels + 1] = label
      return sess:prompt(prompt)
    end,
  }
  local verdict = quorum.validate({}, "artifact", { n = 3, preview = preview })
  restore()
  assert(verdict.accepted, "preview must not change quorum result")
  assert(#labels == 3, "every quorum session must reach the preview")
  assert(labels[1] == "quorum-security", "validator label must identify quorum role")
end)

case("quorum_all_reject_rejected", function()
  local quorum = require("quorum")
  local restore = stub_agent(false)
  local ok, v = pcall(function()
    return quorum.validate({}, "artifact", { n = 3 })
  end)
  restore()
  assert(ok, "quorum.validate should not error: " .. tostring(v))
  assert(v.accepted == false, "all reject -> not accepted")
end)

case("quorum_same_tier_downweights_confidence", function()
  local quorum = require("quorum")
  local restore = stub_agent(true)
  local ok, v = pcall(function()
    return quorum.validate({}, "artifact", { n = 3, tiers = { "weak" } })
  end)
  restore()
  assert(ok, "quorum.validate should not error: " .. tostring(v))
  assert(v.accepted == true, "all approve -> accepted even on one tier")
  assert(v.diverse == false, "single tier -> not diverse")
  assert(v.confidence < 1.0, "non-diverse approval confidence must be down-weighted")
end)

case("roles_stop_at_aggregate_team_budget", function()
  local budget = {
    consume = function()
      return nil, "team agent-call budget exhausted (16; hard maximum 24)"
    end,
  }

  local result = roles.run({}, "developer", "implement", { budget = budget })

  assert(result.ok == false, "budget exhaustion must reject the agent call")
  assert(result.error:match("budget exhausted"), "budget error must be clear")
end)

local function run_stubbed_swarm(approved, goal, opts)
  local swarm = require("swarm")
  local old_env = n00n.env.state_dir
  local old_uv = n00n.uv and n00n.uv.cwd
  local old_async_gather = n00n.async.gather
  local old_async_sem = n00n.async.semaphore
  local restore_agent, stats = stub_agent(approved)

  n00n.env.state_dir = function()
    return nil
  end
  if n00n.uv then
    n00n.uv.cwd = function()
      return "/tmp"
    end
  end
  n00n.async.gather = function(tasks)
    local out = {}
    for i, task in ipairs(tasks) do
      if opts._gather_error and i == 1 then
        out[i] = { ok = false, err = opts._gather_error }
      else
        out[i] = { ok = true, value = task() }
      end
    end
    return out
  end
  n00n.async.semaphore = function()
    return {
      acquire = function()
        return { release = function() end }
      end,
    }
  end

  local activity_labels = {}
  local preview = {
    prompt = function(_, sess, prompt, label)
      activity_labels[#activity_labels + 1] = label
      return sess:prompt(prompt)
    end,
  }
  opts.preview = preview
  local ok, out = pcall(function()
    return swarm.run({}, goal, opts)
  end)

  n00n.env.state_dir = old_env
  if n00n.uv then
    n00n.uv.cwd = old_uv
  end
  n00n.async.gather = old_async_gather
  n00n.async.semaphore = old_async_sem
  restore_agent()
  return ok, out, stats, activity_labels
end

case("swarm_dry_run_terminates_within_max_rounds", function()
  local ok, out, stats, activity_labels =
    run_stubbed_swarm(true, "add a retry helper and write tests", { relay_k = 2, max_rounds = 4 })

  assert(ok, "swarm.run should not error: " .. tostring(out))
  assert(out.ok == true, "swarm must report ok")
  assert(out.text ~= nil and out.text ~= "", "swarm must emit a report")
  assert(activity_labels[1] == "swarm-explorer", "swarm explorer label must identify its agent")
  assert(activity_labels[2] == "swarm-worker", "swarm worker label must identify its agent")
  assert(activity_labels[3] == "quorum-security", "swarm quorum must retain validator labels")
  -- One accepted round = 2 explorer/worker + 3 quorum = 5 sessions * 0.01.
  assert(out.rounds == 1 and out.rounds <= 4, "swarm must terminate within max_rounds")
  assert(stats.sessions == 5, "one accepted round must account for two roles and three validators")
  assert(stats.cost_calls == 5, "every accepted-round model call must be priced")
  assert(math.abs(out.cost - 0.05) < 1e-9, "swarm cost must include role and validator calls")
  assert_usage(out.usage, {
    fresh_input_tokens = 25,
    cache_read_tokens = 15,
    cache_write_tokens = 10,
    input_tokens = 50,
    output_tokens = 100,
  }, "accepted swarm usage")
end)

case("swarm_without_quorum_keeps_known_worker_cost", function()
  local ok, out, stats = run_stubbed_swarm(true, "implement directly", {
    relay_k = 2,
    max_rounds = 1,
    quorum = false,
  })

  assert(ok, "quorum-disabled swarm should not error: " .. tostring(out))
  assert(out.ok == true, "worker output should be accepted without quorum")
  assert(stats.sessions == 2, "quorum-disabled swarm must run only explorer and worker")
  assert(stats.cost_calls == 2, "both worker calls must be priced")
  assert(math.abs(out.cost - 0.02) < 1e-9, "disabled quorum must contribute known zero cost")
  assert_usage(out.usage, {
    fresh_input_tokens = 10,
    cache_read_tokens = 6,
    cache_write_tokens = 4,
    input_tokens = 20,
    output_tokens = 40,
  }, "quorum-disabled swarm usage")
end)

case("swarm_does_not_silently_zero_failed_task_cost", function()
  local ok, out, stats = run_stubbed_swarm(true, "continue after one task error", {
    relay_k = 2,
    max_rounds = 1,
    quorum = false,
    _gather_error = "injected task failure",
  })

  assert(ok, "partially failed swarm should return a result: " .. tostring(out))
  assert(out.ok == true, "remaining worker output should still be accepted")
  assert(stats.sessions == 1, "the failed gathered task must not run in this test")
  assert(out.cost == nil, "unknown failed-task cost must not become zero")
  assert_usage(out.usage, {
    fresh_input_tokens = 5,
    cache_read_tokens = 3,
    cache_write_tokens = 2,
    input_tokens = 10,
    output_tokens = 20,
  }, "partially failed swarm usage")
end)

case("swarm_sums_rejected_and_accepted_round_validator_usage", function()
  local validator_calls = 0
  local ok, out, stats = run_stubbed_swarm(function(opts)
    if opts.name and opts.name:match("^team%-quorum%-") then
      validator_calls = validator_calls + 1
      return validator_calls > 3
    end
    return true
  end, "revise the implementation", { relay_k = 0, max_rounds = 2 })

  assert(ok, "two-round swarm should not error: " .. tostring(out))
  assert(out.ok == true, "second-round approval must produce a report")
  assert(out.rounds == 2, "first rejection and second approval must consume two rounds")
  assert(validator_calls == 6, "both rejected and accepted rounds must run three validators")
  assert(stats.sessions == 10, "two rounds must account for four roles and six validators")
  assert(stats.cost_calls == 10, "every model call across both rounds must be priced")
  assert(math.abs(out.cost - 0.10) < 1e-9, "rejected-round validator cost must not be dropped")
  assert_usage(out.usage, {
    fresh_input_tokens = 50,
    cache_read_tokens = 30,
    cache_write_tokens = 20,
    input_tokens = 100,
    output_tokens = 200,
  }, "two-round swarm usage")
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
