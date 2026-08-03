-- Blackboard: shared coordination substrate for multi-agent sessions.
-- Agents post observations, claim tasks atomically, and query coordination state.
local ok, memory_helpers = pcall(require, "memory.memory_helpers")

local DEFAULT_CLAIM_TTL = 300
local MAX_CLAIM_TTL = 3600
local DEFAULT_QUERY_LIMIT = 100
local MAX_QUERY_LIMIT = 1000

local MAX_EXTRA_DEPTH = 4
local MAX_EXTRA_ENTRIES = 32
local MAX_EXTRA_STRING_BYTES = 2048

local function sensitive_extra_key(key)
  local normalized = key:lower()
  return normalized == "token"
    or normalized == "secret"
    or normalized == "password"
    or normalized == "authorization"
    or normalized == "credential"
    or normalized == "cookie"
    or normalized == "api_key"
    or normalized == "apikey"
end

local function validate_extra(value, depth, budget, seen)
  local kind = type(value)
  if kind == "nil" or kind == "boolean" then
    return true
  end
  if kind == "number" then
    if value ~= value or value == math.huge or value == -math.huge then
      return nil, "extra contains a non-finite number"
    end
    return true
  end
  if kind == "string" then
    if #value > MAX_EXTRA_STRING_BYTES then
      return nil, "extra string exceeds maximum length"
    end
    return true
  end
  if kind ~= "table" then
    return nil, "extra must contain JSON-compatible values"
  end
  if depth >= MAX_EXTRA_DEPTH then
    return nil, "extra exceeds maximum nesting depth"
  end
  if seen[value] then
    return nil, "extra contains a cyclic table"
  end
  seen[value] = true
  local object_keys = false
  local array_length = 0
  for key, child in pairs(value) do
    budget.count = budget.count + 1
    if budget.count > MAX_EXTRA_ENTRIES then
      return nil, "extra exceeds maximum entry count"
    end
    if type(key) == "string" then
      object_keys = true
      if sensitive_extra_key(key) then
        return nil, "extra contains a sensitive key"
      end
    elseif type(key) == "number" and key > 0 and key % 1 == 0 then
      array_length = math.max(array_length, key)
    else
      return nil, "extra keys must be strings or positive array indexes"
    end
    local ok, err = validate_extra(child, depth + 1, budget, seen)
    if not ok then
      return nil, err
    end
  end
  if object_keys and array_length > 0 then
    return nil, "extra cannot mix object keys and array indexes"
  end
  for index = 1, array_length do
    if value[index] == nil then
      return nil, "extra arrays cannot contain holes"
    end
  end
  seen[value] = nil
  return true
end

local function validate_extra_payload(extra)
  if extra == nil then
    return true
  end
  return validate_extra(extra, 0, { count = 0 }, {})
end
local function project_id()
  if ok and memory_helpers then
    local cwd = n00n.uv.cwd()
    local root = n00n.fs.root(cwd, ".git") or cwd
    return memory_helpers.project_id(root)
  end
  local cwd = n00n.uv.cwd()
  local base = n00n.fs.basename(cwd) or "root"
  return base .. "-default"
end

local function blackboard_dir()
  local state = n00n.env.state_dir()
  if not state then
    return nil, "cannot resolve state dir"
  end
  return n00n.fs.joinpath(state, "projects/" .. project_id() .. "/blackboard")
end

local function posts_dir()
  local bb, err = blackboard_dir()
  if not bb then
    return nil, err
  end
  return n00n.fs.joinpath(bb, "posts")
end

local function claims_dir()
  local bb, err = blackboard_dir()
  if not bb then
    return nil, err
  end
  return n00n.fs.joinpath(bb, "claims")
end

local function validate_id(id)
  if not id or id == "" then
    return nil, "id is required"
  end
  if #id > 128 then
    return nil, "id exceeds maximum length of 128"
  end
  if id:find("%.%.") or id:find("/") or id:find("\\") or id:find("%z") or id:find("%c") then
    return nil, "id contains invalid characters (path traversal, control chars, or null not allowed)"
  end
  if id:find("[^%w%-%_.]") then
    return nil, "id contains invalid characters (only alphanumeric, dash, underscore, dot allowed)"
  end
  return true
end

local function generate_id()
  local template = "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx"
  local function replace(c)
    if c == "x" then
      return string.format("%x", math.random(0, 15))
    elseif c == "y" then
      return string.format("%x", math.random(8, 11))
    else
      return c
    end
  end
  return string.gsub(template, ".", replace)
end

local function get_agent_id()
  local current = n00n.session.current()
  if current then
    return current
  end
  return "unknown"
end

local function write_post(post)
  local dir, err = posts_dir()
  if not dir then
    return nil, err
  end
  n00n.fs.mkdir(dir, { parents = true })

  local id = post.id or generate_id()
  local ok, vid = validate_id(id)
  if not ok then
    return nil, vid
  end

  local post_path = n00n.fs.joinpath(dir, id .. ".json")
  local content, enc_err = n00n.json.encode(post)
  if not content then
    return nil, "encode error: " .. tostring(enc_err)
  end

  local ok, write_err = n00n.fs.write(post_path, content)
  if not ok then
    return nil, "write error: " .. tostring(write_err)
  end

  return id
end

local function read_post(post_id)
  local ok, vid = validate_id(post_id)
  if not ok then
    return nil, vid
  end

  local dir, err = posts_dir()
  if not dir then
    return nil, err
  end

  local post_path = n00n.fs.joinpath(dir, post_id .. ".json")
  local content, read_err = n00n.fs.read(post_path)
  if not content then
    return nil, "read error: " .. tostring(read_err)
  end

  local decoded, dec_err = n00n.json.decode(content)
  if not decoded then
    return nil, "decode error: " .. tostring(dec_err)
  end

  return decoded
end

local function is_active_claim(claim)
  if not claim or not claim.expires_at then
    return false
  end
  if claim.status == "released" or claim.status == "failed" or claim.status == "done" then
    return false
  end
  return claim.expires_at >= os.time()
end

local function clean_expired_claims()
  local dir, err = claims_dir()
  if not dir then
    return nil, err
  end

  local entries = n00n.fs.dir(dir)
  if not entries then
    return true
  end

  for _, entry in ipairs(entries) do
    if entry[2] == "directory" then
      local claim_path = n00n.fs.joinpath(dir, entry[1], "claim.json")
      local content, read_err = n00n.fs.read(claim_path)
      if content then
        local claim, dec_err = n00n.json.decode(content)
        if
          claim
          and claim.expires_at
          and claim.expires_at < os.time()
          and not (claim.status == "released" or claim.status == "done" or claim.status == "failed")
        then
          pcall(n00n.fs.rm, claim_path)
        end
      end
    end
  end

  return true
end

local function list_claims(only_active)
  local dir, err = claims_dir()
  if not dir then
    return nil, err
  end

  local entries = n00n.fs.dir(dir)
  if not entries then
    return {}
  end

  local claims = {}
  for _, entry in ipairs(entries) do
    if entry[2] == "directory" then
      local claim_path = n00n.fs.joinpath(dir, entry[1], "claim.json")
      local content, read_err = n00n.fs.read(claim_path)
      if content then
        local claim, dec_err = n00n.json.decode(content)
        if claim then
          if only_active == false or is_active_claim(claim) then
            claims[#claims + 1] = claim
          end
        end
      end
    end
  end

  table.sort(claims, function(a, b)
    return (a.claimed_at or 0) > (b.claimed_at or 0)
  end)

  return claims
end

local function claim_task(task_id, expires_in)
  local ok, vid = validate_id(task_id)
  if not ok then
    return nil, vid
  end

  local ttl = expires_in or DEFAULT_CLAIM_TTL
  if ttl <= 0 then
    return nil, "expires_in must be positive"
  end
  if ttl > MAX_CLAIM_TTL then
    return nil, "expires_in exceeds maximum of " .. MAX_CLAIM_TTL
  end

  local dir, err = claims_dir()
  if not dir then
    return nil, err
  end
  n00n.fs.mkdir(dir, { parents = true })

  local clean_ok, clean_err = clean_expired_claims()
  if not clean_ok then
    return nil, "cleanup error: " .. tostring(clean_err)
  end

  local claim_dir = n00n.fs.joinpath(dir, task_id)
  local claim_path = n00n.fs.joinpath(claim_dir, "claim.json")
  local created_dir = false
  local meta = n00n.fs.metadata(claim_dir)
  if meta and meta.is_dir then
    local content, read_err = n00n.fs.read(claim_path)
    if content then
      local claim, dec_err = n00n.json.decode(content)
      if is_active_claim(claim) then
        return nil, "task already claimed by " .. (claim.agent_id or "unknown")
      end
    end
  else
    local mkdir_ok, mkdir_err = n00n.fs.mkdir(claim_dir)
    if not mkdir_ok then
      return nil, "claim failed: " .. tostring(mkdir_err)
    end
    created_dir = true
  end

  local now = os.time()
  local claim = {
    task_id = task_id,
    agent_id = get_agent_id(),
    claimed_at = now,
    expires_at = now + ttl,
    status = "claimed",
  }

  local content, enc_err = n00n.json.encode(claim)
  if not content then
    if created_dir then
      pcall(n00n.fs.rm, claim_dir)
    end
    return nil, "encode error: " .. tostring(enc_err)
  end

  local write_ok, write_err = n00n.fs.write(claim_path, content)
  if not write_ok then
    if created_dir then
      pcall(n00n.fs.rm, claim_dir)
    end
    return nil, "write error: " .. tostring(write_err)
  end

  return claim
end

local function release_task(task_id)
  local ok, vid = validate_id(task_id)
  if not ok then
    return nil, vid
  end

  local dir, err = claims_dir()
  if not dir then
    return nil, err
  end

  local claim_dir = n00n.fs.joinpath(dir, task_id)
  local claim_path = n00n.fs.joinpath(claim_dir, "claim.json")

  local content, read_err = n00n.fs.read(claim_path)
  if not content then
    return nil, "claim not found: " .. tostring(read_err)
  end

  local claim, dec_err = n00n.json.decode(content)
  if not claim then
    return nil, "decode error: " .. tostring(dec_err)
  end

  local agent_id = get_agent_id()
  if claim.agent_id ~= agent_id then
    return nil, "claim held by another agent"
  end

  claim.status = "released"
  local encoded, enc_err = n00n.json.encode(claim)
  if not encoded then
    return nil, "encode error: " .. tostring(enc_err)
  end

  local write_ok, write_err = n00n.fs.write(claim_path, encoded)
  if not write_ok then
    return nil, "write error: " .. tostring(write_err)
  end

  pcall(n00n.fs.rm, claim_dir)

  return true
end

local function update_task(task_id, status)
  local ok, vid = validate_id(task_id)
  if not ok then
    return nil, vid
  end

  if status ~= "done" and status ~= "failed" then
    return nil, "status must be 'done' or 'failed'"
  end

  local dir, err = claims_dir()
  if not dir then
    return nil, err
  end

  local claim_dir = n00n.fs.joinpath(dir, task_id)
  local claim_path = n00n.fs.joinpath(claim_dir, "claim.json")

  local content, read_err = n00n.fs.read(claim_path)
  if not content then
    return nil, "claim not found: " .. tostring(read_err)
  end

  local claim, dec_err = n00n.json.decode(content)
  if not claim then
    return nil, "decode error: " .. tostring(dec_err)
  end

  local agent_id = get_agent_id()
  if claim.agent_id ~= agent_id then
    return nil, "claim held by another agent"
  end

  claim.status = status
  local encoded, enc_err = n00n.json.encode(claim)
  if not encoded then
    return nil, "encode error: " .. tostring(enc_err)
  end

  local write_ok, write_err = n00n.fs.write(claim_path, encoded)
  if not write_ok then
    return nil, "write error: " .. tostring(write_err)
  end

  return true
end

local function query_posts(filters)
  local dir, err = posts_dir()
  if not dir then
    return nil, err
  end

  local entries = n00n.fs.dir(dir)
  if not entries then
    return {}
  end

  local results = {}
  local limit = math.min(filters.limit or DEFAULT_QUERY_LIMIT, MAX_QUERY_LIMIT)

  for _, entry in ipairs(entries) do
    if entry[2] == "file" and entry[1]:sub(-5) == ".json" then
      local post_path = n00n.fs.joinpath(dir, entry[1])
      local content, read_err = n00n.fs.read(post_path)
      if content then
        local post, dec_err = n00n.json.decode(content)
        if post then
          local match = true
          if filters.type and post.type ~= filters.type then
            match = false
          end
          if filters.task_id and post.task_id ~= filters.task_id then
            match = false
          end
          if filters.agent_id and post.agent_id ~= filters.agent_id then
            match = false
          end
          if filters.tags and #filters.tags > 0 then
            local tag_match = false
            for _, tag in ipairs(filters.tags) do
              for _, pt in ipairs(post.tags or {}) do
                if pt == tag then
                  tag_match = true
                  break
                end
              end
              if tag_match then
                break
              end
            end
            if not tag_match then
              match = false
            end
          end

          if match then
            results[#results + 1] = post
            if #results >= limit then
              break
            end
          end
        end
      end
    end
  end

  table.sort(results, function(a, b)
    return (a.timestamp or 0) > (b.timestamp or 0)
  end)

  return results
end

local description =
  "Shared coordination for multi-agent sessions. Post observations, claim tasks atomically, query state."

local schema = {
  type = "object",
  required = { "action" },
  properties = {
    action = {
      type = "string",
      enum = { "write", "read", "claim_task", "release_task", "update_task", "query", "list_claims" },
      description = "Action.",
    },
    post = {
      type = "object",
      description = "Post data.",
      properties = {
        id = { type = "string", description = "Unique id (optional)." },
        type = {
          type = "string",
          enum = { "observation", "claim", "status", "escalation" },
          description = "Post type.",
        },
        content = { type = "string", description = "Content." },
        tags = { type = "array", items = { type = "string" }, description = "Tags." },
        task_id = { type = "string", description = "Task id." },
        extra = {
          type = "any",
          description = "Optional bounded JSON-compatible payload (max depth 4, 32 entries; sensitive keys rejected).",
        },
      },
      required = { "type", "content" },
    },
    post_id = {
      type = "string",
      description = "Post id.",
    },
    task_id = {
      type = "string",
      description = "Task id.",
    },
    claim = {
      type = "object",
      description = "Claim data.",
      properties = {
        task_id = { type = "string", description = "Task id to claim." },
        expires_in = { type = "integer", description = "TTL seconds." },
      },
      required = { "task_id" },
    },
    status = {
      type = "string",
      description = "Status.",
      enum = { "done", "failed" },
    },
    query = {
      type = "object",
      description = "Query filters.",
      properties = {
        type = {
          type = "string",
          enum = { "observation", "claim", "status", "escalation" },
          description = "Post type.",
        },
        task_id = { type = "string", description = "Task id." },
        tags = { type = "array", items = { type = "string" }, description = "Tags." },
        agent_id = { type = "string", description = "Agent id." },
        limit = { type = "integer", description = "Max results." },
      },
    },
    only_active = {
      type = "boolean",
      description = "Active claims only.",
    },
  },
}

local function handler(input)
  local action = input.action

  if action == "write" then
    if not input.post or not input.post.type or not input.post.content then
      return { llm_output = "Error: post with type and content required for write", is_error = true }
    end

    local valid_types = { observation = true, claim = true, status = true, escalation = true }
    if not valid_types[input.post.type] then
      return { llm_output = "Error: invalid post type", is_error = true }
    end

    local extra_ok, extra_err = validate_extra_payload(input.post.extra)
    if not extra_ok then
      return { llm_output = "Error: invalid extra: " .. extra_err, is_error = true }
    end

    local post = {
      id = input.post.id,
      agent_id = get_agent_id(),
      timestamp = os.time(),
      type = input.post.type,
      content = input.post.content,
      tags = input.post.tags or {},
      task_id = input.post.task_id,
      extra = input.post.extra,
    }

    local id, err = write_post(post)
    if not id then
      return { llm_output = "Error: " .. tostring(err), is_error = true }
    end

    local encoded, enc_err = n00n.json.encode({ post_id = id })
    if not encoded then
      return { llm_output = "Post written: " .. id, post_id = id }
    end
    return { llm_output = encoded, post_id = id }
  elseif action == "read" then
    if not input.post_id then
      return { llm_output = "Error: post_id required for read", is_error = true }
    end

    local post, err = read_post(input.post_id)
    if not post then
      return { llm_output = "Error: " .. tostring(err), is_error = true }
    end

    local encoded, enc_err = n00n.json.encode(post)
    if not encoded then
      return { llm_output = "Error: encode failed", is_error = true }
    end
    return { llm_output = encoded, post = post }
  elseif action == "claim_task" then
    if not input.claim or not input.claim.task_id then
      return { llm_output = "Error: claim.task_id required for claim_task", is_error = true }
    end

    local claim, err = claim_task(input.claim.task_id, input.claim.expires_in)
    if not claim then
      return { llm_output = "Error: " .. tostring(err), is_error = true }
    end

    local encoded, enc_err = n00n.json.encode(claim)
    if not encoded then
      return { llm_output = "Task claimed: " .. claim.task_id, claim = claim }
    end
    return { llm_output = encoded, claim = claim }
  elseif action == "release_task" then
    if not input.task_id then
      return { llm_output = "Error: task_id required for release_task", is_error = true }
    end

    local ok, err = release_task(input.task_id)
    if not ok then
      return { llm_output = "Error: " .. tostring(err), is_error = true }
    end

    return { llm_output = "Task released: " .. input.task_id }
  elseif action == "update_task" then
    if not input.task_id then
      return { llm_output = "Error: task_id required for update_task", is_error = true }
    end
    if not input.status then
      return { llm_output = "Error: status required for update_task", is_error = true }
    end

    local ok, err = update_task(input.task_id, input.status)
    if not ok then
      return { llm_output = "Error: " .. tostring(err), is_error = true }
    end

    return { llm_output = "Task updated: " .. input.task_id .. " -> " .. input.status }
  elseif action == "list_claims" then
    if input.only_active ~= nil and type(input.only_active) ~= "boolean" then
      return { llm_output = "Error: only_active must be a boolean", is_error = true }
    end
    local only_active = input.only_active == nil and true or input.only_active
    local claims, err = list_claims(only_active)
    if not claims then
      return { llm_output = "Error: " .. tostring(err), is_error = true }
    end

    local encoded, enc_err = n00n.json.encode(claims)
    if not encoded then
      return { llm_output = "Error: encode failed", is_error = true }
    end
    return { llm_output = encoded, claims = claims }
  elseif action == "query" then
    local filters = input.query or {}
    local results, err = query_posts(filters)
    if not results then
      return { llm_output = "Error: " .. tostring(err), is_error = true }
    end

    local encoded, enc_err = n00n.json.encode(results)
    if not encoded then
      return { llm_output = "Error: encode failed", is_error = true }
    end
    return { llm_output = encoded, results = results }
  else
    return { llm_output = "Error: unknown action " .. tostring(action), is_error = true }
  end
end

local function header(input)
  return "blackboard: " .. tostring(input.action or "?")
end

n00n.api.register_tool({
  name = "blackboard",
  description = description,
  kind = "execute",
  admission = "cheap",
  audiences = { "main", "general_sub", "workflow" },
  schema = schema,
  handler = handler,
  header = header,
})
