return function(U)
  local function key(text)
    return text:match('^%s*(".-")%s*:') or text:match("^%s*([^:]+)%s*:") or text
  end

  return require("lang.simple_outline")(U, {
    header = "keys",
    nodes = {
      pair = { label = key, recurse = true },
    },
  })
end
