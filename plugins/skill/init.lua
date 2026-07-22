local SKILL_FILE = "SKILL.md"
local NOT_FOUND = "skill not found: "
local REFERENCE_FILE = "lua-api.md"
local REFERENCE_UNAVAILABLE = "(unavailable; full reference inlined below)"
local shorten_path = require("n00n.shorten_path")
local ToolView = require("n00n.tool_view")
local helpers = require("skill_helpers")
local parse_frontmatter = helpers.parse_frontmatter
local build_skill_list = helpers.build_skill_list
local build_skill_names = helpers.build_skill_names

local PROJECT_SKILL_DIRS = {
  ".n00n/skills",
  ".claude/skills",
  ".opencode/skills",
  ".agents/skills",
}
local GLOBAL_SKILL_DIRS = {
  ".claude/skills",
  ".config/opencode/skills",
  ".agents/skills",
}

local function scan_skill_dir(dir, skills)
  local entries = n00n.fs.dir(dir)
  if not entries then
    return
  end
  for _, entry in ipairs(entries) do
    if entry[2] == "directory" then
      local skill_path = n00n.fs.joinpath(dir, entry[1], SKILL_FILE)
      local content = n00n.fs.read(skill_path)
      if content then
        local fm, body = parse_frontmatter(content)
        if body and #body > 0 then
          local name = (fm and fm.name) or entry[1]
          skills[name] = {
            name = name,
            description = (fm and fm.description) or "",
            content = body,
            location = skill_path,
          }
        end
      end
    end
  end
end

local function find_project_ancestors()
  local cwd = n00n.uv.cwd()
  if not cwd then
    return {}
  end
  local dirs = { cwd }
  for _, parent in ipairs(n00n.fs.parents(cwd)) do
    dirs[#dirs + 1] = parent
    local git = n00n.fs.joinpath(parent, ".git")
    if n00n.fs.metadata(git) then
      break
    end
  end
  return dirs
end

local opts = n00n.api.register_options({
  plugin_dev = { default = true, desc = "Offer the builtin n00n-plugin-dev skill for writing n00n plugins." },
})

local ok, builtin, reference = pcall(function()
  return require("plugin_dev"), require("plugin_dev_reference")
end)
if not ok then
  n00n.log.warn("builtin plugin_dev skill unavailable: " .. tostring(builtin))
  builtin = nil
end

local function resolve_builtin_content()
  local state = n00n.env.state_dir()
  if state then
    local dir = n00n.fs.joinpath(state, "docs")
    local path = n00n.fs.joinpath(dir, REFERENCE_FILE)
    local _, err = n00n.fs.mkdir(dir, { parents = true })
    if not err then
      _, err = n00n.fs.write(path, reference.content)
    end
    if not err then
      return (builtin.content:gsub(builtin.reference_placeholder, function()
        return path
      end))
    end
    n00n.log.warn("failed to write lua api reference to " .. path .. ": " .. tostring(err))
  end
  local content = builtin.content:gsub(builtin.reference_placeholder, REFERENCE_UNAVAILABLE)
  return content .. "\n---\n\n" .. reference.content
end

local function discover_skills()
  local skills = {}

  if builtin and opts.plugin_dev then
    skills[builtin.name] = {
      name = builtin.name,
      description = builtin.description,
      content = builtin.content,
      location = "builtin:" .. builtin.name,
      resolve = resolve_builtin_content,
    }
  end

  local config = n00n.env.config_dir()
  if config then
    scan_skill_dir(n00n.fs.joinpath(config, "skills"), skills)
  end

  local home = n00n.uv.os_homedir()
  if home then
    for _, rel in ipairs(GLOBAL_SKILL_DIRS) do
      scan_skill_dir(n00n.fs.joinpath(home, rel), skills)
    end
  end

  for _, ancestor in ipairs(find_project_ancestors()) do
    for _, rel in ipairs(PROJECT_SKILL_DIRS) do
      scan_skill_dir(n00n.fs.joinpath(ancestor, rel), skills)
    end
  end

  return skills
end

local DESCRIPTION =
  "Load a skill that provides instructions and workflows for specific tasks. Use `list=true` to enumerate available skills."

n00n.api.register_tool({
  name = "skill",
  kind = "read",
  description = DESCRIPTION,

  schema = {
    type = "object",
    properties = {
      list = {
        type = "boolean",
        default = false,
        description = "Return the list of available skills with their descriptions instead of loading one.",
      },
      name = { type = "string", description = "Name of the skill to load." },
    },
  },

  header = function(input)
    return input.list and "skill list" or input.name
  end,

  restore = function(_input, output, _is_error, ctx)
    local tol = ctx:tool_output_lines()
    return ToolView.restore(output, {
      max_lines = (tol and tol.other) or 20,
      keep = "head",
    })
  end,

  handler = function(input, ctx)
    local skills = discover_skills()

    if input.list then
      return { llm_output = build_skill_list(skills) }
    end

    if not input.name or #input.name == 0 then
      return { llm_output = "error: name is required" .. build_skill_names(skills), is_error = true }
    end

    local skill = skills[input.name]
    if not skill then
      return { llm_output = NOT_FOUND .. input.name .. build_skill_names(skills), is_error = true }
    end
    if skill.resolve then
      skill.content = skill.resolve()
    end

    local lines = {}
    for i, line in ipairs(n00n.split(skill.content, "\n")) do
      lines[#lines + 1] = string.format("%4d | %s", i, line)
    end
    local formatted = skill.location .. "\n" .. table.concat(lines, "\n")

    local buf = n00n.ui.buf()
    local tol = ctx:tool_output_lines()
    local view = ToolView.new(buf, {
      max_lines = (tol and tol.other) or 20,
      keep = "head",
    })
    buf:on("click", function()
      view:toggle()
    end)

    local ext = skill.location:match("%.([^%.]+)$") or "md"
    if not view:set_highlight(skill.content, ext) then
      for line in formatted:gmatch("([^\n]*)\n?") do
        view:append(line)
      end
    end
    view:finish()

    local short = shorten_path(skill.location)
    local header_buf = n00n.ui.buf()
    header_buf:line({ { short, "path" } })

    return {
      llm_output = formatted,
      body = buf,
      header = header_buf,
    }
  end,
})
