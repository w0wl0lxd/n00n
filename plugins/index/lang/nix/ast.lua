-- Tree-sitter / Nix AST primitives for the nix indexer.
--
-- Private to `lang.nix.analyze`. Extractors must not require this directly:
-- anything an extractor needs is exposed through `analyze.lua` instead, so the
-- tree-sitter / grammar shape stays a single-layer concern.

return function(U)
  local get_text = U.get_text
  local find_child = U.find_child

  local IMPORT_LIKE = {
    ["builtins.fetchTarball"] = true,
    ["fetchTarball"] = true,
    ["builtins.fetchurl"] = true,
    ["fetchurl"] = true,
    ["builtins.fetchGit"] = true,
    ["fetchGit"] = true,
    ["builtins.fetchClosure"] = true,
    ["fetchClosure"] = true,
  }

  local M = {}

  M.get_text = get_text
  M.find_child = find_child

  function M.fn_signature(name, node, source)
    local parts = {}
    local universal = node:field("universal")[1]
    if universal then
      parts[#parts + 1] = get_text(universal, source)
    end
    local formals = node:field("formals")[1]
    if formals then
      for _, child in ipairs(formals:children()) do
        if M.is_formal(child) then
          local name_node = child:field("name")[1]
          if name_node then
            parts[#parts + 1] = get_text(name_node, source)
          end
        elseif M.is_ellipses(child) then
          parts[#parts + 1] = "..."
        end
      end
    end
    if #parts > 0 then
      return name .. "(" .. table.concat(parts, ", ") .. ")"
    end
    return name
  end

  -- Convert raw text from a path/string/uri node into trie segments.
  function M.clean_path(text)
    if not text or text == "" then
      return nil
    end
    text = text:gsub("^path:", "")
    text = text:gsub('^"', ""):gsub('"$', "")
    text = text:gsub("^''", ""):gsub("''$", "")
    local segments = {}
    local i = 1
    while i <= #text do
      local slash = text:find("/", i)
      if slash then
        segments[#segments + 1] = text:sub(i, slash - 1)
        i = slash + 1
      else
        segments[#segments + 1] = text:sub(i)
        break
      end
    end
    if #segments == 0 then
      return nil
    end
    return segments
  end

  -- Extract path segments from a list element. Returns nil for elements
  -- that are not importable (numbers, attrsets, identifiers, etc.).
  function M.segments_from_element(el, source)
    local t = el:type()
    if
      t == "path_expression"
      or t == "spath_expression"
      or t == "string_expression"
      or t == "indented_string_expression"
      or t == "hpath_expression"
      or t == "uri_expression"
    then
      return M.clean_path(get_text(el, source))
    elseif t == "parenthesized_expression" then
      for _, child in ipairs(el:children()) do
        local segments = M.segments_from_element(child, source)
        if segments then
          return segments
        end
      end
    elseif t == "apply_expression" then
      local fn_node = el:field("function")[1]
      if not fn_node then
        return nil
      end
      local fn_type = fn_node:type()
      if fn_type == "variable_expression" then
        local fn_text = get_text(fn_node, source)
        if fn_text == "import" or IMPORT_LIKE[fn_text] then
          local arg = el:field("argument")[1]
          if arg then
            return M.segments_from_element(arg, source)
          end
        end
      elseif fn_type == "select_expression" then
        local fn_text = get_text(fn_node, source)
        if IMPORT_LIKE[fn_text] then
          local arg = el:field("argument")[1]
          if arg then
            return M.segments_from_element(arg, source)
          end
        elseif fn_text == "path" then
          local arg = el:field("argument")[1]
          if arg then
            return M.segments_from_element(arg, source)
          end
        end
      end
    end
    return nil
  end

  function M.collect_list_paths(list_node, source)
    local paths = {}
    for _, el in ipairs(list_node:field("element")) do
      local segs = M.segments_from_element(el, source)
      if segs and #segs > 0 then
        paths[#paths + 1] = segs
      end
    end
    return paths
  end

  function M.is_imports_binding(node, source)
    local ap = node:field("attrpath")[1]
    if not ap or ap:type() ~= "attrpath" then
      return false
    end
    return get_text(ap, source) == "imports"
  end

  function M.is_function_expression(n)
    return n:type() == "function_expression"
  end
  function M.is_attrset_expression(n)
    return n:type() == "attrset_expression"
  end
  function M.is_rec_attrset_expression(n)
    return n:type() == "rec_attrset_expression"
  end
  function M.is_let_attrset_expression(n)
    return n:type() == "let_attrset_expression"
  end
  function M.is_let_expression(n)
    return n:type() == "let_expression"
  end
  function M.is_parenthesized_expression(n)
    return n:type() == "parenthesized_expression"
  end
  function M.is_list_expression(n)
    return n:type() == "list_expression"
  end
  function M.is_apply_expression(n)
    return n:type() == "apply_expression"
  end
  function M.is_with_expression(n)
    return n:type() == "with_expression"
  end
  function M.is_source_code(n)
    return n:type() == "source_code"
  end
  function M.is_binding_set(n)
    return n:type() == "binding_set"
  end
  function M.is_binding(n)
    return n:type() == "binding"
  end
  function M.is_inherit(n)
    return n:type() == "inherit"
  end
  function M.is_inherit_from(n)
    return n:type() == "inherit_from"
  end
  function M.is_formal(n)
    return n:type() == "formal"
  end
  function M.is_ellipses(n)
    return n:type() == "ellipses"
  end
  function M.is_attrpath(n)
    return n:type() == "attrpath"
  end
  function M.is_variable_expression(n)
    return n:type() == "variable_expression"
  end
  function M.is_select_expression(n)
    return n:type() == "select_expression"
  end

  function M.expression(node)
    return node:field("expression")[1]
  end
  function M.formals(node)
    return node:field("formals")[1]
  end
  function M.universal(node)
    return node:field("universal")[1]
  end
  function M.body(node)
    return node:field("body")[1]
  end
  function M.formal_name(node)
    return node:field("name")[1]
  end
  function M.apply_function(node)
    return node:field("function")[1]
  end
  function M.apply_argument(node)
    return node:field("argument")[1]
  end
  function M.binding_attrpath(node)
    return node:field("attrpath")[1]
  end
  function M.binding_expression(node)
    return node:field("expression")[1]
  end

  function M.attrset_lookup(attrset, source, target_name)
    local bs = find_child(attrset, "binding_set")
    if not bs then
      return nil
    end
    for _, b in ipairs(bs:children()) do
      if M.is_binding(b) then
        local ap = M.binding_attrpath(b)
        if ap and get_text(ap, source) == target_name then
          local ve = M.binding_expression(b)
          if ve then
            local text = get_text(ve, source)
            text = text:gsub('^"(.*)"$', "%1")
            text = text:gsub("^''(.*)''$", "%1")
            return text
          end
        end
      end
    end
    return nil
  end

  function M.is_import_apply(val, source)
    local fn_node = M.apply_function(val)
    if not fn_node then
      return false
    end
    if M.is_variable_expression(fn_node) then
      local fn_text = get_text(fn_node, source)
      return fn_text == "import" or IMPORT_LIKE[fn_text]
    end
    if M.is_select_expression(fn_node) then
      return IMPORT_LIKE[get_text(fn_node, source)]
    end
    return false
  end

  function M.derivation_pname(val, source)
    local fn_node = M.apply_function(val)
    if not fn_node or not M.is_select_expression(fn_node) then
      return nil
    end
    local fn_text = get_text(fn_node, source)
    if not fn_text:match("%.mkDerivation$") then
      return nil
    end
    local arg = M.apply_argument(val)
    if not arg then
      return nil
    end
    if M.is_attrset_expression(arg) or M.is_rec_attrset_expression(arg) then
      return M.attrset_lookup(arg, source, "pname") or M.attrset_lookup(arg, source, "name")
    end
    return nil
  end

  return M
end
