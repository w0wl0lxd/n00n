-- Exercises the n00n.image / n00n.base64 primitives view_image is built
-- from. The full tool path is covered by the view_image tests in
-- n00n-lua/tests/plugin_host.rs.

local failures = {}

local function case(name, fn)
  local ok, err = pcall(fn)
  if not ok then
    failures[#failures + 1] = name .. ": " .. tostring(err)
  end
end

local TINY_PNG_B64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="

case("base64_roundtrip", function()
  local bytes = n00n.base64.decode(TINY_PNG_B64)
  assert(n00n.base64.encode(bytes) == TINY_PNG_B64, "roundtrip mismatch")
end)

case("probe_reports_png_dimensions", function()
  local bytes = n00n.base64.decode(TINY_PNG_B64)
  local info, err = n00n.image.probe(bytes)
  assert(err == nil, err)
  assert(info.format == "png", "format: " .. tostring(info.format))
  assert(info.width == 1 and info.height == 1, info.width .. "x" .. info.height)
end)

case("probe_rejects_non_image", function()
  local info, err = n00n.image.probe("definitely not an image")
  assert(info == nil, "expected nil info")
  assert(err ~= nil, "expected an error")
end)

case("decode_rejects_truncated_image", function()
  local bytes = n00n.base64.decode(TINY_PNG_B64)
  local img, err = n00n.image.decode(bytes:sub(1, math.floor(#bytes / 2)))
  assert(img == nil, "expected nil image")
  assert(err ~= nil, "expected an error")
end)

case("decode_resize_crop_encode_pipeline", function()
  local bytes = n00n.base64.decode(TINY_PNG_B64)
  local img, err = n00n.image.decode(bytes)
  assert(err == nil, err)
  assert(img:width() == 1 and img:height() == 1)

  -- resize never upscales
  local same = img:resize(100, 100)
  assert(same:width() == 1 and same:height() == 1)

  local crop = img:crop(0, 0, 1, 1)
  assert(crop:width() == 1 and crop:height() == 1)

  local png = crop:encode("png")
  local info = n00n.image.probe(png)
  assert(info.format == "png")

  local jpeg = img:encode("jpeg")
  local jinfo = n00n.image.probe(jpeg)
  assert(jinfo.format == "jpeg")
end)

case("encode_limited_rejects_over_limit_output", function()
  local bytes = n00n.base64.decode(TINY_PNG_B64)
  local img, err = n00n.image.decode(bytes)
  assert(err == nil, err)
  local encoded, encode_err = img:encode_limited("png", 1)
  assert(encoded == nil, "expected nil encoded output")
  assert(encode_err ~= nil and encode_err:find("byte limit", 1, true), tostring(encode_err))
end)
case("crop_rejects_out_of_bounds", function()
  local bytes = n00n.base64.decode(TINY_PNG_B64)
  local img = n00n.image.decode(bytes)
  local ok, err = pcall(function()
    img:crop(1, 0, 1, 1)
  end)
  assert(not ok, "out-of-bounds crop must fail")
  assert(tostring(err):find("outside 1x1 image", 1, true), tostring(err))
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
