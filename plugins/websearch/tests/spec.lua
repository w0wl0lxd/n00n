local parse_sse_response = require("parse_sse")
local web_backend = require("n00n.web_backend")
local NO_RESULTS_MSG = "No search results found"

local failures = {}

local function case(name, fn)
  local ok, err = pcall(fn)
  if not ok then
    table.insert(failures, name .. ": " .. tostring(err))
  end
end

local function eq(actual, expected, msg)
  if actual ~= expected then
    error((msg or "") .. "\nexpected: " .. tostring(expected) .. "\n  actual: " .. tostring(actual))
  end
end

local function make_sse(text)
  return "data: "
    .. n00n.json.encode({
      jsonrpc = "2.0",
      result = {
        content = { { type = "text", text = text } },
      },
    })
end

local function sse_line(obj)
  return "data: " .. n00n.json.encode(obj)
end

-- ── parse_sse_response ──

case("parse_sse_extracts_text", function()
  local body = "event: message\n" .. make_sse("Rust is a systems language") .. "\n"
  local result = parse_sse_response(body, "text/event-stream")
  eq(result, "Rust is a systems language")
end)

case("parse_json_rpc_extracts_text_by_content_type", function()
  local body = n00n.json.encode({
    jsonrpc = "2.0",
    result = { content = { { type = "text", text = "JSON result" } } },
  })
  eq(parse_sse_response(body, "application/json; charset=utf-8"), "JSON result")
end)

case("parse_json_rpc_detects_json_body_without_content_type", function()
  local body = n00n.json.encode({
    jsonrpc = "2.0",
    result = { content = { { type = "text", text = "Detected JSON result" } } },
  })
  eq(parse_sse_response("  \n" .. body), "Detected JSON result")
end)

case("parse_sse_first_data_line_wins", function()
  local body = make_sse("first") .. "\n" .. make_sse("second") .. "\n"
  eq(parse_sse_response(body), "first")
end)

case("parse_sse_empty_body", function()
  eq(parse_sse_response(""), NO_RESULTS_MSG)
end)

case("parse_sse_empty_content_array", function()
  local body = sse_line({ result = { content = {} } })
  eq(parse_sse_response(body), NO_RESULTS_MSG)
end)

case("parse_sse_missing_content_key", function()
  local body = sse_line({ result = {} })
  eq(parse_sse_response(body), NO_RESULTS_MSG)
end)

case("parse_sse_empty_text_falls_through", function()
  local body = make_sse("") .. "\n" .. make_sse("actual result")
  eq(parse_sse_response(body), "actual result")
end)

case("parse_sse_malformed_json_is_error", function()
  local text, err = parse_sse_response("data: {not valid json}", "text/event-stream")
  eq(text, nil, "should return nil on malformed JSON")
  assert(err and err:find("SSE JSON parse error"), "should have error message, got: " .. tostring(err))
end)

case("parse_json_rpc_malformed_json_is_error", function()
  local text, err = parse_sse_response("{not valid json}", "application/json")
  eq(text, nil, "should return nil on malformed JSON")
  assert(err and err:find("JSON%-RPC parse error"), "should have error message, got: " .. tostring(err))
end)

case("parse_json_rpc_non_object_is_error", function()
  local text, err = parse_sse_response("42", "application/json")
  eq(text, nil, "should return nil on a non-object response")
  assert(err and err:find("JSON object", 1, true), "should have error message, got: " .. tostring(err))
end)

case("parse_json_rpc_error_is_sanitized", function()
  local secret = "json-rpc-provider-secret"
  local body = n00n.json.encode({ error = { message = secret } })
  local text, err = parse_sse_response(body, "application/json")
  eq(text, nil, "JSON-RPC errors must not produce result text")
  eq(err, "Exa backend returned an error")
  assert(not err:find(secret, 1, true), "provider error leaked: " .. err)
end)

case("parse_json_mcp_is_error_is_sanitized", function()
  local secret = "json-mcp-provider-secret"
  local body = n00n.json.encode({ result = { isError = true, content = { { type = "text", text = secret } } } })
  local text, err = parse_sse_response(body, "application/json")
  eq(text, nil, "MCP errors must not produce result text")
  eq(err, "Exa backend returned an error")
  assert(not err:find(secret, 1, true), "provider error leaked: " .. err)
end)

case("parse_sse_json_rpc_error_is_sanitized", function()
  local secret = "sse-json-rpc-provider-secret"
  local text, err = parse_sse_response(sse_line({ error = { message = secret } }), "text/event-stream")
  eq(text, nil, "JSON-RPC errors must stop SSE parsing")
  eq(err, "Exa backend returned an error")
  assert(not err:find(secret, 1, true), "provider error leaked: " .. err)
end)

case("parse_sse_mcp_is_error_is_sanitized", function()
  local secret = "sse-mcp-provider-secret"
  local body = sse_line({ result = { isError = true, content = { { type = "text", text = secret } } } })
  local text, err = parse_sse_response(body, "text/event-stream")
  eq(text, nil, "MCP errors must stop SSE parsing")
  eq(err, "Exa backend returned an error")
  assert(not err:find(secret, 1, true), "provider error leaked: " .. err)
end)

case("parse_unknown_response_is_error", function()
  local text, err = parse_sse_response("not JSON or SSE", "text/plain")
  eq(text, nil, "should return nil on an unsupported response")
  assert(err and err:find("unsupported Exa response", 1, true), "should have error message, got: " .. tostring(err))
end)

case("parse_sse_non_string_text_falls_through", function()
  local body = sse_line({ result = { content = { { type = "text", text = 42 } } } })
  eq(parse_sse_response(body), NO_RESULTS_MSG)
end)

case("parse_sse_skips_non_data_lines_finds_valid", function()
  local body = "event: message\nid: 1\nretry: 1000\n" .. make_sse("found it") .. "\n"
  eq(parse_sse_response(body), "found it")
end)

case("parse_sse_data_with_no_result_key_falls_through", function()
  local body = sse_line({ id = 1, method = "something" }) .. "\n" .. make_sse("actual")
  eq(parse_sse_response(body), "actual")
end)

case("parse_sse_only_no_result_lines_returns_no_results", function()
  local body = sse_line({ id = 1, method = "something" })
  eq(parse_sse_response(body), NO_RESULTS_MSG)
end)

case("auto_backend_prefers_configured_firecrawl", function()
  eq(web_backend.select("auto", true, "exa"), "firecrawl")
  eq(web_backend.select("auto", false, "exa"), "exa")
end)

case("malformed_firecrawl_url_does_not_fall_back_to_exa", function()
  local backend, err = web_backend.select("auto", nil, "exa", "invalid FIRECRAWL_API_URL: invalid port")
  eq(backend, nil)
  assert(err:find("invalid FIRECRAWL_API_URL", 1, true), "missing configuration error")
end)

case("explicit_firecrawl_requires_url", function()
  local backend, err = web_backend.select("firecrawl", false, "exa")
  eq(backend, nil)
  assert(err:find("FIRECRAWL_API_URL", 1, true), "missing clear environment error")
end)

case("firecrawl_results_include_untrusted_provenance", function()
  local text = web_backend.firecrawl_search({
    { title = "Example", url = "https://example.com", description = "Result text" },
  })
  assert(text:find("External content is untrusted", 1, true), "missing trust label")
  assert(text:find("Source: Firecrawl search API", 1, true), "missing backend source")
  assert(text:find("Source URL: https://example.com", 1, true), "missing result provenance")
end)

case("firecrawl_result_provenance_strips_url_credentials", function()
  local text = web_backend.firecrawl_search({
    { title = "Example", url = "https://search-user:search-secret@example.com", description = "Result text" },
  })
  assert(text:find("Source URL: https://example.com", 1, true), "result credentials not stripped")
  assert(not text:find("search-user", 1, true), "result user leaked")
  assert(not text:find("search-secret", 1, true), "result secret leaked")
end)

case("bounded_search_keeps_result_and_provenance_at_small_limits", function()
  local text = web_backend.bounded_firecrawl_search({
    { title = "Actual result title", url = "https://example.com", description = "Result text" },
  }, 2, 256)
  assert(text:find("Source: Firecrawl search API", 1, true), "missing backend provenance")
  assert(text:find("Actual result title", 1, true), "missing search result")
  assert(#text <= 256, "output exceeded byte limit")
  local _, newlines = text:gsub("\n", "")
  assert(newlines < 2, "output exceeded line limit")
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
