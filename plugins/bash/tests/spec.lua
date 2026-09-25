local command_guard = require("command_guard")
local output_collector = require("output_collector")

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

local function has(s, substr, msg)
  if not s:find(substr, 1, true) then
    error((msg or "") .. "\nexpected to contain: " .. tostring(substr) .. "\n  actual: " .. tostring(s))
  end
end

-- broad_bash_command_reason: git log/reflog/rev-list count bounds (defect: the
-- `-<N>` shorthand was not recognized as a bound).

case("git_log_dash_n_shorthand_is_bounded", function()
  eq(command_guard.broad_bash_command_reason("git log --oneline -20"), nil)
end)

case("git_log_attached_dash_n_is_bounded", function()
  eq(command_guard.broad_bash_command_reason("git log -n5 --oneline"), nil)
end)

case("git_log_without_a_bound_is_rejected", function()
  local reason = command_guard.broad_bash_command_reason("git log --oneline")
  has(reason, "history without a max count")
end)

case("git_reflog_and_rev_list_share_the_bound_check", function()
  eq(command_guard.broad_bash_command_reason("git reflog -10"), nil)
  eq(command_guard.broad_bash_command_reason("git rev-list --max-count=5 HEAD"), nil)
end)

-- broad_bash_command_reason: rg output caps (defect: only `| head`/`| tail`
-- were recognized; `rg --max-depth` was rejected despite being self-bounded).

case("rg_max_depth_is_bounded", function()
  eq(command_guard.broad_bash_command_reason("rg --max-depth 1 needle ."), nil)
end)

case("rg_piped_through_head_is_bounded", function()
  eq(command_guard.broad_bash_command_reason("rg needle . | head -n 20"), nil)
end)

case("rg_without_a_bound_is_rejected", function()
  local reason = command_guard.broad_bash_command_reason("rg needle .")
  has(reason, "unbounded result size")
  has(reason, "--max-depth")
end)

-- `-m`/`--max-count` cap matches per file, not the overall result size, so
-- they must not satisfy the guardrail on their own.
case("rg_max_count_alone_is_still_rejected", function()
  local reason = command_guard.broad_bash_command_reason("rg --max-count=5 needle .")
  has(reason, "unbounded result size")
end)

case("git_grep_is_always_rejected_without_a_pipe", function()
  local reason = command_guard.broad_bash_command_reason("git grep needle")
  has(reason, "git grep without result limit")
end)

-- git_subcommand: must resolve past global options injected ahead of the
-- subcommand (defect: the old `cmd:match("^git%s+(%S+)")` captured the first
-- global flag instead of the real subcommand).

case("git_subcommand_skips_injected_global_options", function()
  eq(command_guard.git_subcommand("git --no-optional-locks -c core.fsmonitor=false log --oneline -5"), "log")
end)

case("git_subcommand_skips_arg_taking_global_options", function()
  eq(command_guard.git_subcommand("git -C /tmp log"), "log")
end)

case("git_subcommand_plain", function()
  eq(command_guard.git_subcommand("git config --get remote.origin.url"), "config")
end)

case("git_subcommand_nil_for_non_git", function()
  eq(command_guard.git_subcommand("gh pr list"), nil)
end)

-- Short options cluster in one word: `du -sh` is the same bound as `du -s`,
-- `tree -L2` as `tree -L 2`, and `ls -lR` as `ls -R`. `has_option` only saw
-- standalone words, so `du -sh`/`tree -L2` were rejected as unbounded while
-- `ls -laR` slipped past the recursive-ls guard.

case("du_short_option_cluster_is_a_summarize_bound", function()
  eq(command_guard.broad_bash_command_reason("du -sh ."), nil)
  eq(command_guard.broad_bash_command_reason("du -d1 ."), nil)
end)

case("tree_short_option_cluster_is_a_depth_bound", function()
  eq(command_guard.broad_bash_command_reason("tree -L2 ."), nil)
end)

case("ls_recursive_short_option_cluster_needs_an_output_cap", function()
  has(command_guard.broad_bash_command_reason("ls -lR ."), "recursive ls without output cap")
  has(command_guard.broad_bash_command_reason("ls -laR /tmp"), "recursive ls without output cap")
end)

-- A short option that takes an argument consumes the rest of its cluster:
-- `journalctl -unginx.service` is `-u nginx.service`, not a `-n` line bound.
-- Option letters are case-sensitive: `du -D` dereferences, it is not `-d`.

case("journalctl_attached_unit_argument_is_not_a_line_bound", function()
  has(command_guard.broad_bash_command_reason("journalctl -unginx.service"), "journalctl without tail line bound")
  eq(command_guard.broad_bash_command_reason("journalctl -unginx.service -n 50"), nil)
  eq(command_guard.broad_bash_command_reason("journalctl -fn20"), nil)
end)

case("git_log_attached_pickaxe_argument_is_not_a_count_bound", function()
  has(command_guard.broad_bash_command_reason("git log -Sfunction"), "history without a max count")
end)

case("du_uppercase_dereference_is_not_a_depth_bound", function()
  has(command_guard.broad_bash_command_reason("du -D ."), "du without depth/summarize bound")
end)

case("tree_attached_pattern_argument_is_not_a_depth_bound", function()
  has(command_guard.broad_bash_command_reason("tree -IL ."), "tree without depth bound")
end)

-- A standalone argument-taking option consumes the next word, and `--` ends
-- option parsing: `tree -I -Lignored .` ignores the pattern `-Lignored`, and
-- `tree -- -Lfolder` lists a directory named `-Lfolder`.

case("tree_pattern_argument_word_is_not_a_depth_bound", function()
  has(command_guard.broad_bash_command_reason("tree -I -Lignored ."), "tree without depth bound")
  has(command_guard.broad_bash_command_reason("tree -I -L ."), "tree without depth bound")
  eq(command_guard.broad_bash_command_reason("tree -I target -L 2 ."), nil)
end)

case("tree_operand_after_double_dash_is_not_a_depth_bound", function()
  has(command_guard.broad_bash_command_reason("tree -- -Lfolder"), "tree without depth bound")
  has(command_guard.broad_bash_command_reason("tree -- -L"), "tree without depth bound")
  eq(command_guard.broad_bash_command_reason("tree -L 2 -- -Lfolder"), nil)
end)

case("journalctl_unit_argument_word_is_not_a_line_bound", function()
  has(command_guard.broad_bash_command_reason("journalctl -u -n"), "journalctl without tail line bound")
end)

case("ls_lowercase_r_reverse_is_not_recursive", function()
  eq(command_guard.broad_bash_command_reason("ls -r ."), nil)
end)

case("git_uses_machine_format_detects_porcelain", function()
  eq(command_guard.git_uses_machine_format("git worktree list --porcelain"), true)
  eq(command_guard.git_uses_machine_format("git status"), false)
end)

-- output_collector: the LLM-facing accumulator must stay bounded while
-- streaming, not just at the final truncate() call.

case("output_collector_caps_stored_bytes_while_streaming", function()
  local collector = output_collector.new()
  for _ = 1, 100 do
    output_collector.append_line(collector, string.rep("x", 200), 500, 1024)
  end
  local stored = 0
  for _, part in ipairs(collector.parts) do
    stored = stored + #part
  end
  if stored > 1024 + 4096 then
    error("collector stored " .. stored .. " bytes, expected it capped near the max_bytes budget")
  end
end)

case("output_collector_reports_full_truncated_byte_count", function()
  local collector = output_collector.new()
  for _ = 1, 50 do
    output_collector.append_line(collector, string.rep("y", 100), 500, 256)
  end
  local output = output_collector.collected_output(collector, 500, 256)
  has(output, "[truncated ")
end)

case("output_collector_returns_full_text_under_the_cap", function()
  local collector = output_collector.new()
  output_collector.append_line(collector, "line one", 500, 1024)
  output_collector.append_line(collector, "line two", 500, 1024)
  local output = output_collector.collected_output(collector, 500, 1024)
  eq(output, "line one\nline two")
end)

if #failures > 0 then
  error(#failures .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"))
end
