-- SwarmSys decentralized mode (Li et al., arXiv:2510.10047). Three self-organizing
-- roles (Explorer / Worker / Validator) cycle through exploration -> exploitation
-- -> validation. Coordination emerges via (a) agent & event profiles with ability
-- embeddings, (b) embedding-based probabilistic matching, and (c) a pheromone
-- reinforcement loop: validated contributions strengthen future compatibility,
-- idle/invalid ones decay (implicit evaporation as other profiles grow).
local M = {}

local retrieve = require("retrieve")
local roles = require("roles")
local quorum = require("quorum")
local helpers = require("memory_helpers")

local ROLE_SLOTS = { "explorer", "worker", "validator" }
local PHEROMONE_KEY = "swarm_pheromone.json"
local ALPHA = 0.25
local COMPAT_CEIL = 4.0

local ROLE_SYSTEM = {
  explorer = roles.ROLES.planner.system,
  worker = roles.ROLES.developer.system,
  validator = roles.ROLES.reviewer.system,
}

local ROLE_TIER = {
  explorer = "medium",
  worker = "strong",
  validator = "medium",
}

local function phero_dir()
  local state = n00n.env.state_dir()
  if not state then
    return nil
  end
  local cwd = n00n.uv.cwd()
  local root = n00n.fs.root(cwd, ".git") or cwd
  local pid = helpers.project_id(root)
  return n00n.fs.joinpath(state, "projects", pid, "team")
end

local function phero_load()
  local dir = phero_dir()
  if not dir then
    return {}
  end
  local path, _ = helpers.safe_resolve(dir, PHEROMONE_KEY)
  if not path then
    return {}
  end
  local ok, text = pcall(function()
    return n00n.fs.read(path)
  end)
  if not ok or not text then
    return {}
  end
  local data = n00n.json.decode(text)
  return (data and type(data) == "table") and data or {}
end

local function phero_save(matrix)
  local dir = phero_dir()
  if not dir then
    return
  end
  n00n.fs.mkdir(dir, { parents = true })
  local path, _ = helpers.safe_resolve(dir, PHEROMONE_KEY)
  if not path then
    return
  end
  pcall(function()
    n00n.fs.write(path, n00n.json.encode(matrix))
  end)
end

-- agent_profile: workload counter + last reinforced score (pheromone read via matrix).
local function make_agents()
  local agents = {}
  for _, slot in ipairs(ROLE_SLOTS) do
    agents[#agents + 1] = {
      id = slot,
      slot = slot,
      workload = 0,
      last_score = 0,
    }
  end
  return agents
end

-- event_profile: the task embedding (reuse retrieve.lua's hashing-trick vector).
local function event_vec(goal)
  return retrieve.embed(goal)
end

-- Match score: cosine of the agent's slot/kind seed with the event vec, biased
-- up by the reinforced pheromone compatibility for this slot:kind, penalized by
-- current workload so idle/underused agents are preferred. The pheromone term
-- is what makes validated contributions strengthen future selection.
local function match_score(agent, event, matrix)
  local seed = retrieve.embed(agent.slot .. ":" .. event.kind)
  local base = retrieve.cosine(seed, event.vec)
  local compat = matrix[agent.slot .. ":" .. event.kind] or 0
  return base + 0.15 * compat - 0.1 * agent.workload
end

local function run_agent(ctx, agent, event, opts)
  agent.workload = agent.workload + 1
  local prompt = event.task
  if opts.relay_k and opts.relay_k > 0 then
    local block = retrieve.retrieve(ctx, event.task, agent.slot, opts.relay_k)
    if block and #block > 0 then
      prompt = prompt .. "\n\nRelevant context:\n" .. block
    end
  end
  return roles.run(
    ctx,
    agent.slot,
    prompt,
    { model = opts.model, model_tier = ROLE_TIER[agent.slot], thinking = opts.thinking }
  )
end

-- Validate a round's contributions with the EBFT quorum; return accepted issues.
local function validate_round(ctx, workers_output, opts)
  if #workers_output == 0 then
    return { accepted = false, issues = { "no worker output" }, confidence = 0, diverse = false }
  end
  local artifact = table.concat(workers_output, "\n\n---\n\n")
  return quorum.validate(ctx, artifact, { n = 3, model = opts.model, thinking = opts.thinking })
end

-- @param ctx AgentContext
-- @param goal string Refined goal.
-- @param opts table { relay_k?, max_rounds?, max_concurrent?, model?, thinking?, quorum? }
-- @return { ok: boolean, text: string, cost: number, model: string }
function M.run(ctx, goal, opts)
  opts = opts or {}
  opts.relay_k = opts.relay_k or 6
  local max_rounds = opts.max_rounds or 4
  local max_concurrent = opts.max_concurrent or 8

  local semaphore = n00n.async.semaphore(max_concurrent)
  local matrix = phero_load()
  local agents = make_agents()

  local event = { kind = "goal", vec = event_vec(goal), task = goal }
  local consolidated = {}
  local total_cost = 0.0
  local accepted_in_round = false
  local rounds = 0

  for round = 1, max_rounds do
    rounds = round
    accepted_in_round = false

    -- Matching: pick top-1 agent per role slot by match score.
    local chosen = {}
    for _, slot in ipairs(ROLE_SLOTS) do
      local best, best_score
      for _, a in ipairs(agents) do
        if a.slot == slot then
          local s = match_score(a, event, matrix)
          if not best or s > best_score then
            best, best_score = a, s
          end
        end
      end
      chosen[#chosen + 1] = best
    end

    -- Explorers + Workers run concurrently; Validators run after.
    local explorer_workers = {}
    for _, a in ipairs(chosen) do
      if a.slot ~= "validator" then
        explorer_workers[#explorer_workers + 1] = a
      end
    end

    local tasks = {}
    for _, a in ipairs(explorer_workers) do
      tasks[#tasks + 1] = function()
        local permit = semaphore:acquire()
        local r = run_agent(ctx, a, event, opts)
        permit:release()
        return { agent = a, result = r }
      end
    end

    local contributions = {}
    local step_errors = {}
    local results = n00n.async.gather(tasks)
    for i, res in ipairs(results) do
      if res.ok then
        local a = res.value.agent
        local r = res.value.result
        if r.ok then
          contributions[#contributions + 1] = { agent = a, text = r.text or "" }
          total_cost = total_cost + (r.cost or 0)
        else
          step_errors[#step_errors + 1] = a.slot .. ": " .. (r.error or "unknown error")
        end
      end
    end

    local workers_output = {}
    for _, c in ipairs(contributions) do
      if c.agent.slot == "worker" then
        workers_output[#workers_output + 1] = c.text
      end
    end

    -- Validation quorum gates acceptance of this round's worker output.
    local verdict = opts.quorum == false
        and { accepted = #workers_output > 0, issues = {}, confidence = 1, diverse = false }
      or validate_round(ctx, workers_output, opts)
    if verdict.accepted then
      accepted_in_round = true
      for _, c in ipairs(contributions) do
        consolidated[#consolidated + 1] = c.text
        local key = c.agent.slot .. ":" .. event.kind
        matrix[key] = math.min((matrix[key] or 0) + ALPHA, COMPAT_CEIL)
        c.agent.last_score = (matrix[key] or 0)
      end
      phero_save(matrix)
      break
    else
      -- Feed issues back as a new explorer task for the next round.
      event = {
        kind = "revision",
        vec = event_vec(table.concat(verdict.issues, " ")),
        task = "Address these issues:\n" .. table.concat(verdict.issues, "\n"),
      }
    end

    -- Termination: a round after we already have accepted work that adds
    -- nothing new stops the loop. max_rounds is the hard bound above.
    if not accepted_in_round and #consolidated > 0 then
      break
    end
  end

  local text = #consolidated > 0 and table.concat(consolidated, "\n\n---\n\n") or nil
  if not text then
    local err = #step_errors > 0 and table.concat(step_errors, "; ") or "Swarm produced no accepted contribution."
    return { ok = false, error = err }
  end

  return { ok = true, text = text, cost = total_cost, model = "swarm", rounds = rounds }
end

return M
