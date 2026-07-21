<!--
  PR titles follow Conventional Commits and are validated by semantic-pr and the
  commit-msg hook, e.g. "feat(lua): add hover provider". Agent-authored PRs may
  opt in by ending the title with 🤖🤖🤖.
-->
## Summary

<!-- What changed and why. Link the issue with "Fixes #123" or "Relates to #123". -->

## Type

<!-- Check the one that fits (must match the PR title type). -->
- [ ] feat     – new functionality
- [ ] fix      – bug fix
- [ ] docs     – documentation only
- [ ] style    – formatting / no logic change
- [ ] refactor – code restructure, same behavior
- [ ] perf     – performance improvement
- [ ] test     – tests only
- [ ] build    – build / deps / tooling
- [ ] ci       – CI / workflows
- [ ] chore    – housekeeping
- [ ] revert   – revert a change

## Changelog fragment

<!-- Add a file under changelog.d/ (see changelog.d/README.md), unless this PR
     is labeled `no-changelog`, `dependencies`, or `ci`. -->
- [ ] Added `changelog.d/<pr>.<type>.md`

## Test plan

<!-- How did you verify it? `just ci` runs the local equivalent of CI. -->

## AI use

<!-- Per CONTRIBUTING.md: describe how you used AI, include prompts if unsure. -->
