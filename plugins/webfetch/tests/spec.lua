local SKIP_TAGS = { script = true, style = true, noscript = true }
local web_backend = require("n00n.web_backend")

local function strip_html(html)
  local out = {}
  local in_tag = false
  local tag_buf = {}
  local skip_tag = nil
  local last_was_space = true

  for i = 1, #html do
    local ch = html:sub(i, i)
    if ch == "<" then
      in_tag = true
      tag_buf = {}
    elseif ch == ">" then
      in_tag = false
      local tag_str = table.concat(tag_buf):lower()
      local tag_name = tag_str:match("^%s*(%S+)")

      if skip_tag then
        if tag_name and tag_name:sub(1, 1) == "/" and tag_name:sub(2) == skip_tag then
          skip_tag = nil
        end
      elseif tag_name and SKIP_TAGS[tag_name] then
        skip_tag = tag_name
      end

      if not skip_tag and #out > 0 and not last_was_space then
        out[#out + 1] = " "
        last_was_space = true
      end
    elseif in_tag then
      tag_buf[#tag_buf + 1] = ch
    elseif not skip_tag then
      if ch:match("%s") then
        if not last_was_space and #out > 0 then
          out[#out + 1] = " "
          last_was_space = true
        end
      else
        out[#out + 1] = ch
        last_was_space = false
      end
    end
  end

  local result = table.concat(out)
  return result:match("^%s*(.-)%s*$")
end

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
  eq(strip_html("<div><p>Hello <b>world</b></p></div>"), "Hello world")
  eq(strip_html("   <p>  lots   of    spaces  </p>   "), "lots of spaces")
  eq(strip_html("<p>line1\n\n\nline2</p>"), "line1 line2")
end)

case("strip_html_skip_tags", function()
  eq(strip_html("before<script>alert('xss')</script>after"), "before after")
  eq(strip_html("before<style>.a{color:red}</style>after"), "before after")
  eq(strip_html("before<noscript>enable js</noscript>after"), "before after")
  eq(strip_html("a<SCRIPT>evil()</SCRIPT>b"), "a b")
  eq(strip_html("a<script>var x = '<div>not real</div>';</script>b"), "a b")
end)

case("strip_html_mixed_content", function()
  eq(strip_html("<p>keep</p><script>drop</script><p>also keep</p>"), "keep also keep")
  eq(strip_html("<td>cell1</td><td>cell2</td>"), "cell1 cell2")
  eq(strip_html('<a href="http://example.com" class="link">click</a>'), "click")
  eq(strip_html("before<br/>after"), "before after")
end)

case("strip_html_edge_cases", function()
  eq(strip_html(""), "")
  eq(strip_html("<div><span></span></div>"), "")
  eq(strip_html("hello<div"), "hello")
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

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
