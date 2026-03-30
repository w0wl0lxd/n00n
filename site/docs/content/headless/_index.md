+++
title = "Headless Mode"
weight = 8
[extra]
group = "Reference"
+++

# Headless Mode

Run Maki non-interactively with `--print`. Useful for scripts, CI, and automation.

```bash
maki "explain this codebase" --print
```

Pipe via stdin:

```bash
echo "list all TODO comments" | maki --print
```

## Output Formats

| Format | Description |
|--------|-------------|
| `text` | Raw response only (default) |
| `json` | Single JSON object with metadata |
| `stream-json` | JSONL stream, one event per line |

```bash
maki "fix the tests" --print --output-format json
```

JSON output includes `type`, `subtype`, `is_error`, `duration_ms`, `num_turns`, `result`, `stop_reason`, `session_id`, `total_cost_usd`, and `usage`.

Add `--verbose` to include full turn-by-turn messages in the output.

## Claude Code Compatibility

Maki's `--print` is a drop-in replacement for Claude Code:

```bash
# Before
claude "fix the bug" --print --output-format json

# After
maki "fix the bug" --print --output-format json
```

Same JSON fields, same `--output-format` options, same `--verbose` behavior. Scripts that parse Claude Code output work unchanged.

Difference: ~40% fewer tokens used on average.

## Examples

```bash
# CI check
maki "check for security issues" --print --output-format json | jq -e '.is_error == false'

# Batch processing
for file in src/*.rs; do
  maki "add doc comments to $file" --print --yolo
done

# Cost tracking
maki "refactor this" --print --output-format json | jq '.total_cost_usd'
```
