Add native git and GitHub tooling via new n00n-git crate (gix-based) and Lua plugins.

- n00n-git crate: git status, log, diff, blame, add, commit, checkout, conflicts subcommands
- plugins/git: Lua plugin wrapping n00n-git binary with permission-scoped operations
- plugins/github: Lua plugin for GitHub REST API (issues, PRs, repo metadata, comments)
- Token sources: GITHUB_TOKEN env var, optional token parameter, or gh CLI fallback
- Rate limit detection with retry-after header parsing
- Permission scopes: git.read, git.write, github.read, github.write
