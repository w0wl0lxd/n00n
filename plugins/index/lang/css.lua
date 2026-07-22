return function(U)
  return require("lang.simple_outline")(U, {
    header = "rules",
    nodes = {
      at_rule = { label = "" },
      import_statement = { label = "" },
      keyframes_statement = { label = "" },
      media_statement = { label = "" },
      rule_set = { label = "" },
      scope_statement = { label = "" },
    },
  })
end
