Fix Lua API docs build with Zola 0.23: wrap generated markdown in Tera raw block to prevent `{#` and `{{` from being parsed as template syntax, which broke `zola build` on CI after the 0.23 upgrade.
Fix tool token analysis script to handle current zstd-compressed JSONL session format at `~/.local/state/n00n/sessions/*.jsonl` in addition to legacy `~/.n00n/sessions/*.json`.
