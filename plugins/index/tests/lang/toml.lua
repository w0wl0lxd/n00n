local helpers = require("tests.helpers")
local case = helpers.case
local idx = helpers.idx
local idx_with_meta = helpers.idx_with_meta
local has = helpers.has
local lacks = helpers.lacks

case("toml_top_level_pair", function()
  local src = [==[
    title = "TOML Example"
    version = 1
  ]==]
  local out = idx(src, "toml")
  has(out, {
    "consts:",
    "title",
    "TOML Example",
    "version",
  })
end)

case("toml_table_with_pairs", function()
  local src = [==[
    [package]
    name = "maki"
    version = "0.3.27"
  ]==]
  local out = idx(src, "toml")
  has(out, {
    "consts:",
    "[package]",
    "name",
    "maki",
    "version",
    "0.3.27",
  })
end)

case("toml_table_array_element", function()
  local src = [==[
    [[bin]]
    name = "maki"
    path = "src/main.rs"
  ]==]
  local out = idx(src, "toml")
  has(out, {
    "[[bin]]",
    "name",
    "maki",
    "path",
    "src/main.rs",
  })
end)

case("toml_dotted_keys", function()
  local src = [==[
    a.b.c = 1

    [package.metadata.docs]
    rs = true
  ]==]
  local out = idx(src, "toml")
  has(out, {
    "a.b.c",
    "[package.metadata.docs]",
    "rs",
  })
end)

case("toml_inline_table_and_array_values", function()
  local src = [==[
    [settings]
    tags = ["a", "b", "c"]
    meta = { x = 1, y = 2 }
  ]==]
  local out = idx(src, "toml")
  has(out, {
    "[settings]",
    "tags = [",
    "meta = {",
  })
end)

case("toml_comments_ignored", function()
  local src = [==[
    # top comment
    [server]
    # inline comment
    host = "localhost" # trailing comment
  ]==]
  local out = idx(src, "toml")
  has(out, {
    "[server]",
    "host",
    "localhost",
  })
  lacks(out, {
    "top comment",
    "inline comment",
    "trailing comment",
  })
end)

case("toml_table_field_truncation", function()
  local src = "[data]\n"
  for i = 1, 9 do
    src = src .. "k" .. i .. " = " .. i .. "\n"
  end
  local text, meta = idx_with_meta(src, "toml")
  has(text, { "[data]", "k1", "k8", "[1 more truncated]" })
  lacks(text, { "k9" })
  helpers.assert_truncated_dim(text, meta)
end)

case("toml_array_of_tables_multiple_elements", function()
  local src = [==[
    [[products]]
    name = "widget"

    [[products]]
    name = "gadget"
  ]==]
  local out = idx(src, "toml")
  has(out, {
    "[[products]]",
    "widget",
    "gadget",
  })
end)
