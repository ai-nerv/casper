BIN := app
CFLAGS = -O2 -Wall

include config.mk

define msg
hello world
endef

build: src/main.rs
	cargo build --release
	strip target/release/bin

install: build
	cp target/release/bin /usr/local/bin

.PHONY: build install
