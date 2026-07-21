# Changelog fragments

User-facing changes are recorded as small fragment files in this directory and
aggregated into `CHANGELOG.md` at release time with `just changelog`.

## Adding a fragment

Create a file named `<pr-or-issue>.<type>.md`, for example `123.fixed.md` or
`pr-456.performance.md`. The `<type>` must be one of:

| type          | section      | when                                      |
| ------------- | ------------ | ----------------------------------------- |
| `added`       | Added        | new feature or capability                 |
| `changed`     | Changed      | behavior change, no removal               |
| `fixed`       | Fixed        | bug fix                                   |
| `removed`     | Removed      | feature or public API removed             |
| `deprecated`  | Deprecated   | soon-to-be-removed                        |
| `security`    | Security     | vulnerability fixes                       |
| `performance` | Performance  | speed / memory improvement                |
| `docs`        | Docs         | documentation only                       |

The file body is the entry. Write it as one or more lines; a leading `-` makes
it a bullet, otherwise the line is bulleted for you.

```markdown
# 123.fixed.md
Fixed the `/tasks` window losing focus after a subagent finished.
```

## Skipping

Not every PR needs a fragment. The `changelog.yml` CI check is skipped when the
PR is labeled `no-changelog`, `dependencies`, or `ci`, or when no source files
(`src/`, `n00n-*/src/`, `plugins/`) changed.

## Building

`just changelog [VERSION]` reads every fragment here, groups them by type under
a new versioned heading, prepends the result to `CHANGELOG.md`, and removes the
consumed fragments.
