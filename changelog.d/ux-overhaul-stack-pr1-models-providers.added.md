Revive UX overhaul stack: ModelCatalog, canonical tool aliases, and normalization core.

Adds ModelCatalog for configured model validation, canonical tool name aliases (e.g., `read` → `read_file`), and normalization integration across agent, providers, and permissions. This is part 1 of 3 PRs reviving the abandoned UX overhaul stack from August 2026.

Included features:
- ModelCatalog: enforce configured model specs and availability checks
- Canonical tool aliases: legacy names supported as deprecated aliases
- Normalization integration: tool name resolution across policy boundaries
- Permission and provider review fixes

Superseded by main:
- ModelCatalog is new; main uses permissive model parsing
- Canonical aliases are new; main uses legacy names

Conflict resolution:
- Kept main's provider availability checks
- Kept main's fusion orchestration changes
- Integrated stack's ModelCatalog and canonical alias resolution
