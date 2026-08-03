# gix API Notes for n00n-git

This document documents the gix 0.70 API implementation for the n00n-git binary.

## Implementation Status

### Commands implemented with gix
- **status**: Uses `gix::open()`, `gix::Repository::head_name()`, `gix::Repository::index_or_empty()`, and `gix::Repository::worktree()`. Provides basic file status (clean/deleted) by checking index entries against the worktree.
- **log**: Uses `gix::Repository::head_commit()` and manual parent traversal to get commit history.
- **diff**: Uses `gix::Repository::rev_parse_single()`, `gix::Repository::diff_tree_to_tree()` to get file changes between two references.
- **branches**: Uses `gix::Repository::references()`, `gix::reference::iter::Platform::local_branches()` to list local branches.

### Commands not yet implemented
- **blame**: Not yet implemented with gix - API complexity requires further investigation of `gix::blame::file()` and related types.

## Implementation Notes

The gix 0.70 API is designed for low-level git operations. The current implementation focuses on the available features:
- `revision` - for log and diff reference parsing
- `status` - for index access
- `blob-diff` - for diff operations
- `blame` - enabled but not yet implemented

The status implementation is simplified compared to `git status --porcelain` - it only checks if indexed files exist in the worktree (clean/deleted), without tracking untracked files, staged changes, or conflicts. This is a limitation of the current implementation that could be improved with the full gix status API.
