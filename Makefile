.DEFAULT_GOAL := build

SLINT_LSP ?= slint-lsp
SLINT_FILES := $(shell find ui -type f -name '*.slint' | sort)

.PHONY: build lint format format-check test check check-lib check-app test-router require-slint-lsp

build:
	cargo build --locked

release:
	./scripts/bundle-macos.sh --release

lint:
	cargo clippy --locked --all-targets -- -D warnings

format: require-slint-lsp
	cargo fix --allow-dirty
	cargo fmt --all
	$(SLINT_LSP) format -i $(SLINT_FILES)

format-check: require-slint-lsp
	cargo fmt --all -- --check
	@tmp=$$(mktemp); trap 'rm -f "$$tmp"' 0; \
	for file in $(SLINT_FILES); do \
		$(SLINT_LSP) format "$$file" > "$$tmp" && diff -u "$$file" "$$tmp" || exit 1; \
	done

require-slint-lsp:
	@command -v $(SLINT_LSP) >/dev/null || { echo 'Install slint-lsp 1.18.1: cargo install slint-lsp --version 1.18.1 --locked' >&2; exit 1; }

test:
	cargo test --locked

check: format-check lint test

check-lib:
	cargo check --locked --lib

check-app:
	cargo check --locked --bin switchx

test-router:
	cargo test --locked --test router
