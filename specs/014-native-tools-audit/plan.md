# Implementation Plan: Native Tools Audit

**Branch**: `014-native-tools-audit` | **Date**: 2026-08-03 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/014-native-tools-audit/spec.md`

**Note**: This is a research/discovery feature. No code implementation is required. The deliverables are documentation (spec.md, plan.md, tasks.md) and a follow-up GitHub issue.

## Summary

This feature is a research audit to identify ad-hoc CLI/API calls in n00n workflows that should become native tools. The primary requirement is to document a systematic audit methodology, produce a ranked candidate backlog with token-savings estimates, and recommend follow-up issues for the highest-priority candidates. The technical approach is based on codebase evidence (AGENTS.md, skill files, justfile, plugin code) since session transcript data is unavailable. The audit identifies cargo, just, and docs as the top candidates, with cargo being the highest priority due to high frequency and token cost.

## Technical Context

**Language/Version**: N/A (research/discovery feature, no code implementation)

**Primary Dependencies**: N/A (documentation only)

**Storage**: N/A

**Testing**: N/A (verification is manual review of documentation against GitHub issue #239 acceptance criteria)

**Target Platform**: N/A

**Project Type**: Research/discovery (documentation)

**Performance Goals**: N/A

**Constraints**: N/A

**Scale/Scope**: Documentation only; audit covers 7 candidates across the n00n codebase

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

No constitution violations. This is a documentation-only feature with no code changes, so KISS, DRY, SRP, and YAGNI principles do not apply. The feature is already scoped to research and documentation per the GitHub issue.

## Project Structure

### Documentation (this feature)

```text
specs/014-native-tools-audit/
├── spec.md              # Feature specification (user stories, requirements, success criteria)
├── plan.md              # This file (implementation plan)
├── research.md          # Audit findings, candidate table, design notes (already completed)
└── tasks.md             # Task breakdown for documentation and follow-up issue creation
```

### Source Code (repository root)

No source code changes are required for this feature. The audit produces documentation and a follow-up GitHub issue; implementation of the recommended tools is deferred to future features.

**Structure Decision**: Documentation-only feature. No source code structure changes.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No violations. N/A.
