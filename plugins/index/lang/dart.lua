return function(U)
  local get_text = U.get_text
  local find_child = U.find_child
  local compact_ws = U.compact_ws
  local format_range = U.format_range
  local line_start = U.line_start
  local line_end = U.line_end
  local new_entry = U.new_entry
  local ranged = U.ranged
  local SECTION = U.SECTION

  local function type_params(node, source)
    local tp_node = node:field("type_parameters")[1]
    return tp_node and get_text(tp_node, source) or ""
  end

  local function params_result(node, source)
    local params_node = node:field("parameters")[1]
    local params = params_node and get_text(params_node, source) or "()"
    local ret_node = node:field("return_type")[1]
    local ret = ret_node and (" " .. get_text(ret_node, source)) or ""
    return compact_ws(params .. ret)
  end

  local function extract_class(node, source)
    local name_node = node:field("name")[1]
    if not name_node then
      return nil
    end
    local name = get_text(name_node, source)
    local tp = type_params(node, source)
    local body_node = node:field("body")[1]
    if not body_node then
      return nil
    end
    local members = {}
    for _, child in ipairs(body_node:children()) do
      local ckind = child:type()
      if ckind == "method_definition" then
        local name_node = child:field("name")[1]
        if name_node then
          local sig = get_text(name_node, source) .. params_result(child, source)
          local lr = format_range(line_start(child), line_end(child))
          members[#members + 1] = ranged(sig, lr)
        end
      elseif ckind == "field_definition" then
        local name_node = child:field("name")[1]
        if name_node then
          local sig = get_text(name_node, source)
          local type_node = child:field("type")[1]
          if type_node then
            sig = sig .. " " .. get_text(type_node, source)
          end
          local lr = format_range(line_start(child), line_end(child))
          members[#members + 1] = ranged(sig, lr)
        end
      elseif ckind == "getter_definition" then
        local name_node = child:field("name")[1]
        if name_node then
          local sig = "get " .. get_text(name_node, source)
          local ret_node = child:field("return_type")[1]
          if ret_node then
            sig = sig .. " " .. get_text(ret_node, source)
          end
          local lr = format_range(line_start(child), line_end(child))
          members[#members + 1] = ranged(sig, lr)
        end
      elseif ckind == "setter_definition" then
        local name_node = child:field("name")[1]
        if name_node then
          local sig = "set " .. get_text(name_node, source)
          local params_node = child:field("parameters")[1]
          if params_node then
            sig = sig .. get_text(params_node, source)
          end
          local lr = format_range(line_start(child), line_end(child))
          members[#members + 1] = ranged(sig, lr)
        end
      end
    end
    local entry = new_entry(SECTION.Class, node, "class " .. name .. tp)
    entry.children = members
    return entry
  end

  local function extract_mixin(node, source)
    local name_node = node:field("name")[1]
    if not name_node then
      return nil
    end
    local name = get_text(name_node, source)
    local tp = type_params(node, source)
    return new_entry(SECTION.Type, node, "mixin " .. name .. tp)
  end

  local function extract_extension(node, source)
    local name_node = node:field("name")[1]
    if not name_node then
      return nil
    end
    local name = get_text(name_node, source)
    return new_entry(SECTION.Type, node, "extension " .. name)
  end

  local function extract_extension_type(node, source)
    local name_node = node:field("name")[1]
    if not name_node then
      return nil
    end
    local name = get_text(name_node, source)
    local tp = type_params(node, source)
    return new_entry(SECTION.Type, node, "extension type " .. name .. tp)
  end

  local function extract_enum(node, source)
    local name_node = node:field("name")[1]
    if not name_node then
      return nil
    end
    local name = get_text(name_node, source)
    local tp = type_params(node, source)
    return new_entry(SECTION.Type, node, "enum " .. name .. tp)
  end

  local function extract_function(node, source)
    local name_node = node:field("name")[1]
    if not name_node then
      return nil
    end
    local name = get_text(name_node, source)
    local sig = name .. params_result(node, source)
    return new_entry(SECTION.Function, node, sig)
  end

  local function extract_getter(node, source)
    local name_node = node:field("name")[1]
    if not name_node then
      return nil
    end
    local name = get_text(name_node, source)
    local sig = "get " .. name
    local ret_node = node:field("return_type")[1]
    if ret_node then
      sig = sig .. " " .. get_text(ret_node, source)
    end
    return new_entry(SECTION.Function, node, sig)
  end

  local function extract_setter(node, source)
    local name_node = node:field("name")[1]
    if not name_node then
      return nil
    end
    local name = get_text(name_node, source)
    local sig = "set " .. name
    local params_node = node:field("parameters")[1]
    if params_node then
      sig = sig .. get_text(params_node, source)
    end
    return new_entry(SECTION.Function, node, sig)
  end

  return {
    import_separator = ".",
    is_doc_comment = function(node, source)
      return node:type() == "comment" and get_text(node, source):sub(1, 3) == "///"
    end,

    extract_nodes = function(node, source, _attrs)
      local kind = node:type()

      if kind == "class_definition" then
        local e = extract_class(node, source)
        return e and { e } or {}
      elseif kind == "mixin_declaration" then
        local e = extract_mixin(node, source)
        return e and { e } or {}
      elseif kind == "extension_declaration" then
        local e = extract_extension(node, source)
        return e and { e } or {}
      elseif kind == "extension_type_declaration" then
        local e = extract_extension_type(node, source)
        return e and { e } or {}
      elseif kind == "enum_declaration" then
        local e = extract_enum(node, source)
        return e and { e } or {}
      elseif kind == "function_declaration" then
        local e = extract_function(node, source)
        return e and { e } or {}
      elseif kind == "getter_definition" then
        local e = extract_getter(node, source)
        return e and { e } or {}
      elseif kind == "setter_definition" then
        local e = extract_setter(node, source)
        return e and { e } or {}
      end

      return {}
    end,
  }
end
