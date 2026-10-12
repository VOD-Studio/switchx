.DEFAULT_GOAL := build

SLINT_LSP ?= slint-lsp
PYTHON ?= python3
SLINT_FILES := $(shell find ui -type f -name '*.slint' | sort)

.PHONY: build clean release lint format format-check test check check-lib check-app test-router require-slint-lsp release-check test-release prepare-release

build:
	cargo build --locked

clean:
	cargo clean

release:
	./scripts/bundle-macos.sh --release

lint:
	cargo clippy --locked --workspace --all-targets -- -D warnings

format: require-slint-lsp
	cargo fix --workspace --allow-dirty
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
	cargo test --locked --workspace

check: release-check test-release format-check lint test

release-check:
	$(PYTHON) scripts/release.py check

test-release:
	$(PYTHON) -m unittest discover -s scripts/tests -v

prepare-release:
	@test -n "$(VERSION)" || { echo 'Usage: make prepare-release VERSION=0.1.0 (Python 3.11+ required)' >&2; exit 2; }
	$(PYTHON) scripts/release.py prepare "$(VERSION)"

check-lib:
	cargo check --locked --lib

check-app:
	cargo check --locked --bin switchx

test-router:
	cargo test --locked --test router
