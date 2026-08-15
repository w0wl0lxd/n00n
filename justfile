default:
    @just --list

build *ARGS:
    cargo build {{ARGS}}

run *ARGS:
    cargo run {{ARGS}}

test *ARGS:
    cargo nextest run --workspace {{ARGS}}

# Cursor Phase 0 capture / visibility
cursor-mitm-setup:
    mise run mitm-setup

cursor-capture *ARGS:
    scripts/cursor_capture.sh {{ARGS}}

cursor-capture-e2e:
    scripts/cursor_capture_e2e.sh

cursor-export DUMP:
    scripts/cursor_export_flows.sh {{DUMP}}

cursor-fuzz-frames:
    cargo test -p n00n-providers --lib connect::tests::fuzz_ -- --nocapture

lint:
    cargo clippy --all --all-targets -- -D warnings

lint-fix:
    cargo clippy --all --all-targets --fix

fmt-check:
    cargo fmt --all -- --check
    stylua --check plugins/

fmt:
    cargo fmt --all
    stylua plugins/

pylint:
    ruff check scripts/
    ty check scripts/

gen-docs:
    cargo run -p n00n-docgen

gen-docs-check:
    cargo run -p n00n-docgen -- --check

docs: gen-docs
docs-check: gen-docs-check

# Aggregate changelog.d fragments into CHANGELOG.md (VERSION defaults to the
# workspace version in Cargo.toml).
changelog VERSION:
    ./scripts/build-changelog.sh {{VERSION}}

machete:
    cargo machete

# Run the almas plugin across every mode (supervised/autonomous/swarm) and the
# new ibn/quorum/swarm toggles. Needs a configured provider (n00n auth).
almas-demo *ARGS:
    ./scripts/almas_demo.sh {{ARGS}}

setup-git-hooks:
    git config --unset core.hooksPath 2>/dev/null || true
    hk install

secrets:
    gitleaks detect --source . --redact --no-banner --config .gitleaks.toml

# Check local explore index health for codegraph.
explore-health PROJECT=".":
    #!/usr/bin/env bash
    set -euo pipefail
    project="{{PROJECT}}"
    echo "== codegraph =="
    if command -v codegraph >/dev/null 2>&1; then
        codegraph --version || true
    else
        echo "codegraph CLI: not installed"
    fi
    if [[ -d "$project/.codegraph" ]]; then
        echo "codegraph index: present"
    else
        echo "codegraph index: missing"
    fi

# Full CI check
ci: fmt-check lint pylint test gen-docs-check machete secrets

# Local verification without the kache wrapper. The layered kache->sccache
# cache can serve stale rlibs across worktrees when content is read during
# proc-macro expansion (include_dir!, config macros) but is invisible to the
# cache key (see kunobi-ninja/kache#760); use this when a local result must
# be trusted over CI.
verify-clean TARGET_DIR="/tmp/n00n-verify":
    RUSTC_WRAPPER=sccache CARGO_TARGET_DIR={{TARGET_DIR}} cargo check --workspace --all-targets

# Bump the pinned nightly toolchain (rust-toolchain.toml + CI/benchmarks/docs
# toolchain pins). Do this on a cadence, not eagerly: newer nightlies bring
# new clippy lints that must be fixed in the same change.
bump-nightly DATE:
    sed -i 's|nightly-[0-9]*-[0-9]*-[0-9]*|nightly-{{DATE}}|' rust-toolchain.toml
    sed -i 's|nightly-[0-9]*-[0-9]*-[0-9]*|nightly-{{DATE}}|' .github/workflows/benchmarks.yml .github/workflows/docs.yml .github/workflows/release.yml
    echo "toolchain bumped to nightly-{{DATE}}; run cargo check --workspace and fix any new clippy lints"
