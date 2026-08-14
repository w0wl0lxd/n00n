-- Parsing and policy helpers for the bash tool's unbounded-command guardrail
-- and git-subcommand-aware rtk rewriting. Pure string/table logic, no n00n.*
-- runtime calls, so it is unit-testable standalone.

local M = {}

local function trim(s)
  return s:match("^%s*(.-)%s*$")
end

function M.has_option(command, option)
  if command == option then
    return true
  end

  if command:sub(1, #option + 1) == option .. " " then
    return true
  end

  if command:sub(1, #option + 1) == option .. "=" then
    return true
  end

  local padded = " " .. command .. " "
  if padded:find(" " .. option .. " ", 1, true) then
    return true
  end

  if padded:find(" " .. option .. "=", 1, true) then
    return true
  end

  return false
end
local has_option = M.has_option

function M.has_output_cap(command)
  local normalized = trim(command):lower()
  if normalized == "" then
    return false
  end

  for _, executable in ipairs({ "head", "tail" }) do
    if normalized:find("|%s*" .. executable .. "%s") or normalized:find("|%s*" .. executable .. "$") then
      return true
    end
  end
  return false
end

-- `git log`/`reflog`/`rev-list` accept a count bound as `-n <N>`, `-n<N>`
-- (attached), `--max-count=<N>`, or the bare `-<N>` shorthand.
function M.has_git_history_bound(command)
  if has_option(command, "-n") or has_option(command, "--max-count") then
    return true
  end
  for word in command:gmatch("%S+") do
    if word:match("^%-n%d+$") or word:match("^%-%d+$") then
      return true
    end
  end
  return false
end

function M.shell_word_end(command)
  local quote
  local index = 1
  while index <= #command do
    local char = command:sub(index, index)
    if quote then
      if char == quote then
        quote = nil
      elseif char == "\\" and quote == '"' then
        index = index + 1
      end
    elseif char == "'" or char == '"' then
      quote = char
    elseif char == "\\" then
      index = index + 1
    elseif char:match("%s") then
      return index - 1
    end
    index = index + 1
  end
  return #command
end
local shell_word_end = M.shell_word_end

function M.split_shell_words(command)
  local words = {}
  local index = 1
  while index <= #command do
    while index <= #command and command:sub(index, index):match("%s") do
      index = index + 1
    end
    if index > #command then
      break
    end
    local tail = command:sub(index)
    local word_end = shell_word_end(tail)
    words[#words + 1] = tail:sub(1, word_end)
    index = index + word_end
  end
  return words
end
local split_shell_words = M.split_shell_words

-- Global git options that consume the following word as their value.
-- Long options can also use `=value`, which is handled inline.
local GIT_ARG_OPTIONS = {
  ["-C"] = true,
  ["-c"] = true,
  ["--work-tree"] = true,
  ["--git-dir"] = true,
  ["--namespace"] = true,
  ["--super-prefix"] = true,
  ["--exec-path"] = true,
  ["--config-env"] = true,
  ["--blob"] = true,
}

-- Finds the git subcommand index in `words`, skipping global options like
-- `-c core.fsmonitor=false` or `--no-optional-locks`.
function M.git_subcommand_index(words, start)
  local skip_next = false
  for i = start, #words do
    if skip_next then
      skip_next = false
    elseif words[i]:sub(1, 1) ~= "-" then
      return i
    elseif not words[i]:find("=", 1, true) and GIT_ARG_OPTIONS[words[i]] then
      skip_next = true
    end
  end
  return nil
end
local git_subcommand_index = M.git_subcommand_index

-- Returns the git subcommand of `cmd` (e.g. "config"), skipping global
-- options, or nil if `cmd` is not a git invocation.
function M.git_subcommand(cmd)
  if not cmd:match("^git%s") then
    return nil
  end
  local words = split_shell_words(cmd)
  local index = git_subcommand_index(words, 2)
  return index and words[index] or nil
end

local GIT_MACHINE_FORMAT_FLAGS = {
  ["--porcelain"] = true,
  ["-z"] = true,
  ["--null"] = true,
}
local GIT_MACHINE_FORMAT_PREFIXES = { "--porcelain=", "--format", "--json" }

-- rtk's `git` wrapper reformats output for compactness and can silently drop
-- machine-readable request flags (e.g. `--porcelain`). Never rewrite those.
function M.git_uses_machine_format(cmd)
  for _, word in ipairs(split_shell_words(cmd)) do
    if GIT_MACHINE_FORMAT_FLAGS[word] then
      return true
    end
    for _, prefix in ipairs(GIT_MACHINE_FORMAT_PREFIXES) do
      if word:sub(1, #prefix) == prefix then
        return true
      end
    end
  end
  return false
end

function M.strip_leading_assignments(command)
  local remaining = trim(command)
  while remaining ~= "" do
    local word_end = shell_word_end(remaining)
    local word = remaining:sub(1, word_end)
    if not word:match("^[_%a][_%w]*=") then
      return remaining
    end
    remaining = trim(remaining:sub(word_end + 1))
  end
  return remaining
end
local strip_leading_assignments = M.strip_leading_assignments

function M.broad_bash_command_reason(command)
  local executable_command = strip_leading_assignments(command)
  local normalized = executable_command:lower()
  if normalized == "" then
    return nil
  end

  local context = normalized
  local has_output_cap = M.has_output_cap

  local cmd = normalized:match("^(%S+)")
  if not cmd then
    return nil
  end

  if cmd == "find" and not has_option(normalized, "-maxdepth") and not has_option(normalized, "--maxdepth") then
    return "find without a max depth bound (use -maxdepth <N>)"
  end

  if cmd == "locate" and not has_output_cap(context) then
    if has_option(normalized, "-l") or has_option(normalized, "--limit") then
      return nil
    end
    return "locate without output limit (use -l/--limit, or pipe through head/tail)"
  end

  if cmd == "journalctl" and not has_output_cap(context) then
    if has_option(normalized, "-n") or has_option(normalized, "--lines") then
      return nil
    end
    return "journalctl without tail line bound (use -n/--lines, or pipe through head/tail)"
  end

  -- `-m`/`--max-count` cap matches *per file*, not the overall result size,
  -- so they cannot satisfy this bound on their own; `--max-depth` genuinely
  -- limits traversal scope, like `find -maxdepth`.
  if cmd == "rg" and not has_output_cap(context) then
    if has_option(normalized, "--max-depth") then
      return nil
    end
    return "search with unbounded result size (use rg --max-depth, or pipe through head/tail)"
  end

  if cmd == "grep" and not has_output_cap(context) then
    return "search with unbounded result size (pipe through head/tail)"
  end

  if
    cmd == "ls"
    and (has_option(normalized, "--recursive") or has_option(executable_command, "-R"))
    and not has_output_cap(context)
  then
    return "recursive ls without output cap (pipe through head/tail)"
  end

  if cmd == "du" and not has_output_cap(context) then
    if
      not has_option(normalized, "-d")
      and not has_option(normalized, "--max-depth")
      and not has_option(normalized, "-s")
      and not has_option(normalized, "--summarize")
    then
      return "du without depth/summarize bound (use -d/--max-depth or -s/--summarize, or pipe through head/tail)"
    end
  end

  if cmd == "tree" and not has_output_cap(context) then
    if not has_option(executable_command, "-L") and not has_option(normalized, "--max-depth") then
      return "tree without depth bound (use -L, or pipe through head/tail)"
    end
  end

  if cmd == "git" then
    local words = split_shell_words(executable_command)
    local subcommand_index = git_subcommand_index(words, 2)
    local subcommand = subcommand_index and words[subcommand_index]:lower()
    if subcommand and not has_output_cap(context) then
      if subcommand == "log" or subcommand == "reflog" or subcommand == "rev-list" then
        if M.has_git_history_bound(normalized) then
          return nil
        end
        return subcommand .. " history without a max count (use -n<N>, --max-count=<N>, or -<N>)"
      end

      if subcommand == "grep" then
        -- Same per-file caveat as rg/grep above: git grep has no true
        -- result-size bound, only piping through head/tail is safe.
        return "git grep without result limit (pipe through head/tail)"
      end
    end
  end

  return nil
end

return M
