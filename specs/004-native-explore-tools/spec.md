# Feature Specification: Native Explore Tools

**Feature Branch**: `004-native-explore-tools`

**Created**: 2026-07-27

**Status**: Draft

**Input**: User description: "Bundle arbor, codegraph, and semble into n00n natively, as native agent tools."

---

## User Scenarios & Testing

### User Story 1 - Native `arbor` code-graph queries (Priority: P1)

A user analyzing a codebase wants `arbor` to work without installing a separate `arbor` CLI, so that caller/callee, map, diff, and query results are available out-of-the-box in n00n.

**Why this priority**: `arbor` is already a built-in n00n tool, but it shells out to an external binary per call. Removing that dependency makes the tool faster, removes an install barrier, and keeps the same workflow.

**Independent Test**: Run `arbor` callers, callees, map, diff, and query on a fixture repository on a machine with no `arbor` binary on `PATH`.

**Acceptance Scenarios**:

1. **Given** a project with source files and no `arbor` CLI installed, **When** the user invokes `arbor` with command `callers` and a symbol, **Then** n00n returns the list of callers without spawning an external process.
2. **Given** an unindexed project, **When** the user invokes `arbor`, **Then** n00n indexes it automatically, shows a moving status indicator, and then answers the query.
3. **Given** a previously indexed project, **When** the user invokes `arbor map` with a token budget, **Then** the returned ranked skeleton matches the structure produced by the external `arbor map` command.

---

### User Story 2 - Native `codegraph` cross-file exploration (Priority: P1)

A user exploring cross-file relationships wants `codegraph` to run in-process, so that `codegraph explore`, callers, callees, and impact queries work without the Node `codegraph` CLI.

**Why this priority**: `codegraph` currently shells out to the `codegraph explore` command. In-process execution reduces latency and removes the Node runtime dependency.

**Independent Test**: Run `codegraph` with a natural-language query on a project that has a `.codegraph/` index, on a machine without the `codegraph` CLI.

**Acceptance Scenarios**:

1. **Given** a project with a `.codegraph/` index and no `codegraph` CLI installed, **When** the user invokes `codegraph` with a query, **Then** n00n returns verbatim source grouped by file plus a blast-radius summary.
2. **Given** an out-of-date `.codegraph/` index, **When** the user invokes `codegraph`, **Then** n00n re-indexes the project automatically, shows a moving status indicator, and then answers the query.
3. **Given** a project with no `.codegraph/` index, **When** the user invokes `codegraph`, **Then** n00n creates the index automatically and answers the query.

---

### User Story 3 - Built-in `semblem` semantic code search (Priority: P2)

A user searching for code by meaning wants `semblem` to be a built-in n00n tool, so that natural-language queries return relevant code snippets without an external Python `semblem` CLI or MCP server.

**Why this priority**: `semblem` is currently only available as an MCP server or CLI. Making it a native built-in tool unifies the tool surface and removes the Python runtime dependency.

**Independent Test**: Run `semblem search` with a natural-language query on a repository on a machine without the `semblem` CLI.

**Acceptance Scenarios**:

1. **Given** a repository with supported source files, **When** the user invokes `semblem search` with a query and no embedder is configured, **Then** n00n returns BM25-ranked snippets without making network calls.
2. **Given** a user who wants semantic/hybrid search, **When** they invoke `semblem` with `semantic = true` and no embedder configured, **Then** n00n nags the user with a clear message offering a local model (vLLM Podman presets) or a remote OpenAI-compatible endpoint, and falls back to BM25.
3. **Given** a configured local or remote embedder, **When** the user invokes `semblem search`, **Then** n00n returns hybrid BM25 + semantic ranked snippets.
4. **Given** a source file and a target line, **When** the user invokes `semblem find_related`, **Then** n00n returns code related to that location.

---

### User Story 4 - Reduced tool-dependency overhead (Priority: P3)

A user installing or distributing n00n wants the agent to ship with code-intelligence tooling built in, so that setup guides no longer require `cargo install arbor-graph-cli`, `npm install -g codegraph`, or `uvx --from semble[mcp] semblem`.

**Why this priority**: Fewer external dependencies improves first-run experience and reproducibility across environments.

**Independent Test**: Build n00n on a clean container and verify `arbor`, `codegraph`, and `semblem` tool definitions are present and functional without external binaries.

**Acceptance Scenarios**:

1. **Given** a fresh environment with only Rust build tools, **When** n00n is built and run, **Then** the tools `arbor`, `codegraph`, and `semblem` are listed in the tool set.
2. **Given** a fresh environment, **When** `cargo test -p n00n-lua` and `cargo test -p n00n-agent` run, **Then** the native tool tests pass without `arbor`, `codegraph`, or `semblem` on `PATH`.

---

### Edge Cases

- What happens when a repository contains no supported source files? The tool returns an empty result with a clear message instead of an opaque error.
- What happens when indexing fails partway through? The tool reports the failure, leaves the prior index intact if possible, and does not crash the agent loop.
- What happens when a user requests semantic search but no embedder is configured? The tool nags the user with local/remote setup options and falls back to BM25.
- What happens when a configured embedder is unreachable? The tool falls back to BM25 keyword search and reports the fallback.
- What happens when two tools are asked to index the same project concurrently? Indexing operations are serialized per project or use filesystem locks to avoid corrupt indexes.
- What happens when a user passes a project path outside the current workspace? The tool follows existing n00n permission and sandbox rules.

---

## Requirements

### Functional Requirements

- **FR-001**: `arbor` MUST be a built-in n00n tool that does not require an external `arbor` CLI on `PATH`.
- **FR-002**: `arbor` MUST continue to support the commands `callers`, `callees`, `map`, `diff`, `query`, and `status`.
- **FR-003**: `arbor` MUST build or load its project index in-process when invoked and show a moving status indicator while indexing.
- **FR-004**: `codegraph` MUST be a built-in n00n tool that does not require an external `codegraph` CLI on `PATH`.
- **FR-005**: `codegraph` MUST continue to support natural-language `explore` queries and, where feasible, `callers`, `callees`, and `impact` sub-commands.
- **FR-006**: `codegraph` MUST query the `.codegraph/` SQLite index in-process using a modern, performant Rust SQLite layer (e.g., `rusqlite` with WAL/FTS5).
- **FR-007**: `semblem` MUST be a built-in n00n tool that does not require an external `semblem` CLI or MCP server.
- **FR-008**: `semblem` MUST support `search` and `find_related` commands.
- **FR-009**: `semblem` MUST build or load its project index in-process when invoked and show a moving status indicator while indexing.
- **FR-010**: `semblem` MUST default to BM25-only search and MUST NOT bundle external API keys, providers, or models.
- **FR-011**: `semblem` MUST nag the user to configure an embedder when semantic/hybrid search is requested and no embedder is configured, offering local vLLM Podman presets (3 memory/performance levels) and a remote OpenAI-compatible option.
- **FR-012**: All three tools MUST preserve their existing input schemas and output contracts so existing prompts and scripts continue to work.
- **FR-013**: All three tools MUST respect `output_limits` and `ExploreResult` rendering like other built-in explore tools.
- **FR-014**: Tool definitions MUST not include installation instructions for external binaries in their descriptions.
- **FR-015**: The implementation MUST segment responsibilities into appropriate crates, including a reusable `n00n-search` core for indexing, BM25, and embedder orchestration, and tool-specific crates (`n00n-arbor`, `n00n-codegraph`, `n00n-semble`) for tool logic and Lua bindings.

### Key Entities

- **Arbor Graph**: Code entities (nodes) and relationships (edges) extracted from source files, including centrality scores and impact metadata.
- **CodeGraph Index**: A SQLite database under `.codegraph/` containing `nodes`, `edges`, `files`, `unresolved_refs`, and an FTS5 virtual table.
- **Search Index**: A unified BM25 + optional semantic index of source-file chunks, managed by `n00n-search`.
- **Embedder Config**: A user's choice of embedder source (none, local static, local vLLM, remote OpenAI-compatible) and any required model/URL/key.
- **Vllm Preset**: A preconfigured Podman vLLM runtime definition at one of three memory/performance levels (light, medium, heavy) for local embedding serving.
- **Tool Output Card**: A live UI buffer used by `arbor`, `codegraph`, and `semblem` to display ranked, truncated results and indexing progress.

---

## Success Criteria

### Measurable Outcomes

- **SC-001**: `arbor`, `codegraph`, and `semblem` each return correct results on a fixture repository in 100% of targeted test cases.
- **SC-002**: Tool call latency for `arbor` and `codegraph` queries on the n00n repo is no worse than the external-CLI baseline.
- **SC-003**: The combined token size of the three tool definitions is no larger than the current baseline after removing external-installation notes.
- **SC-004**: `cargo test -p n00n-lua` and `cargo test -p n00n-agent` pass without any of `arbor`, `codegraph`, or `semblem` installed on `PATH`.
- **SC-005**: A clean build of n00n exposes the three tools in its default tool set.
- **SC-006**: `semblem` with no embedder configured returns BM25 results without network calls or bundled API keys.
- **SC-007**: The vLLM Podman preset generator produces runnable commands for three memory/performance levels.

---

## Assumptions

- Existing `.arbor/`, `.codegraph/`, and `semblem` index formats can be read or rebuilt by in-process Rust libraries.
- The n00n binary may grow in size due to new parser/index dependencies; feature flags and crate separation will be used to manage bloat.
- `semblem` semantic search requires explicit user configuration of an embedder; no cloud providers or API keys are bundled.
- Local vLLM serving is optional; Podman runtime commands are generated for users who choose self-hosted embeddings.
- The Lua plugin layer remains the primary tool registration mechanism; Rust tools will be exposed through `n00n.<tool>` tables.
