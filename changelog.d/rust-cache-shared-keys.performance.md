Gave every `Swatinem/rust-cache` step in CI a `shared-key` grouped by platform and compile
profile, and restricted cache writes to `main` with `save-if`. The repository's cache usage was
21.38 GB against GitHub's 10 GB per-repository limit, evicting entries continuously and forcing
cold rebuilds on most jobs.
