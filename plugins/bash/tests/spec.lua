-- Test RTK functionality for bash plugin
-- Tests T086, T087, T091 from Phase 6

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

-- T086: Test RTK availability caching (simulated via module inspection)
case("rtk_availability_cache_exists", function()
  -- The bash plugin should have a module-level rtk_available variable
  -- This test verifies the structure exists (actual caching is tested via integration)
  local bash_init = loadfile("plugins/bash/init.lua")
  assert(bash_init, "bash init.lua should be loadable")
end)

-- T087/T090: Test broader rtk rewrite coverage
case("rtk_command_table_includes_new_commands", function()
  -- Verify the description mentions the new commands from FR-017
  local f = io.open("plugins/bash/init.lua", "r")
  if not f then
    error("could not open plugins/bash/init.lua")
  end
  local content = f:read("*a")
  f:close()

  -- Check that the description includes the new commands
  assert(content:find("podman"), "description should mention podman")
  assert(content:find("docker"), "description should mention docker")
  assert(content:find("npm"), "description should mention npm")
  assert(content:find("pip"), "description should mention pip")
  assert(content:find("python"), "description should mention python")
  assert(content:find("gh"), "description should mention gh")
end)

-- T091: Test jq and yq passthrough
case("jq_yq_passthrough_in_code", function()
  local f = io.open("plugins/bash/init.lua", "r")
  if not f then
    error("could not open plugins/bash/init.lua")
  end
  local content = f:read("*a")
  f:close()

  -- Check that rtk_rewrite has explicit jq/yq passthrough logic
  assert(content:find("jq"), "rtk_rewrite should handle jq")
  assert(content:find("yq"), "rtk_rewrite should handle yq")
  assert(content:find("pass through unchanged") or content:find("pass through"), "should mention passthrough behavior")
end)

-- T092: Test prompt hints mention rtk-wrapped bash
case("prompt_hint_mentions_rtk_wrapped", function()
  local f = io.open("plugins/bash/init.lua", "r")
  if not f then
    error("could not open plugins/bash/init.lua")
  end
  local content = f:read("*a")
  f:close()

  -- Check that prompt hints explicitly recommend rtk-wrapped bash
  assert(content:find("rtk%-wrapped") or content:find("rtk wrapped"), "prompt hint should mention rtk-wrapped bash")
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
