.PHONY: build test install clean

PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin

build:
	cargo build --release

test:
	cargo test

install: build
	install -d "$(BINDIR)"
	install -m 0755 target/release/55pro "$(BINDIR)/55pro"

clean:
	cargo clean
