local M = {}

local ok_helpers, memory_helpers = pcall(require, "memory.memory_helpers")

local function project_id()
  if ok_helpers and memory_helpers then
    local cwd = n00n.uv.cwd()
    local root = n00n.fs.root(cwd, ".git") or cwd
    return memory_helpers.project_id(root)
  end
  local cwd = n00n.uv.cwd()
  local base = n00n.fs.basename(cwd) or "root"
  return base .. "-default"
end

function M.telemetry_dir()
  local state = n00n.env.state_dir()
  if not state then
    return nil, "state dir unavailable"
  end
  return n00n.fs.joinpath(state, "projects", project_id(), "skills")
end

function M.telemetry_path()
  local dir, err = M.telemetry_dir()
  if not dir then
    return nil, err
  end
  return dir
end

function M.append(event, skill_name, data)
  local dir, err = M.telemetry_dir()
  if not dir then
    return nil, err
  end
  local _, mkdir_err = n00n.fs.mkdir(dir, { parents = true })
  if mkdir_err then
    return nil, mkdir_err
  end

  local row = {
    timestamp = os.time(),
    event = event,
    skill_name = skill_name,
    data = data or {},
  }
  local encoded, encode_err = n00n.json.encode(row)
  if not encoded then
    return nil, encode_err or "failed to encode telemetry event"
  end

  -- One file per event avoids read-modify-write races across concurrent writers.
  local digest = "0"
  local ok_hash, hash = pcall(function()
    return n00n.workflow.hash(encoded)
  end)
  if ok_hash and hash then
    digest = tostring(hash)
  else
    digest = string.format("%d-%d", #encoded, math.floor((os.clock() or 0) * 1e6))
  end
  local safe_skill = tostring(skill_name or "none"):gsub("[^%w%._-]", "_")
  local event_name = string.format("event-%d-%s-%s.jsonl", row.timestamp, safe_skill, digest)
  local event_path = n00n.fs.joinpath(dir, event_name)
  local _, write_err = n00n.fs.write(event_path, encoded .. "\n")
  if write_err then
    return nil, write_err
  end
  return event_path
end

function M.build_summary(path)
  if not path then
    return ""
  end
  return "\n\n<skill_telemetry>\nlogged: " .. path .. "\n</skill_telemetry>"
end

return M
