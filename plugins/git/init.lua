local n00n_git = n00n.git

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

  if command == "status" then
    local ok, result = pcall(n00n_git.status, path)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
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
    local ok, result = pcall(n00n_git.log, path, count)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
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
    local ok, result = pcall(n00n_git.diff, path, input.ref_a, input.ref_b)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
    end
    local lines = {}
    for _, file in ipairs(result.files) do
      table.insert(lines, string.format("%s: +%d -%d", file.path, file.additions, file.deletions))
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "branches" then
    local ok, result = pcall(n00n_git.branches, path)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
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
    local ok, result = pcall(n00n_git.blame, path, input.file)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
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
Native git operations using gix/gitoxide. Query local repositories without shelling out to git CLI.

Commands:
- status: Show working tree status (branch, staged/unstaged files).
- log: Show commit history (default 10 commits, use count for more).
- diff <ref_a> <ref_b>: Show diff between two references.
- branches: List all branches with HEAD SHAs.
- blame <file>: Show blame information for a file.

All operations return structured data and work without the git CLI installed.
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
  handler = function(input, _ctx)
    return dispatch(input)
  end,
})
