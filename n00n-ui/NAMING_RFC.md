# Naming RFC: n00n UX vocabulary

Status: accepted for the 2026 UX migration.

## Decision

Built-in tools use canonical `verb_noun` snake_case names no longer than 32 characters. Existing names remain deprecated aliases for at least one minor release. Model-facing definitions contain canonical names only.

Slash commands use friendly, categorized metadata with legacy short commands as aliases. Internal agent and protocol statuses remain stable; presentation code maps them to sentence-case labels.

## Alias contract

Each tool has one canonical name, zero or more deprecated aliases, a category, source, and warning policy. Registration rejects canonical/alias collisions deterministically. Aliases normalize before filtering, permissions, deferred activation, dispatch, and persistence lookups. One structured warning is emitted per alias per registry lifetime.

MCP `server.tool` and provider wire-name translations are outside this migration and remain unchanged.

## Command contract

Command metadata contains a canonical path, aliases, friendly label, category, argument policy, and handler identity. Built-ins have deterministic precedence over conflicting custom commands. Lua handlers remain supported.

## Accessibility contract

User-visible states include a text or glyph signal in addition to color. Footer hints collapse by priority on narrow terminals. `NO_COLOR` removes semantic color without removing state text. High-contrast themes override values, not component logic.

## Review checklist

- Canonical name follows `verb_noun` and is at most 32 characters.
- Legacy aliases resolve at every external boundary and never appear in model definitions.
- Description says when to use and when not to use the operation.
- Machine-readable protocol values are unchanged.
- New UI remains keyboard reachable and understandable without color.
- Generated docs and migration tables match runtime metadata and tests.
