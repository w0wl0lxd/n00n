-- Checkpoint: save/load JSON snapshots for run lifecycle.
local ok, memory_helpers = pcall(require, "memory.memory_helpers")

local function project_id()
  if ok and memory_helpers then
    local cwd = n00n.uv.cwd()
    local root = n00n.fs.root(cwd, ".git") or cwd
    return memory_helpers.project_id(root)
  end
  local cwd = n00n.uv.cwd()
  local base = n00n.fs.basename(cwd) or "root"
  return base .. "-default"
end

local function checkpoint_dir(run_id)
  local state = n00n.env.state_dir()
  if not state then
    return nil, "cannot resolve state dir"
  end
  if run_id == "." or run_id == ".." or run_id:find("[^%w%-%_.]") then
    return nil, "run_id contains invalid characters"
  end
  return n00n.fs.joinpath(state, "projects/" .. project_id() .. "/runs/" .. run_id .. "/checkpoints")
end

local M = {}

function M.save(run_id, checkpoint_id, state)
  local dir, err = checkpoint_dir(run_id)
  if not dir then
    return nil, err
  end

  n00n.fs.mkdir(dir, { parents = true })

  if checkpoint_id == "." or checkpoint_id == ".." or checkpoint_id:find("[^%w%-%_.]") then
    return nil, "checkpoint_id contains invalid characters"
  end

  local checkpoint = {
    checkpoint_id = checkpoint_id,
    run_id = run_id,
    timestamp = os.time(),
    state_snapshot = state,
  }

  local content, enc_err = n00n.json.encode(checkpoint)
  if not content then
    return nil, "encode error: " .. tostring(enc_err)
  end

  local path = n00n.fs.joinpath(dir, checkpoint_id .. ".json")
  local write_ok, write_err = n00n.fs.write(path, content)
  if not write_ok then
    return nil, "write error: " .. tostring(write_err)
  end

  return true
end

function M.load(run_id, checkpoint_id)
  local dir, err = checkpoint_dir(run_id)
  if not dir then
    return nil, err
  end

  local path = n00n.fs.joinpath(dir, checkpoint_id .. ".json")
  local content, read_err = n00n.fs.read(path)
  if not content then
    return nil, "read error: " .. tostring(read_err)
  end

  local decoded, dec_err = n00n.json.decode(content)
  if not decoded then
    return nil, "decode error: " .. tostring(dec_err)
  end

  return decoded.state_snapshot
end

function M.list(run_id)
  local dir, err = checkpoint_dir(run_id)
  if not dir then
    return nil, err
  end

  local entries = n00n.fs.dir(dir)
  if not entries then
    return {}
  end

  local checkpoints = {}
  for _, entry in ipairs(entries) do
    if entry[2] == "file" and entry[1]:sub(-5) == ".json" then
      local path = n00n.fs.joinpath(dir, entry[1])
      local content, read_err = n00n.fs.read(path)
      if content then
        local decoded, dec_err = n00n.json.decode(content)
        if decoded then
          checkpoints[#checkpoints + 1] = {
            checkpoint_id = decoded.checkpoint_id,
            timestamp = decoded.timestamp,
          }
        end
      end
    end
  end

  table.sort(checkpoints, function(a, b)
    return (a.timestamp or 0) < (b.timestamp or 0)
  end)

  return checkpoints
end

function M.latest(run_id)
  local checkpoints, err = M.list(run_id)
  if not checkpoints then
    return nil, err
  end

  if #checkpoints == 0 then
    return nil
  end

  return checkpoints[#checkpoints].checkpoint_id
end

return M
