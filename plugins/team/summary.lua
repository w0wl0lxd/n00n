-- Summary Agent: Meta-RAG context compression for team control.
-- Generates and stores concise natural-language summaries per code unit, then
-- retrieves the top-k summaries for a goal by hashing-trick similarity.
local M = {}

local retrieve = require("retrieve")
local helpers = require("memory_helpers")

local MAX_SUMMARY_CHARS = 400
local MAX_RETRIEVAL_BYTES = 1600

local function base_dir()
  local state = n00n.env.state_dir()
  if not state then
    return nil, "cannot resolve state dir"
  end
  local cwd = n00n.uv.cwd()
  local root = n00n.fs.root(cwd, ".git") or cwd
  local pid = helpers.project_id(root)
  return n00n.fs.joinpath(state, "projects", pid, "summaries")
end

local function summary_path(path)
  local dir, err = base_dir()
  if not dir then
    return nil, err
  end
  local hash = n00n.workflow.hash(path)
  return n00n.fs.joinpath(dir, hash .. ".json")
end

local function read_summary_file(full_path)
  local ok, text = pcall(function()
    return n00n.fs.read(full_path)
  end)
  if not ok or not text then
    return nil
  end
  local data = n00n.json.decode(text)
  if type(data) == "table" and type(data.text) == "string" then
    return data.text
  end
  return nil
end

function M.load(path)
  local p, err = summary_path(path)
  if not p then
    return nil, err
  end
  return read_summary_file(p)
end

function M.exists(path)
  local p, err = summary_path(path)
  if not p then
    return false, err
  end
  local ok, meta = pcall(n00n.fs.metadata, p)
  return ok and meta ~= nil
end

function M.save(path, text)
  local p, err = summary_path(path)
  if not p then
    return nil, err
  end
  local dir = n00n.fs.dirname(p)
  if dir then
    n00n.fs.mkdir(dir, { parents = true })
  end
  local ok, werr = pcall(function()
    return n00n.fs.write(p, n00n.json.encode({ path = path, text = text }))
  end)
  if not ok then
    return nil, tostring(werr)
  end
  return true
end

-- Generate a concise natural-language summary for a single file path.
-- Uses the index tool when available; otherwise reads a bounded chunk.
function M.generate(ctx, path)
  local skeleton, err = n00n.agent.call_tool(ctx, "index", { path = path })
  if not skeleton or #skeleton == 0 then
    local text
    text, err = n00n.fs.read(path)
    if not text then
      return nil, err or "could not read file"
    end
    skeleton = text:sub(1, 1200)
  end

  local model, merr = n00n.agent.resolve_model(ctx, { tier = "weak" })
  if merr then
    return nil, merr
  end
  local tools, terr = n00n.agent.tools(ctx, { spec = model.spec, audience = "general_sub" })
  if terr then
    return nil, terr
  end

  local system =
    "You are a Summary Agent. Summarize the provided code file in one concise paragraph for a developer retrieval system. Include the file path, main responsibilities, and key public APIs."
  local sess, serr = n00n.agent.session(ctx, {
    model_spec = model.spec,
    system = system,
    tools = tools,
    audience = "general_sub",
    name = "summary-agent",
  })
  if serr then
    return nil, serr
  end

  local prompt = "File: " .. path .. "\n\n" .. skeleton
  local res, rerr = sess:prompt(prompt)
  sess:close()
  if rerr then
    return nil, rerr
  end

  local text = ((res and res.text) or ""):sub(1, MAX_SUMMARY_CHARS)
  if #text == 0 then
    return nil, "summary agent produced no text"
  end
  local ok, save_err = M.save(path, text)
  if not ok then
    return nil, save_err
  end
  return text
end

-- Retrieve the top-k summaries most similar to the goal.
function M.retrieve(ctx, goal, k)
  k = k or 6
  local dir, derr = base_dir()
  if not dir then
    return nil, derr
  end
  local ok, entries = pcall(function()
    return n00n.fs.dir(dir, { depth = 1 })
  end)
  if not ok or not entries then
    return nil
  end

  local goal_vec = retrieve.embed(goal)
  local scored = {}
  for _, entry in ipairs(entries) do
    if entry.type == "file" and entry.name:match("%.json$") then
      local full = n00n.fs.joinpath(dir, entry.name)
      local text = read_summary_file(full)
      if text and #text > 0 then
        table.insert(scored, {
          sim = retrieve.cosine(goal_vec, retrieve.embed(text)),
          text = text,
        })
      end
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
  return table.concat(picked, "\n\n"):sub(1, MAX_RETRIEVAL_BYTES)
end

return M
