local truncate = require("n00n.truncate")
local ToolView = require("n00n.tool_view")
local output_limits = require("n00n.output_limits")

local DEFAULT_TIMEOUT_MS = 30000
local TMUX_CHECK_TIMEOUT_MS = 5000

local function shell_quote(s)
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

local function check_tmux_available()
  local id = n00n.fn.jobstart("tmux -V")
  local result = n00n.fn.jobwait(id, TMUX_CHECK_TIMEOUT_MS)
  if not result then
    n00n.fn.jobstop(id)
    return false, "tmux command timed out"
  end
  if result.exit_code ~= 0 then
    return false, "tmux is not installed or not on PATH"
  end
  return true, nil
end

local function check_tmux_server()
  local id = n00n.fn.jobstart("tmux list-sessions")
  local result = n00n.fn.jobwait(id, TMUX_CHECK_TIMEOUT_MS)
  if not result then
    n00n.fn.jobstop(id)
    return false, "tmux server check timed out"
  end
  if result.exit_code ~= 0 then
    return false, "no tmux server running (start one with 'tmux new-session')"
  end
  return true, nil
end

local function is_unix()
  local os_name = n00n.uv.os_getenv and n00n.uv.os_getenv("OS") or nil
  if os_name and os_name:lower():find("windows") then
    return false
  end
  return true
end

local function run_tmux(args, timeout_ms)
  local cmd = "tmux " .. args
  local id = n00n.fn.jobstart(cmd)
  local result = n00n.fn.jobwait(id, timeout_ms or DEFAULT_TIMEOUT_MS)
  if not result then
    n00n.fn.jobstop(id)
    return nil, "tmux command timed out"
  end
  if result.exit_code ~= 0 then
    local stderr = result.stderr or ""
    local stdout = result.stdout or ""
    return nil, "tmux failed (exit " .. result.exit_code .. "): " .. stderr .. stdout
  end
  return result.stdout or "", nil
end

local function parse_pipe_delimited(line, field_count)
  local fields = {}
  local start = 1
  for i = 1, field_count do
    local pipe = line:find("|", start, true)
    if not pipe then
      if i == field_count then
        fields[i] = line:sub(start)
      else
        return nil
      end
      break
    end
    fields[i] = line:sub(start, pipe - 1)
    start = pipe + 1
  end
  return fields
end

local function build_session_list(output)
  local sessions = {}
  for line in output:gmatch("[^\r\n]+") do
    local fields = parse_pipe_delimited(line, 4)
    if fields then
      sessions[#sessions + 1] = {
        session_name = fields[1],
        session_id = fields[2],
        created = fields[3],
        last_attached = fields[4],
      }
    end
  end
  return sessions
end

local function build_window_list(output)
  local windows = {}
  for line in output:gmatch("[^\r\n]+") do
    local fields = parse_pipe_delimited(line, 4)
    if fields then
      windows[#windows + 1] = {
        window_index = fields[1],
        window_name = fields[2],
        window_id = fields[3],
        layout = fields[4],
      }
    end
  end
  return windows
end

local function build_pane_list(output)
  local panes = {}
  for line in output:gmatch("[^\r\n]+") do
    local fields = parse_pipe_delimited(line, 5)
    if fields then
      panes[#panes + 1] = {
        pane_index = fields[1],
        pane_id = fields[2],
        pane_pid = fields[3],
        pane_current_path = fields[4],
        pane_current_command = fields[5],
      }
    end
  end
  return panes
end

local handlers = {}

function handlers.list_sessions(input, _ctx)
  local output, err =
    run_tmux('list-sessions -F "#{session_name}|#{session_id}|#{created}|#{last_attached}"', input.timeout_ms)
  if not output then
    return nil, err
  end
  local sessions = build_session_list(output)
  return { sessions = sessions, count = #sessions }
end

function handlers.list_windows(input, _ctx)
  local target = input.target or input.session or ""
  local args = "list-windows -t "
    .. shell_quote(target)
    .. ' -F "#{window_index}|#{window_name}|#{window_id}|#{layout}"'
  local output, err = run_tmux(args, input.timeout_ms)
  if not output then
    return nil, err
  end
  local windows = build_window_list(output)
  return { windows = windows, count = #windows, target = target }
end

function handlers.list_panes(input, _ctx)
  local target = input.target or input.window or ""
  local args = "list-panes -t "
    .. shell_quote(target)
    .. ' -F "#{pane_index}|#{pane_id}|#{pane_pid}|#{pane_current_path}|#{pane_current_command}"'
  local output, err = run_tmux(args, input.timeout_ms)
  if not output then
    return nil, err
  end
  local panes = build_pane_list(output)
  return { panes = panes, count = #panes, target = target }
end

function handlers.new_session(input, _ctx)
  local name = input.session_name or input.session or ""
  local args
  if name ~= "" then
    args = "new-session -d -s " .. shell_quote(name)
  else
    args = "new-session -d"
  end
  local output, err = run_tmux(args, input.timeout_ms)
  if not output then
    return nil, err
  end
  local session_name = name ~= "" and name or output:match("^%$%d+ (.+)$") or "unknown"
  return { success = true, session_name = session_name }
end

function handlers.kill_session(input, _ctx)
  local target = input.target or input.session or ""
  if target == "" then
    return nil, "target or session is required for kill_session"
  end
  local args = "kill-session -t " .. shell_quote(target)
  local _, err = run_tmux(args, input.timeout_ms)
  if err then
    return nil, err
  end
  return { success = true, target = target }
end

function handlers.new_window(input, _ctx)
  local target = input.target or input.session or ""
  local name = input.window_name or input.window or ""
  if target == "" then
    return nil, "target or session is required for new_window"
  end
  local args = "new-window -d -t " .. shell_quote(target)
  if name ~= "" then
    args = args .. " -n " .. shell_quote(name)
  end
  local output, err = run_tmux(args, input.timeout_ms)
  if not output then
    return nil, err
  end
  local window_index = output:match("^@(%d+)") or "unknown"
  return { success = true, window_index = window_index, target = target }
end

function handlers.kill_window(input, _ctx)
  local target = input.target or input.window or ""
  if target == "" then
    return nil, "target or window is required for kill_window"
  end
  local args = "kill-window -t " .. shell_quote(target)
  local _, err = run_tmux(args, input.timeout_ms)
  if err then
    return nil, err
  end
  return { success = true, target = target }
end

function handlers.send_keys(input, _ctx)
  local target = input.target or input.pane or ""
  local keys = input.keys or ""
  if target == "" then
    return nil, "target or pane is required for send_keys"
  end
  if keys == "" then
    return nil, "keys is required for send_keys"
  end
  local args = "send-keys -t " .. shell_quote(target) .. " " .. shell_quote(keys)
  local _, err = run_tmux(args, input.timeout_ms)
  if err then
    return nil, err
  end
  return { success = true, target = target }
end

function handlers.capture_pane(input, ctx)
  local target = input.target or input.pane or ""
  if target == "" then
    return nil, "target or pane is required for capture_pane"
  end
  local args = "capture-pane -p -t " .. shell_quote(target)
  local output, err = run_tmux(args, input.timeout_ms)
  if not output then
    return nil, err
  end

  local max_lines, max_bytes = output_limits.resolve(input, ctx)
  local truncated = truncate(output, max_lines, max_bytes)

  return { output = truncated, target = target }
end

function handlers.run_command(input, _ctx)
  local command_text = input.command_text or input.raw_command or ""
  if command_text == "" then
    return nil, "command_text is required for run_command"
  end
  local parts = {}
  for part in command_text:gmatch("%S+") do
    parts[#parts + 1] = part
  end
  if #parts == 0 then
    return nil, "command_text is empty for run_command"
  end
  local id = n00n.fn.jobstart({ "tmux", table.unpack(parts) })
  local result = n00n.fn.jobwait(id, input.timeout_ms)
  if not result then
    n00n.fn.jobstop(id)
    return nil, "tmux run_command timed out"
  end
  if result.exit_code ~= 0 then
    local stderr = result.stderr or ""
    local stdout = result.stdout or ""
    return nil, "tmux run_command failed (exit " .. result.exit_code .. "): " .. stderr .. stdout
  end
  return { output = result.stdout or "" }
end

function handlers.resize(input, _ctx)
  local target = input.target or input.pane or ""
  local width = input.width
  local height = input.height
  if target == "" then
    return nil, "target or pane is required for resize"
  end
  if not width and not height then
    return nil, "width or height is required for resize"
  end
  local args = "resize-pane -t " .. shell_quote(target)
  if width then
    args = args .. " -x " .. tostring(width)
  end
  if height then
    args = args .. " -y " .. tostring(height)
  end
  local _, err = run_tmux(args, input.timeout_ms)
  if err then
    return nil, err
  end
  return { success = true, target = target }
end

function handlers.break_pane(input, _ctx)
  local target = input.target or input.pane or ""
  if target == "" then
    return nil, "target or pane is required for break_pane"
  end
  local args = "break-pane -d -t " .. shell_quote(target)
  local output, err = run_tmux(args, input.timeout_ms)
  if not output then
    return nil, err
  end
  local window_index = output:match("^@(%d+)") or "unknown"
  return { success = true, window_index = window_index, target = target }
end

function handlers.join_pane(input, _ctx)
  local target = input.target or input.destination or ""
  local source = input.source or ""
  if target == "" then
    return nil, "target or destination is required for join_pane"
  end
  if source == "" then
    return nil, "source is required for join_pane"
  end
  local args = "join-pane -d -t " .. shell_quote(target) .. " -s " .. shell_quote(source)
  local _, err = run_tmux(args, input.timeout_ms)
  if err then
    return nil, err
  end
  return { success = true, target = target, source = source }
end

local opts = n00n.api.register_options(output_limits.extend({
  timeout_secs = {
    default = 30,
    min = 1,
    desc = "Kill the tmux command after this many seconds.",
  },
}))

n00n.api.register_tool({
  name = "tmux",
  kind = "execute",
  description = [[Manage tmux sessions, windows, and panes with structured output. Requires a running tmux server on Unix-like systems.

Commands:
- list_sessions: List all tmux sessions with metadata.
- list_windows: List windows in a session (requires target/session).
- list_panes: List panes in a window (requires target/window).
- new_session: Create a new session (optional session_name).
- kill_session: Destroy a session (requires target/session).
- new_window: Create a new window in a session (requires target/session, optional window_name).
- kill_window: Destroy a window (requires target/window).
- send_keys: Send keystrokes to a pane (requires target/pane, keys).
- capture_pane: Capture pane contents as text (requires target/pane).
- run_command: Run an arbitrary tmux command (requires command_text).
- resize: Resize a pane (requires target/pane, width or height).
- break_pane: Break a pane into a new window (requires target/pane).
- join_pane: Join a pane from another window (requires target/destination, source).

Targets follow tmux syntax: session_name, session_name:window_index, or session_name:window_index.pane_index.]],

  schema = {
    type = "object",
    properties = {
      command = {
        type = "string",
        enum = {
          "list_sessions",
          "list_windows",
          "list_panes",
          "new_session",
          "kill_session",
          "new_window",
          "kill_window",
          "send_keys",
          "capture_pane",
          "run_command",
          "resize",
          "break_pane",
          "join_pane",
        },
        required = true,
      },
      target = { type = "string", description = "Tmux target (session_name:window_index.pane_index)" },
      session = { type = "string", description = "Session name or ID" },
      session_name = { type = "string", description = "Session name for new_session" },
      window = { type = "string", description = "Window target" },
      window_name = { type = "string", description = "Window name for new_window" },
      pane = { type = "string", description = "Pane target" },
      keys = { type = "string", description = "Keys to send for send_keys" },
      command_text = { type = "string", description = "Raw tmux command for run_command" },
      raw_command = { type = "string", description = "Raw tmux command for run_command (alias)" },
      width = { type = "integer", description = "Pane width for resize" },
      height = { type = "integer", description = "Pane height for resize" },
      source = { type = "string", description = "Source pane for join_pane" },
      destination = { type = "string", description = "Destination window for join_pane" },
      timeout = { type = "integer", description = "Timeout in seconds (default 30)" },
    },
  },

  permission_scopes = function(input)
    local cmd = input.command
    if not cmd then
      return { scopes = {}, force_prompt = false }
    end

    local read_commands = {
      list_sessions = true,
      list_windows = true,
      list_panes = true,
      capture_pane = true,
    }

    local kill_commands = {
      kill_session = true,
      kill_window = true,
    }

    if read_commands[cmd] then
      return { scopes = { "tmux.read" }, force_prompt = false }
    elseif kill_commands[cmd] then
      return { scopes = { "tmux.kill" }, force_prompt = true }
    else
      return { scopes = { "tmux.write" }, force_prompt = false }
    end
  end,

  header = function(input)
    local cmd = input.command or "tmux"
    local parts = { cmd }
    if input.target then
      parts[#parts + 1] = input.target
    elseif input.session then
      parts[#parts + 1] = input.session
    elseif input.session_name then
      parts[#parts + 1] = input.session_name
    end
    if input.window_name then
      parts[#parts + 1] = input.window_name
    end
    if input.keys then
      parts[#parts + 1] = "'" .. input.keys .. "'"
    end
    if input.timeout then
      parts[#parts + 1] = "(" .. n00n.ui.humantime(input.timeout) .. " timeout)"
    end
    return table.concat(parts, " ")
  end,

  restore = function(_input, output, is_error, ctx)
    local tol = ctx:tool_output_lines()
    local buf = n00n.ui.buf()
    local view = ToolView.new(buf, {
      max_lines = (tol and tol.other) or 5,
      keep = "tail",
    })
    if is_error then
      view:append({ { output, "error" } })
    else
      view:append_text(output)
    end
    view:finish()
    return buf
  end,

  handler = function(input, ctx)
    if not input.command then
      return { llm_output = "error: command is required", is_error = true }
    end

    local cmd = input.command:gsub("-", "_")
    local handler = handlers[cmd]
    if not handler then
      return { llm_output = "error: unknown command: " .. tostring(cmd), is_error = true }
    end

    if not is_unix() then
      return { llm_output = "error: tmux is Unix-only and not supported on this platform", is_error = true }
    end

    local available, avail_err = check_tmux_available()
    if not available then
      return { llm_output = "error: " .. avail_err, is_error = true }
    end

    local server_running, server_err = check_tmux_server()
    if not server_running then
      return { llm_output = "error: " .. server_err, is_error = true }
    end

    local timeout_ms = (input.timeout or opts.timeout_secs) * 1000
    input.timeout_ms = timeout_ms

    local result, err = handler(input, ctx)
    if not result then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end

    local json_output = n00n.json.encode(result)
    return { llm_output = json_output }
  end,
})
