-- RAG-grounded retrieval seam (team control agent grounding).
-- Lexical retrieval greps the repo for goal keywords. Vector retrieval ranks
-- grep-gathered chunks by cosine similarity over hashing-trick embeddings
-- (offline; n00n-providers exposes no embeddings API). usearch + real
-- embeddings can replace the embed step later without changing callers.
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
      return n00n.agent.call_tool(ctx, "grep", { pattern = kw, limit = 3 })
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

-- Vector retrieval (offline approximation). n00n-providers exposes no embeddings
-- API, so embed with the hashing trick: each token maps to a fixed-dim
-- bag-of-words vector, and candidate chunks are ranked by cosine similarity to
-- the goal. Drop-in for usearch + real embeddings later.
local VEC_DIM = 256

local function hash_token(tok)
  local h = 2166136261
  for i = 1, #tok do
    h = bit32.band(bit32.bxor(h, string.byte(tok, i) * 16777619), 0xffffffff)
  end
  return h
end

local function embed(text)
  local vec = {}
  for i = 1, VEC_DIM do
    vec[i] = 0
  end
  for w in (text or ""):lower():gmatch("%a%a+") do
    local slot = (hash_token(w) % VEC_DIM) + 1
    vec[slot] = vec[slot] + 1
  end
  return vec
end

local function cosine(a, b)
  local dot, na, nb = 0, 0, 0
  for i = 1, VEC_DIM do
    dot = dot + a[i] * b[i]
    na = na + a[i] * a[i]
    nb = nb + b[i] * b[i]
  end
  if na == 0 or nb == 0 then
    return 0
  end
  return dot / (math.sqrt(na) * math.sqrt(nb))
end

local function retrieve_vector(ctx, goal, k)
  k = k or MAX_SNIPPETS
  local kws = keywords(goal)
  if #kws == 0 then
    return nil
  end
  local goal_vec = embed(goal)
  local scored = {}
  for _, kw in ipairs(kws) do
    local ok, out = pcall(function()
      return n00n.agent.call_tool(ctx, "grep", { pattern = kw, limit = 3 })
    end)
    if ok and out and #out > 0 then
      scored[#scored + 1] = {
        sim = cosine(goal_vec, embed(out)),
        text = "## " .. kw .. "\n" .. out:sub(1, 400),
      }
    end
  end
  if #scored == 0 then
    return nil
  end
  table.sort(scored, function(a, b)
    return a.sim > b.sim
  end)
  local picked = {}
  for i = 1, math.min(k, #scored) do
    picked[#picked + 1] = scored[i].text
  end
  return table.concat(picked, "\n\n"):sub(1, MAX_BLOCK_BYTES)
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

-- Exported so swarm.lua reuses the exact same hashing-trick vectors (no dup).
M.embed = embed
M.cosine = cosine
M.VEC_DIM = VEC_DIM

return M
