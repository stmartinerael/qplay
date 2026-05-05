.PHONY: all build run test fmt clippy doc

all: build

build:
	cargo build

run:
	cargo run

test:
	cargo test

fmt:
	cargo fmt

clippy:
	cargo clippy

doc:
	cargo doc --no-deps
