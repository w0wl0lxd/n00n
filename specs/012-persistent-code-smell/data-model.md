# Data Model: Persistent code-smell and comment index

## Tantivy document schema

| Field       | Type   | Indexed | Stored | Notes                                      |
|-------------|--------|---------|--------|--------------------------------------------|
| `path`      | TEXT   | yes     | yes    | Relative repo path                         |
| `start_line`| u64    | no      | yes    | 1-based start line                         |
| `end_line`  | u64    | no      | yes    | 1-based end line                           |
| `kind`      | TEXT   | yes     | yes    | `conflict`, `todo`, `fixme`, `hack`, `placeholder` |
| `message`   | TEXT   | yes     | yes    | Short human-readable summary               |
| `content`   | TEXT   | yes     | yes    | Full finding content or hunk               |
| `language`  | TEXT   | yes     | yes    | File extension or `text`                   |

## Rust types

- `SmellFinding`: maps 1:1 to the indexed document.
- `SmellIndex`: wraps Tantivy `Index` and exposes `open_or_create`, `update(repo, progress)`, `search(query, kind_filter, top_k)`, and helpers.
- `SmellError`: thiserror enum covering I/O, Tantivy, config, and Git errors.

## Storage layout

```
.n00n/smells/
├── tantivy_index/
└── metadata.json
```

`metadata.json` stores index version and document count, mirroring `n00n-search`.
