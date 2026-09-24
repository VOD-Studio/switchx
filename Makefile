.DEFAULT_GOAL := build

.PHONY: build lint format format-check test check

build:
	cargo build --locked

lint:
	cargo clippy --locked --all-targets -- -D warnings

format:
	cargo fmt --all

format-check:
	cargo fmt --all -- --check

test:
	cargo test --locked

check: format-check lint test
