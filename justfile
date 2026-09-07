set shell := ["sh", "-cu"]

fmt:
    cargo fmt

fmt-check:
    cargo fmt -- --check

clippy:
    cargo clippy --all-targets -- -D warnings

test:
    cargo test

check: fmt-check clippy test
