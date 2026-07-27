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

# Full CI check
ci: fmt-check lint pylint test gen-docs-check machete secrets
