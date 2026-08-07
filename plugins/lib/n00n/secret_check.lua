-- Heuristic secret/PII pattern detection for tool content validation.
--
-- This is intentionally conservative: it flags common secret-bearing keywords and
-- patterns so tools can surface a warning or require a justification. It does not
-- attempt to be exhaustive, and it may false-positive on example keys in docs.

local M = {}

-- Keywords commonly associated with secret-bearing values. Must be checked as
-- whole words/substrings, not as arbitrary letters, to avoid noise.
local SECRET_KEYWORDS = {
  "apikey",
  "api_key",
  "api-key",
  "authkey",
  "auth_key",
  "auth-key",
  "authtoken",
  "auth_token",
  "auth-token",
  "accesstoken",
  "access_token",
  "access-token",
  "clientsecret",
  "client_secret",
  "client-secret",
  "clientid",
  "client_id",
  "client-id",
  "consumerkey",
  "consumer_key",
  "consumer-secret",
  "consumersecret",
  "password",
  "passwd",
  "passphrase",
  "pwd",
  "secret",
  "secretkey",
  "secret_key",
  "secret-token",
  "secrettoken",
  "secret_token",
  "privatekey",
  "private_key",
  "private-key",
  "publickey",
  "public_key",
  "public-key",
  "refreshtoken",
  "refresh_token",
  "refresh-token",
  "sessiontoken",
  "session_token",
  "session-token",
  "idtoken",
  "id_token",
  "id-token",
  "token",
  "bearer",
  "awsaccesskey",
  "aws_access_key",
  "awssecretaccesskey",
  "aws_secret_access_key",
  "authorization:",
  "x-api-key",
  "x_api_key",
  "apikey=",
  "api_key=",
  "api-key=",
  "token=",
  "secret=",
  "password=",
  "passwd=",
}

-- High-entropy-ish token patterns: base64/hex strings that follow `=` or a key word.
local TOKEN_VALUE_PATTERN = "[=:]%s*[\"']?([A-Za-z0-9+/_%-]+)"

local function lower(s)
  return s:lower()
end

local function contains_keyword(s)
  local l = lower(s)
  for _, kw in ipairs(SECRET_KEYWORDS) do
    if l:find(kw, 1, true) then
      return kw
    end
  end
  return nil
end

local function looks_like_secret_value(s)
  -- A value that is a long alphanumeric + symbols blob after a secret key word.
  for match in s:gmatch(TOKEN_VALUE_PATTERN) do
    -- must be at least 16 chars to avoid flagging short examples
    if #match >= 16 then
      return true
    end
  end
  return false
end

-- Returns (ok, reason). If ok is false, reason explains what triggered.
function M.check(text)
  if type(text) ~= "string" or text == "" then
    return true
  end

  local kw = contains_keyword(text)
  if kw and looks_like_secret_value(text) then
    return false, "content may contain a secret pattern near '" .. kw .. "'"
  end

  -- Direct header-style credential exposure.
  local l = lower(text)
  if l:find("authorization: bearer ", 1, true) or l:find("authorization: basic ", 1, true) then
    return false, "content contains an authorization header"
  end

  return true
end

-- Convenience: returns a warning string if triggered, nil otherwise.
function M.reason(text)
  local ok, r = M.check(text)
  if not ok then
    return r
  end
  return nil
end

return M
