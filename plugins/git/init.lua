local git_bin = (n00n.uv.os_getenv and n00n.uv.os_getenv("N00N_GIT_BIN")) or "n00n-git"
local ToolView = require("n00n.tool_view")

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use **git** for local repository operations: status, log, diff, branches, blame, add, commit, checkout.",
})

local function dispatch(input)
  if not input.command or input.command == "" then
    return { llm_output = "error: command is required", is_error = true }
  end
  local command = input.command:gsub("-", "_")
  local path = input.path or "."

  local function binary_missing_message()
    return "n00n-git binary not found. Set N00N_GIT_BIN to an absolute path or install n00n-git on PATH."
  end

  local function run_git_subcommand(args)
    if git_bin:sub(1, 1) ~= "/" and n00n.fn.executable(git_bin) == 0 then
      return nil, binary_missing_message()
    end

    local cmd = { git_bin, command, path }
    for _, arg in ipairs(args) do
      table.insert(cmd, arg)
    end

    local job_id, job_err = n00n.fn.jobstart(cmd)
    if not job_id then
      return nil, "failed to spawn n00n-git: " .. tostring(job_err or "unknown error")
    end

    local result = n00n.fn.jobwait(job_id, 30000)
    if not result then
      n00n.fn.jobstop(job_id)
      return nil, "n00n-git timed out"
    end

    if result.exit_code ~= 0 then
      return nil, "n00n-git exited with code " .. result.exit_code .. ": " .. (result.stderr or "")
    end

    local ok, data = pcall(n00n.json.decode, result.stdout)
    if not ok then
      return nil, "failed to parse n00n-git JSON output: " .. tostring(data)
    end

    if data.error then
      return nil, data.error
    end

    return data, nil
  end

  if command == "status" then
    local result, err = run_git_subcommand({})
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    local lines = {}
    if result.branch then
      table.insert(lines, "On branch " .. result.branch)
    end
    if #result.files == 0 then
      table.insert(lines, "Working tree clean")
    else
      for _, file in ipairs(result.files) do
        local staged_label = file.staged and "staged" or "unstaged"
        table.insert(lines, string.format("  %s: %s (%s)", file.status, file.path, staged_label))
      end
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "log" then
    local count = input.count or 10
    local result, err = run_git_subcommand({ "--count", tostring(count) })
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    local lines = {}
    for _, commit in ipairs(result) do
      table.insert(lines, string.format("%s %s (%s)", commit.id:sub(1, 8), commit.message:sub(1, 60), commit.author))
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "diff" then
    if not input.ref_a or not input.ref_b then
      return { llm_output = "error: ref_a and ref_b required for diff", is_error = true }
    end
    local result, err = run_git_subcommand({ input.ref_a, input.ref_b })
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    local lines = {}
    for _, file in ipairs(result.files) do
      table.insert(lines, string.format("%s: +%d -%d", file.path, file.additions, file.deletions))
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "branches" then
    local result, err = run_git_subcommand({})
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    local lines = {}
    for _, branch in ipairs(result) do
      local marker = branch.is_current and "*" or " "
      table.insert(lines, string.format("%s %s (%s)", marker, branch.name, branch.head:sub(1, 8)))
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "blame" then
    if not input.file then
      return { llm_output = "error: file required for blame", is_error = true }
    end
    local result, err = run_git_subcommand({ input.file })
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    local lines = {}
    for _, line in ipairs(result.lines) do
      table.insert(lines, string.format("%s %s (%s)", line.commit_id:sub(1, 8), line.content, line.author))
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "add" then
    if not input.files or #input.files == 0 then
      return { llm_output = "error: files required for add", is_error = true }
    end
    local _, err = run_git_subcommand(input.files)
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = "staged " .. tostring(#input.files) .. " file(s)" }
  end

  if command == "commit" then
    if not input.message or input.message == "" then
      return { llm_output = "error: message required for commit", is_error = true }
    end
    local result, err = run_git_subcommand({ "--message", input.message })
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = "committed " .. result.commit_id:sub(1, 8) }
  end

  if command == "checkout" then
    if not input.target or input.target == "" then
      return { llm_output = "error: target required for checkout", is_error = true }
    end
    local _, err = run_git_subcommand({ input.target })
    if err then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return { llm_output = "checked out " .. tostring(input.target) }
  end

  return { llm_output = "error: unknown command: " .. tostring(command), is_error = true }
end

n00n.api.register_tool({
  name = "git",
  kind = "read",
  description = [[
Query and modify local git repositories (status, log, diff, branches, blame, add, commit, checkout) by spawning the n00n-git binary.
Set N00N_GIT_BIN to override the binary path.
]],
  schema = {
    type = "object",
    properties = {
      command = {
        type = "string",
        enum = { "status", "log", "diff", "branches", "blame", "add", "commit", "checkout" },
        required = true,
      },
      path = { type = "string" },
      count = { type = "integer" },
      ref_a = { type = "string" },
      ref_b = { type = "string" },
      file = { type = "string" },
      files = {
        type = "array",
        items = { type = "string" },
      },
      message = { type = "string" },
      target = { type = "string" },
    },
  },
  header = function(input)
    local label = input.command or ""
    if input.command == "diff" and input.ref_a and input.ref_b then
      label = label .. " " .. input.ref_a .. " -> " .. input.ref_b
    elseif input.command == "blame" and input.file then
      label = label .. " " .. input.file
    elseif input.command == "checkout" and input.target then
      label = label .. " " .. input.target
    end
    local header = "git " .. label
    if input.path and input.path ~= "." then
      header = header .. " in " .. input.path
    end
    return header
  end,
  permission_scopes = function(input)
    local cmd = input.command or ""
    if cmd == "status" or cmd == "log" or cmd == "diff" or cmd == "branches" or cmd == "blame" then
      return { scopes = { "git.read" }, force_prompt = false }
    end
    return { scopes = { "git.write" }, force_prompt = true }
  end,
  restore = function(_input, output, _is_error, ctx)
    local tol = ctx:tool_output_lines()
    return ToolView.restore(output, {
      max_lines = (tol and tol.other) or 20,
      keep = "head",
    })
  end,
  handler = function(input, _ctx)
    return dispatch(input)
  end,
})
