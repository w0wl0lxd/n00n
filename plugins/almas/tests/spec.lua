local refine = require("refine")
local roles = require("roles")
local route_tier = require("noon.route_tier")

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
  local out = refine.refine_goal("add a retry helper")
  assert(out:find("Acceptance"), "refine must add an acceptance criterion")
end)

case("refine_keeps_existing_criterion", function()
  local out = refine.refine_goal("add a retry helper and verify tests pass")
  assert(not out:find("Acceptance:"), "refine must not double-add a criterion")
end)

case("refine_empty_goal", function()
  assert(refine.refine_goal("") == "", "empty goal stays empty")
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

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
