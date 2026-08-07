local n00n_github = n00n.github
local ToolView = require("n00n.tool_view")

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use **github** for GitHub API operations: list_issues, create_issue, list_prs, get_repo, get_issue, get_pr.",
})

local function dispatch(input)
  if not input.command or input.command == "" then
    return { llm_output = "error: command is required", is_error = true }
  end
  local command = input.command:gsub("-", "_")
  local owner = input.owner
  local repo = input.repo

  if not owner or not repo then
    return { llm_output = "error: owner and repo are required", is_error = true }
  end

  if command == "list_issues" then
    local ok, result = pcall(n00n_github.list_issues, owner, repo, input.token)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
    end
    if #result == 0 then
      return { llm_output = "No issues found" }
    end
    local lines = {}
    for _, issue in ipairs(result) do
      table.insert(lines, string.format("#%d %s (%s)", issue.number, issue.title, issue.state))
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "create_issue" then
    if not input.title or not input.body then
      return { llm_output = "error: title and body required for create_issue", is_error = true }
    end
    local ok, result = pcall(n00n_github.create_issue, owner, repo, input.title, input.body, input.token)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
    end
    return { llm_output = string.format("Created issue #%d: %s\n%s", result.number, result.title, result.html_url) }
  end

  if command == "list_prs" then
    local ok, result = pcall(n00n_github.list_prs, owner, repo, input.token)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
    end
    if #result == 0 then
      return { llm_output = "No pull requests found" }
    end
    local lines = {}
    for _, pr in ipairs(result) do
      table.insert(lines, string.format("#%d %s (%s)", pr.number, pr.title, pr.state))
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "get_repo" then
    local ok, result = pcall(n00n_github.get_repo, owner, repo, input.token)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
    end
    local lines = {
      result.full_name,
      "  Description: " .. (result.description or "none"),
      "  Language: " .. (result.language or "none"),
      "  Stars: " .. result.stargazers_count,
      "  Forks: " .. result.forks_count,
      "  URL: " .. result.html_url,
    }
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "get_issue" then
    if not input.issue_number or type(input.issue_number) ~= "number" then
      return { llm_output = "error: issue_number is required and must be a number", is_error = true }
    end
    local ok, result = pcall(n00n_github.get_issue, owner, repo, input.issue_number, input.token)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
    end
    local lines = {
      string.format("#%d %s (%s)", result.number, result.title, result.state),
      "  Author: " .. result.user.login,
      "  URL: " .. result.html_url,
    }
    if result.body then
      table.insert(lines, "  Body: " .. result.body)
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  if command == "get_pr" then
    if not input.pr_number or type(input.pr_number) ~= "number" then
      return { llm_output = "error: pr_number is required and must be a number", is_error = true }
    end
    local ok, result = pcall(n00n_github.get_pr, owner, repo, input.pr_number, input.token)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
    end
    local lines = {
      string.format("#%d %s (%s)", result.number, result.title, result.state),
      "  Author: " .. result.user.login,
      "  Head: " .. result.head.ref_field,
      "  Base: " .. result.base.ref_field,
      "  URL: " .. result.html_url,
    }
    if result.body then
      table.insert(lines, "  Body: " .. result.body)
    end
    return { llm_output = table.concat(lines, "\n") }
  end

  return { llm_output = "error: unknown command: " .. tostring(command), is_error = true }
end

n00n.api.register_tool({
  name = "github",
  kind = "read",
  description = [[
Query GitHub repositories, issues, and pull requests using the REST API. Token sources: GITHUB_TOKEN env var, optional token parameter, or gh CLI fallback.
]],
  schema = {
    type = "object",
    properties = {
      command = {
        type = "string",
        enum = { "list_issues", "create_issue", "list_prs", "get_repo", "get_issue", "get_pr" },
        required = true,
      },
      owner = { type = "string" },
      repo = { type = "string" },
      title = { type = "string" },
      body = { type = "string" },
      issue_number = { type = "number" },
      pr_number = { type = "number" },
      token = { type = "string" },
    },
  },
  header = function(input)
    local label = input.command or ""
    if input.owner and input.repo then
      label = label .. " " .. input.owner .. "/" .. input.repo
    end
    return "github " .. label
  end,
  permission_scopes = function(input)
    local command = input.command or ""
    if
      command == "list_issues"
      or command == "list_prs"
      or command == "get_repo"
      or command == "get_issue"
      or command == "get_pr"
    then
      return { scopes = { "github.read" }, force_prompt = false }
    elseif command == "create_issue" then
      return { scopes = { "github.write" }, force_prompt = true }
    end
    return { scopes = { "github.read" }, force_prompt = false }
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
