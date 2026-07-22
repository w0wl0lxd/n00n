return function(U)
  return require("lang.simple_outline")(U, {
    header = "configuration",
    nodes = {
      attribute = { label = "", recurse = true },
      block = { label = "", recurse = true },
    },
  })
end
