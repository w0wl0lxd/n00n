local ToolView = require("n00n.tool_view")

local DEFAULT_PREVIEW_LINES = 5
local MAX_PANEL_HEIGHT = 10

local items = {}
local buf = nil
local win = nil
local seen_first = false
local running = {}
local running_order = {}
local activity_expanded = false
local focused_session = nil
local render_panel

local STATUS_MARKERS = {
  completed = { "[✓]", "todo_completed" },
  in_progress = { "[•]", "todo_in_progress" },
  pending = { "[ ]", "todo_pending" },
  cancelled = { "[x]", "todo_cancelled" },
}

local DESCRIPTION =
  [[Create or update a structured todo list to track tasks. Use after EACH completed step. Send complete list each time (replace-all semantics). Use ONLY for multi-step work (3+ steps). Skip for trivial tasks.]]

local function count_done()
  local n = 0
  for _, item in ipairs(items) do
    if item.status == "completed" then
      n = n + 1
    end
  end
  return n
end

local function compact_text(value)
  return (value or ""):gsub("%s+", " "):match("^%s*(.-)%s*$")
end

local function current_todo()
  for _, item in ipairs(items) do
    if item.status == "in_progress" then
      return compact_text(item.content)
    end
  end
  return nil
end

local function session_is_focused(session_id)
  return focused_session == nil or session_id == nil or session_id == focused_session
end

local function running_count()
  local count = 0
  for _, activity in pairs(running) do
    if activity.tool ~= "todo_write" and session_is_focused(activity.session_id) then
      count = count + 1
    end
  end
  return count
end

local function current_activity()
  for i = #running_order, 1, -1 do
    local activity = running[running_order[i]]
    if activity and activity.tool ~= "todo_write" and session_is_focused(activity.session_id) then
      local label = compact_text(activity.summary ~= "" and activity.summary or activity.tool)
      if activity.subagent and activity.subagent ~= "" then
        label = activity.subagent .. ": " .. label
      end
      return label
    end
  end
end

local function activity_key(data)
  return (data.session_id or "") .. "\0" .. data.id
end

local function prune_running_order()
  local compact = {}
  for _, id in ipairs(running_order) do
    if running[id] then
      compact[#compact + 1] = id
    end
  end
  running_order = compact
end

local function update_hint()
  if #items == 0 then
    n00n.ui.set_status_hint(nil)
    return
  end
  local spans = {
    { string.format(" %d/%d ", count_done(), #items), "foreground" },
  }
  local active_todo = current_todo()
  if active_todo then
    local max_width = math.max(n00n.ui.terminal_size().cols - 24, 8)
    local truncated = n00n.ui.truncate_text(active_todo, max_width)
    spans[#spans + 1] = {
      " • " .. truncated.head .. (truncated.tail ~= "" and "…" or "") .. " ",
      "todo_in_progress",
    }
  end
  spans[#spans + 1] = { "Ctrl+T", "keybind_key" }
  spans[#spans + 1] = { " ", "" }
  n00n.ui.set_status_hint(spans)
end

local function ensure_win(visible)
  if buf and win and win:is_open() then
    return
  end
  buf = n00n.ui.buf()
  buf:on("click", function(ev)
    if current_activity() and ev and ev.row == 1 then
      activity_expanded = not activity_expanded
      render_panel(true)
    end
  end)
  win = n00n.ui.open_win(buf, {
    split = "panel",
    height = 4,
    order = 10,
    title = " Todos ",
    border = "rounded",
    focus = false,
    visible = visible,
    footer = {
      { "click Running", "details" },
      { "Ctrl+T", "hide" },
    },
  })
end

local function build_lines(todo_items, include_activity)
  todo_items = todo_items or items
  local lines = {}
  local activity = include_activity ~= false and current_activity() or nil
  if activity then
    local width = (win and win.width or n00n.ui.terminal_size().cols) - 16
    local truncated = n00n.ui.truncate_text(activity, math.max(width, 8))
    local count = running_count()
    local suffix = count > 1 and string.format("  +%d more", count - 1) or ""
    lines[#lines + 1] = {
      { activity_expanded and "[•] Running ▾  " or "[•] Running ▸  ", "todo_in_progress" },
      { truncated.head .. (truncated.tail ~= "" and "…" or "") .. suffix, "bold" },
    }
    if activity_expanded then
      for _, id in ipairs(running_order) do
        local detail = running[id]
        if detail and detail.tool ~= "todo_write" and session_is_focused(detail.session_id) then
          local summary = compact_text(detail.summary ~= "" and detail.summary or detail.tool)
          local owner = detail.subagent and detail.subagent ~= "" and (detail.subagent .. " · ") or ""
          lines[#lines + 1] = {
            { "    " .. owner, "dim" },
            { detail.tool, "tool" },
            { "  " .. summary, "foreground" },
          }
        end
      end
    end
  end
  for _, item in ipairs(todo_items) do
    local marker = STATUS_MARKERS[item.status] or STATUS_MARKERS.pending
    lines[#lines + 1] = {
      { marker[1] .. " " .. item.content, marker[2] },
    }
  end
  return lines
end

render_panel = function(visible)
  ensure_win(visible)
  local lines = build_lines()
  buf:set_lines(lines)
  win:set_config({ height = math.min(#lines + 2, MAX_PANEL_HEIGHT) })
  if win:is_visible() then
    n00n.ui.set_status_hint(nil)
  else
    update_hint()
  end
end

n00n.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use todo_write to plan and track multi-step tasks (must be 3+ steps). Update after EACH step, not only all at once.",
})

n00n.api.register_tool({
  name = "todo_write",
  description = DESCRIPTION,
  audiences = { "main", "research_sub", "general_sub" },
  schema = {
    type = "object",
    required = { "todos" },
    properties = {
      todos = {
        type = "array",
        description = "The updated todo list",
        items = {
          type = "object",
          required = { "content", "status" },
          properties = {
            content = { type = "string", description = "Task description" },
            status = {
              type = "string",
              enum = { "pending", "in_progress", "completed", "cancelled" },
              description = "One of: pending, in_progress, completed, cancelled.",
            },
            priority = {
              type = "string",
              enum = { "high", "medium", "low" },
            },
          },
        },
      },
    },
  },

  header = function(input)
    return string.format("%d todos", #(input.todos or {}))
  end,

  restore = function(input)
    local restored_items = input.todos or {}
    if #restored_items == 0 then
      return nil
    end
    return ToolView.restore_lines(build_lines(restored_items, false), {
      max_lines = DEFAULT_PREVIEW_LINES,
      keep = "head",
    })
  end,

  handler = function(input, ctx)
    local owner, owner_err = ctx:state_owner("root")
    if owner_err then
      error(owner_err)
    end
    local next_items = input.todos or {}
    local _, state_err
    if #next_items == 0 then
      _, state_err = ctx:state_remove("root")
    else
      _, state_err = ctx:state_replace("root", { todos = next_items })
    end
    if state_err then
      error(state_err)
    end
    local is_focused = focused_session == nil or owner == focused_session
    if is_focused then
      focused_session = owner
      items = next_items
    end
    if #next_items == 0 then
      if is_focused then
        if win and win:is_open() then
          win:hide()
        end
        n00n.ui.set_status_hint(nil)
      end
      return "Todos cleared"
    end
    if is_focused then
      local first = not seen_first
      seen_first = true
      render_panel(first)
    end
    return {
      llm_output = "",
      body = ToolView.restore_lines(build_lines(next_items, false), {
        max_lines = DEFAULT_PREVIEW_LINES,
        keep = "head",
      }),
    }
  end,
})

local function toggle()
  if #items == 0 then
    n00n.ui.flash("No todos yet")
    return
  end
  if not win then
    render_panel(true)
    return
  end
  if win:is_visible() then
    win:hide()
    update_hint()
  elseif win:is_open() then
    win:show()
    n00n.ui.set_status_hint(nil)
  else
    render_panel(true)
  end
end

n00n.keymap.set("n", "<C-t>", toggle, { desc = "Toggle todo panel" })

local function refresh_activity()
  if #items == 0 then
    return
  end
  if win and win:is_open() and win:is_visible() then
    render_panel(true)
  else
    update_hint()
  end
end

n00n.api.create_autocmd("ToolStart", {
  callback = function(ev)
    local data = ev.data or {}
    if not data.id or data.tool == "todo_write" then
      return
    end
    local key = activity_key(data)
    if running[key] then
      running[key] = nil
      prune_running_order()
    end
    running[key] = {
      tool = data.tool or "tool",
      summary = data.summary or "",
      subagent = data.subagent,
      session_id = data.session_id,
    }
    running_order[#running_order + 1] = key
    if session_is_focused(data.session_id) then
      refresh_activity()
    end
  end,
})

n00n.api.create_autocmd("ToolDone", {
  callback = function(ev)
    local data = ev.data or {}
    if data.id then
      running[activity_key(data)] = nil
      prune_running_order()
      if running_count() == 0 then
        activity_expanded = false
      end
      if session_is_focused(data.session_id) then
        refresh_activity()
      end
    end
  end,
})
local function focused_items(data)
  local snapshot = data.state_snapshot
  local plugins = type(snapshot) == "table" and snapshot.plugins or nil
  local plugin = type(plugins) == "table" and plugins.todo_write or nil
  local root = type(plugin) == "table" and plugin.root or nil
  local payload = type(root) == "table" and root.payload or nil
  return type(payload) == "table" and type(payload.todos) == "table" and payload.todos or {}
end

n00n.api.create_autocmd("SessionFocus", {
  callback = function(ev)
    local data = ev.data or {}
    focused_session = data.session_id
    items = focused_items(data)
    seen_first = #items > 0
    activity_expanded = false
    if #items == 0 then
      if win and win:is_open() then
        win:hide()
      end
      n00n.ui.set_status_hint(nil)
    elseif win and win:is_open() and win:is_visible() then
      render_panel(true)
    else
      update_hint()
    end
  end,
})

local function clear_activity(ev)
  local data = ev and ev.data or {}
  for id, activity in pairs(running) do
    if data.session_id == nil or activity.session_id == data.session_id then
      running[id] = nil
    end
  end
  prune_running_order()
  if session_is_focused(data.session_id) then
    activity_expanded = false
    refresh_activity()
  end
end

local function clear_todos(ev)
  local data = ev and ev.data or {}
  if not session_is_focused(data.session_id) then
    clear_activity(ev)
    return
  end
  items = {}
  seen_first = false
  clear_activity(ev)
  if win and win:is_open() then
    win:hide()
  end
  n00n.ui.set_status_hint(nil)
end

n00n.api.create_autocmd({ "TurnEnd", "TurnError" }, { callback = clear_activity })
n00n.api.create_autocmd("SessionReset", { callback = clear_todos })
