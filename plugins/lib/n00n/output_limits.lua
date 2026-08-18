-- Shared per-tool output limit options, so the tools that support them
-- cannot drift apart.

local DEFAULT_MAX_OUTPUT_LINES = 500
local DEFAULT_MAX_OUTPUT_BYTES = 16 * 1024
local DEFAULT_MAX_LINE_BYTES = 400

local M = {}

M.DEFAULT_MAX_LINE_BYTES = DEFAULT_MAX_LINE_BYTES
M.specs = {
  max_output_lines = { type = "integer", desc = "Override `agent.max_output_lines` for this tool." },
  max_output_bytes = { type = "integer", desc = "Override `agent.max_output_bytes` for this tool." },
}

function M.extend(spec)
  for name, s in pairs(M.specs) do
    spec[name] = s
  end
  return spec
end

--- Returns max_lines, max_bytes: tool override when set, agent-wide otherwise.
function M.resolve(opts, ctx)
  return opts.max_output_lines or ctx:config("max_output_lines", DEFAULT_MAX_OUTPUT_LINES),
    opts.max_output_bytes or ctx:config("max_output_bytes", DEFAULT_MAX_OUTPUT_BYTES)
end

function M.resolve_capped(opts, ctx, default_max_bytes)
  local max_lines = opts.max_output_lines or ctx:config("max_output_lines", DEFAULT_MAX_OUTPUT_LINES)
  local configured_max_bytes = ctx:config("max_output_bytes", DEFAULT_MAX_OUTPUT_BYTES)
  local requested_max_bytes = opts.max_output_bytes or default_max_bytes
  return max_lines, math.min(requested_max_bytes, configured_max_bytes)
end

return M
