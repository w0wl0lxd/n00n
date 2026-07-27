-- The /sessions picker: a tree of sessions in this directory. Main sessions
-- are shown by default; sub-tasks are grouped underneath them and can be
-- expanded with the Right arrow. Live sessions get a colored icon, and row
-- order is frozen while the picker is open so rows never jump around under
-- the cursor while background agents keep working.

local TextInput = require("n00n.text_input")
local ListPicker = require("n00n.list_picker")

local FILTER_PREFIX = "❯ "
local RENAME_PREFIX = "Rename: "
local CONFIRM_HINT = "  Ctrl+D again to delete"
local CANNOT_DELETE_GROUP_HINT = "Cannot delete a group"
local DELETE_FOCUSED_HINT = "Cannot delete the current session"
local RENAME_USAGE = "Usage: /rename <title>"
local EMPTY_HINT = "  No sessions yet. Press Ctrl+N to start one."
local NO_MATCHES_HINT = "  No matches"
local LOADING_HINT = "  Loading sessions…"
local CURRENT_LABEL = "current"
local TICK_MS = 100
local AGE_TICKS = 10
-- Placeholder only: the host swaps "spinner:*"-styled spans for the live
-- animated frame, so working rows spin without this plugin redrawing.
local WORKING_ICON = "· "
local AGE_UNITS = {
  { 31536000, "y" },
  { 2592000, "mo" },
  { 604800, "w" },
  { 86400, "d" },
  { 3600, "h" },
  { 60, "m" },
}
local SUBTASK_PREFIXES = {
  { "team:", "team" },
  { "workflow:", "workflow" },
  { "task:", "task" },
}
local MAX_GROUP_CHILDREN = 20
local GROUP_PREFIX = "group:"
local GROUP_KIND = "group"
local FILTER_KEYS = {
  { "Enter", "open" },
  { "Ctrl+N", "new" },
  { "Ctrl+R", "rename" },
  { "Ctrl+D", "delete" },
  { "Right", "expand" },
  { "Left", "collapse" },
}
local RENAME_KEYS = {
  { "Enter", "save" },
  { "Esc", "cancel" },
}

local board = nil

local function icon_of(s)
  if s.is_group then
    return "  ", "dim"
  end
  if s.status == "needs_input" then
    return "◆ ", "warning"
  end
  if s.status == "working" then
    return WORKING_ICON, "accent", true
  end
  if s.focused then
    return "● ", "accent"
  end
  if s.live then
    return "○ ", "accent"
  end
  return "  ", "dim"
end

local function by_recency(a, b)
  if a.focused ~= b.focused then
    return a.focused
  end
  local ra, rb = board.rank[a.id], board.rank[b.id]
  if ra and rb then
    return ra < rb
  end
  if ra then
    return true
  end
  if rb then
    return false
  end
  local a_updated, b_updated = a.updated_at or 0, b.updated_at or 0
  if a_updated ~= b_updated then
    return a_updated > b_updated
  end
  return (a.id or "") < (b.id or "")
end

-- Assign stable ranks to new nodes so they keep their position while the
-- picker is open, even if their `updated_at` keeps changing.
local function assign_ranks(fresh)
  table.sort(fresh, by_recency)
  local base = board.min_rank - #fresh
  for i, s in ipairs(fresh) do
    board.rank[s.id] = base + i
  end
  board.min_rank = base
end

local function dispw(s)
  return utf8.len(s) or #s
end

local function age(updated_at)
  local secs = math.max(os.time() - (updated_at or 0), 0)
  for _, u in ipairs(AGE_UNITS) do
    if secs >= u[1] then
      return math.floor(secs / u[1]) .. u[2] .. " ago"
    end
  end
  return "just now"
end

local function filter_words()
  return ListPicker.split_words(board.input:value())
end

local function sel_index()
  for i, s in ipairs(board.items) do
    if s.id == board.sel_id then
      return i
    end
  end
  return nil
end

local function selected()
  local idx = sel_index()
  return idx and board.items[idx] or nil
end

local function find_stored(id)
  for i, st in ipairs(board.stored or {}) do
    if st.id == id then
      return i
    end
  end
  return nil
end

local function normalize_session(s, expanded_state)
  s.title = s.title or ""
  s.display_title = s.display_title or s.title
  if s.display_title == "" then
    s.display_title = "New session"
  end
  s.kind = s.kind or "main"
  s.updated_at = s.updated_at or 0
  s.children = {}
  s.expanded = expanded_state[s.id] or false
  s.depth = 0
end

local function sort_tree(nodes)
  table.sort(nodes, by_recency)
  for _, n in ipairs(nodes) do
    if #n.children > 0 then
      sort_tree(n.children)
    end
  end
end

local function build_tree(sessions)
  local by_id = {}
  for _, s in ipairs(sessions) do
    by_id[s.id] = s
  end
  local roots = {}
  for _, s in ipairs(sessions) do
    local p = s.parent_id
    if p and by_id[p] then
      table.insert(by_id[p].children, s)
    else
      table.insert(roots, s)
    end
  end
  sort_tree(roots)
  return roots
end

local function task_count(count)
  return count .. (count == 1 and " task" or " tasks")
end

local function group_label(children, start_idx, finish)
  local count = finish - start_idx + 1
  local count_text = task_count(count)
  local newest = children[start_idx].updated_at
  local oldest = children[start_idx].updated_at
  for i = start_idx + 1, finish do
    newest = math.max(newest, children[i].updated_at)
    oldest = math.min(oldest, children[i].updated_at)
  end
  local newest_age = age(newest)
  local oldest_age = age(oldest)
  if newest_age == oldest_age then
    return count_text .. " · " .. newest_age
  end
  return count_text .. " · " .. newest_age .. " – " .. oldest_age
end

local function make_bucket(parent, children, start_idx, finish, all_nodes, rank, expanded_state)
  local bucket_id = GROUP_PREFIX .. parent.id .. ":" .. start_idx
  local bucket = {
    id = bucket_id,
    title = "",
    display_title = group_label(children, start_idx, finish),
    kind = GROUP_KIND,
    is_group = true,
    children = {},
    parent_id = parent.id,
    updated_at = children[start_idx].updated_at,
    focused = false,
    live = false,
    status = "idle",
    expanded = expanded_state[bucket_id] or false,
    depth = 0,
  }
  rank[bucket.id] = rank[children[start_idx].id] - 0.5
  for i = start_idx, finish do
    local child = children[i]
    child.group_id = bucket.id
    table.insert(bucket.children, child)
  end
  table.insert(all_nodes, bucket)
  return bucket
end

local function group_node(node, all_nodes, rank, expanded_state)
  if #node.children > MAX_GROUP_CHILDREN then
    local buckets = {}
    for i = 1, #node.children, MAX_GROUP_CHILDREN do
      local finish = math.min(i + MAX_GROUP_CHILDREN - 1, #node.children)
      table.insert(buckets, make_bucket(node, node.children, i, finish, all_nodes, rank, expanded_state))
    end
    node.children = buckets
  end
  for _, child in ipairs(node.children) do
    group_node(child, all_nodes, rank, expanded_state)
  end
end

local function flatten_visible(nodes, depth, items)
  for _, n in ipairs(nodes) do
    n.depth = depth
    table.insert(items, n)
    if n.expanded and #n.children > 0 then
      flatten_visible(n.children, depth + 1, items)
    end
  end
end

local function apply_filter()
  local prev_pos = sel_index() or 1
  local words = filter_words()
  board.items = {}
  if #words == 0 then
    flatten_visible(board.roots, 0, board.items)
  else
    for _, n in ipairs(board.nodes) do
      n.depth = 0
      if not n.is_group and (ListPicker.matches(n.display_title, words) or ListPicker.matches(n.title, words)) then
        table.insert(board.items, n)
      end
    end
  end
  local idx = sel_index() or math.min(prev_pos, math.max(#board.items, 1))
  board.sel_id = board.items[idx] and board.items[idx].id or nil
end

-- Selection restarts from the top on every query change, so clearing the
-- filter never leaves the list scrolled to wherever a match happened to sit.
local function filter_changed()
  board.sel_id = nil
  board.confirm = nil
  apply_filter()
end

local function set_sel(i)
  board.sel_id = board.items[i] and board.items[i].id or nil
  board.confirm = nil
  render()
end

local function move_sel(delta, wrap)
  local n = #board.items
  if n == 0 then
    return
  end
  local cur = sel_index() or 1
  if wrap then
    set_sel((cur - 1 + delta) % n + 1)
  else
    set_sel(math.min(math.max(cur + delta, 1), n))
  end
end

local function page_size()
  return math.max(board.height - board.reserved - 1, 1)
end

local function toggle_expand()
  local s = selected()
  if not s or #s.children == 0 then
    return
  end
  s.expanded = not s.expanded
  apply_filter()
  render()
end

local function collapse_or_parent()
  local s = selected()
  if not s then
    return
  end
  if #s.children > 0 and s.expanded then
    s.expanded = false
    apply_filter()
    render()
    return
  end
  local parent_id = s.group_id or s.parent_id
  if parent_id then
    board.sel_id = parent_id
    apply_filter()
    render()
  end
end

local function update_footer()
  if board.rename then
    board.win:set_config({ footer = RENAME_KEYS })
    return
  end
  local footer = {}
  if board.counts.needs_input > 0 then
    footer[#footer + 1] = { "◆ " .. board.counts.needs_input, "needs input" }
  end
  if board.counts.working > 0 then
    footer[#footer + 1] = { "● " .. board.counts.working, "working" }
  end
  for _, f in ipairs(FILTER_KEYS) do
    footer[#footer + 1] = f
  end
  board.win:set_config({ footer = footer })
end

local render

render = function()
  local lines = {}
  local inner = board.width - 4
  local input = board.rename and board.rename.input or board.input
  local prefix = board.rename and RENAME_PREFIX or FILTER_PREFIX
  for _, ln in ipairs(input:render(prefix, dispw(prefix), inner).lines) do
    lines[#lines + 1] = ln
  end
  lines[#lines + 1] = {}
  -- The query and its blank spacer stay pinned while the list scrolls.
  if #lines ~= board.reserved then
    board.reserved = #lines
    board.win:set_config({ reserved_top = board.reserved })
  end
  local cursor_line = board.reserved
  local words = filter_words()
  for i, s in ipairs(board.items) do
    local selected = s.id == board.sel_id
    local icon, icon_style, spinning = icon_of(s)
    local base = selected and "selected" or "item"
    local right, right_style
    if s.is_group then
      local count = #s.children
      right = task_count(count)
      right_style = selected and "selected" or "dim"
    else
      right = s.focused and CURRENT_LABEL or age(s.updated_at)
      right_style = selected and "selected" or (s.focused and "accent" or "dim")
    end
    if selected then
      icon_style = "selected"
    end
    if spinning then
      icon_style = "spinner:" .. icon_style
    end
    local expand = (#s.children > 0) and (s.expanded and "▾ " or "▸ ") or "  "
    local indent = string.rep("  ", s.depth or 0)
    local line = { { indent, base }, { expand, base }, { icon, icon_style } }
    for _, sp in
      ipairs(ListPicker.highlight_spans(s.display_title, words, base, selected and "match_selected" or "match"))
    do
      line[#line + 1] = sp
    end
    local used = (2 * (s.depth or 0)) + dispw(expand) + dispw(icon) + dispw(s.display_title)
    if board.confirm == s.id then
      line[#line + 1] = { CONFIRM_HINT, selected and "match_selected" or "error" }
      used = used + dispw(CONFIRM_HINT)
    end
    local pad = math.max(inner - used - dispw(right), 1)
    line[#line + 1] = { string.rep(" ", pad), base }
    line[#line + 1] = { right, right_style }
    lines[#lines + 1] = line
    if selected then
      cursor_line = #lines
    end
  end
  if board.loading then
    lines[#lines + 1] = { { LOADING_HINT, "dim" } }
  elseif #board.items == 0 then
    lines[#lines + 1] = { { #board.nodes == 0 and EMPTY_HINT or NO_MATCHES_HINT, "dim" } }
  end
  board.buf:set_lines(lines)
  board.win:set_cursor(cursor_line)
end

-- Rebuilds the tree from live runtimes and the stored snapshot, then
-- renders. Live runtimes win over their stored copies for status and focus,
-- but the stored copy contributes its richer metadata (display_title,
-- parent_id, kind) for grouping and labelling. Until the background scan
-- lands (`board.stored`) only live sessions are shown. `live()` suspends
-- this coroutine, and the picker may close or another refresh may finish
-- meanwhile, so bail out unless this board is still current.
local function refresh()
  local this_board = board
  local live, live_err = n00n.session.live()
  if board ~= this_board then
    return
  end
  if live_err then
    n00n.ui.flash(live_err)
    render()
    return
  end

  local stored_map = {}
  for _, st in ipairs(board.stored or {}) do
    stored_map[st.id] = st
  end

  local expanded_state = {}
  for _, n in ipairs(board.nodes or {}) do
    expanded_state[n.id] = n.expanded
  end

  local seen, all = {}, {}
  for _, s in ipairs(live) do
    seen[s.id] = true
    s.live = true
    local st = stored_map[s.id]
    if st then
      s.display_title = s.display_title or st.display_title
      s.title = s.title or st.title
      s.kind = s.kind or st.kind
      s.parent_id = s.parent_id or st.parent_id
      s.updated_at = s.updated_at or st.updated_at
    end
    normalize_session(s, expanded_state)
    all[#all + 1] = s
  end
  for _, st in ipairs(board.stored or {}) do
    if not seen[st.id] then
      st.status = "idle"
      st.focused = false
      normalize_session(st, expanded_state)
      all[#all + 1] = st
    end
  end

  board.counts = { needs_input = 0, working = 0 }
  for _, s in ipairs(all) do
    if board.counts[s.status] then
      board.counts[s.status] = board.counts[s.status] + 1
    end
  end

  if board.loading then
    assign_ranks(all)
  else
    local fresh = {}
    for _, s in ipairs(all) do
      if not board.rank[s.id] then
        fresh[#fresh + 1] = s
      end
    end
    if #fresh > 0 then
      assign_ranks(fresh)
    end
  end
  table.sort(all, by_recency)

  board.nodes = all
  board.roots = build_tree(all)
  for _, root in ipairs(board.roots) do
    group_node(root, all, board.rank, expanded_state)
  end
  table.sort(all, by_recency)

  apply_filter()
  update_footer()
  render()
end

local function close()
  if board then
    board.win:close()
    board = nil
  end
end

local function open_selected()
  local s = selected()
  if not s then
    return
  end
  if s.is_group then
    toggle_expand()
    return
  end
  if not s.focused then
    local _, err = n00n.session.focus(s.id)
    if err then
      n00n.ui.flash(err)
      return
    end
  end
  close()
end

local function open_blank()
  local _, err = n00n.session.new({ focus = true })
  if err then
    n00n.ui.flash(err)
    return
  end
  close()
end

local function delete_selected()
  local s = selected()
  if not s then
    return
  end
  if s.is_group then
    n00n.ui.flash(CANNOT_DELETE_GROUP_HINT)
    return
  end
  if s.focused then
    n00n.ui.flash(DELETE_FOCUSED_HINT)
    return
  end
  if board.confirm ~= s.id then
    board.confirm = s.id
    render()
    return
  end
  board.confirm = nil
  local _, err = n00n.session.delete(s.id)
  if err then
    n00n.ui.flash(err)
    return
  end
  board.deleted[s.id] = true
  local si = find_stored(s.id)
  if si then
    table.remove(board.stored, si)
  end
  refresh()
end

local function start_rename()
  local s = selected()
  if not s or s.is_group then
    return
  end
  local input = TextInput.new()
  input:insert_text(s.display_title or s.title)
  board.rename = { id = s.id, input = input }
  board.confirm = nil
  update_footer()
  render()
end

local function stop_rename()
  board.rename = nil
  update_footer()
  render()
end

local function commit_rename()
  local title = board.rename.input:value():match("^%s*(.-)%s*$")
  local id = board.rename.id
  local current = selected()
  local stored_title = current and current.kind ~= "main" and (current.kind .. ": " .. title) or title
  stop_rename()
  if title == "" then
    return
  end
  local _, err = n00n.session.set_title({ id = id, title = stored_title })
  if err then
    n00n.ui.flash(err)
  else
    for _, n in ipairs(board.nodes) do
      if n.id == id then
        n.title = stored_title
        n.display_title = title
      end
    end
    local si = find_stored(id)
    if si then
      board.stored[si].title = title
      board.stored[si].display_title = title
    end
  end
  refresh()
end

local function handle_rename_key(key)
  if key == "esc" then
    stop_rename()
  elseif key == "enter" then
    commit_rename()
  elseif key ~= "up" and key ~= "down" then
    if board.rename.input:handle_key(key) ~= "ignored" then
      render()
    end
  end
end

local function handle_key(key)
  if key == "ctrl+c" then
    close()
  elseif board.rename then
    handle_rename_key(key)
  elseif key == "esc" then
    if board.confirm then
      board.confirm = nil
      render()
    elseif not board.input:is_empty() then
      board.input:clear()
      filter_changed()
      render()
    else
      close()
    end
  elseif key == "up" then
    move_sel(-1, true)
  elseif key == "down" then
    move_sel(1, true)
  elseif key == "pageup" then
    move_sel(-page_size())
  elseif key == "pagedown" then
    move_sel(page_size())
  elseif key == "right" then
    toggle_expand()
  elseif key == "left" then
    collapse_or_parent()
  elseif key == "enter" then
    open_selected()
  elseif key == "ctrl+n" then
    open_blank()
  elseif key == "ctrl+r" then
    start_rename()
  elseif key == "ctrl+d" then
    delete_selected()
  else
    local r = board.input:handle_key(key)
    if r ~= "ignored" then
      if r == "changed" then
        filter_changed()
      else
        board.confirm = nil
      end
      render()
    end
  end
end

local function open()
  if board then
    return
  end
  local buf = n00n.ui.buf()
  local win = n00n.ui.open_win(buf, {
    title = " Sessions ",
    width = "70%",
    height = "70%",
    border = "rounded",
    reserved_top = 2,
    focus = true,
    footer = FILTER_KEYS,
  })
  board = {
    win = win,
    buf = buf,
    width = win.width,
    height = win.height,
    reserved = 2,
    input = TextInput.new(),
    nodes = {},
    roots = {},
    items = {},
    rank = {},
    deleted = {},
    min_rank = 0,
    counts = { needs_input = 0, working = 0 },
    sel_id = nil,
    frame = 0,
    loading = true,
  }
  refresh()
  local this_board = board
  n00n.async.run(function()
    local stored, err = n00n.session.list()
    if board ~= this_board then
      return
    end
    if err then
      n00n.ui.flash(err)
      stored = {}
    end
    -- A delete may have landed while the scan was in flight; never let the
    -- stale snapshot resurrect that session as a ghost row.
    local kept = {}
    for _, st in ipairs(stored) do
      if not board.deleted[st.id] then
        kept[#kept + 1] = st
      end
    end
    board.stored = kept
    board.loading = false
    refresh()
  end)
  while board do
    local ev = board.win:recv(TICK_MS)
    if not ev or ev.type == "close" then
      board = nil
    elseif ev.type == "timeout" then
      board.frame = board.frame + 1
      if board.dirty then
        board.dirty = false
        refresh()
      elseif board.frame % AGE_TICKS == 0 then
        render()
      end
    elseif ev.type == "key" then
      handle_key(ev.key)
    elseif ev.type == "paste" then
      local input = board.rename and board.rename.input or board.input
      input:insert_text(ev.text)
      if not board.rename then
        filter_changed()
      else
        board.confirm = nil
      end
      render()
    elseif ev.type == "resize" then
      board.width = ev.width
      board.height = ev.height
      render()
    end
  end
end

-- A background agent flipping state deserves a heads-up even with the picker
-- closed, so the flash names the session and where to find it. Autocmds run
-- synchronously while refresh needs an async roundtrip, so the dirty flag
-- defers it to the next tick of the recv loop.
local last_status = {}
n00n.api.create_autocmd("SessionStatusChanged", {
  callback = function(ev)
    local d = ev.data or {}
    local prev = last_status[d.session_id]
    last_status[d.session_id] = d.status
    if not d.focused then
      if d.status == "needs_input" then
        n00n.ui.flash("◆ " .. d.title .. " needs input · /sessions")
      elseif d.status == "idle" and prev == "working" then
        n00n.ui.flash("✓ " .. d.title .. " finished · /sessions")
      end
    end
    if board then
      board.dirty = true
    end
  end,
})

n00n.api.register_command({
  name = "/sessions",
  description = "Browse and switch sessions",
  handler = open,
})

n00n.api.register_command({
  name = "/rename",
  description = "Rename the current session",
  handler = function(args)
    local title = (args or ""):match("^%s*(.-)%s*$")
    if title == "" then
      n00n.ui.flash(RENAME_USAGE)
      return
    end
    local id, err = n00n.session.current()
    if err then
      n00n.ui.flash(err)
      return
    end
    local _, set_err = n00n.session.set_title({ id = id, title = title })
    n00n.ui.flash(set_err or ('Renamed to "' .. title .. '"'))
  end,
})
