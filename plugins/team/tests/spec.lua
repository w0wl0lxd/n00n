local roles = require("roles")
local route_tier = require("n00n.route_tier")

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

case("roles_catalogue_has_five", function()
  local n = 0
  for _ in pairs(roles.ROLES) do
    n = n + 1
  end
  assert(n == 5, "expected 5 roles, got " .. n)
  assert(roles.ROLES.developer.tier == "strong", "developer must be strong")
  assert(roles.ROLES.product_manager.tier == "weak", "product_manager must be weak")
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
  n00n.agent.resolve_model = function(ctx, opts)
    resolved_tier = opts.tier
    return { spec = "mock-model" }, nil
  end
  n00n.agent.tools = function()
    return {}, nil
  end

  local session_name = nil
  n00n.agent.session = function(ctx, opts)
    session_name = opts.name
    return {
      prompt = function()
        return { text = "implemented!", input_tokens = 10, output_tokens = 20 }
      end,
      close = function() end,
    },
      nil
  end
  n00n.agent.usage_cost = function()
    return 0.05, nil
  end

  local dummy_ctx = {}
  local res = roles.run(dummy_ctx, "developer", "implement helper", { model_tier = "medium" })

  n00n.agent.resolve_model = old_resolve
  n00n.agent.tools = old_tools
  n00n.agent.session = old_session
  n00n.agent.usage_cost = old_cost

  assert(res.ok == true, "roles.run should succeed")
  assert(resolved_tier == "medium", "should use custom model tier")
  assert(session_name == "developer", "session name should match role")
  assert(res.text == "implemented!", "returned text should match mock")
  assert(res.cost == 0.05, "cost should match mock")
end)

case("roles_run_with_exact_model_and_thinking", function()
  local old_resolve = n00n.agent.resolve_model
  local old_tools = n00n.agent.tools
  local old_session = n00n.agent.session
  local old_cost = n00n.agent.usage_cost
  local resolved, session_opts
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
        return { text = "done", input_tokens = 0, output_tokens = 0 }
      end,
      close = function() end,
    },
      nil
  end
  n00n.agent.usage_cost = function()
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

  n00n.agent.resolve_model = function()
    return { spec = "mock-model" }, nil
  end
  n00n.agent.tools = function()
    return {}, nil
  end
  n00n.agent.session = function()
    return {
      prompt = function()
        return { text = approved and "APPROVED" or "issues: bad", input_tokens = 10, output_tokens = 20 }
      end,
      close = function() end,
    },
      nil
  end
  n00n.agent.usage_cost = function()
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
  end
end

case("quorum_diverse_approvers_accepted", function()
  local quorum = require("quorum")
  local restore = stub_agent(true)
  local ok, v = pcall(function()
    return quorum.validate({}, "artifact", { n = 3 })
  end)
  restore()
  assert(ok, "quorum.validate should not error: " .. tostring(v))
  assert(v.accepted == true, "3 distinct-tier approvers must be accepted")
  assert(v.diverse == true, "weak/medium/strong tiers are diverse")
  assert(v.confidence == 1.0, "all approved -> confidence 1.0")
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

case("swarm_dry_run_terminates_within_max_rounds", function()
  local swarm = require("swarm")

  local old_env = n00n.env.state_dir
  local old_uv = n00n.uv and n00n.uv.cwd
  local old_async_gather = n00n.async.gather
  local old_async_sem = n00n.async.semaphore
  local restore_agent = stub_agent(true)

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
      out[i] = { ok = true, value = task() }
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

  local dummy_ctx = {}
  local ok, out = pcall(function()
    return swarm.run(dummy_ctx, "add a retry helper and write tests", { relay_k = 2, max_rounds = 4 })
  end)

  n00n.env.state_dir = old_env
  if n00n.uv then
    n00n.uv.cwd = old_uv
  end
  n00n.async.gather = old_async_gather
  n00n.async.semaphore = old_async_sem
  restore_agent()

  assert(ok, "swarm.run should not error: " .. tostring(out))
  assert(out.ok == true, "swarm must report ok")
  assert(out.text ~= nil and out.text ~= "", "swarm must emit a report")
  -- One accepted round = 2 explorer/worker + 3 quorum = 5 sessions * 0.01.
  -- Bounded by max_rounds means cost stays at one round, not 4.
  assert(out.cost <= 0.06, "swarm must terminate within max_rounds (cost " .. tostring(out.cost) .. ")")
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
