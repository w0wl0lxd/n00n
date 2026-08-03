local helpers = require("skill_helpers")
local parse_frontmatter = helpers.parse_frontmatter
local build_skill_list = helpers.build_skill_list
local build_skill_names = helpers.build_skill_names

local failures = {}

local function case(name, fn)
  local ok, err = pcall(fn)
  if not ok then
    table.insert(failures, name .. ": " .. tostring(err))
  end
end

local function eq(actual, expected, msg)
  if actual ~= expected then
    error((msg or "") .. "\nexpected: " .. tostring(expected) .. "\n  actual: " .. tostring(actual))
  end
end

-- ── parse_frontmatter ──

case("frontmatter_with_name_and_description", function()
  local fm, body =
    parse_frontmatter("---\nname: git-release\ndescription: Create releases\n---\n## Instructions\nDo stuff")
  eq(fm.name, "git-release")
  eq(fm.description, "Create releases")
  eq(fm["disable-model-invocation"], false)
  assert(body:find("Instructions"), "body should contain content after closing ---")
end)

case("frontmatter_parses_comma_separated_paths", function()
  local fm, _ = parse_frontmatter("---\nname: scoped\npaths: src/**, docs/**/*.md\n---\nBody")
  eq(type(fm.paths), "table")
  eq(#fm.paths, 2)
  eq(fm.paths[1], "src/**")
  eq(fm.paths[2], "docs/**/*.md")
end)

case("frontmatter_parses_paths_array_and_manual_only", function()
  local fm, _ = parse_frontmatter(
    "---\nname: scoped\npaths:\n  - apps/**\n  - packages/**\ndisable-model-invocation: true\n---\nBody"
  )
  eq(type(fm.paths), "table")
  eq(#fm.paths, 2)
  eq(fm.paths[1], "apps/**")
  eq(fm.paths[2], "packages/**")
  eq(fm["disable-model-invocation"], true)
end)

case("no_frontmatter_returns_content_as_body", function()
  local fm, body = parse_frontmatter("Just content without frontmatter")
  eq(fm.name, nil)
  eq(body, "Just content without frontmatter")
end)

case("frontmatter_with_leading_whitespace", function()
  local fm, body = parse_frontmatter("  \n---\nname: trimmed\n---\nBody here")
  eq(fm.name, "trimmed")
  eq(body, "Body here")
end)

case("frontmatter_no_closing_delimiter", function()
  local input = "---\nname: oops\nThis never closes"
  local fm, body = parse_frontmatter(input)
  eq(fm.name, nil)
  eq(body, input)
end)

case("frontmatter_invalid_yaml_falls_back", function()
  local fm, body = parse_frontmatter("---\n: invalid: yaml: [[\n---\nBody")
  eq(fm.name, nil)
  eq(body, "Body")
end)

case("frontmatter_empty_body_after_close", function()
  local fm, body = parse_frontmatter("---\nname: x\n---\n   ")
  eq(fm.name, "x")
  eq(body, "")
end)

case("frontmatter_body_with_embedded_triple_dashes", function()
  local fm, body = parse_frontmatter("---\nname: tricky\n---\nSome text\n---\nMore text")
  eq(fm.name, "tricky")
  assert(body:find("Some text"), "body should start after first closing ---")
end)

case("frontmatter_only_dashes_no_yaml", function()
  local fm, body = parse_frontmatter("---\n\n---\nBody")
  eq(body, "Body")
end)

-- ── build_skill_list ──

case("build_skill_list_empty", function()
  local result = build_skill_list({})
  assert(result:find("No skills available"), "empty list should say no skills available")
  assert(result:find("<available_skills>"), "should have opening tag")
  assert(result:find("</available_skills>"), "should have closing tag")
end)

case("build_skill_list_single_skill", function()
  local skills = {
    test = { name = "test-skill", description = "A test skill" },
  }
  local result = build_skill_list(skills)
  assert(result:find("test%-skill"), "should contain skill name")
  assert(result:find("A test skill"), "should contain description")
  assert(not result:find("No skills available"), "should not say no skills")
end)

case("build_skill_list_sorted_alphabetically", function()
  local skills = {
    z = { name = "zebra", description = "Z skill" },
    a = { name = "alpha", description = "A skill" },
    m = { name = "middle", description = "M skill" },
  }
  local result = build_skill_list(skills)
  local alpha_pos = result:find("alpha")
  local middle_pos = result:find("middle")
  local zebra_pos = result:find("zebra")
  assert(alpha_pos < middle_pos, "alpha should come before middle")
  assert(middle_pos < zebra_pos, "middle should come before zebra")
end)

-- ── build_skill_names ──

case("build_skill_names_empty", function()
  eq(build_skill_names({}), "")
end)

case("build_skill_names_sorted", function()
  local skills = {
    z = { name = "zebra" },
    a = { name = "alpha" },
    m = { name = "middle" },
  }
  local result = build_skill_names(skills)
  assert(result:find("alpha"), "should contain alpha")
  assert(result:find("middle"), "should contain middle")
  assert(result:find("zebra"), "should contain zebra")
  local alpha_pos = result:find("alpha")
  local middle_pos = result:find("middle")
  local zebra_pos = result:find("zebra")
  assert(alpha_pos < middle_pos, "alpha should come before middle")
  assert(middle_pos < zebra_pos, "middle should come before zebra")
end)

case("build_skill_names_excludes_manual_only_by_default", function()
  local skills = {
    a = { name = "alpha", manual_only = false },
    b = { name = "beta", manual_only = true },
  }
  local result = build_skill_names(skills)
  assert(result:find("alpha"), "normal skill should be listed")
  assert(not result:find("beta"), "manual-only skill should be hidden by default")
end)

case("build_skill_names_includes_manual_only_when_requested", function()
  local skills = {
    a = { name = "alpha", manual_only = false },
    b = { name = "beta", manual_only = true },
  }
  local result = build_skill_names(skills, true)
  assert(result:find("alpha"), "normal skill should be listed")
  assert(result:find("beta"), "manual-only skill should be included")
end)

-- ── conflict diagnostics ──

case("build_conflict_report_empty", function()
  eq(helpers.build_conflict_report({}), "")
  eq(helpers.build_conflict_report(nil), "")
end)

case("build_conflict_report_lists_shadowed_locations", function()
  local report = helpers.build_conflict_report({
    ["dup-skill"] = {
      winner = "/active/SKILL.md",
      shadowed = { "/shadow-a/SKILL.md", "/shadow-b/SKILL.md" },
    },
  })
  assert(report:find("<skill_conflicts>"), "should open conflicts block")
  assert(report:find("dup%-skill"), "should name the skill")
  assert(report:find("/active/SKILL.md"), "should show active location")
  assert(report:find("/shadow%-a/SKILL.md"), "should show shadowed location")
end)

case("record_skill_conflict_tracks_last_writer_wins", function()
  local conflicts = {}
  helpers.record_skill_conflict(conflicts, "x", { location = "a" }, { location = "b" })
  eq(conflicts.x.winner, "b")
  eq(#conflicts.x.shadowed, 1)
  eq(conflicts.x.shadowed[1], "a")
  helpers.record_skill_conflict(conflicts, "x", { location = "b" }, { location = "c" })
  eq(conflicts.x.winner, "c")
  eq(#conflicts.x.shadowed, 2)
  eq(conflicts.x.shadowed[2], "b")
end)

case("build_skill_stats_formats_cache_hit", function()
  local stats = helpers.build_skill_stats({ cache_hit = true, skill_count = 3, conflict_count = 1 })
  assert(stats:find('cache_hit="true"'), "should mark cache hit")
  assert(stats:find('skills="3"'), "should include skill count")
  assert(stats:find('conflicts="1"'), "should include conflict count")
end)

-- ── progressive loading ──

case("extract_section_returns_named_heading", function()
  local body = "# Title\n\n## Setup\ninstall things\n\n## Run\ndo work"
  local section, err = helpers.extract_section(body, "Setup")
  eq(err, nil)
  eq(section, "install things")
end)

case("extract_section_missing_returns_error", function()
  local _, err = helpers.extract_section("## A\nx", "Missing")
  assert(err and err:find("section not found"), "should report missing section")
end)

case("preview_body_uses_synopsis_when_present", function()
  local preview, truncated = helpers.preview_body("line1\nline2\nline3", 1, "short synopsis")
  eq(preview, "short synopsis")
  eq(truncated, false)
end)

case("preview_body_truncates_without_synopsis", function()
  local preview, truncated = helpers.preview_body("one\ntwo\nthree", 2, nil)
  eq(preview, "one\ntwo")
  eq(truncated, true)
end)

case("frontmatter_parses_tool_policy_fields", function()
  local fm, _ = parse_frontmatter("---\nname: gated\nallowed-tools: read, write\ndisallowed-tools: bash\n---\nBody")
  eq(fm["allowed-tools"][1], "read")
  eq(fm["allowed-tools"][2], "write")
  eq(fm["disallowed-tools"][1], "bash")
end)

case("validate_skill_flags_conflicting_tool_policy", function()
  local issues = helpers.validate_skill({
    name = "gated",
    description = "desc",
    allowed_tools = { "read" },
    disallowed_tools = { "bash" },
    content = "body",
  })
  assert(#issues > 0, "conflicting tool policy should fail validation")
  assert(issues[1]:find("mutually exclusive"), "should explain conflict")
end)

case("build_tool_policy_lines_formats_allow_and_deny", function()
  local text = helpers.build_tool_policy_lines({
    allowed_tools = { "read", "grep" },
  })
  assert(text:find("allowed%-tools"), "should include allowed tools")
  assert(text:find("read"), "should list read")
end)

case("tool_policy_hint_appends_to_list_entries", function()
  local hint = helpers.tool_policy_hint({ allowed_tools = { "read" } })
  assert(hint:find("allowed%-tools"), "hint should mention allowed tools")
end)

-- ── skill policy envelope ──

case("skill_policy_blocks_disallowed_tool", function()
  local policy = require("skill_policy")
  local decision = policy.evaluate({
    name = "gated",
    disallowed_tools = { "bash" },
  }, "bash")
  eq(decision.allowed, false)
  assert(decision.reason:find("disallowed"), "should explain denial")
end)

case("skill_policy_allows_allowlisted_tool", function()
  local policy = require("skill_policy")
  local decision = policy.evaluate({
    name = "gated",
    allowed_tools = { "read", "grep" },
  }, "grep")
  eq(decision.allowed, true)
end)

case("skill_policy_rejects_tool_outside_allowlist", function()
  local policy = require("skill_policy")
  local decision = policy.evaluate({
    name = "gated",
    allowed_tools = { "read" },
  }, "bash")
  eq(decision.allowed, false)
end)

case("skill_policy_normalizes_dashed_tool_names", function()
  local policy = require("skill_policy")
  local decision = policy.evaluate({
    name = "gated",
    allowed_tools = { "code-execution" },
  }, "code_execution")
  eq(decision.allowed, true)
end)

case("skill_policy_matches_mcp_wire_names_to_bare_allowlist", function()
  local policy = require("skill_policy")
  local allowed = policy.evaluate({
    name = "gated",
    allowed_tools = { "read", "grep" },
  }, "docs__read")
  eq(allowed.allowed, true)
  local denied = policy.evaluate({
    name = "gated",
    allowed_tools = { "read" },
  }, "docs__bash")
  eq(denied.allowed, false)
end)

case("skill_policy_matches_canonical_and_legacy_builtin_names", function()
  local policy = require("skill_policy")
  for _, names in ipairs({
    { canonical = "read_file", legacy = "read" },
    { canonical = "edit_file_bulk", legacy = "multi_edit" },
    { canonical = "run_shell", legacy = "bash" },
  }) do
    local canonical_allowed = policy.evaluate({
      name = "gated",
      allowed_tools = { names.canonical },
    }, names.legacy)
    eq(canonical_allowed.allowed, true, names.legacy .. " should match " .. names.canonical)

    local legacy_allowed = policy.evaluate({
      name = "gated",
      allowed_tools = { names.legacy },
    }, names.canonical)
    eq(legacy_allowed.allowed, true, names.canonical .. " should match " .. names.legacy)

    local canonical_denied = policy.evaluate({
      name = "gated",
      disallowed_tools = { names.canonical },
    }, names.legacy)
    eq(canonical_denied.allowed, false, names.legacy .. " should be denied by " .. names.canonical)

    local legacy_denied = policy.evaluate({
      name = "gated",
      disallowed_tools = { names.legacy },
    }, names.canonical)
    eq(legacy_denied.allowed, false, names.canonical .. " should be denied by " .. names.legacy)
  end
end)

case("skill_policy_always_allows_skill_loader_aliases", function()
  local policy = require("skill_policy")
  for _, tool_name in ipairs({ "load_skill", "skill" }) do
    local decision = policy.evaluate({
      name = "gated",
      allowed_tools = { "read_file" },
      disallowed_tools = { tool_name },
    }, tool_name)
    eq(decision.allowed, true, tool_name .. " should remain available")
  end
end)

-- ── ranking and plan ──

case("score_skill_prefers_tag_and_name_matches", function()
  local score = helpers.score_skill({
    name = "agent-review",
    description = "review agent code",
    tags = { "agent", "api" },
  }, "src/api/agent.rs")
  assert(score > 0, "matching skill should have positive score")
end)

case("rank_skills_orders_highest_score_first", function()
  local ranked = helpers.rank_skills({
    low = { name = "low", description = "unrelated" },
    high = { name = "agent-helper", description = "agent workflows", tags = { "agent" } },
  }, "src/api/agent.rs")
  eq(ranked[1].name, "high")
end)

case("extract_plan_collects_sections_and_steps", function()
  local plan, err = helpers.extract_plan("# Title\n\n## Setup\ninstall\n\n1. run tests\n2. ship")
  eq(err, nil)
  assert(plan:find("Setup"), "plan should include section")
  assert(plan:find("run tests"), "plan should include numbered step")
end)

case("frontmatter_parses_structured_steps", function()
  local fm, _ = parse_frontmatter(
    "---\nname: workflow\ndescription: test\nsteps:\n  - name: Setup\n    section: Setup\n    tools: read, bash\n---\nBody"
  )
  eq(type(fm.steps), "table")
  eq(#fm.steps, 1)
  eq(fm.steps[1].name, "Setup")
  eq(fm.steps[1].section, "Setup")
  eq(fm.steps[1].tools[1], "read")
end)

case("build_plan_prefers_structured_steps", function()
  local plan, err = helpers.build_plan({
    name = "workflow",
    steps = {
      { name = "Setup", section = "Setup", tools = { "read" } },
    },
  }, nil)
  eq(err, nil)
  assert(plan:find("1. Setup"), "structured plan should number steps")
  assert(plan:find("tools: read"), "structured plan should list tools")
end)

case("graph_rank_bonus_only_for_path_scoped_skills", function()
  local signals = { arbor_indexed = true, codegraph_indexed = true }
  eq(helpers.graph_rank_bonus({ paths = { "src/**" } }, signals), 8)
  eq(helpers.graph_rank_bonus({ name = "generic" }, signals), 0)
end)

case("score_skill_adds_graph_bonus_when_enabled", function()
  local score = helpers.score_skill(
    {
      name = "scoped",
      description = "scoped helper",
      paths = { "src/**" },
      tags = { "src" },
    },
    "src/api/agent.rs",
    {
      graph_rank = true,
      signals = { arbor_indexed = true, codegraph_indexed = false },
    }
  )
  local baseline = helpers.score_skill({
    name = "scoped",
    description = "scoped helper",
    paths = { "src/**" },
    tags = { "src" },
  }, "src/api/agent.rs")
  assert(score > baseline, "graph rank should increase score")
end)

case("skill_telemetry_build_summary_formats_path", function()
  local telemetry = require("skill_telemetry")
  local summary = telemetry.build_summary("/tmp/events.jsonl")
  assert(summary:find("<skill_telemetry>"), "summary should be tagged")
  assert(summary:find("/tmp/events.jsonl"), "summary should include path")
end)

-- ── builtin plugin_dev skill ──

case("builtin_plugin_dev_skill_loads", function()
  local builtin = require("plugin_dev")
  eq(builtin.name, "n00n-plugin-dev")
  assert(#builtin.description > 0, "description should not be empty")
  assert(builtin.content:find("# Writing n00n plugins", 1, true), "content should contain the authoring guide")
  assert(#builtin.reference_placeholder > 0, "reference_placeholder should not be empty")
  assert(
    builtin.content:find(builtin.reference_placeholder, 1, true),
    "content should contain the reference path placeholder"
  )
  assert(builtin.content:find("n00n.api.register_tool", 1, true), "index should list register_tool")
end)

case("builtin_plugin_dev_reference_loads", function()
  local ref = require("plugin_dev_reference")
  assert(ref.content:find("### `n00n.api.register_tool", 1, true), "reference should document register_tool")
  assert(ref.content:find("Shared helper modules", 1, true), "reference should include helper modules")
end)

case("builtin_index_line_numbers_match_reference", function()
  local builtin = require("plugin_dev")
  local ref = require("plugin_dev_reference")
  local lines = n00n.split(ref.content, "\n")
  local checked = 0
  for ln, sig in builtin.content:gmatch("\n%- L(%d+) `([^`]+)`") do
    local target = lines[tonumber(ln)]
    local heading = "### `" .. sig .. "`"
    assert(
      target and target:find(heading, 1, true) == 1,
      "index L" .. ln .. " should point at " .. heading .. ", got: " .. tostring(target)
    )
    checked = checked + 1
  end
  assert(checked > 100, "expected the index to cover the reference, checked " .. checked)
end)

case("path_matches_pattern_matches_nested_paths", function()
  local root = "/tmp/project"
  assert(helpers.path_matches_pattern("/tmp/project/src/api/agent.rs", "src/**", root))
  assert(helpers.path_matches_pattern("/tmp/project/docs/guide.md", "src/**", root) == false)
end)

case("skill_fingerprint_includes_content_digest", function()
  local tmpdir = "/tmp/n00n_spec_skill_fp_" .. tostring(os.clock()):gsub("%.", "")
  n00n.fs.mkdir(tmpdir)
  local path = n00n.fs.joinpath(tmpdir, "SKILL.md")
  n00n.fs.write(path, "body")
  local fp = helpers.skill_fingerprint(path)
  assert(fp, "fingerprint should exist")
  local parts = {}
  for part in fp:gmatch("[^:]+") do
    parts[#parts + 1] = part
  end
  eq(#parts, 4, "fingerprint should include path, mtime, size, digest")
  n00n.fs.rm(tmpdir, { recursive = true })
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
