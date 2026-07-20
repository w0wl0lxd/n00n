local failures = {}

local function case(name, fn)
  local ok, err = pcall(fn)
  if not ok then
    table.insert(failures, name .. ": " .. tostring(err))
  end
end

local function eq(actual, expected, msg)
  if actual ~= expected then
    error((msg or "") .. "\nexpected: " .. tostring(expected) .. "\n  actual: " .. tostring(actual))
  end
end

local _tmpdir_counter = 0
local function mktmpdir()
  _tmpdir_counter = _tmpdir_counter + 1
  local name = "/tmp/noon_write_spec_" .. tostring(os.time()) .. "_" .. _tmpdir_counter
  noon.fs.mkdir(name)
  return name
end

local function rmtree(dir)
  local entries = noon.fs.dir(dir)
  if entries then
    for _, e in ipairs(entries) do
      local p = noon.fs.joinpath(dir, e[1])
      if e[2] == "directory" then
        rmtree(p)
      else
        noon.fs.rm(p)
      end
    end
  end
  noon.fs.rm(dir)
end

case("write_new_file_succeeds", function()
  local tmpdir = mktmpdir()
  local path = noon.fs.joinpath(tmpdir, "hello.txt")
  noon.fs.write(path, "hello world")
  local content = noon.fs.read(path)
  eq(content, "hello world", "round-trip content mismatch")
  rmtree(tmpdir)
end)

case("write_creates_parent_directories", function()
  local tmpdir = mktmpdir()
  local path = noon.fs.joinpath(tmpdir, "a/b/c/deep.txt")
  local parent = noon.fs.dirname(path)
  noon.fs.mkdir(parent, { parents = true })
  noon.fs.write(path, "nested")
  local content = noon.fs.read(path)
  eq(content, "nested", "nested file content mismatch")
  rmtree(tmpdir)
end)

case("write_overwrites_existing_file", function()
  local tmpdir = mktmpdir()
  local path = noon.fs.joinpath(tmpdir, "overwrite.txt")
  noon.fs.write(path, "first")
  noon.fs.write(path, "second")
  local content = noon.fs.read(path)
  eq(content, "second", "overwrite should replace content entirely")
  rmtree(tmpdir)
end)

case("write_empty_content", function()
  local tmpdir = mktmpdir()
  local path = noon.fs.joinpath(tmpdir, "empty.txt")
  noon.fs.write(path, "")
  eq(noon.fs.read(path), "", "empty file should round-trip as empty string")
  rmtree(tmpdir)
end)

case("write_large_content", function()
  local tmpdir = mktmpdir()
  local path = noon.fs.joinpath(tmpdir, "large.txt")
  local lines = {}
  for i = 1, 5000 do
    lines[i] = "line " .. i
  end
  local content = table.concat(lines, "\n") .. "\n"
  noon.fs.write(path, content)
  eq(noon.fs.read(path), content, "large file content mismatch")
  rmtree(tmpdir)
end)

case("write_preserves_content_exactly", function()
  local tmpdir = mktmpdir()
  local vectors = {
    { "with_nl", "line\n" },
    { "without_nl", "line" },
    { "special", "tab\there\nnewline\n\\backslash\n" },
  }
  for _, v in ipairs(vectors) do
    local path = noon.fs.joinpath(tmpdir, v[1] .. ".txt")
    noon.fs.write(path, v[2])
    eq(noon.fs.read(path), v[2], v[1] .. " content mismatch")
  end
  rmtree(tmpdir)
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
