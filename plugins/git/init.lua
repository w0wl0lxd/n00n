local git_bin = os.getenv("N00N_GIT_BIN") or "n00n-git"

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use **git** for local repository operations: status, log, diff, branches, blame.",
})

local function dispatch(input)
  if not input.command or input.command == "" then
    return { llm_output = "error: command is required", is_error = true }
  end
  local command = input.command:gsub("-", "_")
  local path = input.path or "."

  local function run_git_subcommand(args)
    if git_bin:sub(1, 1) ~= "/" and n00n.fn.executable(git_bin) == 0 then
      return nil, "n00n-git binary not found in PATH. Set N00N_GIT_BIN to absolute path or install n00n-git."
    end

    local cmd = { git_bin, command, path }
    for _, arg in ipairs(args) do
      table.insert(cmd, arg)
    end

    local job_id = n00n.fn.jobstart(cmd)
    if job_id <= 0 then
      return nil, "failed to spawn n00n-git process"
    end

    local exit_codes = { n00n.fn.jobwait({ job_id }) }
    local exit_code = exit_codes[1]

    local result = n00n.fn.jobstop({ job_id })
    if not result or not result.stdout then
      return nil, "failed to read n00n-git output"
    end

    if exit_code ~= 0 then
      local stderr = result.stderr or ""
      return nil, "n00n-git exited with code " .. exit_code .. ": " .. stderr
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
        local status = file.staged and "staged" or "unstaged"
        table.insert(lines, string.format("  %s: %s (%s)", file.status, file.path, status))
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

  return { llm_output = "error: unknown command: " .. tostring(command), is_error = true }
end

n00n.api.register_tool({
  name = "git",
  kind = "read",
  description = [[
Native git operations using gix/gitoxide. Query local repositories by spawning the n00n-git binary.

Commands:
- status: Show working tree status (branch, staged/unstaged files).
- log: Show commit history (default 10 commits, use count for more).
- diff <ref_a> <ref_b>: Show diff between two references.
- branches: List all branches with HEAD SHAs.
- blame <file>: Show blame information for a file.

The n00n-git binary is spawned via jobstart/jobwait. Set N00N_GIT_BIN environment variable to override the binary path.
Use this for repository-aware queries, understanding history, and tracking changes.
]],
  schema = {
    type = "object",
    properties = {
      command = {
        type = "string",
        enum = { "status", "log", "diff", "branches", "blame" },
        required = true,
      },
      path = { type = "string" },
      count = { type = "integer" },
      ref_a = { type = "string" },
      ref_b = { type = "string" },
      file = { type = "string" },
    },
  },
  header = function(input)
    local label = input.command or ""
    if input.command == "diff" and input.ref_a and input.ref_b then
      label = label .. " " .. input.ref_a .. " -> " .. input.ref_b
    elseif input.command == "blame" and input.file then
      label = label .. " " .. input.file
    end
    return "git " .. label
  end,
  permission_scopes = function(input)
    local command = input.command or ""
    if command == "status" or command == "log" or command == "diff" or command == "branches" or command == "blame" then
      return { scopes = { "git.read" }, force_prompt = false }
    end
    return { scopes = { "git.write" }, force_prompt = true }
  end,
  handler = function(input, _ctx)
    return dispatch(input)
  end,
})
