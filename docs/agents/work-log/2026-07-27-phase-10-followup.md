# Work log — 2026-07-27

## Session

Continued finishing the stacked `phase-10-followup` work in the n00n worktree on branch `stack/phase-10-followup` (PR #170). Addressed the remaining P1/P2 review findings from the CodeRabbit/Codex passes.

## Changes

- `justfile`: tolerate `codegraph --version` failures in `explore-health`.
- `n00n-arbor/src/graph_json.rs`: cleaned imports, mapped `read_to_string` errors to `ArborError::Io`, made `resolve_symbol` file filtering path-component-safe with `Path::ends_with`.
- `n00n-arbor/src/graph_query.rs`: imported `Client`/`index_health`, replaced `u64::try_from(...).ok()` with an explicit fallback while allowing the clippy lint.
- `n00n-arbor/src/index_health.rs`: stopped swallowing `arbor status` spawn failures as "fresh"; file I/O errors now use `ArborError::Io`.
- `n00n-arbor/src/lib.rs`: added `Io` variant, reused `index_health::status_needs_index`.
- `n00n-codegraph/src/lib.rs`: fixed `Error` import, extracted `CODEGRAPH_BINARY`, drained stdout/stderr concurrently, cleaned up timeout kill/wait/join error paths.
- `n00n-daemon/src/lock.rs`: implemented non-Unix `pid_alive` with `tasklist` and conservative alive fallback.
- `n00n-lua/src/api/codegraph.rs`: used `Path`, returned `(value, err)` pairs from `check_binary` and `explore`, updated module docs.
- `plugins/codegraph/init.lua`: consumed the new `(output, err)` return.
- `plugins/explore/init.lua`: consumed `call_tool`'s string return as `output, err`, disabled caching by default, regenerated description so docs include the backend list.
- `plugins/explore/router.lua`: case-preserving symbol extraction, injective cache keys, file-extension query routing, cleaner relations routing, explicit `command` priority over extension heuristic.
- `plugins/explore/tests/spec.lua`: added cases for routing, case preservation, cache-key injectivity and stability, file-path detection.
- `plugins/arbor/init.lua`: added `graph_index_available` guards for `map`/`diff`.
- `n00n-docgen` regenerated `site/docs/content/tools/_index.md` and `site/docs/content/lua-api/_index.md`.
- `spikes/phase0.1-arbor-json/src/main.rs`: accept graph path as CLI argument.
- `src/cmd/agent.rs`: explicit EOF handling, propagate read errors, suppress non-Unix unused binding warnings.

## Verification

- `cargo fmt --all`
- `cargo clippy --all --all-features -- -D warnings` — clean
- `cargo nextest run --workspace` — 4040 passed, 1 skipped
- `cargo run -p n00n-docgen -- --check` — all docs up to date
- `stylua plugins/`
- `cargo deny check` — pre-existing license/advisory failures unrelated to this diff (unlicensed internal crates, deprecated `AGPL-3.0` identifier, and unmaintained transitive crates such as `bincode`, `yaml-rust`, `atomic-polyfill`).
