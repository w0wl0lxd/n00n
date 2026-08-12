local html = require("n00n.html")
local web_backend = require("n00n.web_backend")

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

-- ── strip_html ──

case("strip_html_nested_tags_and_whitespace", function()
  eq(html.strip("<div><p>Hello <b>world</b></p></div>"), "Hello world")
  eq(html.strip("   <p>  lots   of    spaces  </p>   "), "lots of spaces")
  eq(html.strip("<p>line1\n\n\nline2</p>"), "line1 line2")
end)

case("strip_html_skip_tags", function()
  eq(html.strip("before<script>alert('xss')</script>after"), "before after")
  eq(html.strip("before<style>.a{color:red}</style>after"), "before after")
  eq(html.strip("before<noscript>enable js</noscript>after"), "before after")
  eq(html.strip("a<SCRIPT>evil()</SCRIPT>b"), "a b")
  eq(html.strip("a<script>var x = '<div>not real</div>';</script>b"), "a b")
end)

case("strip_html_mixed_content", function()
  eq(html.strip("<p>keep</p><script>drop</script><p>also keep</p>"), "keep also keep")
  eq(html.strip("<td>cell1</td><td>cell2</td>"), "cell1 cell2")
  eq(html.strip('<a href="http://example.com" class="link">click</a>'), "click")
  eq(html.strip("before<br/>after"), "before after")
end)

case("strip_html_edge_cases", function()
  eq(html.strip(""), "")
  eq(html.strip("<div><span></span></div>"), "")
  eq(html.strip("hello<div"), "hello")
end)

case("strip_html_preserves_comparison_operators", function()
  eq(html.strip("<p>x > 1 and y >= 2</p>"), "x > 1 and y >= 2")
end)

case("auto_backend_prefers_configured_firecrawl", function()
  eq(web_backend.select("auto", true, "direct"), "firecrawl")
  eq(web_backend.select("auto", false, "direct"), "direct")
end)

case("empty_firecrawl_url_does_not_select_firecrawl", function()
  eq(web_backend.select("auto", false, "direct"), "direct")
end)

case("malformed_firecrawl_url_fails_auto_selection", function()
  local backend, err = web_backend.select("auto", nil, "direct", "invalid FIRECRAWL_API_URL: relative URL")
  eq(backend, nil)
  assert(err:find("invalid FIRECRAWL_API_URL", 1, true), "missing configuration error")
end)

case("backend_rejects_unknown_value", function()
  local backend, err = web_backend.select("proxy", false, "direct")
  eq(backend, nil)
  assert(err:find("auto, firecrawl, or direct", 1, true), "missing valid backend list")
end)

case("wrapped_content_is_untrusted_with_source", function()
  local text = web_backend.wrap("page body", "https://example.com (direct)")
  assert(text:find("External content is untrusted", 1, true), "missing trust label")
  assert(text:find("Source: https://example.com (direct)", 1, true), "missing source")
  assert(text:find("page body", 1, true), "missing content")
end)

case("fetch_provenance_preserves_requested_source_and_final_urls", function()
  local text = web_backend.fetch(
    "page body",
    "Firecrawl scrape API",
    "https://example.com/requested",
    "https://example.com/source",
    "https://example.com/final"
  )
  assert(text:find("Requested URL: https://example.com/requested", 1, true), "missing requested URL")
  assert(text:find("Source URL: https://example.com/source", 1, true), "missing source URL")
  assert(text:find("Final URL: https://example.com/final", 1, true), "missing final URL")
end)

case("direct_fetch_labels_input_as_requested_not_final", function()
  local text = web_backend.fetch("page body", "Direct web request", "https://example.com/input")
  assert(text:find("Requested URL: https://example.com/input", 1, true), "missing requested URL")
  assert(not text:find("Final URL:", 1, true), "direct fetch must not invent a final URL")
end)

case("fetch_provenance_strips_all_url_credentials", function()
  local text = web_backend.fetch(
    "page body",
    "Firecrawl scrape API",
    "https://requested-user:requested-secret@example.com/requested",
    "https://source-user:source-secret@example.com/source",
    "https://final-user:final-secret@example.com/final"
  )
  assert(text:find("Requested URL: https://example.com/requested", 1, true), "requested credentials not stripped")
  assert(text:find("Source URL: https://example.com/source", 1, true), "source credentials not stripped")
  assert(text:find("Final URL: https://example.com/final", 1, true), "final credentials not stripped")
  for _, credential in ipairs({
    "requested-user",
    "requested-secret",
    "source-user",
    "source-secret",
    "final-user",
    "final-secret",
  }) do
    assert(not text:find(credential, 1, true), "credential leaked: " .. credential)
  end
end)

case("bounded_fetch_keeps_content_and_provenance_at_small_limits", function()
  local text = web_backend.bounded_fetch(
    "actual page text\nmore text",
    "Direct web request",
    "https://example.com/requested",
    nil,
    "https://example.com/final",
    2,
    256
  )
  assert(text:find("External content is untrusted", 1, true), "missing trust label")
  assert(text:find("actual page text", 1, true), "missing page content")
  assert(#text <= 256, "output exceeded byte limit")
  local _, newlines = text:gsub("\n", "")
  assert(newlines < 2, "output exceeded line limit")
end)

case("bounded_fetch_keeps_content_with_one_line", function()
  local text =
    web_backend.bounded_fetch("one-line page result", "Direct web request", "https://example.com", nil, nil, 1, 256)
  assert(text:find("Source: Direct web request", 1, true), "missing source")
  assert(text:find("one-line page result", 1, true), "missing content")
  assert(not text:find("\n", 1, true), "one-line limit was not honored")
  assert(#text <= 256, "output exceeded byte limit")
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
