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

machete:
    cargo machete

# Run the almas plugin across every mode (supervised/autonomous/swarm) and the
# new ibn/quorum/swarm toggles. Needs a configured provider (n00n auth).
almas-demo *ARGS:
    ./scripts/almas_demo.sh {{ARGS}}

# Full CI check
ci: fmt-check lint pylint test gen-docs-check machete
