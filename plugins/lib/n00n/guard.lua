-- Runaway guard for subagent budgets.
--
-- Combines a user-configurable call limit with heuristic runaway detection:
-- repeated identical prompts, consecutive subagent errors, and wall-clock timeouts.
--
-- Use as a drop-in replacement for a simple { consume = ... } budget table:
--   guard.consume() is still called before a call.
--   guard.observe(prompt, err) is called after a call if available.
--
-- For richer control, subagent.launch also supports guard:check(prompt) before a
-- call and guard:record(prompt, err) after a call, which lets the guard see the
-- prompt and the result.

local M = {}

local DEFAULT_MAX_REPEATED_PROMPT = 3
local DEFAULT_MAX_CONSECUTIVE_ERRORS = 3

-- Provider-side capacity or transient transport failures should not consume
-- agent-call budgets or count against runaway heuristics, because the call
-- did not make progress and the runtime will retry.
local TRANSIENT_ERROR_PATTERNS = {
  "server_is_overloaded",
  "our servers are currently overloaded",
  "provider is overloaded",
  "try again later",
  "try again shortly",
  "try again in a moment",
  "rate limited",
  "connection error",
  "stream timed out",
  "credential store is busy",
  "this session is busy",
  "openai session is busy",
  "openai coding plan is busy",
  "coding plan request admission timed out",
  "admission timed out",
}

local function is_transient_error(err)
  if not err or type(err) ~= "string" then
    return false
  end
  local lower = err:lower()
  for _, pattern in ipairs(TRANSIENT_ERROR_PATTERNS) do
    if lower:find(pattern, 1, true) then
      return true
    end
  end
  return false
end

local function guard_error(kind, detail)
  return kind .. " runaway guard triggered" .. (detail and ": " .. detail or "")
end

function M.new(opts)
  opts = opts or {}

  local start_time = os.time()

  local self = {
    limit = opts.max_calls,
    timeout_secs = opts.timeout_secs,
    max_repeated = opts.max_repeated_prompt or DEFAULT_MAX_REPEATED_PROMPT,
    max_consecutive_errors = opts.max_consecutive_errors or DEFAULT_MAX_CONSECUTIVE_ERRORS,

    used = 0,
    prompts = {},
    consecutive_errors = 0,
    start_time = start_time,
  }

  -- Check before a subagent call. Returns (ok, err).
  function self.check(_, prompt)
    if self.timeout_secs then
      local elapsed = os.time() - self.start_time
      if elapsed > self.timeout_secs then
        return nil, guard_error("timeout", "elapsed " .. elapsed .. "s > " .. self.timeout_secs .. "s")
      end
    end

    if self.limit and self.used >= self.limit then
      return nil, guard_error("budget", "agent-call limit " .. self.limit .. " reached")
    end

    if self.consecutive_errors >= self.max_consecutive_errors then
      return nil, guard_error("consecutive errors", self.consecutive_errors .. " in a row")
    end

    if prompt then
      local count = self.prompts[prompt] or 0
      if count >= self.max_repeated then
        return nil, guard_error("repeated prompt", "prompt seen " .. count .. " times")
      end
    end

    self.used = self.used + 1
    return true
  end

  -- Record a subagent result. Returns (ok, err); updates prompt-frequency and
  -- consecutive-error heuristics. Transient provider capacity or transport
  -- failures refund the call slot and reset the consecutive-error counter so a
  -- temporary outage does not exhaust budgets or trip the runaway detector.
  function self.record(_, prompt, err)
    if prompt then
      self.prompts[prompt] = (self.prompts[prompt] or 0) + 1
    end

    if is_transient_error(err) then
      if self.used > 0 then
        self.used = self.used - 1
      end
      self.consecutive_errors = 0
      return true
    end

    if err then
      self.consecutive_errors = self.consecutive_errors + 1
      if self.consecutive_errors > self.max_consecutive_errors then
        return nil, guard_error("consecutive errors", self.consecutive_errors .. " in a row")
      end
    else
      self.consecutive_errors = 0
    end

    return true
  end

  -- Backwards-compatible API for callers that already use a consume/observe
  -- budget table.
  function self.consume(_)
    return self:check(nil)
  end

  function self.observe(_, prompt, err)
    return self:record(prompt, err)
  end

  return self
end

return M
