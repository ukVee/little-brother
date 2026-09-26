# cement-ir — the Rust runtime core's build/install/test command.
#
# The shell `cement-ir` entry point stays the only surface; this Makefile is
# the repo's command for the Rust core. The binary is `cement-ir-rt` and is
# INSTALLED to /usr/local/bin (2026-09-26 decision: root-owned, because it
# sits on the sudo auth path). It must never be run out of target/ — a
# `cargo clean` must not be able to break sudo. `cement-ir doctor` checks the
# installed binary's presence and freshness against these sources.
CARGO ?= cargo
MANIFEST := crates/Cargo.toml
RT := crates/target/release/cement-ir-rt
PREFIX ?= /usr/local/bin

.PHONY: build test clippy install
build:
	$(CARGO) build --release --manifest-path $(MANIFEST)

test:
	$(CARGO) test --manifest-path $(MANIFEST)

clippy:
	$(CARGO) clippy --manifest-path $(MANIFEST) --all-targets -- -D warnings

install: build
	install -m 0755 -o root -g root $(RT) $(PREFIX)/cement-ir-rt
