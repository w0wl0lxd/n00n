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

fmt:
    cargo fmt --all

pylint:
    ruff check scripts/
    ty check scripts/

# Full CI check
ci: fmt-check lint pylint test
