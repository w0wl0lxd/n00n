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

local function validate_id(id)
  if not id or id == "" then
    return nil, "id is required"
  end
  if #id > 128 then
    return nil, "id exceeds maximum length of 128"
  end
  if id:find("%.%.") or id:find("/") or id:find("\\") or id:find("%z") or id:find("%c") then
    return nil, "id contains invalid characters (path traversal, control chars, or null not allowed)"
  end
  if id:find("[^%w%-%_.]") then
    return nil, "id contains invalid characters (only alphanumeric, dash, underscore, dot allowed)"
  end
  return true
end

local function checkpoint_dir(run_id)
  local state = n00n.env.state_dir()
  if not state then
    return nil, "cannot resolve state dir"
  end
  local ok, err = validate_id(run_id)
  if not ok then
    return nil, err
  end
  return n00n.fs.joinpath(state, "projects/" .. project_id() .. "/runs/" .. run_id .. "/checkpoints")
end

local function checkpoint_less(a, b)
  local a_has_sequence = type(a.sequence) == "number"
  local b_has_sequence = type(b.sequence) == "number"
  if a_has_sequence ~= b_has_sequence then
    return not a_has_sequence
  end
  if a_has_sequence and a.sequence ~= b.sequence then
    return a.sequence < b.sequence
  end
  if a.timestamp ~= b.timestamp then
    return a.timestamp < b.timestamp
  end
  return a.checkpoint_id < b.checkpoint_id
end

local M = {}

function M.save(run_id, checkpoint_id, state, sequence)
  local dir, err = checkpoint_dir(run_id)
  if not dir then
    return nil, err
  end

  local mkdir_ok, mkdir_err = n00n.fs.mkdir(dir, { parents = true })
  if not mkdir_ok then
    return nil, "mkdir error: " .. tostring(mkdir_err)
  end

  local ok, vid = validate_id(checkpoint_id)
  if not ok then
    return nil, vid
  end

  if sequence ~= nil and (type(sequence) ~= "number" or sequence < 0 or sequence ~= math.floor(sequence)) then
    return nil, "sequence must be a non-negative integer"
  end

  local checkpoint = {
    checkpoint_id = checkpoint_id,
    run_id = run_id,
    timestamp = os.time(),
    sequence = sequence,
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
  if type(decoded) ~= "table" then
    return nil, "decode error: " .. tostring(dec_err or "checkpoint must be a JSON object")
  end
  if type(decoded.state_snapshot) ~= "table" then
    return nil, "decode error: checkpoint state_snapshot must be a JSON object"
  end

  return decoded.state_snapshot
end

function M.list(run_id)
  local dir, err = checkpoint_dir(run_id)
  if not dir then
    return nil, err
  end

  local entries, dir_err = n00n.fs.dir(dir)
  if not entries then
    local metadata, metadata_err = n00n.fs.metadata(dir)
    if metadata_err then
      return nil, "checkpoint directory metadata error: " .. tostring(metadata_err)
    end
    if not metadata then
      return {}
    end
    return nil, "checkpoint directory read error: " .. tostring(dir_err)
  end

  local checkpoints = {}
  for _, entry in ipairs(entries) do
    if entry[2] == "file" and entry[1]:sub(-5) == ".json" then
      local path = n00n.fs.joinpath(dir, entry[1])
      local content, read_err = n00n.fs.read(path)
      if not content then
        return nil, "read error for " .. entry[1] .. ": " .. tostring(read_err)
      end
      local decoded, dec_err = n00n.json.decode(content)
      if type(decoded) ~= "table" then
        return nil, "decode error for " .. entry[1] .. ": " .. tostring(dec_err or "checkpoint must be a JSON object")
      end
      if type(decoded.checkpoint_id) ~= "string" or type(decoded.timestamp) ~= "number" then
        return nil, "decode error for " .. entry[1] .. ": invalid checkpoint metadata"
      end
      if decoded.sequence ~= nil and type(decoded.sequence) ~= "number" then
        return nil, "decode error for " .. entry[1] .. ": invalid checkpoint sequence"
      end
      checkpoints[#checkpoints + 1] = {
        checkpoint_id = decoded.checkpoint_id,
        timestamp = decoded.timestamp,
        sequence = decoded.sequence,
      }
    end
  end

  table.sort(checkpoints, checkpoint_less)

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

function M.prune(run_id, keep_n)
  local dir, err = checkpoint_dir(run_id)
  if not dir then
    return nil, err
  end

  if not keep_n or keep_n < 0 then
    keep_n = 1
  end

  local checkpoints, list_err = M.list(run_id)
  if not checkpoints then
    return nil, list_err
  end

  if #checkpoints <= keep_n then
    return true
  end

  table.sort(checkpoints, function(a, b)
    return checkpoint_less(b, a)
  end)

  local to_remove = {}
  for i = keep_n + 1, #checkpoints do
    to_remove[#to_remove + 1] = checkpoints[i].checkpoint_id
  end

  for _, ckpt_id in ipairs(to_remove) do
    local path = n00n.fs.joinpath(dir, ckpt_id .. ".json")
    local rm_ok, rm_err = n00n.fs.rm(path)
    if not rm_ok then
      return nil, "remove error for " .. ckpt_id .. ": " .. tostring(rm_err)
    end
  end

  return true
end

return M
