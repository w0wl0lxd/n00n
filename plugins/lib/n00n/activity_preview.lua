local ToolView = require("n00n.tool_view")

local ActivityPreview = {}
ActivityPreview.__index = ActivityPreview

local MAX_ROWS = 5
local MAX_LINE_BYTES = 500
local BODY_INDENT_COLS = 4

local function status_style(status)
  if status == "error" then
    return "error"
  end
  if status == "running" then
    return "bold"
  end
  return "dim"
end

function ActivityPreview.new(ctx, description, opts)
  local width = math.max(n00n.ui.terminal_size().cols - BODY_INDENT_COLS, 1)
  local view = ToolView.new(n00n.ui.buf(), {
    max_lines = MAX_ROWS,
    max_expand_lines = MAX_ROWS,
    max_line_bytes = MAX_LINE_BYTES,
    max_width = width,
    keep = "tail",
    hide_collapsed = true,
  })
  local self = setmetatable({
    description = description,
    started_at = os.time(),
    view = view,
    rows = {},
    row_index = {},
    activity_ids = {},
    turns = {},
    prompt_serial = 0,
    session_rows = opts and opts.session_rows or false,
  }, ActivityPreview)
  view.buf:on("click", function()
    view:toggle()
  end)
  self:render()
  local _, live_err = ctx:live_buf(view.buf)
  if live_err then
    return nil, live_err
  end
  return self, nil
end

function ActivityPreview:render()
  local elapsed = math.max(os.time() - self.started_at, 0)
  self.view:set_header({ { { self.description .. " · " .. n00n.ui.humantime(elapsed), "bold" } } })
  self.view:clear()
  for _, row in ipairs(self.rows) do
    local detail = row.message or row.status
    self.view:append({ { row.label .. " - " .. detail, status_style(row.status) } })
  end
end

function ActivityPreview:set_row(key, label, message, status)
  local row = self.row_index[key]
  if row then
    row.label = label
    row.message = message
    row.status = status
  else
    row = { key = key, label = label, message = message, status = status }
    self.rows[#self.rows + 1] = row
    self.row_index[key] = row
    if #self.rows > MAX_ROWS then
      local removed = table.remove(self.rows, 1)
      self.row_index[removed.key] = nil
    end
  end
  self:render()
end

function ActivityPreview:update(progress, label, session_key)
  local previous_ids = self.activity_ids[session_key] or {}
  local current_ids = {}
  for _, activity in ipairs(progress.activities or {}) do
    local activity_key = session_key .. "/" .. (activity.id or activity.tool)
    current_ids[activity_key] = true
    if self.row_index[activity_key] or not previous_ids[activity_key] then
      local row_label = activity.message and (label .. "/" .. activity.tool) or activity.tool
      self:set_row(activity_key, row_label, activity.message, activity.status or "running")
    end
  end
  self.activity_ids[session_key] = current_ids
end

function ActivityPreview:prompt(sess, message, label)
  self.prompt_serial = self.prompt_serial + 1
  local prompt_key = label .. "#" .. self.prompt_serial
  local session_key = tostring(sess)
  local baseline_turn = self.turns[session_key] or 0
  if self.session_rows then
    self:set_row(prompt_key, label, nil, "running")
  end

  local result, prompt_err
  local function run_prompt()
    result, prompt_err = sess:prompt(message)
  end
  local function poll_progress()
    while true do
      local progress, progress_err = sess:get_progress()
      if not progress then
        error(progress_err or "session progress unavailable", 0)
      end
      if (progress.turn_id or 0) > baseline_turn then
        self.turns[session_key] = progress.turn_id
        self:update(progress, label, session_key)
        if progress.done then
          return
        end
      end
    end
  end

  local gathered = n00n.async.gather({ run_prompt, poll_progress })
  if not gathered[1].ok then
    prompt_err = gathered[1].err
  elseif not gathered[2].ok and not prompt_err then
    prompt_err = gathered[2].err
  end
  if self.session_rows then
    self:set_row(prompt_key, label, nil, prompt_err and "error" or "success")
  end
  return result, prompt_err
end

return ActivityPreview
