local helpers = require("tests.helpers")
local case = helpers.case
local has = helpers.has
local lacks = helpers.lacks
local idx = helpers.idx
local indexer = require("indexer")

local cases = {
  {
    name = "astro",
    source = "---\nconst title = 'Hello'\n---\n<main id=\"page\"><h1>{title}</h1></main>",
    needles = { "components:", "frontmatter:", "markup:" },
  },
  {
    name = "css",
    source = "@import 'base.css';\n.card { color: red; }",
    needles = { "rules:", "@import", ".card" },
  },
  {
    name = "scss",
    source = "$gap: 1rem;\n.card { &__title { margin: $gap; } }",
    needles = { "rules:", ".card" },
  },
  {
    name = "json",
    source = '{"name":"n00n","scripts":{"test":"cargo test"}}',
    needles = { "keys:", '"name"', '"scripts"', '"test"' },
  },
  {
    name = "hcl",
    source = 'resource "aws_s3_bucket" "assets" {\n  bucket = "assets"\n}',
    needles = { "configuration:", "resource", "bucket" },
  },
  {
    name = "svelte",
    source = "<script>let count = 0;</script>\n<main>{count}</main>\n<style>main { color: red; }</style>",
    needles = { "components:", "script:", "markup:", "style:" },
  },
  {
    name = "vue",
    source = "<template><main>{{ title }}</main></template>\n<script setup>const title = 'Hi'</script>",
    needles = { "components:", "template:", "script:" },
  },
  {
    name = "containerfile",
    source = "FROM rust:1.94 AS build\nWORKDIR /app\nRUN cargo build --release",
    needles = { "instructions:", "FROM", "WORKDIR", "RUN" },
  },
  {
    name = "make",
    source = "BIN := n00n\n\nbuild: src/main.rs\n\tcargo build",
    needles = { "targets:", "BIN := n00n", "build:" },
  },
}

for _, item in ipairs(cases) do
  case(item.name .. "_indexes_common_structure", function()
    has(idx(item.source, item.name), item.needles)
  end)
end

case("additional_formats_map_extensions_and_filenames", function()
  assert(indexer.EXT_TO_LANG.astro == "astro")
  assert(indexer.EXT_TO_LANG.tf == "hcl")
  assert(indexer.EXT_TO_LANG.tfvars == "hcl")
  assert(indexer.EXT_TO_LANG.dockerfile == "containerfile")
  assert(indexer.EXT_TO_LANG.mk == "make")
  assert(indexer.FILENAME_TO_LANG.Dockerfile == "containerfile")
  assert(indexer.FILENAME_TO_LANG.Containerfile == "containerfile")
  assert(indexer.FILENAME_TO_LANG.Makefile == "make")
  assert(indexer.FILENAME_TO_LANG.GNUmakefile == "make")
end)

case("additional_formats_direct_extension_names_match_language", function()
  local same_name_exts = { "astro", "css", "scss", "json", "svelte", "vue", "hcl" }
  for _, name in ipairs(same_name_exts) do
    assert(indexer.EXT_TO_LANG[name] == name, name .. " extension should map to itself")
  end
  assert(indexer.EXT_TO_LANG.unknownext == nil, "unrelated extension should not be mapped")
end)

case("astro_without_frontmatter_still_indexes_markup", function()
  local src = "<main><h1>Hello</h1></main>"
  local out = idx(src, "astro")
  has(out, { "components:", "markup:" })
  lacks(out, { "frontmatter:" })
end)

case("css_media_and_keyframes_rules", function()
  local src = [==[
@media (min-width: 600px) {
  .card { color: blue; }
}
@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}
]==]
  local out = idx(src, "css")
  has(out, { "rules:", "@media", ".card", "@keyframes spin" })
end)

case("css_rule_with_long_selector_gets_truncated", function()
  local selector = "." .. string.rep("a", 150)
  local src = selector .. " { color: red; }"
  local out = idx(src, "css")
  has(out, { "rules:", "[truncated]" })
  lacks(out, { "red" })
end)

case("scss_nested_selector_and_variable", function()
  local src = [==[
$radius: 4px;
.card {
  border-radius: $radius;
  &__title {
    font-weight: bold;
  }
}
]==]
  local out = idx(src, "scss")
  has(out, { "rules:", ".card" })
end)

case("scss_reuses_css_extractor", function()
  assert(require("lang.scss") == require("lang.css"), "scss.lua should delegate to lang.css")
end)

case("json_nested_object_recurses_into_keys", function()
  local src = '{"outer":{"inner":1,"list":[{"deep":2}]}}'
  local out = idx(src, "json")
  has(out, { "keys:", '"outer"', '"inner"', '"list"', '"deep"' })
end)

case("json_array_of_objects_recurses_into_keys", function()
  local src = '{"items":[{"id":1},{"id":2}]}'
  local out = idx(src, "json")
  has(out, { "keys:", '"items"', '"id"' })
end)

case("hcl_nested_block_and_multiple_attributes", function()
  local src = [==[
resource "aws_instance" "web" {
  ami = "ami-123"
  network_interface {
    device_index = 0
  }
}
]==]
  local out = idx(src, "hcl")
  has(out, {
    "configuration:",
    'resource "aws_instance" "web"',
    'ami = "ami-123"',
    "network_interface",
    "device_index = 0",
  })
end)

case("svelte_snippet_statement", function()
  local src = "{#snippet row(item)}<li>{item}</li>{/snippet}"
  local out = idx(src, "svelte")
  has(out, { "components:", "snippet:" })
end)

case("vue_full_component_with_style", function()
  local src = [==[
<template><div>{{ msg }}</div></template>
<script>export default { data(){ return { msg: 'hi' } } }</script>
<style scoped>div { color: blue; }</style>
]==]
  local out = idx(src, "vue")
  has(out, { "components:", "template:", "script:", "style:" })
end)

case("containerfile_comments_ignored_and_multiple_instructions", function()
  local src = [==[
# base image
FROM rust:1.94 AS build
ARG VERSION=1
ENV PATH=/usr/bin
COPY . .
CMD ["./app"]
]==]
  local out = idx(src, "containerfile")
  has(out, { "instructions:", "FROM", "ARG", "ENV", "COPY", "CMD" })
  lacks(out, { "# base image" })
end)

case("make_variable_assignment_and_multiple_rules", function()
  local src = "CC := gcc\n\nall: main.o\n\tgcc -o all main.o\n\nclean:\n\trm -f *.o\n"
  local out = idx(src, "make")
  has(out, { "targets:", "CC := gcc", "all:", "clean:" })
end)

case("additional_formats_empty_or_trivial_sources_produce_empty_output", function()
  local trivial = {
    { "css", "" },
    { "scss", "" },
    { "json", "{}" },
    { "hcl", "" },
    { "containerfile", "" },
    { "make", "" },
    { "astro", "" },
    { "svelte", "" },
    { "vue", "" },
  }
  for _, t in ipairs(trivial) do
    local out = idx(t[2], t[1])
    assert(out == "", "expected empty output for " .. t[1] .. ", got: " .. out)
  end
end)
