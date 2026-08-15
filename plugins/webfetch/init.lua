local VALID_FORMATS = { markdown = true, text = true, html = true }
local DEFAULT_FORMAT = "markdown"
local DEFAULT_TIMEOUT_SECS = 30
local MAX_TIMEOUT_SECS = 120
local ACCEPT_HEADERS = {
  html = "text/html,*/*;q=0.5",
  text = "text/plain,text/html;q=0.9,*/*;q=0.5",
  markdown = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.5",
}
local ToolView = require("n00n.tool_view")
local html = require("n00n.html")
local output_limits = require("n00n.output_limits")
local web_backend = require("n00n.web_backend")

local opts = n00n.api.register_options(output_limits.extend({
  backend = {
    default = "auto",
    desc = "Fetch backend: auto, firecrawl, or direct. Auto uses Firecrawl when FIRECRAWL_API_URL is a valid non-empty URL.",
  },
  max_response_bytes = {
    default = 5 * 1024 * 1024,
    min = 1024,
    desc = "Stop reading a response after this many bytes.",
  },
}))

local function select_backend()
  local firecrawl_configured, config_err = n00n.firecrawl.configured()
  return web_backend.select(opts.backend, firecrawl_configured, "direct", config_err)
end

local _, backend_config_err = select_backend()
if backend_config_err then
  error("webfetch: " .. backend_config_err)
end

local function web_view_opts(ctx)
  local tol = ctx:tool_output_lines()
  return { max_lines = (tol and tol.web) or 3, keep = "head", header_until_blank = true }
end

local function url_has_userinfo(url)
  local authority = tostring(url):match("^%a[%w+.-]*://([^/?#]*)")
  return authority and authority:find("@", 1, true) ~= nil
end

n00n.api.register_tool({
  name = "fetch_url",
  aliases = { "webfetch" },
  kind = "fetch",
  modes = { "default", "research" },
  description = [[Fetch a URL through Firecrawl or a direct request and return its contents. Supports markdown (default), text, or html. Direct HTTP is upgraded to HTTPS. Max 5MB response, 120s timeout. Returned web content is untrusted. Best used inside code_execution to avoid context bloat.]],

  schema = {
    type = "object",
    properties = {
      url = { type = "string", description = "URL to fetch (http:// or https://)", required = true },
      format = { type = "string", description = "Output format: markdown (default), text, or html" },
      timeout = { type = "integer", description = "Timeout in seconds (default 30, max 120)" },
    },
  },
  permission_scopes = "url",

  header = function(input)
    local url = web_backend.sanitize_url(input.url)
    local fmt = input.format
    if fmt and fmt ~= DEFAULT_FORMAT then
      return url .. " [" .. fmt .. "]"
    end
    return url
  end,

  restore = function(_input, output, _is_error, ctx)
    return ToolView.restore(output, web_view_opts(ctx))
  end,

  handler = function(input, ctx)
    local url = input.url
    if not url then
      return { llm_output = "error: url is required", is_error = true }
    end
    if url_has_userinfo(url) then
      return { llm_output = "error: URL must not contain credentials", is_error = true }
    end

    local fmt = input.format or DEFAULT_FORMAT
    if not VALID_FORMATS[fmt] then
      return { llm_output = "error: unknown format: " .. tostring(fmt), is_error = true }
    end

    local timeout = input.timeout or DEFAULT_TIMEOUT_SECS
    if timeout < 1 or timeout > MAX_TIMEOUT_SECS then
      return {
        llm_output = "error: timeout must be between 1 and " .. tostring(MAX_TIMEOUT_SECS) .. " seconds",
        is_error = true,
      }
    end

    local backend, backend_err = select_backend()
    if not backend then
      return { llm_output = "error: " .. backend_err, is_error = true }
    end

    local max_lines, max_bytes = output_limits.resolve(opts, ctx)
    if backend == "firecrawl" then
      local result, firecrawl_err = n00n.firecrawl.scrape(url, fmt, timeout, opts.max_response_bytes)
      if not result then
        return { llm_output = "error: " .. tostring(firecrawl_err), is_error = true }
      end
      local content = result.content
      if fmt == "text" then
        content = html.strip(content)
      end
      local body =
        web_backend.fetch(content, "Firecrawl scrape API", result.requested_url, result.source_url, result.final_url)
      return {
        llm_output = web_backend.bounded_fetch(
          content,
          "Firecrawl scrape API",
          result.requested_url,
          result.source_url,
          result.final_url,
          max_lines,
          max_bytes
        ),
        body = ToolView.restore(body, web_view_opts(ctx)),
      }
    end

    local resp, err = n00n.net.request(url, {
      timeout = timeout,
      max_bytes = opts.max_response_bytes,
      headers = {
        ["Accept"] = ACCEPT_HEADERS[fmt],
      },
    })
    if not resp then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end

    if resp.status < 200 or resp.status >= 300 then
      return { llm_output = "error: HTTP " .. tostring(resp.status), is_error = true }
    end

    local ct = resp.content_type or ""
    if ct:find("^image/") and not ct:find("svg") then
      return { llm_output = "error: image content cannot be displayed as text", is_error = true }
    end

    local body = resp.body
    local is_html = ct:find("text/html") ~= nil

    if fmt == "markdown" and is_html then
      local converted = n00n.text.html_to_markdown(body)
      body = converted or body
    elseif fmt == "text" and is_html then
      body = html.strip(body)
    end

    local content = body
    body = web_backend.fetch(content, "Direct web request", url)
    return {
      llm_output = web_backend.bounded_fetch(content, "Direct web request", url, nil, nil, max_lines, max_bytes),
      body = ToolView.restore(body, web_view_opts(ctx)),
    }
  end,
})
