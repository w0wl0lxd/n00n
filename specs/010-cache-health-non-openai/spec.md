# Feature Specification: CacheHealth for all non-OpenAI providers

**Feature Branch**: `feat/cache-health-non-openai`

**Created**: 2026-08-03

**Status**: Approved

**Input**: Finish the Q3 F2 slice: make every provider emit `ProviderEvent::CacheHealth` after a turn, so the status bar cache timer is accurate and clears for providers without cache support.

## User Scenarios & Testing

### User Story 1 — Status bar shows correct cache state for OpenAI-compatible providers (P1)

As a user running any OpenAI-compatible provider, I want the cache timer to show a hit and 5-minute TTL when cache tokens are reported, and clear when none are.

**Why this priority**: Currently only OpenRouter and Mistral emit CacheHealth; other compat providers leave a stale badge from a previous provider.

**Independent Test**: Unit test `OpenAiCompatProvider::emit_cache_health` with a mock `TokenUsage` containing `cache_read` and `cache_creation` and a flume channel.

**Acceptance Scenarios**:

1. **Given** an OpenAI-compat provider with `cache_ttl_seconds = 300`, **when** a response has `cache_read > 0`, **then** a `CacheHealth { kind: Prompt, valid_until: now+300, ttl_seconds: 300, hit: true }` event is sent.
2. **Given** a provider with `cache_ttl_seconds = 0`, **when** a response has zero cache tokens, **then** a `CacheHealth { valid_until: 0 }` event is sent.

---

### User Story 2 — Non-OpenAI providers clear the cache badge (P1)

As a user on Cursor, Copilot, or Devin, I want the cache badge to disappear because these providers do not support prompt caching.

**Why this priority**: Without an event, the UI keeps the previous provider's cache badge, which is misleading.

**Independent Test**: Unit tests in `n00n-providers` asserting `ProviderEvent::CacheHealth { valid_until: 0 }` after a mocked stream for `cursor`, `copilot`, and `devin`.

**Acceptance Scenarios**:

1. **Given** a Cursor turn, **when** the stream completes, **then** a `CacheHealth { valid_until: 0 }` event is emitted.
2. **Given** a Copilot turn, **when** the stream completes, **then** a `CacheHealth { valid_until: 0 }` event is emitted.
3. **Given** a Devin turn, **when** the stream completes, **then** a `CacheHealth { valid_until: 0 }` event is emitted.

## Requirements

### Functional Requirements

- **FR-001**: `OpenAiCompatConfig` gains a `cache_ttl_seconds` field.
- **FR-002**: `build.rs` reads an optional `cache_ttl_seconds` u64 from provider TOMLs, defaulting to 0.
- **FR-003**: `OpenAiCompatProvider` exposes an `emit_cache_health` method that emits `ProviderEvent::CacheHealth` based on `TokenUsage` and the configured TTL.
- **FR-004**: OpenRouter and Mistral use `emit_cache_health` and remove their duplicate manual emission.
- **FR-005**: All other `OpenAiCompatProvider` callers emit CacheHealth via the helper.
- **FR-006**: Local/custom/copilot responses paths and cursor/copilot/devin emit a `valid_until=0` CacheHealth.
- **FR-007**: Anthropic, Google, and OpenAI platform continue to manage their own CacheHealth and are unchanged.

### Success Criteria

- **SC-001**: `cargo test -p n00n-providers` passes.
- **SC-002**: No provider `stream_message` returns without emitting `ProviderEvent::CacheHealth`.
- **SC-003**: `cargo clippy --all --tests -- -D warnings` passes.

### Assumptions

- Existing Anthropic, Google, and OpenAI platform CacheHealth logic is correct and stays as-is.
- OpenRouter and Mistral cache TTL is 300 seconds.
- Providers without a cache TTL do not support prompt caching.
