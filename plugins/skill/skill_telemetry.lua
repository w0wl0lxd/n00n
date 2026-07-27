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

function M.telemetry_path()
  local state = n00n.env.state_dir()
  if not state then
    return nil, "state dir unavailable"
  end
  return n00n.fs.joinpath(state, "projects", project_id(), "skills", "events.jsonl")
end

function M.append(event, skill_name, data)
  local path, err = M.telemetry_path()
  if not path then
    return nil, err
  end
  local dir = n00n.fs.dirname(path)
  local _, mkdir_err = n00n.fs.mkdir(dir, { parents = true })
  if mkdir_err then
    return nil, mkdir_err
  end

  local existing = ""
  local read_ok, text = pcall(n00n.fs.read, path)
  if read_ok and text then
    existing = text
  end

  local row = {
    timestamp = os.time(),
    event = event,
    skill_name = skill_name,
    data = data or {},
  }
  local encoded, encode_err = n00n.json.encode(row)
  if not encode_err and encoded then
    local _, write_err = n00n.fs.write(path, existing .. encoded .. "\n")
    if write_err then
      return nil, write_err
    end
    return path
  end
  return nil, encode_err or "failed to encode telemetry event"
end

function M.build_summary(path)
  if not path then
    return ""
  end
  return "\n\n<skill_telemetry>\nlogged: " .. path .. "\n</skill_telemetry>"
end

return M
