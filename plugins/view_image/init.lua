local shorten_path = require("n00n.shorten_path")

local DESCRIPTION =
  [[Lossless viewer: oversized images return native tile 1, never resized. GIF needs `allow_gif_animation=true` only for a known capable provider; otherwise `static_image=true` (also animated WebP).]]

-- Anthropic's most restrictive supported transport caps one base64 image at
-- 5 MB. OpenAI's request cap is larger. Compare the exact encoded length so
-- every emitted image works with the conservative shared limit.
local MAX_BASE64_BYTES = 5 * 1000 * 1000
-- `n00n.image.encode_limited` enforces this before its PNG buffer can grow.
local MAX_PNG_BYTES = 3_750_000
-- Reading larger local sources is allowed for lossless tiling, but bounded.
-- n00n.image.decode separately enforces a host-side 50 MP decode-bomb cap.
local MAX_INPUT_BYTES = 50 * 1024 * 1024
local DEFAULT_TILE_EDGE = 2000
-- 8000 is the conservative common provider hard edge. Do not resize sources
-- over it: send a native-resolution tile instead.
local MAX_PROVIDER_EDGE = 8000
-- A cropped RGBA region can need four bytes per pixel. Four MP bounds the
-- working crop to roughly 16 MB before the separately bounded PNG encoding.
local MAX_CHUNK_PIXELS = 4 * 1000 * 1000

local MEDIA_TYPES = {
  png = "image/png",
  jpeg = "image/jpeg",
  gif = "image/gif",
  webp = "image/webp",
}

local function format_size(bytes)
  if bytes >= 1024 * 1024 then
    return string.format("%.1fMB", bytes / (1024 * 1024))
  end
  return string.format("%dKB", math.ceil(bytes / 1024))
end

local function caption(path, bytes, width, height, note)
  -- Shortened path, not basename: two screenshot.png in different dirs must
  -- stay distinguishable when several images land in one turn.
  return string.format("[image: %s %s %dx%d%s]", shorten_path(path), format_size(bytes), width, height, note or "")
end

local function base64_size(bytes)
  return 4 * math.ceil(bytes / 3)
end

local function fail(msg)
  return { llm_output = msg, is_error = true }
end

local function png_output(path, img, _info, note)
  local encoded, encode_err = img:encode_limited("png", MAX_PNG_BYTES)
  if not encoded then
    return fail(
      string.format(
        "%s lossless PNG region exceeds the bounded %s encoded output limit (%s); retry with smaller tile_width/tile_height or crop bounds. Original unchanged",
        path,
        format_size(MAX_PNG_BYTES),
        encode_err or "encoding failed"
      )
    )
  end
  local encoded_base64_size = base64_size(#encoded)
  if encoded_base64_size > MAX_BASE64_BYTES then
    return fail("internal error: bounded PNG output exceeds base64 transport limit")
  end
  return {
    llm_output = caption(path, #encoded, img:width(), img:height(), note),
    image = { media_type = MEDIA_TYPES.png, data = n00n.base64.encode(encoded) },
  }
end

local function required_integer(input, key, minimum, label)
  local value = input[key]
  label = label or key
  if value == nil then
    return nil, label .. " is required"
  end
  if type(value) ~= "number" or value ~= math.floor(value) or value < minimum then
    return nil, label .. " must be an integer >= " .. minimum
  end
  return value, nil
end

local function bounded_chunk(width, height, label)
  if width > MAX_PROVIDER_EDGE or height > MAX_PROVIDER_EDGE then
    return nil, string.format("%s dimensions must be at most %dx%d", label, MAX_PROVIDER_EDGE, MAX_PROVIDER_EDGE)
  end
  if width * height > MAX_CHUNK_PIXELS then
    return nil, string.format("%s area must be at most %d pixels", label, MAX_CHUNK_PIXELS)
  end
  return true, nil
end

local function optional_integer(input, key, default, label)
  if input[key] == nil then
    return default, nil
  end
  return required_integer(input, key, 1, label)
end

local function tile_image(path, img, info, input, static_note)
  local tile_index, index_err = required_integer(input, "tile_index", 1)
  if index_err then
    return fail(index_err)
  end
  local tile_width, width_err = optional_integer(input, "tile_width", DEFAULT_TILE_EDGE, "tile_width")
  if width_err then
    return fail(width_err)
  end
  local tile_height, height_err = optional_integer(input, "tile_height", DEFAULT_TILE_EDGE, "tile_height")
  if height_err then
    return fail(height_err)
  end
  local _, chunk_err = bounded_chunk(tile_width, tile_height, "tile")
  if chunk_err then
    return fail(chunk_err)
  end
  local columns = math.ceil(info.width / tile_width)
  local rows = math.ceil(info.height / tile_height)
  local tile_count = columns * rows
  if tile_index > tile_count then
    return fail(
      string.format("tile_index %d is outside 1..%d for %dx%d source", tile_index, tile_count, info.width, info.height)
    )
  end

  local zero_index = tile_index - 1
  local x = (zero_index % columns) * tile_width
  local y = math.floor(zero_index / columns) * tile_height
  local width = math.min(tile_width, info.width - x)
  local height = math.min(tile_height, info.height - y)
  local tile = img:crop(x, y, width, height)
  local note = string.format(
    ", tile %d/%d, source bounds x=[%d,%d) y=[%d,%d)%s",
    tile_index,
    tile_count,
    x,
    x + width,
    y,
    y + height,
    static_note or ""
  )
  return png_output(path, tile, info, note)
end

local function crop_image(path, img, info, input, static_note)
  local values = input.crop
  if type(values) ~= "table" or #values ~= 4 then
    return fail("crop must contain exactly [x, y, width, height]")
  end
  local x, x_err = required_integer(values, 1, 0, "crop x")
  if x_err then
    return fail(x_err)
  end
  local y, y_err = required_integer(values, 2, 0, "crop y")
  if y_err then
    return fail(y_err)
  end
  local width, width_err = required_integer(values, 3, 1, "crop width")
  if width_err then
    return fail(width_err)
  end
  local height, height_err = required_integer(values, 4, 1, "crop height")
  if height_err then
    return fail(height_err)
  end
  local _, chunk_err = bounded_chunk(width, height, "crop")
  if chunk_err then
    return fail(chunk_err)
  end
  if x + width > info.width or y + height > info.height then
    return fail(
      string.format(
        "crop source bounds x=[%d,%d) y=[%d,%d) are outside %dx%d image",
        x,
        x + width,
        y,
        y + height,
        info.width,
        info.height
      )
    )
  end

  local crop = img:crop(x, y, width, height)
  local note =
    string.format(", crop source bounds x=[%d,%d) y=[%d,%d)%s", x, x + width, y, y + height, static_note or "")
  return png_output(path, crop, info, note)
end

local function load_image(path, input)
  local bytes, read_err = n00n.fs.read_bytes_limited(path, MAX_INPUT_BYTES)
  if not bytes then
    return fail("cannot read " .. path .. ": " .. (read_err or "unknown error"))
  end
  local size = buffer.len(bytes)

  local info, probe_err = n00n.image.probe(bytes)
  if not info then
    return fail(path .. " is not an image (" .. (probe_err or "unrecognized format") .. ")")
  end
  local media_type = MEDIA_TYPES[info.format]
  if not media_type then
    return fail("unsupported image format " .. info.format .. ": only png, jpeg, gif, and webp can be viewed")
  end

  local tile_requested = input.tile_index ~= nil or input.tile_width ~= nil or input.tile_height ~= nil
  local crop_requested = input.crop ~= nil
  if tile_requested and crop_requested then
    return fail("tile and crop parameters cannot be combined")
  end
  local mode = crop_requested and "crop" or (tile_requested and "tile" or "original")
  local static_image = input.static_image == true
  local allow_gif_animation = input.allow_gif_animation == true
  if static_image and allow_gif_animation then
    return fail("static_image and allow_gif_animation cannot be combined")
  end
  -- Lua handlers have no provider format capability. GIF bytes pass only when
  -- the caller explicitly confirms its current provider supports animation.
  if info.format == "gif" and not static_image and not allow_gif_animation then
    return fail(
      path
        .. " is GIF. Provider capability is unavailable; use allow_gif_animation=true only when the current provider explicitly supports GIF animation, or static_image=true for an explicit first-frame PNG"
    )
  end
  if info.format == "webp" and info.animated and not static_image then
    return fail(
      path
        .. " is animated webp. Animation/provider capability is unavailable; use static_image=true to send an explicit first-frame PNG"
    )
  end
  if info.format == "gif" and allow_gif_animation and mode ~= "original" then
    return fail(
      "lossless GIF animation cannot be tiled or cropped; use static_image=true for an explicit first-frame PNG region"
    )
  end
  local original_fits_transport = base64_size(size) <= MAX_BASE64_BYTES
  local original_fits_dimensions = info.width <= MAX_PROVIDER_EDGE and info.height <= MAX_PROVIDER_EDGE
  local needs_default_tile = mode == "original" and (not original_fits_transport or not original_fits_dimensions)
  if info.format == "gif" and allow_gif_animation and needs_default_tile then
    return fail(
      path
        .. " exceeds a common image limit and cannot preserve GIF animation as one image; use static_image=true for explicit first-frame tiles"
    )
  end

  -- Decode fully even on the pass-through path: corrupt bytes in message
  -- history would fail every later provider request. This also enforces the
  -- host-side decode-bomb pixel limit.
  local img, decode_err = n00n.image.decode(bytes)
  if not img then
    return fail("cannot decode " .. path .. ": " .. (decode_err or "unknown error"))
  end

  local static_note = static_image and ", explicit static first frame" or ""
  if needs_default_tile then
    local output = tile_image(path, img, info, { tile_index = 1 }, static_note)
    if output.is_error then
      return output
    end
    local columns = math.ceil(info.width / DEFAULT_TILE_EDGE)
    local rows = math.ceil(info.height / DEFAULT_TILE_EDGE)
    local tile_count = columns * rows
    output.llm_output = output.llm_output
      .. string.format(
        " Source is %dx%d and cannot pass through as one common-provider image%s. ToolOutput carries one image, so native-resolution tile 1/%d is attached. Fetch each remaining region with tile_index=2..%d; tile schema: tile_index (one-based), tile_width and tile_height (default %d, each tile at most %d pixels).",
        info.width,
        info.height,
        original_fits_dimensions and " because it exceeds the byte transport limit" or " because an edge exceeds 8000px",
        tile_count,
        tile_count,
        DEFAULT_TILE_EDGE,
        MAX_CHUNK_PIXELS
      )
    return output
  end
  if mode == "original" and static_image then
    return png_output(path, img, info, static_note)
  end
  if mode == "original" then
    return {
      llm_output = caption(path, size, info.width, info.height),
      image = { media_type = media_type, data = n00n.base64.encode(bytes) },
    }
  end
  if mode == "tile" then
    return tile_image(path, img, info, input, static_note)
  end
  return crop_image(path, img, info, input, static_note)
end

n00n.api.register_tool({
  name = "view_image",
  kind = "read",
  description = DESCRIPTION,
  -- No interpreter audience: the code_execution bridge flattens tool output
  -- to text, so the pixels could never reach the model from there.
  audiences = { "main", "research_sub", "general_sub" },

  schema = {
    type = "object",
    properties = {
      path = {
        type = "string",
        description = "Path.",
        required = true,
        alias = "file_path",
      },
      tile_index = {
        type = "integer",
        minimum = 1,
        description = "One-based tile.",
      },
      tile_width = {
        type = "integer",
        minimum = 1,
        maximum = MAX_PROVIDER_EDGE,
        description = "Default 2000; max 4MP.",
      },
      tile_height = {
        type = "integer",
        minimum = 1,
        maximum = MAX_PROVIDER_EDGE,
        description = "Default 2000; max 4MP.",
      },
      crop = {
        type = "array",
        items = { type = "integer" },
        minItems = 4,
        maxItems = 4,
        description = "[x,y,w,h]; <=8000 edge/4MP.",
      },
      static_image = {
        type = "boolean",
        description = "First-frame PNG.",
      },
      allow_gif_animation = {
        type = "boolean",
        description = "Raw GIF opt-in.",
      },
    },
  },

  header = function(input)
    local buf = n00n.ui.buf()
    buf:line({ { shorten_path(input.path or ""), "path" } })
    return buf
  end,

  handler = function(input, _ctx)
    local raw = input.path
    if not raw then
      return fail("error: path is required")
    end
    local path = n00n.fs.abspath(raw)
    local meta = n00n.fs.metadata(path)
    if not meta then
      return fail("error: path not found: " .. path)
    end
    if not meta.is_file then
      return fail("error: " .. path .. " is not a regular file")
    end
    if meta.size > MAX_INPUT_BYTES then
      return fail(
        string.format(
          "%s is too large to read for viewing (%s; limit %s)",
          path,
          format_size(meta.size),
          format_size(MAX_INPUT_BYTES)
        )
      )
    end
    return load_image(path, input)
  end,
})
