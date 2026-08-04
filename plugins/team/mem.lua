local helpers = require("memory_helpers")

local M = {}

local function base_dir()
  local state = n00n.env.state_dir()
  if not state then
    return nil, "cannot resolve state dir"
  end
  local cwd = n00n.uv.cwd()
  local root = n00n.fs.root(cwd, ".git") or cwd
  local pid = helpers.project_id(root)
  return n00n.fs.joinpath(state, "projects", pid, "team")
end

M.base_dir = base_dir

function M.slug(goal)
  local cwd = n00n.uv.cwd()
  local root = n00n.fs.root(cwd, ".git") or cwd
  return helpers.project_id(root) .. "-" .. helpers.fnv1a_64(goal)
end

function M.load(_ctx, slug)
  local dir, err = base_dir()
  if not dir then
    return nil, err
  end
  local path, perr = helpers.safe_resolve(dir, slug .. ".md")
  if not path then
    return nil, perr
  end
  return n00n.fs.read(path)
end

function M.save(_ctx, slug, content)
  local dir, err = base_dir()
  if not dir then
    return nil, err
  end
  local mkdir_ok, mkdir_err = n00n.fs.mkdir(dir, { parents = true })
  if not mkdir_ok then
    return nil, "mkdir error: " .. tostring(mkdir_err)
  end
  local path, perr = helpers.safe_resolve(dir, slug .. ".md")
  if not path then
    return nil, perr
  end
  return n00n.fs.write(path, content)
end

function M.load_state(_ctx, slug)
  local dir, err = base_dir()
  if not dir then
    return nil, err
  end
  local path, perr = helpers.safe_resolve(dir, slug .. ".state.json")
  if not path then
    return nil, perr
  end
  local ok, text, read_err = pcall(n00n.fs.read, path)
  if not ok then
    return nil, "read error: " .. tostring(text)
  end
  if not text then
    return nil, read_err and ("read error: " .. tostring(read_err)) or nil
  end
  local data, decode_err = n00n.json.decode(text)
  if not data then
    return nil, "decode error: " .. tostring(decode_err)
  end
  if type(data) ~= "table" then
    return nil, "decode error: resume state must be a JSON object"
  end
  return data
end

function M.save_state(_ctx, slug, data)
  local dir, err = base_dir()
  if not dir then
    return nil, err
  end
  local mkdir_ok, mkdir_err = n00n.fs.mkdir(dir, { parents = true })
  if not mkdir_ok then
    return nil, "mkdir error: " .. tostring(mkdir_err)
  end
  local path, perr = helpers.safe_resolve(dir, slug .. ".state.json")
  if not path then
    return nil, perr
  end
  local content, encode_err = n00n.json.encode(data)
  if not content then
    return nil, "encode error: " .. tostring(encode_err)
  end
  local ok, write_ok, write_err = pcall(n00n.fs.write, path, content)
  if not ok then
    return nil, "write error: " .. tostring(write_ok)
  end
  if not write_ok then
    return nil, "write error: " .. tostring(write_err)
  end
  return true
end

return M
