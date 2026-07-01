.PHONY: build test install clean

PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin

build:
	cargo build --release

test:
	cargo test

install: build
	install -d "$(BINDIR)"
	install -m 0755 target/release/fivefivepro "$(BINDIR)/55pro"
	ln -sf "$(BINDIR)/55pro" "$(BINDIR)/5.5pro"

clean:
	cargo clean
