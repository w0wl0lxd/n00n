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

# Local verification without any caching wrapper. Both kache and sccache
# key on the rustc invocation, and content read during proc-macro expansion
# (include_dir!, config macros) is invisible to both (see
# kunobi-ninja/kache#760); an empty RUSTC_WRAPPER disables the config's
# rustc-wrapper and a fresh target dir forces real compilation. Use this
# when a local result must be trusted over CI.
verify-clean:
    #!/usr/bin/env bash
    set -euo pipefail
    target_dir="$(mktemp -d /tmp/n00n-verify-XXXXXX)"
    trap 'rm -rf "$target_dir"' EXIT
    RUSTC_WRAPPER= CARGO_TARGET_DIR="$target_dir" cargo check --workspace --all-targets

# Bump the pinned nightly toolchain (rust-toolchain.toml + CI/benchmarks/docs
# toolchain pins). Do this on a cadence, not eagerly: newer nightlies bring
# new clippy lints that must be fixed in the same change.
bump-nightly DATE:
    #!/usr/bin/env bash
    set -euo pipefail
    date="{{DATE}}"
    case "$date" in
      [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
      *) echo "bump-nightly: expected DATE in YYYY-MM-DD form, got '$date'" >&2; exit 1 ;;
    esac
    perl -pi -e "s/nightly-\d{4}-\d{2}-\d{2}/nightly-$date/g" rust-toolchain.toml .github/workflows/benchmarks.yml .github/workflows/docs.yml .github/workflows/release.yml .github/workflows/rust.yml
    grep -q "nightly-$date" rust-toolchain.toml || { echo "bump-nightly: no pin matched; nothing was changed" >&2; exit 1; }
    echo "toolchain bumped to nightly-$date; run cargo check --workspace and fix any new clippy lints"
