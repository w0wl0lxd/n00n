return function(U)
  return require("lang.simple_outline")(U, {
    header = "targets",
    nodes = {
      define_directive = { label = "" },
      include_directive = { label = "" },
      rule = { label = "" },
      variable_assignment = { label = "" },
    },
  })
end
