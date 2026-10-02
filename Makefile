# Building and installing crystal. `make install` puts it in ~/.local/bin;
# `make install PREFIX=/usr/local` puts it in /usr/local/bin.

PREFIX ?= $(HOME)/.local
BIN := $(PREFIX)/bin

.PHONY: build release test lint install uninstall clean

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

# A running daemon goes on running the crystal it was started from, so after
# copying the new one in, it's restarted on it: running sessions come back,
# Claude Code in its conversation. The old file is removed first, not
# written over, since macOS kills a program whose signed file changes under
# it.
install: release
	mkdir -p $(BIN)
	rm -f $(BIN)/crystal
	cp target/release/crystal $(BIN)/crystal
	$(BIN)/crystal restart-server

uninstall:
	-$(BIN)/crystal kill-server
	rm -f $(BIN)/crystal

clean:
	cargo clean
