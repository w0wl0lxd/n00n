local helpers = require("tests.helpers")
local case = helpers.case
local has = helpers.has
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
