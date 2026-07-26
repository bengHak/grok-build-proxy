override BINARY := grok-build-proxy
override CARGO := cargo
override DIST_DIR := dist
override MAKE := make
override PACKAGE_VERSION := $(shell cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(p["version"] for p in data["packages"] if p["name"] == "grok-build-proxy"))')

.PHONY: build build-arm64 build-amd64 prepare-dist dist verify-dist print-version test lint fmt check clean

build:
	$(CARGO) build --locked --release
	mkdir -p bin
	cp target/release/$(BINARY) bin/$(BINARY)

build-arm64:
	rustup target add aarch64-apple-darwin
	$(CARGO) build --locked --release --target aarch64-apple-darwin
	mkdir -p bin
	cp target/aarch64-apple-darwin/release/$(BINARY) bin/$(BINARY)-darwin-arm64

build-amd64:
	rustup target add x86_64-apple-darwin
	$(CARGO) build --locked --release --target x86_64-apple-darwin
	mkdir -p bin
	cp target/x86_64-apple-darwin/release/$(BINARY) bin/$(BINARY)-darwin-amd64

prepare-dist:
	rm -rf bin $(DIST_DIR)
	mkdir -p bin $(DIST_DIR)

dist: prepare-dist
	$(MAKE) build-arm64 build-amd64
	mkdir -p $(DIST_DIR)/arm64 $(DIST_DIR)/amd64
	cp bin/$(BINARY)-darwin-arm64 $(DIST_DIR)/arm64/$(BINARY)
	cp bin/$(BINARY)-darwin-amd64 $(DIST_DIR)/amd64/$(BINARY)
	cp LICENSE README.md $(DIST_DIR)/arm64/
	cp LICENSE README.md $(DIST_DIR)/amd64/
	tar -C $(DIST_DIR)/arm64 -czf $(DIST_DIR)/$(BINARY)_Darwin_arm64.tar.gz $(BINARY) LICENSE README.md
	tar -C $(DIST_DIR)/amd64 -czf $(DIST_DIR)/$(BINARY)_Darwin_amd64.tar.gz $(BINARY) LICENSE README.md
	cd $(DIST_DIR) && shasum -a 256 $(BINARY)_Darwin_arm64.tar.gz $(BINARY)_Darwin_amd64.tar.gz > checksums.txt
	rm -rf $(DIST_DIR)/arm64 $(DIST_DIR)/amd64
	$(MAKE) verify-dist

verify-dist:
	@test "$$(wc -l < $(DIST_DIR)/checksums.txt | tr -d ' ')" = "2"
	@test "$$(awk 'NF == 2 && $$1 ~ /^[0-9a-f]{64}$$/ { print $$2 }' $(DIST_DIR)/checksums.txt)" = "$$(printf '%s\n' $(BINARY)_Darwin_arm64.tar.gz $(BINARY)_Darwin_amd64.tar.gz)"
	cd $(DIST_DIR) && shasum -a 256 -c checksums.txt
	@test "$$(tar -tzf $(DIST_DIR)/$(BINARY)_Darwin_arm64.tar.gz | LC_ALL=C sort)" = "$$(printf '%s\n' $(BINARY) LICENSE README.md | LC_ALL=C sort)"
	@test "$$(tar -tzf $(DIST_DIR)/$(BINARY)_Darwin_amd64.tar.gz | LC_ALL=C sort)" = "$$(printf '%s\n' $(BINARY) LICENSE README.md | LC_ALL=C sort)"

print-version:
	@printf '%s\n' $(PACKAGE_VERSION)

test:
	$(CARGO) test --locked --all-targets

lint:
	$(CARGO) clippy --locked --all-targets --all-features -- -D warnings

fmt:
	$(CARGO) fmt

check:
	$(CARGO) fmt --check
	$(CARGO) clippy --locked --all-targets --all-features -- -D warnings
	$(CARGO) test --locked --all-targets
	sh -n install.sh
	./scripts/check-build-contract.sh

clean:
	$(CARGO) clean
	rm -rf bin $(DIST_DIR)
