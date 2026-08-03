# gix API Notes for n00n-git

This document documents the gix 0.70 API research and implementation decisions for the n00n-git binary.

## Implementation Status

### Commands using git CLI fallback
- **diff**: Uses `git diff --numstat` - gix diff API requires complex tree traversal and blob comparison
- **blame**: Uses `git blame --line-porcelain` - gix blame API requires complex resource management and line mapping

### Commands using gix (attempted, fell back to git CLI)
- **status**: Initially attempted gix status API, but the API requires a Progress trait implementation and complex Platform struct handling. Fell back to `git status --porcelain -b`.
- **log**: Initially attempted gix rev_walk API, but the API has changed significantly in 0.70 with different iterator patterns. Fell back to `git log --pretty=format`.
- **branches**: Initially attempted gix references API, but the iterator patterns are complex. Fell back to `git branch --format`.

## Why git CLI fallbacks?

The gix 0.70 API is designed for low-level git operations with complex trait requirements (Progress, Platform, etc.). For a simple binary that outputs JSON, the git CLI provides:
1. Simpler implementation
2. Stable output formats (--porcelain, --numstat, --line-porcelain)
3. Less error-prone parsing
4. Better performance for one-off operations

## Future improvements

If gix API usage is desired in the future:
1. Implement the Progress trait for status operations
2. Use gix's higher-level convenience methods when available
3. Consider using gix's `gix` CLI as a library instead of direct API calls
4. Benchmark git CLI vs gix for performance-critical paths
