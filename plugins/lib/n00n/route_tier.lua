-- Cost-aware model-tier router (OrchMAS-style adaptive role allocation).
-- Pure lexical heuristic: no model call. Maps a subtask prompt to one of
-- "weak" | "medium" | "strong" so cheap work stays cheap and hard work
-- gets a bigger model. Used by the `task` tool (opt-in auto_tier) and Team.
local M = {}

-- Signals that the work needs deep reasoning -> strong/medium.
local STRONG = {
  "architect",
  "design",
  "redesign",
  "refactor",
  "debug",
  "race condition",
  "deadlock",
  "concurrency",
  "thread",
  "async",
  "reentrant",
  "distributed",
  "security",
  "vulnerab",
  "exploit",
  "optimi",
  "algorithm",
  "complex",
  "integration",
  "migrat",
  "root cause",
  "subtle",
  "edge case",
  "performance",
  "memory leak",
  "fix bug",
  "incident",
  "cryptograph",
}

-- Signals that the work is mechanical -> weak.
local WEAK = {
  "search",
  "find",
  "list",
  "locate",
  "summar",
  "grep",
  "boilerplate",
  "rename",
  "format",
  "count",
  "read",
  "enumerate",
  "trivial",
  "simple",
  "one-liner",
  "lookup",
  "print",
  "indent",
  "sort",
}

-- @param prompt string Subtask description.
-- @return "weak" | "medium" | "strong"
function M.route_tier(prompt)
  if not prompt or prompt == "" then
    return "medium"
  end
  local p = prompt:lower()
  local strong_hits, weak_hits = 0, 0
  for _, sig in ipairs(STRONG) do
    if p:find(sig, 1, true) then
      strong_hits = strong_hits + 1
    end
  end
  for _, sig in ipairs(WEAK) do
    if p:find(sig, 1, true) then
      weak_hits = weak_hits + 1
    end
  end
  if strong_hits > 0 then
    return strong_hits >= 2 and "strong" or "medium"
  end
  if weak_hits > 0 then
    return "weak"
  end
  return "medium"
end

return M
