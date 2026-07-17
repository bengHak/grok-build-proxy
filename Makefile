BINARY := grok-build-proxy
VERSION ?= $(shell cat VERSION)
HOST_TARGET := $(shell rustc -vV | sed -n 's/^host: //p')
DIST_DIR := dist

.PHONY: build build-arm64 build-amd64 dist test lint fmt check clean

build:
	GROK_BUILD_PROXY_VERSION=$(VERSION) cargo build --release
	mkdir -p bin
	cp target/release/$(BINARY) bin/$(BINARY)

build-arm64:
	rustup target add aarch64-apple-darwin
	GROK_BUILD_PROXY_VERSION=$(VERSION) cargo build --release --target aarch64-apple-darwin
	mkdir -p bin
	cp target/aarch64-apple-darwin/release/$(BINARY) bin/$(BINARY)-darwin-arm64

build-amd64:
	rustup target add x86_64-apple-darwin
	GROK_BUILD_PROXY_VERSION=$(VERSION) cargo build --release --target x86_64-apple-darwin
	mkdir -p bin
	cp target/x86_64-apple-darwin/release/$(BINARY) bin/$(BINARY)-darwin-amd64

dist: clean build-arm64 build-amd64
	mkdir -p $(DIST_DIR)/arm64 $(DIST_DIR)/amd64
	cp bin/$(BINARY)-darwin-arm64 $(DIST_DIR)/arm64/$(BINARY)
	cp bin/$(BINARY)-darwin-amd64 $(DIST_DIR)/amd64/$(BINARY)
	cp LICENSE README.md $(DIST_DIR)/arm64/
	cp LICENSE README.md $(DIST_DIR)/amd64/
	tar -C $(DIST_DIR)/arm64 -czf $(DIST_DIR)/$(BINARY)_Darwin_arm64.tar.gz $(BINARY) LICENSE README.md
	tar -C $(DIST_DIR)/amd64 -czf $(DIST_DIR)/$(BINARY)_Darwin_amd64.tar.gz $(BINARY) LICENSE README.md
	cd $(DIST_DIR) && shasum -a 256 $(BINARY)_Darwin_arm64.tar.gz $(BINARY)_Darwin_amd64.tar.gz > checksums.txt
	rm -rf $(DIST_DIR)/arm64 $(DIST_DIR)/amd64

test:
	cargo test --all-targets

lint:
	cargo clippy --all-targets --all-features -- -D warnings

fmt:
	cargo fmt

check:
	cargo fmt --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test --all-targets
	sh -n install.sh
	GROK_BUILD_PROXY_VERSION=$(VERSION) cargo build --release

clean:
	cargo clean
	rm -rf bin $(DIST_DIR)
