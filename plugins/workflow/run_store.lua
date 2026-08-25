local M = {}
local META_FILENAME = "meta.json"

local function remove_created(path, label)
  local ok, err = n00n.fs.rm(path)
  if not ok then
    return "failed to remove partial workflow " .. label .. ": " .. tostring(err)
  end
  return nil
end

function M.create(dir, journal_path, meta)
  if type(dir) ~= "string" or type(journal_path) ~= "string" then
    return nil, "cannot resolve workflow run paths"
  end

  local content, encode_err = n00n.json.encode(meta)
  if not content then
    return nil, "failed to encode workflow metadata: " .. tostring(encode_err)
  end

  local dir_metadata, dir_inspect_err = n00n.fs.metadata(dir)
  if dir_metadata and not dir_metadata.is_dir then
    return nil, "workflow run path is not a directory"
  end
  if dir_inspect_err then
    return nil, "failed to inspect workflow run directory: " .. tostring(dir_inspect_err)
  end
  local created_dir = not dir_metadata
  if created_dir then
    local mkdir_ok, mkdir_err = n00n.fs.mkdir(dir, { parents = true })
    if not mkdir_ok then
      return nil, "failed to create workflow run directory: " .. tostring(mkdir_err)
    end
  end

  local meta_path = n00n.fs.joinpath(dir, META_FILENAME)
  local existing_meta, meta_inspect_err = n00n.fs.metadata(meta_path)
  if existing_meta then
    return nil, "workflow metadata already exists"
  end
  if meta_inspect_err then
    return nil, "failed to inspect workflow metadata: " .. tostring(meta_inspect_err)
  end
  local existing_journal, journal_inspect_err = n00n.fs.metadata(journal_path)
  if existing_journal then
    return nil, "workflow journal already exists"
  end
  if journal_inspect_err then
    return nil, "failed to inspect workflow journal: " .. tostring(journal_inspect_err)
  end

  local meta_ok, meta_err = n00n.fs.write(meta_path, content)
  if not meta_ok then
    local cleanup_err
    if created_dir then
      cleanup_err = remove_created(dir, "run directory")
    end
    local err = "failed to write workflow metadata: " .. tostring(meta_err)
    if cleanup_err then
      err = err .. "; " .. cleanup_err
    end
    return nil, err
  end

  local journal_ok, journal_err = n00n.fs.write(journal_path, "")
  if not journal_ok then
    local cleanup_err = remove_created(meta_path, "metadata")
    if created_dir and not cleanup_err then
      cleanup_err = remove_created(dir, "run directory")
    end
    local err = "failed to create workflow journal: " .. tostring(journal_err)
    if cleanup_err then
      err = err .. "; " .. cleanup_err
    end
    return nil, err
  end
  return true
end

return M
