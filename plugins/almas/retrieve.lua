-- RAG-grounded retrieval seam (ALMAS Control Agent grounding).
-- v1 uses lexical retrieval (grep over the repo for goal keywords). The vector
-- backend (usearch + embeddings API) is a documented stub that returns nil until
-- wired in a later PR, so it drops in as a drop-in without touching callers.
local M = {}

local MAX_SNIPPETS = 6
local MAX_BLOCK_BYTES = 1600

-- Pull lowercase alphanumeric tokens of length >= 4 from a goal.
local function keywords(goal)
  local out = {}
  for w in (goal or ""):lower():gmatch("%a%a%a%a+") do
    out[#out + 1] = w
  end
  return out
end

-- Lexical retrieval: grep the repo for goal keywords, bound the output.
-- Tolerant: a failing/unsupported grep just yields no hits for that keyword.
local function retrieve_lexical(ctx, goal, k)
  k = k or MAX_SNIPPETS
  local kws = keywords(goal)
  if #kws == 0 then
    return nil
  end
  local picked = {}
  for _, kw in ipairs(kws) do
    if #picked >= k then
      break
    end
    local ok, out = pcall(function()
      return noon.agent.call_tool(ctx, "grep", { pattern = kw, head_limit = 3 })
    end)
    if ok and out and #out > 0 then
      picked[#picked + 1] = "## " .. kw .. "\n" .. out:sub(1, 400)
    end
  end
  if #picked == 0 then
    return nil
  end
  return table.concat(picked, "\n\n"):sub(1, MAX_BLOCK_BYTES)
end

-- Vector backend (deferred): usearch + embeddings. Returns nil until wired.
-- Signature matches retrieve_lexical so it can be swapped freely.
local function retrieve_vector(_ctx, _goal, _k)
  return nil
end

-- @param ctx AgentContext
-- @param goal string Goal (or step) text used to source context.
-- @param role string Role requesting context (for future weighting).
-- @param k integer? Max snippets.
-- @return string? Context block, or nil if nothing useful was found.
function M.retrieve(ctx, goal, role, k)
  local hits = retrieve_lexical(ctx, goal, k)
  if hits and #hits > 0 then
    return hits
  end
  return retrieve_vector(ctx, goal, k)
end

return M
