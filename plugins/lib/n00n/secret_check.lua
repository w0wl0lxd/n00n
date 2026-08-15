-- Heuristic secret/PII pattern detection for tool content validation.
--
-- Example:
--
--     local secret_check = require("n00n.secret_check")
--     local reason = secret_check.reason("api_key=sk_test_abcdefghijklmnopqrstuvwxyz")
--     if reason then error(reason) end
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

-- Generic credential suffixes with word boundaries to catch assignments like
-- my_api_key, user_credential, etc.
local CREDENTIAL_SUFFIXES = {
  "_key",
  "-key",
  "_credential",
  "-credential",
  "_secret",
  "-secret",
  "_token",
  "-token",
  "_password",
  "-password",
}

local TOKEN_VALUE_PATTERN = "[=:]%s*[\"']?([A-Za-z0-9+/_%-]+)"

-- PEM and OpenSSH armor markers. Armored key blocks start on their own line
-- and need no assignment delimiter, so they bypass keyword checks.
local PRIVATE_KEY_BEGIN = "-----BEGIN "
local PRIVATE_KEY_SUFFIX = " PRIVATE KEY-----"

-- PII patterns: email, phone-like, SSN-like
local EMAIL_PATTERN = "[%w%.%-_+]+@[%w%-]+%.[%a][%a%.]*%a"
local PHONE_PATTERNS = {
  "%(%d%d%d%)%s?%d%d%d[%-.]%d%d%d%d%f[%D]",
  "%f[%d]%d%d%d[%-.]%d%d%d[%-.]%d%d%d%d%f[%D]",
}
local SSN_PATTERN = "%d%d%d[-.]%d%d[-.]%d%d%d%d"

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

local function contains_credential_suffix(s)
  local l = lower(s)
  for _, suffix in ipairs(CREDENTIAL_SUFFIXES) do
    -- Use word boundary pattern to match suffix at end of identifier
    local pattern = "%w+" .. suffix .. "%f[%W]"
    if l:find(pattern) then
      return suffix
    end
  end
  return nil
end

local function contains_pii(s)
  if s:find(EMAIL_PATTERN) then
    return "email address"
  end
  for _, pattern in ipairs(PHONE_PATTERNS) do
    if s:find(pattern) then
      return "phone number"
    end
  end
  if s:find(SSN_PATTERN) then
    return "SSN-like pattern"
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

  -- Check for generic credential-key assignments with word boundaries
  local suffix = contains_credential_suffix(text)
  if suffix and looks_like_secret_value(text) then
    return false, "content may contain a secret pattern near '" .. suffix .. "'"
  end

  -- Check for PII patterns
  local pii = contains_pii(text)
  if pii then
    return false, "content may contain PII (" .. pii .. ")"
  end

  if text:find(PRIVATE_KEY_BEGIN, 1, true) and text:find(PRIVATE_KEY_SUFFIX, 1, true) then
    return false, "content contains a private key block"
  end

  -- Direct header-style credential exposure, with and without a colon.
  local l = lower(text)
  if l:find("authorization") then
    local value = text:match("[Bb]earer%s+([%w%+%/%=%._%-]+)") or text:match("[Bb]asic%s+([%w%+%/%=%._%-]+)")
    if value and #value >= 16 then
      return false, "content contains an authorization header"
    end
  end
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
