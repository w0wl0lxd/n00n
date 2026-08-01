# Feature Specification: Explore Tooling Enhancement

**Feature Branch**: `009-explore-tooling-enhancement`

**Created**: 2026-08-01

**Status**: Draft

**Input**: User description: "expand, refine, and further optimize + enhance the Codegraph, Semble, and Arbor integrations and tooling, as well as fully working with rtk where needed, and ensure they're used as a first tier codebase solution alongside the index tool"

## User Scenarios & Testing

### User Story 1 - Smarter explore router (Priority: P1)

A user asking a natural-language question about a codebase wants the agent to automatically pick the right exploration backend without manual tool selection, so that one `explore` call routes to index for file skeletons, arbor for relations, codegraph for cross-file structure, or semblem for keyword search.

**Why this priority**: The explore router is the primary entry point for codebase questions. Making it smarter reduces the cognitive load on the agent and users, ensures the right tool is used for each query type, and establishes explore as the default first-tier exploration interface.

**Independent Test**: Can be fully tested by running explore with various query patterns (file paths, relation keywords, natural-language questions) and verifying the correct backend is selected and returns appropriate results.

**Acceptance Scenarios**:

1. **Given** a user query that is a file path or contains a file extension, **When** the user invokes `explore` with `intent = "auto"`, **Then** the router routes to `index` and returns a single-file skeleton.
2. **Given** a user query containing relation keywords like "callers", "callees", "trace path", or "map", **When** the user invokes `explore` with `intent = "auto"`, **Then** the router routes to `arbor` with the appropriate command.
3. **Given** a user query that is a natural-language question about cross-file structure, **When** the user invokes `explore` with `intent = "auto"`, **Then** the router routes to `codegraph` and returns grouped source snippets.
4. **Given** a user query that is a keyword or natural-language search, **When** the user invokes `explore` with `intent = "search"`, **Then** the router routes to `semblem` and returns ranked code snippets.

---

### User Story 2 - First-tier prompts and tool descriptions (Priority: P1)

A user wants the agent to consistently use explore, index, arbor, codegraph, and semblem as the primary exploration tools before falling back to grep and bash, so that token-efficient structural tools are prioritized over broad searches.

**Why this priority**: Prompt engineering is the primary way to guide agent behavior. Updating the tool descriptions and prompt sequence establishes these tools as the default exploration tier, reducing unnecessary grep calls and improving token efficiency.

**Independent Test**: Can be fully tested by reviewing the updated `NATIVE_EFFICIENT_TOOLS` list, prompt hints, and tool descriptions to verify they consistently position explore/index/arbor/codegraph/semblem before grep/bash.

**Acceptance Scenarios**:

1. **Given** the agent's default prompt sequence, **When** the agent is initialized, **Then** the `NATIVE_EFFICIENT_TOOLS` list includes explore, index, arbor, codegraph, and semblem without "optional" qualifiers.
2. **Given** the agent's tool usage hints, **When** the agent receives a codebase question, **Then** the hints explicitly recommend explore first, then index, then arbor/codegraph/semblem, then read, then grep/bash.
3. **Given** the tool descriptions for explore, arbor, codegraph, and semblem, **When** the agent considers which tool to use, **Then** the descriptions clearly position them as first-tier exploration tools without mentioning external CLI installation.

---

### User Story 3 - CodeGraph expansion to 1.5.0 (Priority: P2)

A user wants to use CodeGraph for callers, callees, impact, affected, node, query, and sync operations beyond the current explore-only support, so that cross-file structural queries are fully supported through the native SQLite index.

**Why this priority**: CodeGraph 1.5.0 introduces a Rust engine with near-instant sync and additional commands. Exposing these commands makes CodeGraph a more complete structural exploration tool and reduces dependency on the external CLI.

**Independent Test**: Can be fully tested by invoking the new CodeGraph commands (callers, callees, impact, node, query, sync) through the n00n-codegraph API and verifying correct results against the CLI output.

**Acceptance Scenarios**:

1. **Given** a project with a `.codegraph/` index, **When** the user invokes `codegraph` with command `callers` and a symbol, **Then** n00n returns the list of callers by querying the native SQLite database.
2. **Given** a project with a stale `.codegraph/` index, **When** the user invokes `codegraph` with command `sync`, **Then** n00n triggers a re-index using the CodeGraph 1.5.0 CLI.
3. **Given** a project with a `.codegraph/` index, **When** the user invokes `codegraph` with command `node` and a node ID, **Then** n00n returns the node details from the SQLite database.
4. **Given** a user invoking any codegraph command, **When** the CodeGraph 1.5.0 CLI is unavailable, **Then** n00n falls back to the native SQLite queries where supported or returns a clear error.

---

### User Story 4 - Arbor expansion (Priority: P2)

A user wants to use Arbor for entry-points, file-graph, inspect, path, refactor, check, and summary operations beyond the current callers/callees/map/diff/query/status support, so that project orientation and refactoring workflows are more complete.

**Why this priority**: Arbor 2.5.0 already supports these commands. Exposing them through n00n-arbor makes Arbor a more comprehensive code-graph tool and reduces the need to drop to the external CLI for advanced workflows.

**Independent Test**: Can be fully tested by invoking the new Arbor commands (entry-points, file-graph, inspect, path, refactor, check, summary) through the n00n-arbor API and verifying correct results against the CLI output.

**Acceptance Scenarios**:

1. **Given** a project with an `.arbor/` index, **When** the user invokes `arbor` with command `entry-points`, **Then** n00n returns the list of entry points by calling the Arbor CLI.
2. **Given** a project with an `.arbor/` index, **When** the user invokes `arbor` with command `file-graph` and a file path, **Then** n00n returns the file-level graph by calling the Arbor CLI.
3. **Given** a user invoking any arbor command, **When** the Arbor CLI is unavailable, **Then** n00n falls back to the native in-memory `graph.json` for callers/callees/trace_path or returns a clear error for unsupported commands.

---

### User Story 5 - Semblem hybrid with upstream CLI (Priority: P3)

A user wants semantic search with the upstream `semble` CLI for remote git URLs, `--content docs/config/all`, and `find-related` operations, while keeping the native `n00n-search` BM25 index as a fallback for offline cases.

**Why this priority**: The upstream `semble` CLI v0.5.1 supports features not available in the native wrapper (remote URLs, content filters, savings). Wrapping it while keeping BM25 fallback provides the best of both worlds: advanced semantic search when available, offline keyword search when not.

**Independent Test**: Can be fully tested by invoking semblem with the upstream CLI commands (search with remote URL, find-related, savings) and verifying correct results, then testing BM25 fallback when the CLI is unavailable.

**Acceptance Scenarios**:

1. **Given** a remote git URL, **When** the user invokes `semblem` with command `search` and the URL, **Then** n00n calls the upstream `semble` CLI with the URL and returns ranked snippets.
2. **Given** a local project, **When** the user invokes `semblem` with command `search` and `--content docs`, **Then** n00n calls the upstream `semble` CLI with the content filter and returns documentation snippets.
3. **Given** a user invoking semblem without the upstream CLI, **When** no embedder is configured, **Then** n00n falls back to the native `n00n-search` BM25 index and returns keyword results.
4. **Given** a user invoking semblem with mode `hybrid` or `semantic`, **When** no embedder is configured, **Then** n00n nags with vLLM preset options and falls back to BM25.

---

### User Story 6 - RTK hardening for bash (Priority: P3)

A user wants bash commands to be automatically rewritten through `rtk` for token-efficient output, with session-cached availability detection and broader command coverage, so that shell output is consistently compressed without manual intervention.

**Why this priority**: RTK is already integrated in the bash plugin but can be hardened with session caching, broader coverage, and clearer prompt hints. This makes token-efficient shell output the default behavior for all supported commands.

**Independent Test**: Can be fully tested by invoking bash with various commands (git, cargo, rg, grep, find, ls) and verifying rtk rewriting occurs, checking that `jq`/`yq` pass through unchanged, and confirming availability is cached per session.

**Acceptance Scenarios**:

1. **Given** a bash session with rtk installed, **When** the user invokes `bash` with a git command, **Then** the command is rewritten through `rtk` and output is compressed.
2. **Given** a bash session with rtk installed, **When** the user invokes `bash` with `jq` or `yq`, **Then** the command passes through unchanged.
3. **Given** a bash session, **When** rtk availability is checked once, **Then** the result is cached for the duration of the session and not re-checked on subsequent calls.
4. **Given** the agent's prompt hints, **When** the agent considers shell commands, **Then** the hints explicitly recommend rtk-wrapped bash for git, cargo, rg, grep, and other verbose commands.

---

### Edge Cases

- What happens when the explore router cannot determine the intent from the query? The router defaults to `cross_file` (codegraph) and the agent can override with explicit `intent` parameter.
- What happens when CodeGraph 1.5.0 CLI is unavailable but the SQLite database exists? Native queries work for supported commands; unsupported commands return a clear error suggesting CLI installation.
- What happens when Arbor CLI is unavailable but `graph.json` exists? Native in-memory queries work for callers/callees/trace_path; unsupported commands return a clear error.
- What happens when the upstream `semble` CLI is unavailable? The tool falls back to native `n00n-search` BM25 and returns keyword results.
- What happens when rtk is not installed? Bash commands run normally without rewriting; no error is raised.
- What happens when multiple tools attempt to index the same project concurrently? Each tool uses its own lock file (`.codegraph/.lock`, `.arbor/.lock`, `.n00n/search/.lock`) to serialize indexing operations.
- What happens when a user requests semantic search but no embedder is configured? The tool nags with vLLM preset options and a remote OpenAI-compatible option, then falls back to BM25.

## Requirements

### Functional Requirements

- **FR-001**: The explore router MUST support intents: `auto`, `file`, `relations`, `cross_file`, `search`, `skeleton`, `symbol`, `impact`, `trace`.
- **FR-002**: The explore router MUST route to `index` for `file` and `skeleton` intents.
- **FR-003**: The explore router MUST route to `arbor` for `relations`, `symbol`, `impact`, and `trace` intents.
- **FR-004**: The explore router MUST route to `codegraph` for `cross_file` intent.
- **FR-005**: The explore router MUST route to `semblem` for `search` intent.
- **FR-006**: The `NATIVE_EFFICIENT_TOOLS` list MUST include `explore`, `index`, `arbor`, `codegraph`, and `semblem` without "optional" qualifiers.
- **FR-007**: Tool descriptions for explore, arbor, codegraph, and semblem MUST position them as first-tier exploration tools.
- **FR-008**: CodeGraph MUST be upgraded to target version 1.5.0.
- **FR-009**: CodeGraph MUST expose commands: `explore`, `callers`, `callees`, `impact`, `affected`, `node`, `query`, `sync`.
- **FR-010**: CodeGraph MUST prefer native SQLite queries from `.codegraph/codegraph.db` and fall back to the CLI.
- **FR-011**: Arbor MUST expose commands: `callers`, `callees`, `map`, `diff`, `query`, `status`, `entry-points`, `file-graph`, `inspect`, `path`, `refactor`, `check`, `summary`.
- **FR-012**: Arbor MUST use native in-memory `graph.json` for `callers`, `callees`, and `trace_path` when the CLI is unavailable.
- **FR-013**: Semblem MUST wrap the upstream `semble` CLI v0.5.1 for `search`, `find-related`, and `savings` commands.
- **FR-014**: Semblem MUST support `--content docs/config/all` flags and remote git URLs.
- **FR-015**: Semblem MUST keep the native `n00n-search` BM25 index as a fallback when the CLI is unavailable or no embedder is configured.
- **FR-016**: The bash plugin MUST cache rtk availability per session.
- **FR-017**: The bash plugin MUST rewrite commands through `rtk` for git, cargo, rg, grep, find, ls, cat, head, tail, and other supported commands.
- **FR-018**: The bash plugin MUST pass `jq` and `yq` commands through unchanged.
- **FR-019**: Prompt hints MUST explicitly recommend rtk-wrapped bash for verbose shell commands.

### Key Entities

- **ExploreIntent**: The classification of a user query (file, relations, cross_file, search, skeleton, symbol, impact, trace) used to route to the appropriate backend.
- **ExploreRoute**: The mapping from intent to backend tool and input parameters.
- **CodeGraphIndex**: The SQLite database under `.codegraph/codegraph.db` containing nodes, edges, files, and FTS5 virtual table.
- **ArborGraph**: The in-memory graph loaded from `.arbor/graph.json` with nodes, edges, and symbol table.
- **SearchIndex**: The BM25 index under `.n00n/search/` managed by `n00n-search` with optional dense vector cache.
- **EmbedderConfig**: The user's choice of embedder (none, local vLLM, remote OpenAI-compatible) for semantic search.
- **ToolResult**: The common envelope returned to Lua plugins for rendering with llm_output, body, and is_error.
- **RtkRewrite**: The transformation of a shell command through rtk for token-efficient output.

## Success Criteria

### Measurable Outcomes

- **SC-001**: The explore router correctly routes at least 90% of test queries to the intended backend.
- **SC-002**: The `NATIVE_EFFICIENT_TOOLS` list and prompt hints consistently position explore/index/arbor/codegraph/semblem before grep/bash.
- **SC-003**: CodeGraph 1.5.0 commands (callers, callees, impact, node, query, sync) return correct results on a test repository.
- **SC-004**: Arbor commands (entry-points, file-graph, inspect, path, refactor, check, summary) return correct results on a test repository.
- **SC-005**: Semblem upstream CLI commands (search with remote URL, find-related, savings) return correct results.
- **SC-006**: Semblem falls back to BM25 when the upstream CLI is unavailable and returns keyword results.
- **SC-007**: RTK availability is cached per session and not re-checked on subsequent bash calls.
- **SC-008**: Tool definition token count does not increase beyond the current baseline after removing external-installation notes.
- **SC-009**: Tool call latency for arbor, codegraph, and semblem does not regress beyond the current baseline.

## Assumptions

- CodeGraph 1.5.0 SQLite database schema is backward-compatible with the 1.4.1 format used for native queries.
- Arbor 2.5.0 CLI output format for new commands (entry-points, file-graph, inspect, path, refactor, check, summary) is stable and parseable.
- The upstream `semble` CLI v0.5.1 is available on PATH or can be installed by the user for semantic search features.
- The native `n00n-search` BM25 index is sufficient for offline keyword search when the upstream CLI is unavailable.
- RTK is installed on the user's system for bash command rewriting; if not, bash commands run normally.
- The existing `n00n-config` tool output line budget can be extended with per-tool budgets if needed.
- The explore router's regex-based intent detection is sufficient for the majority of queries; false positives can be corrected with explicit `intent` parameter.
