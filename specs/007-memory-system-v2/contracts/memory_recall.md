# Contract: Memory recall and prompt injection

## Search semantics

- `search` uses **keyword/token overlap** and tag filters only. Not semantic paraphrase.
- Tool description and search output MUST state this limitation.
- Results include `score=` for transparency.

## Lite layer prompt injection

- Only entries with `layer: lite` in frontmatter are injected into `after_instructions`.
- Max 5 entries, 120 chars per line, 800 bytes total.
- Content is sanitized: control chars stripped, markdown heading/list prefixes removed, common injection phrases stripped.
- Injected text is prefixed with `Project memory (lite):` and formatted as bullet list with path labels.
- Untrusted user-editable content MUST NOT be injected without sanitization.

## Path safety

- All file paths use existing `safe_resolve` sandbox (unchanged).
- No `paths` frontmatter field in v2 (deferred — security undefined).

## Blackboard boundary

- **Memory**: durable project knowledge across sessions.
- **Blackboard**: ephemeral multi-agent coordination (claims, tasks).
- No automatic bridging in v2.
