default:
    @just --list

build *ARGS:
    cargo build {{ARGS}}

run *ARGS:
    cargo run {{ARGS}}

test *ARGS:
    cargo nextest run --workspace {{ARGS}}

lint:
    cargo clippy --all --tests -- -D warnings

lint-fix:
    cargo clippy --all --tests --fix

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

# Check local explore index health for arbor and codegraph.
explore-health PROJECT=".":
    #!/usr/bin/env bash
    set -euo pipefail
    project="{{PROJECT}}"
    echo "== arbor =="
    if command -v arbor >/dev/null 2>&1; then
        arbor status "$project" || true
    else
        echo "arbor CLI: not installed"
    fi
    if [[ -f "$project/.arbor/graph.json" ]]; then
        echo "arbor graph.json: present"
    else
        echo "arbor graph.json: missing"
    fi
    echo "== codegraph =="
    if command -v codegraph >/dev/null 2>&1; then
        codegraph --version
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
