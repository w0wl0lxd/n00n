return function(U)
  local A = require("lang.nix.analyze")(U)
  local format_skeleton = U.format_skeleton
  local MAX_DEPTH = 5
  return {
    import_separator = "/",
    extract = function(source, root)
      local entries = A.collect(root, source, MAX_DEPTH)
      for _, ie in ipairs(A.collect_imports(root, source)) do
        entries[#entries + 1] = ie
      end
      return format_skeleton(entries, {}, nil, "/")
    end,
  }
end
