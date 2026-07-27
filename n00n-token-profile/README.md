# n00n-token-profile

Offline cold-start token profiling and CI regression gates.

## What it measures

| Surface | Gate | Notes |
|---------|------|-------|
| `main_tools_schemas` | hard | `{name,input_schema}` only, sorted; exact tool_count |
| `system_prompt` | hard | pinned Vars, empty instructions/slots, Build mode |
| `main_tools_payload` | soft | full `definitions_active` (includes `code_execution` describe); stderr warn only |
| `cache_prefix` | soft | non-dynamic system blocks + schemas; stderr warn only |

Fixture matches production cold-start: `ToolFilter::from_config`, `ActiveTools::default()`, fresh registry + builtins. MCP and live AGENTS.md are excluded.

Token counts use n00n's tiktoken estimator (`count_*_for_model`), not provider billing.

## Commands

```bash
cargo test -p n00n-token-profile
cargo run -p n00n-token-profile --example write_baseline   # intentional growth only
```

Update `baselines/cold_start.json` in the same PR when a hard surface grows on purpose.
