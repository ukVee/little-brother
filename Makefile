# cement-ir — the Rust runtime core's build/install/test command.
#
# The shell `cement-ir` entry point stays the only surface; this Makefile is
# the repo's command for the Rust core. The binary is `cement-ir-rt` and is
# INSTALLED to /usr/local/bin (2026-09-26 decision: root-owned, because it
# sits on the sudo auth path). It must never be run out of target/ — a
# `cargo clean` must not be able to break sudo. `cement-ir doctor` checks the
# installed binary's presence and freshness against these sources.
#
# TWO STEPS, TWO USERS -- and `install` does NOT build:
#
#   make build          # as you; your toolchain, your target/ dir
#   sudo make install   # as root; copies the built binary, compiles nothing
#
# `install: build` was wrong, and the first real attempt (2026-09-26) is the
# proof: `sudo make install` tried to build AS ROOT, and root has no rustup
# default toolchain -- `rustup default stable` as your user does not touch
# /root/.rustup, so it failed twice with the same message. Even where root does
# have a toolchain, building as root duplicates the toolchain and leaves
# root-owned artifacts in crates/target/ that make your next ordinary
# `make build` fail on permissions. So install compiles nothing, and refuses to
# install a binary older than the sources (the same staleness rule doctor
# applies) rather than quietly cementing a stale build on the sudo auth path.
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

install:
	@test "$$(id -u)" = 0 || { \
	  echo "make install: needs root (the binary is root-owned; it sits on the sudo auth path)."; \
	  echo "      sudo make -C $(CURDIR) install"; \
	  echo "  Refusing early ON PURPOSE: /usr/local/bin is ukv-writable by ACL here, so"; \
	  echo "  \`install -o root\` as a normal user COPIES the file and only then fails on the"; \
	  echo "  chown -- leaving a 0600 non-executable half-install on the auth path."; \
	  exit 1; \
	}
	@test -x $(RT) || { \
	  echo "make install: $(RT) is not built."; \
	  echo "  Build it as YOUR user first (never as root):"; \
	  echo "      make -C $(CURDIR) build"; \
	  echo "  then:  sudo make -C $(CURDIR) install"; \
	  exit 1; \
	}
	@stale=$$(find crates -name '*.rs' -newer $(RT) -print -quit 2>/dev/null); \
	  test -z "$$stale" || { \
	    echo "make install: $(RT) is STALE ($$stale is newer)."; \
	    echo "  Rebuild as your user:  make -C $(CURDIR) build"; \
	    echo "  Installing it now would put a stale binary on the sudo auth path."; \
	    exit 1; \
	  }
	install -m 0755 -o root -g root $(RT) $(PREFIX)/cement-ir-rt
	@echo "installed $(PREFIX)/cement-ir-rt"
	@$(PREFIX)/cement-ir-rt --version
	@# This install's own `sudo` triggers face auth, which socket-activates a
	@# daemon running the binary we are ABOUT to replace -- and it then stays
	@# warm for IRFACED_IDLE_SECS. So the first test after an install silently
	@# exercises the OLD code. That produced two misleading results in a row on
	@# 2026-09-26 (a spin measurement and a latch test, both of the wrong
	@# binary). Stop it here: the socket unit keeps listening and the next auth
	@# spawns what was just installed.
	@systemctl stop irfaced.service 2>/dev/null \
	  && echo "stopped the warm irfaced (the socket reactivates it on the next auth)" \
	  || true
