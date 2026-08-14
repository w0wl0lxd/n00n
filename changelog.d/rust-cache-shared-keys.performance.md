Gave every `Swatinem/rust-cache` step in CI a `shared-key` grouped by platform and compile
profile, and restricted cache writes to `main` with `save-if`. The repository's cache usage was
21.38 GB against GitHub's 10 GB per-repository limit, evicting entries continuously and forcing
cold rebuilds on most jobs.

Also moved every `dtolnay/rust-toolchain` step ahead of the `rust-cache` step that follows it.
`rust-cache` derives its key from the active compiler, so running it first keyed each cache for
whichever toolchain `rust-toolchain.toml` selected rather than the one the job goes on to install.
The MSRV job was the clearest case: it cached under `linux-msrv` for the default toolchain and
then built with 1.97.0. Coverage was affected too, because its `llvm-tools-preview` component was
added after the cache step.
