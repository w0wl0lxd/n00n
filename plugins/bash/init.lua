local ToolView = require("n00n.tool_view")
local output_limits = require("n00n.output_limits")
local command_guard = require("command_guard")
local output_collector = require("output_collector")

local split_shell_words = command_guard.split_shell_words
local git_subcommand_index = command_guard.git_subcommand_index
local git_subcommand = command_guard.git_subcommand
local git_uses_machine_format = command_guard.git_uses_machine_format
local strip_leading_assignments = command_guard.strip_leading_assignments
local broad_bash_command_reason = command_guard.broad_bash_command_reason

local RTK_REWRITE_TIMEOUT_MS = 10000
local RTK_UNSUPPORTED_FLAGS = {
  " -o ",
  " -not ",
  " ! ",
  " -exec ",
  " -execdir ",
  " -print0",
  " -delete",
  " -ok ",
  " -okdir ",
  " -fprint",
  " -fls ",
  " -fprintf ",
}
-- Preserve shell wrappers such as BASH_ENV hooks for build commands.
local RTK_SKIP_TOOLS = {
  cargo = true,
  nextest = true,
  rustc = true,
}
local SEPARATOR = "──────"
local BROAD_COMMAND_JUSTIFICATION_REQUIRED = "error: justification is required for unbounded command execution"
local RTK_REWRITE_REQUIRED = "error: rtk is enabled, but this managed command could not be safely rewritten"
-- rtk_rewrite error reason: rtk has no rewrite for this exact command, as
-- opposed to a policy rejection. Only this reason is safe to swallow when a
-- compound segment falls back to running unchanged.
local RTK_REWRITE_REASON_UNAVAILABLE = "unavailable"
local RTK_MANAGED_COMMANDS = {
  cargo = true,
  cat = true,
  docker = true,
  find = true,
  gh = true,
  git = true,
  grep = true,
  ls = true,
  npm = true,
  pip = true,
  pip3 = true,
  podman = true,
  python = true,
  python3 = true,
  rg = true,
}
local RTK_STRING_COMMAND_WRAPPERS = {
  bash = true,
  eval = true,
  sh = true,
}
local RTK_COMMAND_WRAPPERS = {
  bash = true,
  command = true,
  env = true,
  eval = true,
  exec = true,
  ionice = true,
  nice = true,
  nohup = true,
  setsid = true,
  sh = true,
  stdbuf = true,
  sudo = true,
  timeout = true,
  watch = true,
  xargs = true,
}

local rtk_available
local rtk_probe_error
local rtk_enforcement_required
local rtk_rewrite
local rtk_rewrite_compound
local rtk_single_command

local function shell_quote(s)
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

local function unquote(s)
  local q = s:sub(1, 1)
  if (q == '"' or q == "'") and s:sub(-1) == q then
    return s:sub(2, -2)
  end
  return s
end

local function parse_cd_hint(input)
  if input.workdir then
    return input.command, input.workdir
  end
  local rest = input.command:match("^cd%s+(.+)$")
  if rest then
    local dir, tail = rest:match("^(.-)%s+&&%s+(.+)$")
    if dir and dir ~= "" then
      return tail, unquote(dir)
    end
  end
  return input.command, nil
end

local function trim(s)
  return s:match("^%s*(.-)%s*$")
end

local function normalize_sep(s)
  return s:gsub("\\", "/")
end

local function relative_path(p)
  local np = normalize_sep(p)
  local cwd = n00n.uv.cwd()
  if cwd then
    cwd = normalize_sep(cwd)
    if np:sub(1, #cwd + 1) == cwd .. "/" then
      local rel = np:sub(#cwd + 2)
      return rel == "" and "." or rel
    end
    if np == cwd then
      return "."
    end
  end
  local home = n00n.uv.os_homedir()
  if home then
    home = normalize_sep(home)
    if np:sub(1, #home + 1) == home .. "/" then
      local rel = np:sub(#home + 2)
      return rel == "" and "~" or "~/" .. rel
    end
  end
  return p
end

local function build_header_lines(command)
  local header = {}
  local highlighted = n00n.ui.highlight(command, "bash")
  if highlighted then
    for _, line in ipairs(highlighted) do
      header[#header + 1] = line
    end
  else
    header[#header + 1] = command
  end
  header[#header + 1] = { { SEPARATOR, "dim" } }
  return header
end

local RTK_GIT_FALLBACK = {
  remote = true,
  config = true,
  tag = true,
  blame = true,
  shortlog = true,
  ["show-ref"] = true,
  ["for-each-ref"] = true,
  ["rev-parse"] = true,
}

local function rtk_find_unsupported(cmd)
  if not cmd:match("^rtk find ") then
    return false
  end
  for _, flag in ipairs(RTK_UNSUPPORTED_FLAGS) do
    if cmd:find(flag, 1, true) then
      return true
    end
  end
  return false
end

local function normalize_command(command)
  -- rtk rewrite recognizes `head -N` and `head --lines=N` but not `head -n N`.
  local n, rest = command:match("^head%s+%-n%s+(%d+)(.*)$")
  if n then
    return "head -" .. n .. rest
  end
  n, rest = command:match("^head%s+%-n(%d+)(.*)$")
  if n then
    return "head -" .. n .. rest
  end
  return command
end

local GIT_SANITIZE_SUBCOMMANDS = {
  diff = true,
  show = true,
  log = true,
}

-- Harden git commands against repo-config injection of external diff/pagers.
-- Inserts `--no-optional-locks` (prevents write locks) and `--no-ext-diff`
-- for subcommands that may invoke an external diff driver.
local function sanitize_git_command(command)
  local trimmed = trim(command)
  if not trimmed:lower():match("^git%s") then
    return command
  end

  local words = split_shell_words(trimmed)
  if #words < 2 or words[1]:lower() ~= "git" then
    return command
  end

  -- Strip any -c core.fsmonitor=... override and force it to false. A repo or
  -- parent config with core.fsmonitor set to a command can execute code during
  -- git status/diff/log; this disables it without trusting the environment.
  local i = 2
  while i <= #words do
    if words[i] == "-c" and words[i + 1] then
      local value = words[i + 1]:lower()
      if value:sub(1, #"core.fsmonitor") == "core.fsmonitor" then
        table.remove(words, i)
        table.remove(words, i)
      else
        i = i + 2
      end
    else
      i = i + 1
    end
  end
  table.insert(words, 2, "-c")
  table.insert(words, 3, "core.fsmonitor=false")

  local subcommand_index = git_subcommand_index(words, 2)
  local option_end = subcommand_index and subcommand_index - 1 or #words
  local has_optional_locks = false
  for i = 2, option_end do
    if words[i] == "--no-optional-locks" then
      has_optional_locks = true
      break
    end
  end

  if not has_optional_locks then
    table.insert(words, 2, "--no-optional-locks")
    if subcommand_index then
      subcommand_index = subcommand_index + 1
    end
  end

  if subcommand_index then
    local subcommand = words[subcommand_index]:lower()
    if GIT_SANITIZE_SUBCOMMANDS[subcommand] then
      local has_no_ext_diff = false
      local i = subcommand_index + 1
      while i <= #words do
        if words[i] == "--no-ext-diff" then
          has_no_ext_diff = true
          i = i + 1
        elseif words[i] == "--ext-diff" then
          -- Remove an explicit --ext-diff so the later --no-ext-diff cannot be
          -- overridden by it.
          table.remove(words, i)
        else
          i = i + 1
        end
      end
      if not has_no_ext_diff then
        table.insert(words, subcommand_index + 1, "--no-ext-diff")
      end
    end
  end

  return table.concat(words, " ")
end

rtk_rewrite = function(command, ctx)
  local config = ctx:config()
  if config and config.no_rtk then
    return nil
  end

  if rtk_available == nil then
    local id = n00n.fn.jobstart("rtk --version")
    local result = n00n.fn.jobwait(id, RTK_REWRITE_TIMEOUT_MS)
    if result and result.exit_code == 0 then
      rtk_available = true
      rtk_probe_error = nil
    elseif result and result.exit_code == 127 then
      rtk_available = false
    elseif result then
      rtk_available = false
      rtk_probe_error = "availability check failed with exit code " .. result.exit_code
    else
      n00n.fn.jobstop(id)
      rtk_available = false
      rtk_probe_error = "availability check timed out"
    end
  end

  if not rtk_available then
    if rtk_probe_error and rtk_enforcement_required(command) then
      return nil, RTK_REWRITE_REQUIRED .. ": " .. rtk_probe_error
    end
    return nil
  end

  local cmd = normalize_command(command:match("^%s*(.-)%s*$"))

  -- jq and yq must pass through unchanged (FR-018)
  local normalized = strip_leading_assignments(cmd)
  local first_word = normalized:match("^(%S+)")
  if not first_word then
    return nil
  end
  if
    (first_word == "jq" or first_word == "yq" or first_word:match("/jq$") or first_word:match("/yq$"))
    and not rtk_enforcement_required(command)
  then
    return nil
  end

  local executable = unquote(first_word):gsub("\\(.)", "%1"):gsub("['\"]", "")
  if RTK_SKIP_TOOLS[executable] or RTK_SKIP_TOOLS[executable:match("([^/]+)$")] then
    return nil
  end

  if git_subcommand(cmd) and git_uses_machine_format(cmd) then
    return nil
  end

  local id = n00n.fn.jobstart("rtk rewrite " .. shell_quote(cmd))
  local result = n00n.fn.jobwait(id, RTK_REWRITE_TIMEOUT_MS)
  if not result then
    n00n.fn.jobstop(id)
    if rtk_enforcement_required(command) then
      return nil, RTK_REWRITE_REQUIRED .. ": rewrite timed out"
    end
    return nil
  end

  if result.exit_code ~= 0 and result.exit_code ~= 3 then
    -- rtk's rewrite has no equivalent for this command. For a small set of
    -- read-only `git` subcommands we can still route through `rtk git`, which
    -- falls back to generic git filtering for unsupported subcommands.
    local git_sub = git_subcommand(cmd)
    if git_sub and RTK_GIT_FALLBACK[git_sub] then
      return "rtk " .. cmd
    end
    if not rtk_single_command(command) then
      local compound, compound_error = rtk_rewrite_compound(command, ctx)
      if compound or compound_error then
        return compound, compound_error
      end
    end
    if cmd == "ls" or cmd:match("^ls%s+") then
      return "rtk " .. cmd
    end
    if cmd == "find" or cmd:match("^find%s+") then
      if rtk_find_unsupported("rtk " .. cmd .. " ") then
        return nil, RTK_REWRITE_REQUIRED .. ": the command uses unsupported find flags"
      end
      return "rtk " .. cmd
    end
    if rtk_enforcement_required(command) then
      return nil, RTK_REWRITE_REQUIRED .. ": rtk rewrite rejected it", RTK_REWRITE_REASON_UNAVAILABLE
    end
    return nil
  end

  local rewritten = (result.stdout or ""):match("^%s*(.-)%s*$")
  if rewritten == "" or rewritten == cmd then
    if not rtk_single_command(command) then
      local compound, compound_error = rtk_rewrite_compound(command, ctx)
      if compound or compound_error then
        return compound, compound_error
      end
    end
    if cmd == "ls" or cmd:match("^ls%s+") then
      return "rtk " .. cmd
    end
    if cmd == "find" or cmd:match("^find%s+") then
      if rtk_find_unsupported("rtk " .. cmd .. " ") then
        return nil, RTK_REWRITE_REQUIRED .. ": the command uses unsupported find flags"
      end
      return "rtk " .. cmd
    end
    if rtk_enforcement_required(command) then
      return nil, RTK_REWRITE_REQUIRED .. ": rtk returned no rewrite", RTK_REWRITE_REASON_UNAVAILABLE
    end
    return nil
  end
  if rtk_find_unsupported(rewritten) then
    return nil, RTK_REWRITE_REQUIRED .. ": the command uses unsupported find flags"
  end
  rewritten = rewritten:gsub("%f[%w]rtk%s+jq%f[%W]", "jq"):gsub("%f[%w]rtk%s+yq%f[%W]", "yq")
  if rtk_enforcement_required(rewritten) then
    if not rtk_single_command(command) then
      local compound, compound_error = rtk_rewrite_compound(command, ctx)
      if compound or compound_error then
        return compound, compound_error
      end
    end
    return nil, RTK_REWRITE_REQUIRED .. ": rtk left a managed command unwrapped"
  end
  return rewritten
end

local DEFAULT_MAX_LINE_BYTES = 400
local LIVE_OUTPUT_FLUSH_LINES = 32
local LIVE_OUTPUT_FLUSH_MS = 1000

local function create_bash_view(command, ctx)
  local tol = ctx:tool_output_lines()
  local buf = n00n.ui.buf()
  local view = ToolView.new(buf, {
    max_lines = (tol and tol.bash) or 5,
    keep = "tail",
    max_line_bytes = DEFAULT_MAX_LINE_BYTES,
  })
  view:set_header(build_header_lines(command))
  buf:on("click", function()
    view:toggle()
  end)
  return buf, view
end

local cwd = n00n.uv.cwd() or "."

local COMPLEX_TYPES = {
  command_substitution = true,
  process_substitution = true,
  subshell = true,
  arithmetic_expansion = true,
}

local function is_complex(node)
  if COMPLEX_TYPES[node:type()] then
    return true
  end
  for child in node:iter_children() do
    if is_complex(child) then
      return true
    end
  end
  return false
end

local LEAF_COMMAND_TYPES = {
  command = true,
  redirected_statement = true,
  negated_command = true,
}

local function command_segment(node, source)
  local range = n00n.treesitter.get_range(node)
  local raw = source:sub(range[3] + 1, range[6])
  local text = raw:match("^%s*(.-)%s*$")
  if text == "" then
    return nil
  end
  local relative_start, relative_end = raw:find(text, 1, true)
  return {
    text = text,
    start_byte = range[3] + relative_start - 1,
    end_byte = range[3] + relative_end,
  }
end

local function collect_commands(node, source)
  local out = {}
  local kind = node:type()
  if LEAF_COMMAND_TYPES[kind] then
    local segment = command_segment(node, source)
    if segment then
      out[#out + 1] = segment
    end
  elseif kind == "pipeline" then
    for child in node:iter_children() do
      if child:named() then
        local segment = command_segment(child, source)
        if segment then
          out[#out + 1] = segment
        end
      end
    end
  else
    for child in node:iter_children() do
      if child:named() then
        local nested = collect_commands(child, source)
        for _, cmd in ipairs(nested) do
          out[#out + 1] = cmd
        end
      end
    end
  end
  return out
end

local function collect_guard_commands(node, source)
  if node:type() == "pipeline" then
    local commands = {}
    for child in node:iter_children() do
      if child:named() then
        commands[#commands + 1] = trim(n00n.treesitter.get_node_text(child, source))
      end
    end

    local guarded = {}
    for index = 1, #commands do
      guarded[#guarded + 1] = table.concat(commands, " | ", index)
    end
    return guarded
  end

  if LEAF_COMMAND_TYPES[node:type()] then
    return { trim(n00n.treesitter.get_node_text(node, source)) }
  end

  local guarded = {}
  for child in node:iter_children() do
    if child:named() then
      local nested = collect_guard_commands(child, source)
      for _, command in ipairs(nested) do
        guarded[#guarded + 1] = command
      end
    end
  end
  return guarded
end

local function normalized_executable(word)
  return unquote(word):gsub("\\(.)", "%1"):gsub("['\"]", ""):match("([^/]+)$")
end

local function rtk_manages_segment(segment, depth)
  depth = depth or 0
  if depth > 8 then
    return true
  end
  local normalized = strip_leading_assignments(trim(segment))
  local words = split_shell_words(normalized)
  local executable = words[1] and normalized_executable(words[1])
  if not executable or executable:find("$", 1, true) then
    return executable ~= nil
  end
  if RTK_MANAGED_COMMANDS[executable] then
    return true
  end
  if executable == "rtk" then
    return normalized_executable(words[2] or "") == "proxy"
      and rtk_manages_segment(table.concat(words, " ", 3), depth + 1)
  end
  if RTK_COMMAND_WRAPPERS[executable] then
    for index = 2, #words do
      local candidate = normalized_executable(words[index])
      if candidate and RTK_MANAGED_COMMANDS[candidate] then
        return true
      end
      if RTK_STRING_COMMAND_WRAPPERS[executable] then
        local nested = unquote(words[index])
        if nested ~= words[index] and rtk_manages_segment(nested, depth + 1) then
          return true
        end
      end
    end
  end
  return false
end

local function tree_contains_rtk_command(node, source)
  if LEAF_COMMAND_TYPES[node:type()] then
    local segment = n00n.treesitter.get_node_text(node, source)
    if rtk_manages_segment(segment) then
      return true
    end
  end
  for child in node:iter_children() do
    if child:named() and tree_contains_rtk_command(child, source) then
      return true
    end
  end
  return false
end

rtk_single_command = function(command)
  local parser = n00n.treesitter.get_parser(command, "bash")
  if not parser then
    return false
  end
  local root = parser:parse()[1]:root()
  return not root:has_error() and not is_complex(root) and #collect_commands(root, command) == 1
end

rtk_enforcement_required = function(command)
  local parser = n00n.treesitter.get_parser(command, "bash")
  if not parser then
    return rtk_manages_segment(command)
  end

  local root = parser:parse()[1]:root()
  if root:has_error() then
    return rtk_manages_segment(command)
  end
  return tree_contains_rtk_command(root, command)
end

rtk_rewrite_compound = function(command, ctx)
  local parser = n00n.treesitter.get_parser(command, "bash")
  if not parser then
    return nil
  end
  local root = parser:parse()[1]:root()
  if root:has_error() then
    return nil
  end

  local output = {}
  local cursor = 0
  local changed = false
  for _, segment in ipairs(collect_commands(root, command)) do
    if segment.start_byte == 0 and segment.end_byte == #command then
      return nil, RTK_REWRITE_REQUIRED .. ": complex command could not be safely segmented"
    end
    if segment.start_byte < cursor or command:sub(segment.start_byte + 1, segment.end_byte) ~= segment.text then
      return nil, RTK_REWRITE_REQUIRED .. ": could not safely locate a compound command segment"
    end
    local rewritten, rewrite_error, reason = rtk_rewrite(segment.text, ctx)
    if rewrite_error then
      if reason ~= RTK_REWRITE_REASON_UNAVAILABLE then
        return nil, rewrite_error
      end
      -- rtk has no rewrite for this segment; run it exactly as written
      -- rather than failing the whole compound command.
      rewritten = nil
    end
    output[#output + 1] = command:sub(cursor + 1, segment.start_byte)
    output[#output + 1] = rewritten or segment.text
    changed = changed or rewritten ~= nil
    cursor = segment.end_byte
  end
  output[#output + 1] = command:sub(cursor + 1)
  if changed then
    return table.concat(output)
  end
  return nil
end

local function broad_command_reason(command)
  local parser = n00n.treesitter.get_parser(command, "bash")
  if not parser then
    return broad_bash_command_reason(command)
  end

  local root = parser:parse()[1]:root()
  if root:has_error() or is_complex(root) then
    return broad_bash_command_reason(command)
  end

  local segments = collect_guard_commands(root, command)
  for _, segment in ipairs(segments) do
    local reason = broad_bash_command_reason(segment)
    if reason then
      return reason
    end
  end

  return nil
end

local description = [[Execute a bash command.
Commands run in ]] .. cwd .. [[ by default.

- Reserve for git, builds, tests, and system CLI operations. Do NOT use for file edits/writes.
- When rtk is installed, managed commands are rewritten through it or rejected; there is no per-call bypass.
- Use `workdir` instead of `cd`. Chain dependent commands with `&&`.
- Unbounded/broad commands (e.g. find without -maxdepth, rg without limits) require `justification`; the tool fails without it.
- Interactive commands fail immediately. Truncated beyond 500 lines or 16KB.]]
n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Reserve `run_shell` for system CLI. When `rtk` is installed, managed commands are rewritten through it or rejected with no per-call bypass. Do NOT use `run_shell` for file modifications.",
})

local opts = n00n.api.register_options(output_limits.extend({
  timeout_secs = {
    default = 120,
    min = 5,
    desc = "Kill the command after this many seconds. A call's `timeout` param overrides it.",
  },
}))

n00n.api.register_tool({
  name = "run_shell",
  aliases = { "bash" },
  kind = "execute",
  description = description,
  schema = {
    type = "object",
    properties = {
      command = { type = "string", description = "Bash command to execute", required = true },
      timeout = { type = "integer", description = "Timeout seconds (default 120)" },
      workdir = { type = "string", description = "Working directory (default: cwd)" },
      description = { type = "string", description = "Short description (3-5 words) of what the command does" },
      justification = {
        type = "string",
        description = "Required for unbounded commands. Explain scope and bounds.",
      },
    },
  },
  permission_scopes = function(input)
    local command = input.command
    if not command or command:match("^%s*$") then
      return nil
    end

    local parser = n00n.treesitter.get_parser(command, "bash")
    if not parser then
      return { scopes = { command }, force_prompt = true }
    end

    local root = parser:parse()[1]:root()
    if root:has_error() or is_complex(root) then
      return { scopes = { command }, force_prompt = true }
    end

    local collected = collect_commands(root, command)
    local segments = {}
    for _, segment in ipairs(collected) do
      segments[#segments + 1] = segment.text
    end
    if #segments == 0 then
      segments = { command }
    end
    if broad_command_reason(command) then
      return { scopes = segments, force_prompt = true }
    end
    return { scopes = segments, force_prompt = false }
  end,

  header = function(input)
    local command, workdir = parse_cd_hint(input)
    local s = input.description or command
    if workdir then
      s = s .. " in " .. relative_path(workdir)
    end
    if input.timeout then
      local buf = n00n.ui.buf()
      buf:line({ { s }, { " (" .. n00n.ui.humantime(input.timeout) .. " timeout)", "dim" } })
      return buf
    end
    return s
  end,

  restore = function(input, output, is_error, ctx)
    local command = input.command
    local buf, view = create_bash_view(command, ctx)
    local timeout_secs = output:match("^tool bash timed out after (%d+)s$")
    if timeout_secs then
      view:append({ { "Timed out after " .. timeout_secs .. "s", "dim" } })
    elseif is_error then
      local body, code = output:match("^(.-)\nExit code: (%d+)$")
      if body then
        view:append_text(body)
        view:append({ { "Exit code: " .. code, "dim" } })
      else
        view:append_text(output)
      end
    else
      if output == "Exit code: 0" or output == "" then
        view:clear()
        view:append({ { "No output", "dim" } })
      else
        view:append_text(output)
      end
    end
    view:finish()
    return buf
  end,

  handler = function(input, ctx)
    if not input.command then
      return { llm_output = "error: command is required", is_error = true }
    end

    local command, workdir = parse_cd_hint(input)
    local reason = broad_command_reason(command)
    if reason and (not input.justification or trim(input.justification) == "") then
      return { llm_output = BROAD_COMMAND_JUSTIFICATION_REQUIRED .. ": " .. reason, is_error = true }
    end
    local timeout_secs = input.timeout or opts.timeout_secs

    local max_lines, max_bytes = output_limits.resolve(opts, ctx)

    ctx:set_deadline(timeout_secs)

    command = sanitize_git_command(command)

    local rewritten, rewrite_error = rtk_rewrite(command, ctx)
    if rewrite_error then
      return { llm_output = rewrite_error, is_error = true }
    end
    if rewritten then
      command = rewritten
    end

    local buf, view = create_bash_view(command, ctx)
    ctx:live_buf(buf)

    local collector = output_collector.new()
    local has_output = false
    local finished = false
    local flush_scheduled = false
    local flush_fallback_warned = false
    local published_line_count = 0

    local function finish(exit_code)
      finished = true
      local output = output_collector.collected_output(collector, max_lines, max_bytes)

      local is_error = exit_code ~= 0
      local llm_output
      if exit_code == 0 then
        llm_output = output == "" and "Exit code: 0" or output
      else
        if output == "" then
          llm_output = "Exit code: " .. exit_code
        else
          llm_output = output .. "\nExit code: " .. exit_code
        end
      end

      if output == "" then
        view:clear()
        view:append({ { "No output", "dim" } })
      end

      if is_error then
        view:append({ { "Exit code: " .. exit_code, "dim" } })
      elseif has_output then
        view:flush()
      end
      view:finish()

      ctx:finish({ llm_output = llm_output, is_error = is_error, body = buf })
    end

    view:append({ { "Waiting for output...", "dim" } })

    local function flush_view()
      view:flush()
      published_line_count = collector.line_count
    end

    local function schedule_view_flush()
      if flush_scheduled then
        return
      end
      local scheduled, err = pcall(n00n.fn.defer, LIVE_OUTPUT_FLUSH_MS, function()
        flush_scheduled = false
        if not finished and collector.line_count > published_line_count then
          flush_view()
        end
      end)
      if scheduled then
        flush_scheduled = true
      else
        if not flush_fallback_warned then
          flush_fallback_warned = true
          n00n.log.warn("bash live output debounce unavailable: " .. tostring(err))
        end
        flush_view()
      end
    end

    local function append_output(line)
      if not has_output then
        has_output = true
        view:clear()
      end
      output_collector.append_line(collector, line, max_lines, max_bytes)
      view:append_buffered(line)
      if collector.line_count <= 2 or collector.line_count % LIVE_OUTPUT_FLUSH_LINES == 0 then
        flush_view()
      else
        schedule_view_flush()
      end
    end

    n00n.fn.jobstart(command, {
      cwd = workdir,
      env = {
        GIT_TERMINAL_PROMPT = "0",
        GIT_PAGER = "",
        GIT_EXEC_PATH = "",
      },
      on_stdout = function(_, line)
        append_output(line)
      end,
      on_stderr = function(_, line)
        append_output(line)
      end,
      on_exit = function(_, code)
        finish(code)
      end,
    })

    return nil
  end,
})
