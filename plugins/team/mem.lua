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
  n00n.fs.mkdir(dir, { parents = true })
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
  local ok, text = pcall(n00n.fs.read, path)
  if not ok or not text then
    return nil
  end
  local data = n00n.json.decode(text)
  if type(data) == "table" then
    return data
  end
  return nil
end

function M.save_state(_ctx, slug, data)
  local dir, err = base_dir()
  if not dir then
    return nil, err
  end
  n00n.fs.mkdir(dir, { parents = true })
  local path, perr = helpers.safe_resolve(dir, slug .. ".state.json")
  if not path then
    return nil, perr
  end
  local ok, werr = pcall(n00n.fs.write, path, n00n.json.encode(data))
  if not ok then
    return nil, tostring(werr)
  end
  return true
end

return M
