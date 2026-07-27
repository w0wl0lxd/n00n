local helpers = require("agent_control_helpers")

local failures = {}

local function case(name, fn)
  local ok, err = pcall(fn)
  if not ok then
    table.insert(failures, name .. ": " .. tostring(err))
  end
end

local function eq(actual, expected, msg)
  if actual ~= expected then
    error((msg or "") .. "\nexpected: " .. tostring(expected) .. "\n  actual: " .. tostring(actual))
  end
end

case("validate_id_accepts_safe", function()
  eq(helpers.validate_id("agent-1"), true)
end)

case("validate_id_rejects_empty", function()
  local ok, err = helpers.validate_id("")
  eq(ok, nil)
  assert(err:find("required"), err)
end)

case("validate_id_rejects_traversal", function()
  local ok = helpers.validate_id("../etc")
  eq(ok, nil)
end)

case("agent_line_with_title", function()
  local line = helpers.agent_line({ id = "a1", status = "idle", title = "main" })
  eq(line, "a1 · idle · main")
end)

case("agent_line_without_title", function()
  local line = helpers.agent_line({ id = "a1", status = "working" })
  eq(line, "a1 · working")
end)

case("build_resume_prompt_encodes_team_args", function()
  local prompt, err = helpers.build_resume_prompt(
    { run_id = "run-1", mode = "swarm" },
    "continue carefully",
    function(value)
      return '{"goal":"resume","resume":"run-1","mode":"swarm","continue":"continue carefully"}'
    end
  )
  assert(prompt, err)
  assert(prompt:find("run%-1"), prompt)
  assert(prompt:find("swarm"), prompt)
  assert(prompt:find("continue carefully"), prompt)
end)

case("policy_scope_requires_single_key", function()
  local ok, err = helpers.policy_scope_keys({
    scope = { tag = "bg", session_type = "team" },
  })
  eq(ok, nil)
  assert(err:find("exactly one"), err)
end)

case("policy_scope_rejects_unknown_key", function()
  local ok, err = helpers.policy_scope_keys({ scope = { foo = "bar" } })
  eq(ok, nil)
  assert(err:find("unknown key"), err)
end)

if #failures > 0 then
  error(table.concat(failures, "\n"))
end
