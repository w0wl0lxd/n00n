local helpers = require("memory_helpers")

local M = {}

local function base_dir()
  local state = noon.env.state_dir()
  if not state then
    return nil, "cannot resolve state dir"
  end
  local cwd = noon.uv.cwd()
  local root = noon.fs.root(cwd, ".git") or cwd
  local pid = helpers.project_id(root)
  return noon.fs.joinpath(state, "projects", pid, "almas")
end

function M.slug(goal)
  local cwd = noon.uv.cwd()
  local root = noon.fs.root(cwd, ".git") or cwd
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
  return noon.fs.read(path)
end

function M.save(_ctx, slug, content)
  local dir, err = base_dir()
  if not dir then
    return nil, err
  end
  noon.fs.mkdir(dir, { parents = true })
  local path, perr = helpers.safe_resolve(dir, slug .. ".md")
  if not path then
    return nil, perr
  end
  return noon.fs.write(path, content)
end

return M
