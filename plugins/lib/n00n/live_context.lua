-- Live context: combine n00n.session.live() + blackboard query for visibility.
local M = {}

function M.snapshot(ctx)
  local sessions, err = n00n.session.live()
  if not sessions then
    return nil, "failed to get live sessions: " .. tostring(err)
  end

  local enriched = {}
  local bb_posts = {}
  local bb_claims = {}

  local bb_ok, bb_result = pcall(function()
    local posts_result =
      n00n.agent.call_tool(ctx or {}, "blackboard", { action = "query", query = { type = "status" } })
    if posts_result and posts_result.results then
      bb_posts = posts_result.results
    end
    local claims_result = n00n.agent.call_tool(ctx or {}, "blackboard", { action = "list_claims", only_active = true })
    if claims_result and claims_result.claims then
      bb_claims = claims_result.claims
    end
  end)

  if not bb_ok then
    for _, session in ipairs(sessions) do
      local entry = {
        session_id = session.id,
        agent_type = session.agent_type or "unknown",
        status = session.status or "unknown",
        last_activity = session.updated_at or os.time(),
        active_task_id = nil,
        active_claim = nil,
        recent_posts = {},
        metadata = {
          title = session.title,
          focused = session.focused,
        },
      }
      enriched[#enriched + 1] = entry
    end
    return enriched
  end

  for _, session in ipairs(sessions) do
    local recent_posts = {}
    local active_claim = nil
    local active_task_id = nil

    for _, post in ipairs(bb_posts) do
      if post.task_id == session.id or post.agent_id == session.id then
        recent_posts[#recent_posts + 1] = {
          type = post.type,
          content = post.content,
          timestamp = post.timestamp,
        }
        if post.task_id and not active_task_id then
          active_task_id = post.task_id
        end
      end
    end

    for _, claim in ipairs(bb_claims) do
      if claim.task_id == session.id or claim.agent_id == session.id then
        if claim.status == "claimed" and claim.expires_at and claim.expires_at >= os.time() then
          active_claim = {
            task_id = claim.task_id,
            agent_id = claim.agent_id,
            claimed_at = claim.claimed_at,
            expires_at = claim.expires_at,
          }
        end
      end
    end

    table.sort(recent_posts, function(a, b)
      return (a.timestamp or 0) > (b.timestamp or 0)
    end)

    if #recent_posts > 10 then
      local trimmed = {}
      for i = 1, 10 do
        trimmed[i] = recent_posts[i]
      end
      recent_posts = trimmed
    end

    local entry = {
      session_id = session.id,
      agent_type = session.agent_type or "unknown",
      status = session.status or "unknown",
      last_activity = session.updated_at or os.time(),
      active_task_id = active_task_id,
      active_claim = active_claim,
      recent_posts = recent_posts,
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
