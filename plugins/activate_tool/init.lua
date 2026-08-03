local DESCRIPTION =
  [[Activate one deferred tool for the next turn. Use when the required capability is not currently available and you know its canonical name. Do not use when a loaded sibling already provides the capability; use that sibling directly. Search first with `search_tools` when the canonical name is unknown.]]

n00n.api.register_tool({
  name = "activate_tool",
  modes = { "default", "research", "build", "compact" },
  description = DESCRIPTION,

  schema = {
    type = "object",
    required = { "tool_name" },
    properties = {
      tool_name = {
        type = "string",
        description = "Canonical deferred tool name to add for the next turn. Use a name returned by `search_tools` or the documented inventory.",
        required = true,
      },
    },
  },

  header = function(input)
    local buf = n00n.ui.buf()
    buf:line({ { "activate: " .. (input.tool_name or "unknown"), "tool" } })
    return buf
  end,

  handler = function(input, ctx)
    local tool_name = input.tool_name
    if not tool_name then
      return { llm_output = "error: tool_name is required", is_error = true }
    end
    local llm_output = string.format("activated tool: %s (available next turn)", tool_name)
    return { llm_output = llm_output }
  end,
})
