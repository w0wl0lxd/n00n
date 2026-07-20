local refine = require("refine")
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

case("refine_adds_acceptance", function()
  local out = refine.refine_goal_lexical("add a retry helper")
  assert(out:find("Acceptance"), "refine must add an acceptance criterion")
end)

case("refine_keeps_existing_criterion", function()
  local out = refine.refine_goal_lexical("add a retry helper and verify tests pass")
  assert(not out:find("Acceptance:"), "refine must not double-add a criterion")
end)

case("refine_empty_goal", function()
  assert(refine.refine_goal_lexical("") == "", "empty goal stays empty")
end)

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

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
