local shorten_path = require("n00n.shorten_path")
local ToolView = require("n00n.tool_view")

local DEFAULT_PREVIEW_LINES = 5
local DEFAULT_MAX_EXPAND_LINES = 2000

local ExploreResult = {}
local Card = {}
Card.__index = Card

function Card:update(output)
  self.view:replace_text(output or "")
end

local function view_opts(ctx, opts)
  local output_lines = ctx and ctx:tool_output_lines()
  return {
    max_lines = (opts and opts.max_lines) or (output_lines and output_lines.explore) or DEFAULT_PREVIEW_LINES,
    max_expand_lines = (opts and opts.max_expand_lines) or DEFAULT_MAX_EXPAND_LINES,
    keep = "head",
  }
end

local function new_card(opts)
  local buf = n00n.ui.buf()
  local view = ToolView.new(buf, opts)
  local card = setmetatable({ buf = buf, view = view }, Card)
  buf:on("click", function()
    view:toggle()
  end)
  return card
end

function ExploreResult.new(opts)
  return new_card(view_opts(nil, opts))
end

function ExploreResult.live(ctx, opts)
  local card = new_card(view_opts(ctx, opts))
  local _, err = ctx:live_buf(card.buf)
  if err then
    return nil, err
  end
  return card, nil
end

function ExploreResult.header(label, project)
  local buf = n00n.ui.buf()
  local spans = { { label or "", "tool" } }
  if project then
    spans[#spans + 1] = { " in ", "dim" }
    spans[#spans + 1] = { shorten_path(project), "path" }
  end
  buf:line(spans)
  return buf
end

function ExploreResult.restore(output, ctx, opts)
  local card = new_card(view_opts(ctx, opts))
  card:update(output)
  return card.buf
end

return ExploreResult
