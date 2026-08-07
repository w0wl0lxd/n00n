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
local build_conflict_report = helpers.build_conflict_report
local build_skill_stats = helpers.build_skill_stats
local build_discovery_fingerprint = helpers.build_discovery_fingerprint
local record_skill_conflict = helpers.record_skill_conflict
local resolve_skill_content = helpers.resolve_skill_content
local build_tool_policy_lines = helpers.build_tool_policy_lines
local validate_skill = helpers.validate_skill
local build_validation_report = helpers.build_validation_report
local rank_skills = helpers.rank_skills
local graph_rank_signals = helpers.graph_rank_signals
local build_plan = helpers.build_plan
local skill_policy = require("skill_policy")
local skill_telemetry = require("skill_telemetry")
local build_envelope = skill_policy.build_envelope
local policy_instruction_content = skill_policy.instruction_content

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

local function infer_skill_name(skill_dir, root_dir)
  local relative = skill_dir
  if root_dir and skill_dir:sub(1, #root_dir) == root_dir then
    local offset = #root_dir + 2
    if offset <= #skill_dir then
      relative = skill_dir:sub(offset)
    end
  end
  return (relative:gsub("[/\\]", ":"))
end

local discovery_cache = {
  fingerprint = nil,
  skills = nil,
  conflicts = nil,
  stats = nil,
}

local function scan_skill_dir(dir, root_dir, scope_root, skills, conflicts, visited, depth)
  visited = visited or {}
  depth = depth or 0
  local max_depth = 32

  if depth > max_depth then
    return
  end

  local meta = n00n.fs.metadata(dir)
  if not meta then
    return
  end

  local canonical = dir
  if visited[canonical] then
    return
  end
  visited[canonical] = true

  local entries = n00n.fs.dir(dir)
  if not entries then
    return
  end

  local skill_path = n00n.fs.joinpath(dir, SKILL_FILE)
  local content = n00n.fs.read(skill_path)
  if content then
    local fm, body = parse_frontmatter(content)
    if body and #body > 0 then
      local inferred_name = infer_skill_name(dir, root_dir)
      local name = (fm and fm.name) or inferred_name
      local skill = {
        name = name,
        description = (fm and fm.description) or "",
        location = skill_path,
        manual_only = fm ~= nil and fm["disable-model-invocation"] == true,
        paths = fm and fm.paths or nil,
        scope_root = scope_root or root_dir or dir,
        allowed_tools = fm and fm["allowed-tools"] or nil,
        disallowed_tools = fm and fm["disallowed-tools"] or nil,
        synopsis = fm and fm.synopsis or nil,
        tags = fm and fm.tags or nil,
        steps = fm and fm.steps or nil,
      }
      if skills[name] then
        record_skill_conflict(conflicts, name, skills[name], skill)
      end
      skills[name] = skill
    end
  end

  for _, entry in ipairs(entries) do
    if entry[2] == "directory" then
      local child = n00n.fs.joinpath(dir, entry[1])
      scan_skill_dir(child, root_dir, scope_root, skills, conflicts, visited, depth + 1)
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

local function collect_skill_roots()
  local roots = {}

  local config = n00n.env.config_dir()
  if config then
    roots[#roots + 1] = { root = n00n.fs.joinpath(config, "skills"), scope_root = config }
  end

  local home = n00n.uv.os_homedir()
  if home then
    for _, rel in ipairs(GLOBAL_SKILL_DIRS) do
      roots[#roots + 1] = { root = n00n.fs.joinpath(home, rel), scope_root = home }
    end
  end

  for _, ancestor in ipairs(find_project_ancestors()) do
    for _, rel in ipairs(PROJECT_SKILL_DIRS) do
      roots[#roots + 1] = { root = n00n.fs.joinpath(ancestor, rel), scope_root = ancestor }
    end
  end

  return roots
end

local function discover_skills_uncached(roots)
  local skills = {}
  local conflicts = {}
  local visited = {}

  if builtin and opts.plugin_dev then
    skills[builtin.name] = {
      name = builtin.name,
      description = builtin.description,
      content = builtin.content,
      location = "builtin:" .. builtin.name,
      resolve = resolve_builtin_content,
      manual_only = false,
      paths = nil,
      scope_root = nil,
    }
  end

  for _, entry in ipairs(roots) do
    scan_skill_dir(entry.root, entry.root, entry.scope_root, skills, conflicts, visited, 0)
  end

  return skills, conflicts
end

local function count_conflicts(conflicts)
  local count = 0
  for _ in pairs(conflicts) do
    count = count + 1
  end
  return count
end

local function discover_skills()
  local roots = collect_skill_roots()
  local fingerprint = build_discovery_fingerprint(roots)
  if discovery_cache.fingerprint == fingerprint and discovery_cache.skills then
    discovery_cache.stats = {
      cache_hit = true,
      skill_count = discovery_cache.stats and discovery_cache.stats.skill_count or 0,
      conflict_count = discovery_cache.stats and discovery_cache.stats.conflict_count or 0,
    }
    return discovery_cache.skills, discovery_cache.conflicts or {}, discovery_cache.stats
  end

  local skills, conflicts = discover_skills_uncached(roots)
  local skill_count = 0
  for _ in pairs(skills) do
    skill_count = skill_count + 1
  end
  local stats = {
    cache_hit = false,
    skill_count = skill_count,
    conflict_count = count_conflicts(conflicts),
  }
  discovery_cache.fingerprint = fingerprint
  discovery_cache.skills = skills
  discovery_cache.conflicts = conflicts
  discovery_cache.stats = stats
  return skills, conflicts, stats
end

local function normalize_path(path)
  if not path or #path == 0 then
    return nil
  end
  local p = (path:gsub("\\", "/"))
  if p:match("^/") or p:match("^%a:[/]") then
    return p
  end
  local cwd = ((n00n.uv.cwd() or "."):gsub("\\", "/"))
  return (n00n.fs.joinpath(cwd, p):gsub("\\", "/"))
end

local function path_in_scope(skill, focus_path)
  if not skill.paths or #skill.paths == 0 then
    return true
  end
  if not focus_path or focus_path == "" then
    return true
  end
  local absolute_focus = normalize_path(focus_path)
  if not absolute_focus then
    return false
  end
  local root = skill.scope_root or n00n.uv.cwd() or "."
  for _, pattern in ipairs(skill.paths) do
    if helpers.path_matches_pattern(absolute_focus, pattern, root) then
      return true
    end
  end
  return false
end

local function select_skills(skills, input)
  local selected = {}
  local include_manual = input.include_manual == true
  local focus_path = input.path
  for name, skill in pairs(skills) do
    local manual_blocked = skill.manual_only and not include_manual
    if not manual_blocked and path_in_scope(skill, focus_path) then
      selected[name] = skill
    end
  end
  return selected
end

local DESCRIPTION =
  "Load a skill that provides instructions and workflows for specific tasks. Use `list=true` to enumerate available skills; then call with the exact skill `name`."

n00n.api.register_tool({
  name = "skill",
  kind = "read",
  defer_loading = true,
  namespace = "memory",
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
      path = {
        type = "string",
        description = "Optional path in focus; when set, only skills whose frontmatter `paths` match this path are returned.",
      },
      include_manual = {
        type = "boolean",
        default = false,
        description = "Include skills with disable-model-invocation=true.",
      },
      include_conflicts = {
        type = "boolean",
        default = false,
        description = "Append duplicate-name conflict diagnostics to list output.",
      },
      include_stats = {
        type = "boolean",
        default = false,
        description = "Append discovery cache and count stats to list output.",
      },
      preview = {
        type = "boolean",
        default = false,
        description = "Return a short synopsis or first lines instead of the full skill body.",
      },
      full = {
        type = "boolean",
        default = false,
        description = "Load the full skill body (default when preview and section are unset).",
      },
      section = {
        type = "string",
        description = "Load only the markdown section under the given ## heading.",
      },
      preview_lines = {
        type = "integer",
        description = "Maximum lines for preview mode when no synopsis frontmatter is set.",
      },
      validate = {
        type = "boolean",
        default = false,
        description = "With list=true, run skill lint checks and return a validation report.",
      },
      rank = {
        type = "boolean",
        default = false,
        description = "With list=true and path set, sort skills by relevance to the focus path.",
      },
      plan = {
        type = "boolean",
        default = false,
        description = "Return a lightweight section/step plan instead of the full skill body.",
      },
      graph_rank = {
        type = "boolean",
        default = false,
        description = "With list=true and rank=true, add graph-index bonuses for path-scoped skills.",
      },
      include_telemetry = {
        type = "boolean",
        default = false,
        description = "Append skill telemetry summary and log list/load/plan events.",
      },
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
    local all_skills, conflicts, stats = discover_skills()
    local skills = select_skills(all_skills, input)

    -- List/discovery omit active_skill so an existing agent policy is preserved.
    -- Successful name/plan/full loads always set active_skill: either the policy
    -- envelope, or a name-only object that clears the previous policy.
    local function discovery_state(active_skill)
      local state = {
        discovery_cache_hit = stats.cache_hit,
        skill_count = stats.skill_count,
        conflict_count = stats.conflict_count,
      }
      if active_skill then
        state.active_skill = active_skill
      end
      return state
    end

    if input.list then
      if input.validate then
        local results = {}
        for name, skill in pairs(skills) do
          results[#results + 1] = { name = name, issues = validate_skill(skill) }
        end
        table.sort(results, function(a, b)
          return a.name < b.name
        end)
        local output = build_validation_report(results)
        if input.include_stats then
          output = output .. build_skill_stats(stats)
        end
        return {
          llm_output = output,
          state = discovery_state(nil),
        }
      end

      local ranked = nil
      if input.rank == true and input.path and #input.path > 0 then
        local rank_options = nil
        if input.graph_rank == true then
          rank_options = { graph_rank = true, signals = graph_rank_signals() }
        end
        ranked = rank_skills(skills, input.path, rank_options)
      end
      local output = build_skill_list(skills, ranked)
      if input.include_conflicts then
        output = output .. build_conflict_report(conflicts)
      end
      if input.include_stats then
        output = output .. build_skill_stats(stats)
      end
      if input.include_telemetry == true then
        local telemetry_path = skill_telemetry.append("list", nil, {
          skill_count = stats.skill_count,
          ranked = ranked ~= nil,
          graph_rank = input.graph_rank == true,
        })
        output = output .. skill_telemetry.build_summary(telemetry_path)
      end
      return {
        llm_output = output,
        state = discovery_state(nil),
        usage = {
          output_tokens = math.floor(#output / 4),
        },
      }
    end

    if not input.name or #input.name == 0 then
      return {
        llm_output = "error: name is required" .. build_skill_names(skills, input.include_manual == true),
        is_error = true,
        state = discovery_state(nil),
      }
    end

    local skill = skills[input.name]
    if not skill then
      local fallback_names = build_skill_names(skills, input.include_manual == true)
      return {
        llm_output = NOT_FOUND .. input.name .. fallback_names,
        is_error = true,
        state = discovery_state(nil),
      }
    end

    local envelope = build_envelope(skill)
    -- Name-only marker when the skill has no tool policy: agent clears any prior gate.
    local active_skill = envelope or { name = skill.name }

    if input.plan == true then
      local body, body_err = helpers.read_skill_body(skill)
      if not body and not (skill.steps and #skill.steps > 0) then
        return {
          llm_output = "error: " .. tostring(body_err),
          is_error = true,
          state = discovery_state(active_skill),
        }
      end
      local plan, plan_err = build_plan(skill, body)
      if not plan then
        return {
          llm_output = "error: " .. tostring(plan_err),
          is_error = true,
          state = discovery_state(active_skill),
        }
      end
      local output = skill.location .. "\n\n<skill_plan>\n" .. plan .. "\n</skill_plan>"
      if input.include_telemetry == true then
        local telemetry_path = skill_telemetry.append("plan", skill.name, {
          structured = skill.steps ~= nil,
        })
        output = output .. skill_telemetry.build_summary(telemetry_path)
      end
      return {
        llm_output = output,
        state = discovery_state(active_skill),
        usage = {
          output_tokens = math.floor(#output / 4),
        },
      }
    end

    local content, load_err = resolve_skill_content(skill, input)
    if not content then
      return {
        llm_output = "error: " .. tostring(load_err),
        is_error = true,
        state = discovery_state(active_skill),
      }
    end

    local policy = build_tool_policy_lines(skill)
    local lines = {}
    for i, line in ipairs(n00n.split(content, "\n")) do
      lines[#lines + 1] = string.format("%4d | %s", i, line)
    end
    local formatted = skill.location .. "\n" .. policy .. table.concat(lines, "\n")

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
    if not view:set_highlight(content, ext) then
      for line in formatted:gmatch("([^\n]*)\n?") do
        view:append(line)
      end
    end
    view:finish()

    local short = shorten_path(skill.location)
    local header_buf = n00n.ui.buf()
    header_buf:line({ { short, "path" } })

    local result = {
      llm_output = formatted,
      body = buf,
      header = header_buf,
      state = discovery_state(active_skill),
      usage = {
        output_tokens = math.floor(#formatted / 4),
      },
    }
    local instruction = policy_instruction_content(envelope)
    if instruction then
      result.instructions = {
        { path = skill.location, content = instruction },
      }
    end
    if input.include_telemetry == true then
      local telemetry_path = skill_telemetry.append("load", skill.name, {
        preview = input.preview == true,
        section = input.section,
      })
      result.llm_output = result.llm_output .. skill_telemetry.build_summary(telemetry_path)
    end
    return result
  end,
})
