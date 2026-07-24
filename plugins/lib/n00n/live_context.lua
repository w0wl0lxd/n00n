-- Live context: combine n00n.session.live() + blackboard query for visibility.
local M = {}

function M.snapshot()
  local sessions, err = n00n.session.live()
  if not sessions then
    return nil, "failed to get live sessions: " .. tostring(err)
  end

  local enriched = {}
  for _, session in ipairs(sessions) do
    local entry = {
      session_id = session.id,
      agent_type = session.agent_type or "unknown",
      status = session.status or "unknown",
      last_activity = session.updated_at or os.time(),
      active_task_id = nil,
      metadata = {
        title = session.title,
        focused = session.focused,
      },
    }
    enriched[#enriched + 1] = entry
  end

  return enriched
end

return M
